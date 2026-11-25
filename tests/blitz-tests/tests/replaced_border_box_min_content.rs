//! A replaced element's content box is never smaller than zero. Under a min-content measure a
//! percentage `max-width` resolves against zero, and with `box-sizing: border-box` the border is
//! then taken off it: the content-box limit must stop at zero, not go below it.

use blitz_dom::DocumentConfig;
use blitz_dom::node::{ImageData, RasterImageData, SpecialElementData};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

#[test]
fn a_bordered_picture_in_a_min_content_track_lays_out_with_a_zero_content_box() {
    let html = r#"<html><body style="margin:0">
        <div style="display:grid; grid-template-columns:min-content">
            <img id="picture" style="box-sizing:border-box; max-width:100%; border:1px solid black">
        </div>
    </body></html>"#;
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let id = doc
        .query_selector("#picture")
        .expect("a valid selector")
        .expect("the picture");
    // A decoded 1200 x 600 picture, as if it had loaded.
    let pixels: Vec<u8> = [0u8, 0, 0, 255].repeat(1200 * 600);
    doc.get_node_mut(id)
        .expect("the node")
        .element_data_mut()
        .expect("an element")
        .special_data = SpecialElementData::Image(Box::new(ImageData::Raster(
        RasterImageData::new(1200, 600, Arc::new(pixels)),
    )));
    doc.get_node_mut(id)
        .expect("the node")
        .mark_ancestors_dirty();
    doc.resolve(0.0);
    let size = doc.get_node(id).expect("the node").final_layout().size;
    assert!(
        size.width >= 2.0 && size.height >= 2.0,
        "the border box holds at least its border: {size:?}"
    );
}
