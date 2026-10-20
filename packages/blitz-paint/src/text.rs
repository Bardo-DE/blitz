use anyrender::{AntiAliasing, PaintScene};
use blitz_dom::CopyStyleGetters;
use blitz_dom::{
    BaseDocument, NodeId,
    node::{TextBrush, TextOverflowMarker},
};
use kurbo::{Affine, Rect};
use parley::{Affinity, Cursor, GlyphRun, Layout, Line, PositionedLayoutItem, Selection};
use peniko::Fill;
use std::sync::Arc;

use crate::color::{Color, ToColorColor as _};
use crate::text_decorations::{
    DecorationRunGeometry, DecorationStackEntry, LineDecoration, ResolvedDecoration,
    ResolvedTextShadow, WinAscentCache, draw_line_text_shadows, flush_line_decorations,
    resolve_decoration_entry,
};
use crate::{FONT_EMBOLDEN_ENABLED, SELECTION_COLOR};

/// Draw the backgrounds of inline elements (e.g. `<span style="background: ...">`).
///
/// Each glyph run carries the node id of the innermost inline element it belongs to
/// (via its brush). We look up that node's `background-color` and, if non-transparent,
/// fill a rectangle covering the run's advance and its font's ascent/descent so that the
/// background sits behind the text.
///
/// The inline root's own background is painted separately (as a normal block box), so
/// runs belonging to the root are skipped to avoid drawing it twice.
pub(crate) fn draw_inline_backgrounds<'a>(
    scene: &mut impl PaintScene,
    lines: impl Iterator<Item = Line<'a, TextBrush>>,
    doc: &BaseDocument,
    transform: Affine,
    inline_root_id: NodeId,
) {
    for line in lines {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };

            let node_id = glyph_run.style().brush.id;
            if node_id == inline_root_id {
                continue;
            }

            let Some(styles) = doc.get_node(node_id).and_then(|node| node.primary_styles()) else {
                continue;
            };

            let current_color = styles.clone_color();
            let bg_color = styles
                .get_background()
                .background_color
                .resolve_to_absolute(&current_color)
                .as_srgb_color();
            if bg_color == Color::TRANSPARENT {
                continue;
            }

            let metrics = glyph_run.run().font_metrics();
            let x = glyph_run.offset() as f64;
            let w = glyph_run.advance() as f64;
            let baseline = glyph_run.baseline() as f64;
            let y0 = baseline - metrics.ascent as f64;
            let y1 = baseline + metrics.descent as f64;
            let rect = Rect::new(x, y0, x + w, y1);

            scene.fill(
                Fill::NonZero,
                transform,
                bg_color,
                None,
                &rect,
                AntiAliasing::Enabled,
            );
        }
    }
}

/// Per glyph run values recorded while walking a line, so the line can be painted in
/// passes (shadows beneath, then glyphs) without re-resolving styles.
pub(crate) struct RunRecord {
    pub(crate) fill_color: Color,
    pub(crate) shadows: Option<Arc<[ResolvedTextShadow]>>,
    /// How many of the run's glyphs are drawn: all of them, unless a `text-overflow` cut
    /// falls in or before the run.
    pub(crate) glyphs: usize,
}

/// Reusable scratch storage for drawing text across all inline formatting contexts in a
/// document. The vectors are cleared between uses without releasing their allocations.
#[derive(Default)]
pub(crate) struct DrawTextContext {
    stack: Vec<DecorationStackEntry>,
    path_scratch: Vec<NodeId>,
    deco_boxes: Vec<LineDecoration>,
    run_records: Vec<RunRecord>,
    win_ascent_ratios: WinAscentCache,
}

#[derive(Clone, Copy)]
pub(crate) struct TextPaintOptions {
    pub scale: f64,
    pub brush_override: Option<Color>,
}

