//! Preview viewport — bottom-left.
//!
//! Phase 8: a blank `<canvas>` placeholder. Phase 9 wires the renderer
//! to draw a stub quad with the loaded material applied. Phase 11 adds
//! the mesh-kind selector so materials that read `world_normal` /
//! `world_tangent` can be inspected against curved + volumetric shapes
//! (plane / sphere / box / cylinder / torus).
//!
//! The canvas dimensions are pinned at 800×600 so the renderer's
//! swap-chain texture is built at a sensible resolution at boot time.
//! Without explicit `width` / `height` attributes the browser
//! defaults a `<canvas>` to 300×150, which is too small for the
//! visibility-buffer tile compute kernel to show useful detail and
//! also makes the CSS-scaled preview look pixelated.

use dominator::{clone, events, html, with_node, Dom};
use futures_signals::signal::SignalExt;

use crate::state::{EditState, PreviewMeshKind};

pub fn render(state: &EditState) -> Dom {
    let preview_mesh = state.preview_mesh.clone();
    let preview_mesh_for_label = preview_mesh.clone();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("height", "100%")
        .child(html!("div", {
            .style("padding", "8px 12px")
            .style("background", "#222")
            .style("color", "#aaa")
            .style("font-size", "12px")
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "12px")
            .child(html!("span", {
                .style("flex", "0 0 auto")
                .text_signal(
                    preview_mesh_for_label.signal_cloned()
                        .map(|m| format!("Preview — live material on {}", m.label().to_lowercase()))
                )
            }))
            .child(html!("label", {
                .style("color", "#666")
                .style("font-size", "11px")
                .style("margin-left", "auto")
                .text("Mesh:")
            }))
            .child(html!("select" => web_sys::HtmlSelectElement, {
                .style("background", "#111")
                .style("color", "#eee")
                .style("border", "1px solid #444")
                .style("padding", "2px 6px")
                .style("font-size", "12px")
                .children(PreviewMeshKind::all().iter().enumerate().map(|(i, kind)| {
                    html!("option", {
                        .attr("value", &i.to_string())
                        .text(kind.label())
                    })
                }).collect::<Vec<_>>())
                .with_node!(elem => {
                    // Initialize the dropdown selection from the
                    // Mutable's current value on mount. dominator's
                    // event handlers fire only on user interaction;
                    // without this the <select> defaults to its first
                    // <option> regardless of what the state says.
                    .future(preview_mesh.signal_cloned().for_each(clone!(elem => move |kind| {
                        let elem = elem.clone();
                        async move {
                            if let Some(idx) = PreviewMeshKind::all()
                                .iter()
                                .position(|k| *k == kind)
                            {
                                elem.set_value(&idx.to_string());
                            }
                        }
                    })))
                    .event(clone!(preview_mesh, elem => move |_: events::Change| {
                        let v = elem.value();
                        if let Ok(idx) = v.parse::<usize>() {
                            if let Some(kind) = PreviewMeshKind::all().get(idx) {
                                preview_mesh.set_neq(*kind);
                            }
                        }
                    }))
                })
            }))
        }))
        .child(html!("canvas" => web_sys::HtmlCanvasElement, {
            .attr("id", "preview-canvas")
            .attr("width", "800")
            .attr("height", "600")
            .style("width", "800px")
            .style("height", "600px")
            .style("display", "block")
            .style("margin", "8px auto")
            .style("background", "#000")
        }))
    })
}
