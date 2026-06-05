//! Viewport: the real WebGPU canvas (reparented into the slot) + the overlay
//! chrome from viewport.jsx — transform-tool palette, shading-mode toggles,
//! nav-cube, and readout chips. (The prototype's MaterialBall/fake-grid/fake-
//! gizmo are CSS stand-ins for the render the real canvas already produces.)
//!
//! M6-A: the chrome + view-state (active tool / shading). Picking + gizmo drag
//! + shading→renderer-debug wiring layer in next.

use crate::controller::CameraAxis;
use crate::engine::gizmo::{gizmo_mode, GizmoMode};
use crate::engine::scene::NodeId;
use crate::prelude::*;

const PANEL_BG: &str = "oklch(0.18 0.006 255 / 0.78)";
const CHIP_BG: &str = "oklch(0.13 0.006 255 / 0.85)";

pub fn render() -> Dom {
    // The palette drives the shared gizmo mode (so Move/Rotate/Scale actually
    // switch which gizmo handles show).
    let tool = gizmo_mode();
    let shading = Mutable::new("material".to_string());
    // Drive the renderer view mode from the shading toggle: material = normal
    // lit, solid = unlit/flat, wire = wireframe view (uniform clay fill + edges,
    // material-independent).
    spawn_local(clone!(shading => async move {
        let mut first = true;
        shading.signal_cloned().for_each(move |s| {
            let skip = first;
            first = false;
            async move {
                if skip {
                    return;
                }
                let (view_mode, wireframe) = match s.as_str() {
                    "solid" => (1u32, false),
                    "wire" => (0u32, true),
                    _ => (0u32, false),
                };
                crate::engine::context::with_renderer_mut(move |r| {
                    r.set_debug_view_mode(view_mode);
                    r.set_debug_wireframe(wireframe);
                })
                .await;
            }
        })
        .await;
    }));

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
        .child(camera_dropdown())
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
        .child(entry(GizmoMode::Universal, "target", "Universal (all handles) · T", tool))
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
            // Ordered least→most rendered, left→right: wireframe, solid, shaded.
            .child(sbtn("wire", "sphere", "Wireframe", shading))
            .child(sbtn("solid", "sphere-solid", "Solid", shading))
            .child(sbtn("material", "material", "Material preview", shading))
        }))
        .child(selection_stats())
    })
}

/// Bottom-right camera-source dropdown: pick the **Built-in** free editor camera
/// (orbit/pan/zoom) or one of the scene's `Camera` nodes. Choosing a scene camera
/// locks the view to that camera's transform + config (input is suppressed in
/// `canvas.rs`). Rebuilds when the scene tree (cameras added/removed) or the
/// active selection changes.
fn camera_dropdown() -> Dom {
    let ctrl = controller();
    html!("div", {
        .style("position", "absolute")
        .style("right", "12px")
        .style("bottom", "12px")
        .style("display", "flex")
        .style("gap", "6px")
        .child_signal(map_ref! {
            let _rev = ctrl.scene.revision.signal(),
            let active = ctrl.active_camera.signal() => {
                Some(camera_drop_button(*active))
            }
        })
    })
}

/// The dropdown button itself, labelled with the active camera. Items are the
/// "Built-in" camera plus every scene `Camera` node.
fn camera_drop_button(active: Option<NodeId>) -> Dom {
    let cameras = collect_scene_cameras();
    let label = match active {
        None => "Built-in".to_string(),
        Some(id) => cameras
            .iter()
            .find(|(cid, _)| *cid == id)
            .map(|(_, n)| n.clone())
            // Active camera node was deleted — show Built-in (the render loop
            // also falls back), and self-heal the state.
            .unwrap_or_else(|| {
                controller().active_camera.set_neq(None);
                "Built-in".to_string()
            }),
    };
    let items = move |close: Close| -> Vec<Dom> {
        let mut rows = vec![MenuItem::new("Built-in")
            .checked(active.is_none())
            .on_click({
                let close = close.clone();
                move || {
                    controller().active_camera.set_neq(None);
                    (close.borrow_mut())();
                }
            })
            .render()];
        for (id, name) in cameras.iter() {
            let id = *id;
            rows.push(
                MenuItem::new(name.clone())
                    .checked(active == Some(id))
                    .on_click({
                        let close = close.clone();
                        move || {
                            controller().active_camera.set_neq(Some(id));
                            (close.borrow_mut())();
                        }
                    })
                    .render(),
            );
        }
        rows
    };
    DropButton::new()
        .label(label)
        .icon("camera")
        .variant(BtnVariant::Ghost)
        .size(BtnSize::Sm)
        .items(items)
        .render()
}