/// Draw the glyphs and decorations of `lines`. With a `text_overflow` marker, lines that
/// overflow are cut where it says and end with the marker.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stroke_text<'a>(
    scene: &mut impl PaintScene,
    lines: impl Iterator<Item = Line<'a, TextBrush>>,
    doc: &BaseDocument,
    transform: Affine,
    options: TextPaintOptions,
    inline_root_id: NodeId,
    context: &mut DrawTextContext,
    text_overflow: Option<&TextOverflowMarker>,
) {
    let TextPaintOptions {
        scale,
        brush_override,
    } = options;
    let DrawTextContext {
        stack,
        path_scratch,
        deco_boxes,
        run_records,
        win_ascent_ratios,
    } = context;
    stack.clear();
    path_scratch.clear();
    deco_boxes.clear();
    run_records.clear();

    // Persistent stack mirroring the ancestor path (inline root -> current run's
    // node) as we walk the runs. The `text-decoration-*` properties are *not*
    // inherited; instead a decoration set on an ancestor is propagated to the
    // descendant text it wraps. Rather than re-resolving styles for the whole
    // ancestor chain on every run, we cache each node's resolved values here and,
    // for each run, only resolve styles for the nodes newly descended into (popping
    // as we ascend). `path_scratch` is a reusable buffer for the run's node path.
    for (line_index, line) in lines.enumerate() {
        let cut = text_overflow.and_then(|marker| marker.lines.get(line_index).copied().flatten());
        // Glyphs of this line still to draw before the cut.
        let mut glyphs_left = cut.map_or(usize::MAX, |cut| cut.glyphs_kept);

        // Decorations accumulated for this line, keyed by decorating box, so each box is
        // painted once (spanning all its runs) using its own font — matching Firefox, which
        // draws one decoration per box rather than one stepped segment per differently-sized
        // run. Clearing preserves the allocation for the next line and inline context.
        deco_boxes.clear();
        run_records.clear();
        let mut line_has_shadows = false;
        // The underline of the text an input method is composing on this line, spanning its runs.
        let mut composing: Option<LineDecoration> = None;

        // Pass 1: resolve styles for every glyph run on the line (recording the values
        // needed to paint it) and accumulate decorations. Painting is deferred so text
        // shadows can be drawn beneath *all* of the line's text and decorations.
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            let font = run.font();
            let font_size = run.font_size();
            let metrics = run.font_metrics();
            let style = glyph_run.style();

            let css_font_size = font_size as f64 / scale;

            // Reconcile the stack with this run's ancestor path. Build the path
            // (inline root -> run node), keep the shared prefix already on the stack,
            // pop the rest, and resolve styles only for the newly-descended nodes.
            path_scratch.clear();
            let mut walk_id = Some(style.brush.id);
            while let Some(node_id) = walk_id {
                path_scratch.push(node_id);
                if node_id == inline_root_id {
                    break;
                }
                walk_id = doc.get_node(node_id).and_then(|node| node.parent);
            }
            path_scratch.reverse();

            let shared = stack
                .iter()
                .zip(path_scratch.iter())
                .take_while(|(entry, node_id)| entry.node_id == **node_id)
                .count();
            stack.truncate(shared);
            for &node_id in &path_scratch[shared..] {
                let entry = resolve_decoration_entry(doc, node_id, stack.last(), scale);
                stack.push(entry);
            }

            // The glyph colour and `text-shadow` come from the run's own node (the stack
            // top): both inherit, so the innermost inline element already carries the
            // right values. Shadows are not part of a `background-clip: text` mask, so
            // they are dropped when the brush is overridden.
            let (fill_color, shadows) = stack
                .last()
                .map(|e| (e.fill_color, e.text_shadows.clone()))
                .unwrap_or((Color::BLACK, None));
            let fill_color = brush_override.unwrap_or(fill_color);
            let shadows = shadows.filter(|_| brush_override.is_none());
            line_has_shadows |= shadows.is_some();
            let glyphs = if cut.is_some() {
                let drawn = glyphs_left.min(glyph_run.glyphs().count());
                glyphs_left -= drawn;
                drawn
            } else {
                usize::MAX
            };
            run_records.push(RunRecord {
                fill_color,
                shadows,
                glyphs,
            });
            // Nothing of this run is drawn, so it has no decoration either.
            if glyphs == 0 {
                continue;
            }

            // Accumulate this run's contribution to each decorating box on its ancestor
            // path. The decoration is drawn once per box after the whole line has been
            // walked (see `flush_line_decorations`), so mixed font sizes within a box
            // produce a single straight line rather than one stepped segment per run.
            let geometry = DecorationRunGeometry {
                baseline: glyph_run.baseline(),
                ascent: metrics.ascent,
                descent: metrics.descent,
                underline_offset: metrics.underline_offset,
                underline_size: metrics.underline_size,
                strikethrough_size: metrics.strikethrough_size,
                font: font.clone(),
                font_size,
                css_font_size,
            };
            let run_node_id = style.brush.id;
            let run_x0 = glyph_run.offset() as f64;
            let mut run_x1 = run_x0 + glyph_run.advance() as f64;
            // Decorations stop at the cut.
            if let Some(cut) = cut {
                run_x1 = run_x1.min(cut.marker_x as f64);
            }

            for entry in stack.iter() {
                if entry.decoration.is_none() {
                    continue;
                }
                let idx = match deco_boxes.iter().position(|d| d.node_id == entry.node_id) {
                    Some(idx) => idx,
                    None => {
                        deco_boxes.push(LineDecoration {
                            node_id: entry.node_id,
                            deco: entry.decoration.clone().unwrap(),
                            min_x: f64::INFINITY,
                            max_x: f64::NEG_INFINITY,
                            own: None,
                            first: None,
                        });
                        deco_boxes.len() - 1
                    }
                };
                let acc = &mut deco_boxes[idx];
                acc.min_x = acc.min_x.min(run_x0);
                acc.max_x = acc.max_x.max(run_x1);
                if acc.first.is_none() {
                    acc.first = Some(geometry.clone());
                }
                // A run whose innermost node is the box itself is the box's own text, so
                // its font is the one Firefox positions and sizes the decoration with.
                if entry.node_id == run_node_id {
                    acc.own = Some(geometry.clone());
                }
            }

            // A text input's editor underlines the text an input method is composing.
            if style.underline.is_some() {
                let acc = composing.get_or_insert_with(|| LineDecoration {
                    node_id: run_node_id,
                    deco: ResolvedDecoration::composition_underline(fill_color),
                    min_x: f64::INFINITY,
                    max_x: f64::NEG_INFINITY,
                    own: None,
                    first: None,
                });
                acc.min_x = acc.min_x.min(run_x0);
                acc.max_x = acc.max_x.max(run_x1);
                acc.first.get_or_insert(geometry);
            }
        }

        // Pass 2: text shadows, beneath all of the line's text and decorations.
        if line_has_shadows {
            draw_line_text_shadows(
                scene,
                &line,
                run_records,
                deco_boxes,
                win_ascent_ratios,
                transform,
                options,
                inline_root_id,
            );
        }

        // Pass 3: the glyphs themselves.
        for (glyph_run, record) in glyph_runs(&line).zip(run_records.iter()) {
            draw_glyph_run(
                scene,
                &glyph_run,
                record.fill_color,
                transform,
                record.glyphs,
            );
        }

        // The marker follows the kept glyphs, in the block container's colour.
        if let (Some(cut), Some(marker)) = (cut, text_overflow) {
            draw_text_overflow_marker(
                scene,
                &marker.layout,
                doc,
                transform,
                scale,
                brush_override,
                cut.marker_x,
                line.metrics().baseline,
            );
        }

        // Pass 4: decorations, on top of the glyphs.
        flush_line_decorations(
            scene,
            transform,
            options,
            deco_boxes,
            win_ascent_ratios,
            inline_root_id,
            line.metrics().baseline,
            None,
        );
        if let Some(composing) = composing {
            flush_line_decorations(
                scene,
                transform,
                options,
                std::slice::from_ref(&composing),
                win_ascent_ratios,
                inline_root_id,
                line.metrics().baseline,
                None,
            );
        }
    }
}

