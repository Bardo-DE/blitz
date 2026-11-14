//! A box in a table row that is not a table part is laid out in an anonymous cell (CSS 2.1
//! section 17.2.1), not dropped. Mail templates give a cell `display: block` for narrow screens.

use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

/// The laid-out height of the first node matching `selector` in `html`.
fn height_of(html: &str, selector: &str) -> f32 {
    let mut doc = HtmlDocument::from_html(html, DocumentConfig::default());
    doc.set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
    doc.resolve(0.0);
    let id = doc
        .query_selector(selector)
        .expect("selector parses")
        .unwrap_or_else(|| panic!("{selector} is in the document"));
    doc.get_node(id).expect("node").final_layout().size.height
}

#[test]
fn a_box_that_is_not_a_table_part_is_laid_out_as_a_cell() {
    let cases = [
        (
            "a cell given display: block, in a row",
            r#"<table><tr><td id="t" style="display:block"><p>Hello</p></td></tr></table>"#,
        ),
        (
            "a flex box in a row",
            r#"<table><tr><td id="t" style="display:flex"><span>Hello</span></td></tr></table>"#,
        ),
    ];
    for (name, html) in cases {
        assert!(
            height_of(html, "#t") > 0.0,
            "{name}: laid out with no height"
        );
    }
}

/// The laid-out width of the first node matching `selector` in `html`.
fn width_of(html: &str, selector: &str) -> f32 {
    let mut doc = HtmlDocument::from_html(html, DocumentConfig::default());
    doc.set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
    doc.resolve(0.0);
    let id = doc
        .query_selector(selector)
        .expect("selector parses")
        .unwrap_or_else(|| panic!("{selector} is in the document"));
    doc.get_node(id).expect("node").final_layout().size.width
}

#[test]
fn a_narrow_column_width_gives_way_to_its_content() {
    // GitHub's run mails set `width: 24px` on a "Status" header beside a `width: 100%` one; a
    // 50px block stands in for the word, which no break can make narrower.
    let narrow = r#"<table style="width:100%;border-spacing:0"><tr><th id="t" style="width:4px;padding:0"><div style="width:50px;height:1px"></div></th><th style="width:100%">Job</th></tr></table>"#;
    let baseline = narrow.replace("width:4px;", "");
    eprintln!(
        "narrow th {} / its div {} / job {} / no width th {}",
        width_of(narrow, "#t"),
        width_of(narrow, "#t > div"),
        width_of(narrow, "th:nth-child(2)"),
        width_of(&baseline, "#t"),
    );
    let width = width_of(narrow, "#t");
    assert!(width >= 49.5, "a 4px column is as wide as its content, not {width}");
    // A width wider than the content still holds.
    let wide = r#"<table style="border-spacing:0"><tr><th id="t" style="width:300px;padding:0"><div style="width:50px;height:1px"></div></th><td>x</td></tr></table>"#;
    assert!(
        (width_of(wide, "#t") - 300.0).abs() < 0.5,
        "a 300px column stays 300px"
    );
}
