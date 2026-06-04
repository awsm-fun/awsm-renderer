//! Viewport: the real WebGPU canvas (reparented into the slot) + the overlay
//! chrome from viewport.jsx — transform-tool palette, shading-mode toggles,
//! nav-cube, and readout chips. (The prototype's MaterialBall/fake-grid/fake-
//! gizmo are CSS stand-ins for the render the real canvas already produces.)
//!
//! M6-A: the chrome + view-state (active tool / shading). Picking + gizmo drag
//! + shading→renderer-debug wiring layer in next.

use crate::engine::gizmo::{gizmo_mode, GizmoMode};
use crate::prelude::*;

const PANEL_BG: &str = "oklch(0.18 0.006 255 / 0.78)";
const CHIP_BG: &str = "oklch(0.13 0.006 255 / 0.85)";

pub fn render() -> Dom {
    // The palette drives the shared gizmo mode (so Move/Rotate/Scale actually
    // switch which gizmo handles show).
    let tool = gizmo_mode();
    let shading = Mutable::new("material".to_string());

    html!("div", {
        .style("position", "absolute")
        .style("inset", "0")
        .style("overflow", "hidden")
        // The live WebGPU canvas, reparented into this slot.
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .after_inserted(|elem| {
                crate::engine::context::with_canvas(|canvas| {
                    if let Err(err) = elem.append_child(canvas) {
                        Modal::error(format!("Failed to mount viewport canvas: {err:?}"));
                    }
                });
                // Size the surface to this slot now — the ResizeObserver doesn't
                // reliably fire its first callback on the reparent, which would
                // leave the render at 300×150 and break click/gizmo picking.
                crate::engine::context::sync_canvas_size();
            })
        }))
        // Screen-space selection box (orange rect around the selected object,
        // recomputed each frame by the render loop).
        .child_signal(crate::engine::selection_box::rect_signal().map(|rect| {
            rect.map(|[x, y, w, h]| {
                html!("div", {
                    .style("position", "absolute")
                    .style("pointer-events", "none")
                    .style("left", format!("{x}px"))
                    .style("top", format!("{y}px"))
                    .style("width", format!("{w}px"))
                    .style("height", format!("{h}px"))
                    .style("border", "1.5px solid #f0973a")
                    .style("border-radius", "2px")
                    .style("box-shadow", "0 0 0 1px rgba(240,151,58,0.25)")
                })
            })
        }))
        // Overlay chrome (sits above the canvas).
        .child(nav_cube())
        .child(tool_palette(&tool))
        .child(shading_and_stats(&shading))
        .child(camera_chip())
    })
}

fn tool_palette(tool: &Mutable<GizmoMode>) -> Dom {
    let entry = |t: GizmoMode, icon: &str, title: &str, tool: &Mutable<GizmoMode>| -> Dom {
        let active = tool.signal().map(move |cur| cur == t);
        html!("button", {
            .class("t")
            .attr("title", title)
            .style("position", "relative")
            .style("width", "30px")
            .style("height", "30px")
            .style("display", "flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("border-radius", "var(--r1)")
            .style("cursor", "pointer")
            .style("border", "1px solid transparent")
            .style_signal("background", active.map(|on| if on { "var(--accent)" } else { "transparent" }))
            .style_signal("color", tool.signal().map(move |cur| if cur == t { "oklch(0.18 0.02 255)" } else { "var(--text-1)" }))
            .event(clone!(tool => move |_: events::Click| tool.set_neq(t)))
            .child(Icon::new(icon).size(16.0).render())
        })
    };
    html!("div", {
        .style("position", "absolute")
        .style("left", "12px")
        .style("top", "50%")
        .style("transform", "translateY(-50%)")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "3px")
        .style("padding", "4px")
        .style("background", PANEL_BG)
        .style("backdrop-filter", "blur(10px)")
        .style("border", "1px solid var(--line)")
        .style("border-radius", "var(--r3)")
        .style("box-shadow", "var(--shadow-2)")
        .child(entry(GizmoMode::Select, "select", "Select · Q", tool))
        .child(entry(GizmoMode::Move, "move", "Move · W", tool))
        .child(entry(GizmoMode::Rotate, "rotate", "Rotate · E", tool))
        .child(entry(GizmoMode::Scale, "scale", "Scale · R", tool))
    })
}