/// Draw a `text-overflow` marker starting at `x` on a line whose baseline is `baseline`.
/// The marker is painted in its block container's colour and is not decorated.
#[allow(clippy::too_many_arguments)]
fn draw_text_overflow_marker(
    scene: &mut impl PaintScene,
    marker: &Layout<TextBrush>,
    doc: &BaseDocument,
    transform: Affine,
    scale: f64,
    brush_override: Option<Color>,
    x: f32,
    baseline: f32,
) {
    let Some(marker_line) = marker.lines().next() else {
        return;
    };
    let dy = baseline - marker_line.metrics().baseline;
    let transform = transform * Affine::translate((x as f64, dy as f64));
    for glyph_run in glyph_runs(&marker_line) {
        // The marker's brush is the block container, whose colour paint uses.
        let color = brush_override.unwrap_or_else(|| {
            resolve_decoration_entry(doc, glyph_run.style().brush.id, None, scale).fill_color
        });
        draw_glyph_run(scene, &glyph_run, color, transform, usize::MAX);
    }
}

/// The glyph runs of a line, in order (matching the order of the recorded `RunRecord`s).
pub(crate) fn glyph_runs<'a, 'b>(
    line: &'b Line<'a, TextBrush>,
) -> impl Iterator<Item = GlyphRun<'a, TextBrush>> + 'b {
    line.items().filter_map(|item| match item {
        PositionedLayoutItem::GlyphRun(glyph_run) => Some(glyph_run),
        PositionedLayoutItem::InlineBox(_) => None,
    })
}