/// All scene `Camera` nodes as `(id, name)`, depth-first.
fn collect_scene_cameras() -> Vec<(NodeId, String)> {
    use crate::engine::scene::node::Node;
    use crate::engine::scene::NodeKind;
    fn walk(node: &std::sync::Arc<Node>, out: &mut Vec<(NodeId, String)>) {
        if matches!(node.kind.get_cloned(), NodeKind::Camera(_)) {
            out.push((node.id, node.name.get_cloned()));
        }
        for c in node.children.lock_ref().iter() {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    for root in controller().scene.nodes.lock_ref().iter() {
        walk(root, &mut out);
    }
    out
}

/// A reactive object/tris chip bound to the primary selection. The triangle
/// count is the **real** total of the selected node's subtree (its meshes plus
/// every descendant's), recomputed off the renderer whenever the selection or
/// the scene changes.
fn selection_stats() -> Dom {
    let ctrl = controller();
    let tris: Mutable<u64> = Mutable::new(0);
    // Recompute the selected subtree's triangle count on selection / scene change.
    spawn_local(clone!(tris, ctrl => async move {
        map_ref! {
            let _rev = ctrl.scene.revision.signal(),
            let sel = ctrl.selected.signal_cloned() => sel.last().copied()
        }
        .for_each(clone!(tris => move |sel_id| clone!(tris => async move {
            let count = match sel_id {
                Some(id) => subtree_triangle_count(id).await,
                None => 0,
            };
            tris.set_neq(count);
        })))
        .await;
    }));
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
            let sel = ctrl.selected.signal_cloned(),
            let tris = tris.signal() => {
                match sel.last() {
                    None => "\u{2014}".to_string(),
                    Some(id) => {
                        let name = crate::engine::scene::mutate::find_by_id(&controller().scene, *id)
                            .map(|n| n.name.get_cloned())
                            .unwrap_or_else(|| "\u{2014}".to_string());
                        format!("{name} \u{00b7} {}", fmt_tris(*tris))
                    }
                }
            }
        })
    })
}

/// Format a triangle count as `"1.2k tris"` / `"342 tris"`.
fn fmt_tris(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k tris", n as f64 / 1000.0)
    } else {
        format!("{n} tris")
    }
}

/// Total triangles in a node's subtree (the node plus all descendants),
/// summed from the renderer meshes each materialized node owns.
async fn subtree_triangle_count(root: NodeId) -> u64 {
    use crate::engine::bridge::bridge;
    use crate::engine::scene::node::Node;

    fn collect(node: &std::sync::Arc<Node>, out: &mut Vec<NodeId>) {
        out.push(node.id);
        for c in node.children.lock_ref().iter() {
            collect(c, out);
        }
    }

    let mut ids = Vec::new();
    if let Some(node) = crate::engine::scene::mutate::find_by_id(&controller().scene, root) {
        collect(&node, &mut ids);
    }
    let mesh_keys: Vec<_> = {
        let b = bridge();
        let nodes = b.nodes.lock().unwrap();
        ids.iter()
            .filter_map(|id| nodes.get(id))
            .flat_map(|n| n.model_meshes.lock().unwrap().clone())
            .collect()
    };
    if mesh_keys.is_empty() {
        return 0;
    }
    crate::engine::context::with_renderer_mut(move |r| {
        mesh_keys
            .iter()
            .filter_map(|mk| r.meshes.mesh_triangle_count(*mk))
            .sum::<usize>() as u64
    })
    .await
}

/// Dispatch a camera axis-snap (through the controller, so it's MCP-drivable).
fn snap_camera(axis: CameraAxis) {
    spawn_local(async move {
        let _ = controller()
            .dispatch(EditorCommand::SnapCameraToAxis { axis })
            .await;
    });
}

/// A clickable nav-cube axis dot: snaps the camera to that axis on click.
fn axis_dot(cx: &str, cy: &str, color: &str, axis: CameraAxis, title: &str) -> Dom {
    svg!("circle", {
        .attr("cx", cx).attr("cy", cy).attr("r", "6").attr("fill", color)
        .attr("style", "cursor:pointer")
        .attr("title", title)
        .event(move |_: events::Click| snap_camera(axis))
    })
}

/// The view-orientation nav cube (top-right). Clicking an axis dot snaps the
/// camera to look down that axis (X / Y / Z).
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
        .attr("title", "Click an axis to snap the view")
        .child(svg!("svg", {
            .attr("width", "58")
            .attr("height", "58")
            .attr("viewBox", "0 0 58 58")
            .child(svg!("line", { .attr("x1", "29").attr("y1", "29").attr("x2", "29").attr("y2", "9").attr("stroke", "var(--axis-y)").attr("stroke-width", "2") }))
            .child(axis_dot("29", "8", "var(--axis-y)", CameraAxis::PosY, "Top (Y)"))
            .child(svg!("line", { .attr("x1", "29").attr("y1", "29").attr("x2", "46").attr("y2", "37").attr("stroke", "var(--axis-x)").attr("stroke-width", "2") }))
            .child(axis_dot("48", "38", "var(--axis-x)", CameraAxis::PosX, "Right (X)"))
            .child(svg!("line", { .attr("x1", "29").attr("y1", "29").attr("x2", "14").attr("y2", "38").attr("stroke", "var(--axis-z)").attr("stroke-width", "2") }))
            .child(axis_dot("12", "39", "var(--axis-z)", CameraAxis::PosZ, "Front (Z)"))
        }))
    })
}