fn shading_and_stats(shading: &Mutable<String>) -> Dom {
    let sbtn = |v: &'static str, icon: &str, title: &str, shading: &Mutable<String>| -> Dom {
        let on = shading.signal_cloned().map(move |s| s == v);
        html!("button", {
            .class("t")
            .attr("title", title)
            .style("width", "26px")
            .style("height", "26px")
            .style("display", "flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("border-radius", "var(--r1)")
            .style("cursor", "pointer")
            .style("border", "1px solid transparent")
            .style_signal("background", on.map(|o| if o { "var(--accent)" } else { "transparent" }))
            .style_signal("color", shading.signal_cloned().map(move |s| if s == v { "oklch(0.18 0.02 255)" } else { "var(--text-1)" }))
            .event(clone!(shading => move |_: events::Click| shading.set_neq(v.to_string())))
            .child(Icon::new(icon).size(15.0).render())
        })
    };
    html!("div", {
        .style("position", "absolute")
        .style("left", "12px")
        .style("bottom", "12px")
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "8px")
        .child(html!("div", {
            .style("display", "flex")
            .style("gap", "2px")
            .style("padding", "3px")
            .style("background", PANEL_BG)
            .style("backdrop-filter", "blur(10px)")
            .style("border", "1px solid var(--line)")
            .style("border-radius", "var(--r2)")
            .style("box-shadow", "var(--shadow-1)")
            .child(sbtn("solid", "sphere", "Solid", shading))
            .child(sbtn("material", "material", "Material preview", shading))
            .child(sbtn("wire", "grid", "Wireframe", shading))
        }))
        .child(selection_stats())
    })
}

fn camera_chip() -> Dom {
    html!("div", {
        .style("position", "absolute")
        .style("right", "12px")
        .style("bottom", "12px")
        .style("display", "flex")
        .style("gap", "6px")
        .child(chip_text("persp · 50mm"))
    })
}

/// A reactive object/tris chip bound to the primary selection.
fn selection_stats() -> Dom {
    let ctrl = controller();
    html!("span", {
        .class("mono")
        .style("font-size", "10.5px")
        .style("font-weight", "500")
        .style("white-space", "nowrap")
        .style("color", "oklch(0.86 0.01 255)")
        .style("padding", "4px 8px")
        .style("background", CHIP_BG)
        .style("backdrop-filter", "blur(8px)")
        .style("border-radius", "var(--r1)")
        .style("border", "1px solid var(--line)")
        .text_signal(map_ref! {
            let _rev = ctrl.scene.revision.signal(),
            let sel = ctrl.selected.signal_cloned() => {
                let name = sel.last().and_then(|id| {
                    crate::engine::scene::mutate::find_by_id(&controller().scene, *id)
                        .map(|n| n.name.get_cloned())
                }).unwrap_or_else(|| "\u{2014}".to_string());
                format!("{name} \u{00b7} 1.2k tris")
            }
        })
    })
}

fn chip_text(text: &str) -> Dom {
    html!("span", {
        .class("mono")
        .style("font-size", "10.5px")
        .style("font-weight", "500")
        .style("white-space", "nowrap")
        .style("color", "oklch(0.86 0.01 255)")
        .style("padding", "4px 8px")
        .style("background", CHIP_BG)
        .style("backdrop-filter", "blur(8px)")
        .style("border-radius", "var(--r1)")
        .style("border", "1px solid var(--line)")
        .text(text)
    })
}

/// The view-orientation nav cube (top-right). Static for now — live orientation
/// from the camera is a polish item (plan §13: "stub the purely-visual").
fn nav_cube() -> Dom {
    html!("div", {
        .style("position", "absolute")
        .style("top", "12px")
        .style("right", "12px")
        .style("width", "58px")
        .style("height", "58px")
        .style("background", "oklch(0.18 0.006 255 / 0.7)")
        .style("backdrop-filter", "blur(10px)")
        .style("border", "1px solid var(--line)")
        .style("border-radius", "50%")
        .style("box-shadow", "var(--shadow-1)")
        .attr("title", "View orientation")
        .child(svg!("svg", {
            .attr("width", "58")
            .attr("height", "58")
            .attr("viewBox", "0 0 58 58")
            .child(svg!("line", { .attr("x1", "29").attr("y1", "29").attr("x2", "29").attr("y2", "9").attr("stroke", "var(--axis-y)").attr("stroke-width", "2") }))
            .child(svg!("circle", { .attr("cx", "29").attr("cy", "8").attr("r", "5").attr("fill", "var(--axis-y)") }))
            .child(svg!("line", { .attr("x1", "29").attr("y1", "29").attr("x2", "46").attr("y2", "37").attr("stroke", "var(--axis-x)").attr("stroke-width", "2") }))
            .child(svg!("circle", { .attr("cx", "48").attr("cy", "38").attr("r", "5").attr("fill", "var(--axis-x)") }))
            .child(svg!("line", { .attr("x1", "29").attr("y1", "29").attr("x2", "14").attr("y2", "38").attr("stroke", "var(--axis-z)").attr("stroke-width", "2") }))
            .child(svg!("circle", { .attr("cx", "12").attr("cy", "39").attr("r", "5").attr("fill", "var(--axis-z)") }))
        }))
    })
}