/// Draw a glyph run's glyphs in a single colour.
///
/// Only the first `max_glyphs` glyphs are drawn (all of them for `usize::MAX`).
pub(crate) fn draw_glyph_run(
    scene: &mut impl PaintScene,
    glyph_run: &GlyphRun<'_, TextBrush>,
    color: Color,
    transform: Affine,
    max_glyphs: usize,
) {
    let run = glyph_run.run();
    let font_size = run.font_size();
    // Zero-size text is invisible, but hinting can give its glyphs ink. Fully
    // transparent text is skipped too: backends draw color (emoji) glyphs opaque.
    if font_size <= 0.0 || color.components[3] <= 0.0 {
        return;
    }
    let glyph_xform = run
        .synthesis()
        .skew()
        .map(|angle| Affine::skew(-angle.to_radians().tan() as f64, 0.0));

    let embolden = if FONT_EMBOLDEN_ENABLED {
        font_embolden(font_size as f64, color, transform)
    } else {
        kurbo::Vec2::default()
    };

    let normalized_coords: &[i16] = bytemuck::cast_slice(run.normalized_coords());
    scene.draw_glyphs(
        run.font(),
        font_size,
        !FONT_EMBOLDEN_ENABLED,
        normalized_coords,
        embolden,
        Fill::NonZero,
        &anyrender::Paint::from(color),
        1.0,
        transform,
        glyph_xform,
        glyph_run
            .positioned_glyphs()
            .take(max_glyphs)
            .map(|glyph| anyrender::Glyph {
                id: glyph.id as _,
                x: glyph.x,
                y: glyph.y,
            }),
        AntiAliasing::Enabled,
    );
}

/// The layout-space box covered by a glyph run: its advance horizontally and the font's
/// ascent/descent vertically.
pub(crate) fn glyph_run_rect(glyph_run: &GlyphRun<'_, TextBrush>) -> Rect {
    let metrics = glyph_run.run().font_metrics();
    let x0 = glyph_run.offset() as f64;
    let x1 = x0 + glyph_run.advance() as f64;
    let baseline = glyph_run.baseline() as f64;
    let y0 = baseline - metrics.ascent as f64;
    let y1 = baseline + metrics.descent as f64;
    Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
}

/// Draw selection highlight rectangles for the given byte range in a layout.
/// Uses Parley's Selection type for accurate geometry calculation. With a `text_overflow`
/// marker, the highlight stops where cut lines are cut, as the text after it is hidden.
pub(crate) fn draw_text_selection(
    scene: &mut impl PaintScene,
    layout: &Layout<TextBrush>,
    transform: Affine,
    selection_start: usize,
    selection_end: usize,
    text_overflow: Option<&TextOverflowMarker>,
) {
    let anchor = Cursor::from_byte_index(layout, selection_start, Affinity::Downstream);
    let focus = Cursor::from_byte_index(layout, selection_end, Affinity::Downstream);
    let selection = Selection::new(anchor, focus);

    selection.geometry_with(layout, |rect, line_idx| {
        let mut rect = kurbo::Rect::new(rect.x0, rect.y0, rect.x1, rect.y1);
        let cut = text_overflow.and_then(|marker| marker.lines.get(line_idx).copied().flatten());
        if let Some(cut) = cut {
            rect.x1 = rect.x1.min(cut.marker_x as f64);
            if rect.x1 <= rect.x0 {
                return;
            }
        }
        scene.fill(
            Fill::NonZero,
            transform,
            SELECTION_COLOR,
            None,
            &rect,
            AntiAliasing::Enabled,
        );
    });
}

