use crate::prelude::*;
use crate::scene::Node;

/// Section header label. Accepts both `&'static str` and runtime-built
/// strings (e.g. a label with an asset id baked in) — both inputs route
/// through `AsRef<str>` so callers don't pay anything beyond a borrow.
pub(crate) fn section_header(label: impl AsRef<str>) -> Dom {
    html!("div", {
        .style("font-size", "0.75rem")
        .style("font-weight", "600")
        .style("text-transform", "uppercase")
        .style("letter-spacing", "0.05em")
        .style("color", ColorText::Byline.value())
        .text(label.as_ref())
    })
}

/// "Capture as Mesh asset" button for procedural-mesh kinds (F10).
/// Calls `actions::object::capture_as_mesh_asset` on the node — that
/// action handles every part of the capture (asset insert, bytes into
/// pending + mesh_cache, kind rewrite, history commit).
pub(crate) fn capture_as_mesh_button(node: Arc<Node>) -> Dom {
    use awsm_web_shared::atoms::buttons::{Button, ButtonSize, ButtonStyle};
    let node_id = node.id;
    html!("div", {
        .style("display", "flex")
        .style("justify-content", "flex-start")
        .style("padding-top", "0.25rem")
        .child(Button::new()
            .with_text("Capture as Mesh asset")
            .with_style(ButtonStyle::Outline)
            .with_size(ButtonSize::Sm)
            .with_on_click(move || {
                let _ = crate::actions::object::capture_as_mesh_asset(node_id);
            })
            .render())
    })
}

pub(crate) fn field_row(label: &'static str, control: Dom) -> Dom {
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "7rem 1fr")
        .style("gap", "0.5rem")
        .style("align-items", "center")
        .child(html!("span", {
            .style("font-size", "0.8rem")
            .style("color", ColorText::Byline.value())
            .text(label)
        }))
        .child(control)
    })
}
