//! CSS 2.1 Appendix E: all positioned descendants with z-index: auto share
//! one paint level (step 8) and paint in tree order among themselves.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn center_pixel(html: &str) -> [u8; 3] {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(100, 100, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, 100, 100, 0, 0),
        100,
        100,
    );
    let idx = (50 * 100 + 50) * 4;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

#[test]
fn later_relative_sibling_paints_above_earlier_abspos() {
    let px = center_pixel(
        r#"<html><body style="margin:0">
            <div style="position:relative; width:100px; height:100px;">
                <div style="position:absolute; inset:0; background:#0000ff;"></div>
                <div style="position:relative; width:100px; height:100px; background:#ff0000;"></div>
            </div>
        </body></html>"#,
    );
    assert_eq!(
        px,
        [255, 0, 0],
        "later positioned (z-index auto) sibling must paint above earlier abspos sibling"
    );
}

#[test]
fn abspos_paints_above_earlier_static_sibling() {
    let px = center_pixel(
        r#"<html><body style="margin:0">
            <div style="position:relative; width:100px; height:100px;">
                <div style="width:100px; height:100px; background:#0000ff;"></div>
                <div style="position:absolute; inset:0; background:#ff0000;"></div>
            </div>
        </body></html>"#,
    );
    assert_eq!(px, [255, 0, 0], "abspos must paint above in-flow content");
}

#[test]
fn earlier_abspos_stays_below_static_when_later_in_tree_order_is_static() {
    // In-flow content paints below positioned content even when the
    // positioned element comes first in tree order.
    let px = center_pixel(
        r#"<html><body style="margin:0">
            <div style="position:relative; width:100px; height:100px;">
                <div style="position:absolute; inset:0; background:#ff0000;"></div>
                <div style="width:100px; height:100px; background:#0000ff;"></div>
            </div>
        </body></html>"#,
    );
    assert_eq!(
        px,
        [255, 0, 0],
        "positioned content paints above in-flow content regardless of tree order"
    );
}

#[test]
fn abspos_hoisted_from_inline_block_in_span_paints_in_tree_order() {
    // The abspos is hoisted past its layout parent (an inline-block nested in
    // a span) to the positioned inline root, and must still paint before the
    // later positioned inline-block.
    let px = center_pixel(
        r#"<html><body style="margin:0">
            <div style="position:relative; width:100px; height:100px; font-size:0;"><span><span style="display:inline-block"><div style="position:absolute; left:0; top:0; width:100px; height:100px; background:#ff0000;"></div></span></span><span style="display:inline-block; position:relative; vertical-align:top; width:100px; height:100px; background:#0000ff;"></span></div>
        </body></html>"#,
    );
    assert_eq!(
        px,
        [0, 0, 255],
        "later positioned inline-block must paint above the earlier hoisted abspos"
    );
}

/// A `position: fixed` box after positioned rows in tree order paints over them and takes the hit,
/// also inside a stacking context (`z-index: 1`): the fixed box stays in that context.
#[test]
fn later_fixed_box_paints_and_hits_above_earlier_relative_rows() {
    for context in [
        "",
        "z-index:1",
        "z-index:1; margin-left:30px; margin-top:20px",
    ] {
        later_fixed_box_over_rows(context);
    }
}

fn later_fixed_box_over_rows(context: &str) {
    let html = format!(
        r#"<html><body style="margin:0">
        <div style="position:relative; height:100px; {context}">
            <div style="position:relative">
                <div id="row" style="position:relative; height:100px; background:#0000ff;"></div>
            </div>
            <div style="position:fixed; left:25px; top:25px">
                <div id="card" style="width:50px; height:50px; background:#ff0000;"></div>
            </div>
        </div>
    </body></html>"#
    );
    assert_eq!(
        center_pixel(&html),
        [255, 0, 0],
        "{context}: the fixed card paints over the row"
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(100, 100, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let card = doc.query_selector("#card").unwrap().unwrap();
    assert_eq!(
        doc.hit(50.0, 50.0).map(|hit| hit.node_id),
        Some(card),
        "{context}: the card takes the hit"
    );
}