/// Approximates macOS font smoothing (stem dilation). CoreGraphics dilates each side by an amount
/// in device pixels that grows with the device ppem up to 0.3px (fitted to Safari's ink at 9-72px
/// and 1x-3x scale), and keeps dilating at large sizes, so there is no ppem cutoff. glifo applies
/// the embolden in user units, so the device amounts are divided by the transform's scale.
fn font_embolden(font_size: f64, color: Color, transform: Affine) -> kurbo::Vec2 {
    const MAX_DEVICE_EMBOLDEN: f64 = 0.3;
    let [a, b, c, d, _, _] = transform.as_coeffs();
    let (scale_x, scale_y) = (a.hypot(b), c.hypot(d));
    let device_ppem = font_size * (a * d - b * c).abs().sqrt();
    if scale_x == 0.0 || scale_y == 0.0 || !device_ppem.is_finite() {
        return kurbo::Vec2::default();
    }
    let fs = device_ppem * embolden_scale(color);
    kurbo::Vec2::new(
        (0.0121 * fs).min(MAX_DEVICE_EMBOLDEN) / scale_x,
        (0.015125 * fs).min(MAX_DEVICE_EMBOLDEN) / scale_y,
    )
}

/// Scales Pathfinder's stem-darkening factors by the text's relative luminance. CoreText
/// dilates light text more than dark text; the points are fitted to Safari's ink for text
/// colours from black to white on black and white backgrounds.
fn embolden_scale(color: Color) -> f64 {
    const POINTS: [(f64, f64); 4] = [(0.1, 0.5), (0.125, 0.72), (0.35, 0.72), (1.0, 1.3)];
    let lum = color.discard_alpha().relative_luminance() as f64;
    let mut prev = POINTS[0];
    if lum <= prev.0 {
        return prev.1;
    }
    for point in &POINTS[1..] {
        if lum <= point.0 {
            return prev.1 + (point.1 - prev.1) * (lum - prev.0) / (point.0 - prev.0);
        }
        prev = *point;
    }
    prev.1
}

#[cfg(test)]
mod tests {
    use super::PositionedLayoutItem;
    use anyrender::recording::{RenderCommand, Scene};
    use blitz_dom::{Attribute, BaseDocument, DocumentConfig, NodeId, qual_name};
    use blitz_traits::shell::{ColorScheme, Viewport};
    use kurbo::{Affine, Shape as _};
    use std::io::Write as _;

    /// Without a usable font text measures 0x0, which would make these tests vacuous, so
    /// they skip. The skip is written straight to stderr, past the test harness's output
    /// capture, so it shows.
    fn no_font(layout: &parley::Layout<blitz_dom::node::TextBrush>) -> bool {
        let skip = layout.width() == 0.0;
        if skip {
            let test = std::thread::current().name().unwrap_or("test").to_string();
            let _ = writeln!(
                std::io::stderr(),
                "{test}: SKIPPED, no usable font (text measures 0x0)"
            );
        }
        skip
    }

    /// One glyph run drawn: the x of its transform (device pixels) and its glyph ids and x
    /// positions (in the transform's coordinates).
    type DrawnRun = (f64, Vec<(u32, f32)>);

    /// Paint `<div style="{div_style}">{text}</div>` at 2x scale and return each glyph run
    /// drawn, with the document and the div.
    fn paint(div_style: &str, text: &str) -> (Vec<DrawnRun>, BaseDocument, NodeId) {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 2.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = doc.root_node().id;
        let style = |value: &str| Attribute {
            name: qual_name!("style"),
            value: value.to_string(),
        };
        let mut mutator = doc.mutate();
        // No user-agent stylesheet is loaded, so every element is styled as a block here.
        let html = mutator.create_element(qual_name!("html"), vec![style("display:block")]);
        let body = mutator.create_element(qual_name!("body"), vec![style("display:block")]);
        let div_style = format!("display:block; {div_style}");
        let div = mutator.create_element(qual_name!("div"), vec![style(&div_style)]);
        let text = mutator.create_text_node(text);
        mutator.append_children(div, &[text]);
        mutator.append_children(body, &[div]);
        mutator.append_children(html, &[body]);
        mutator.append_children(root_id, &[html]);
        drop(mutator);
        doc.resolve(0.0);

        let mut scene = Scene::new();
        crate::paint_scene(&mut scene, &mut doc, 2.0, 800, 600, 0, 0);
        let runs = scene
            .commands
            .iter()
            .filter_map(|command| match command {
                RenderCommand::GlyphRun(run) => Some((
                    run.transform.translation().x,
                    run.glyphs.iter().map(|g| (g.id, g.x)).collect(),
                )),
                _ => None,
            })
            .collect();
        (runs, doc, div)
    }

    #[test]
    fn overflowing_text_is_cut_and_ends_with_the_marker() {
        let (runs, doc, div) = paint(
            "width:100px; overflow:hidden; white-space:nowrap; text-overflow:ellipsis",
            "A label far too long to fit in its box",
        );
        let text_layout = doc
            .get_node(div)
            .unwrap()
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_deref()
            .unwrap();
        if no_font(&text_layout.layout) {
            return;
        }
        let marker = text_layout.text_overflow.as_ref().expect("a marker");
        let cut = marker.lines[0].expect("the line is cut");

        // The kept glyphs, then the marker: one glyph at the cut, inside the box.
        let ((marker_origin, marker_run), text_runs) =
            runs.split_last().expect("glyphs were drawn");
        let text_origin = text_runs[0].0;
        let text_glyphs: Vec<_> = text_runs.iter().flat_map(|(_, glyphs)| glyphs).collect();
        assert_eq!(text_glyphs.len(), cut.glyphs_kept);
        assert!(text_glyphs.iter().all(|(_, x)| *x < cut.marker_x));
        assert_eq!(marker_run.len(), 1);
        assert_ne!(marker_run[0].0, 0);
        // The marker is drawn from the cut, in the text's own coordinates.
        let marker_x = (marker_origin - text_origin) as f32 + marker_run[0].1;
        assert!((marker_x - cut.marker_x).abs() < 0.01);
        assert!(marker_x + marker.layout.width() <= 200.0);
    }

    #[test]
    fn clipped_text_is_drawn_whole() {
        let (runs, doc, div) = paint(
            "width:100px; overflow:hidden; white-space:nowrap; text-overflow:clip",
            "A label far too long to fit in its box",
        );
        let layout = &doc
            .get_node(div)
            .unwrap()
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_deref()
            .unwrap()
            .layout;
        if no_font(layout) {
            return;
        }
        let laid_out: usize = layout
            .lines()
            .flat_map(|line| line.items().collect::<Vec<_>>())
            .map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => run.glyphs().count(),
                PositionedLayoutItem::InlineBox(_) => 0,
            })
            .sum();
        let drawn: usize = runs.iter().map(|(_, glyphs)| glyphs.len()).sum();
        assert_eq!(drawn, laid_out);
    }

    #[test]
    fn selection_stops_at_the_cut() {
        let text = "A label far too long to fit in its box";
        let (_, doc, div) = paint(
            "width:100px; overflow:hidden; white-space:nowrap; text-overflow:ellipsis",
            text,
        );
        let text_layout = doc
            .get_node(div)
            .unwrap()
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_deref()
            .unwrap();
        if no_font(&text_layout.layout) {
            return;
        }
        let marker = text_layout.text_overflow.as_ref().expect("a marker");
        let cut = marker.lines[0].expect("the line is cut");

        let mut scene = Scene::new();
        super::draw_text_selection(
            &mut scene,
            &text_layout.layout,
            Affine::IDENTITY,
            0,
            text.len(),
            Some(marker),
        );
        let rects: Vec<_> = scene
            .commands
            .iter()
            .filter_map(|command| match command {
                RenderCommand::Fill(fill) => Some(fill.shape.bounding_box()),
                _ => None,
            })
            .collect();
        assert!(!rects.is_empty(), "the kept text is highlighted");
        for rect in rects {
            assert!(
                rect.x1 <= cut.marker_x as f64 + 1e-3,
                "{rect:?} past the cut"
            );
        }
    }
}
