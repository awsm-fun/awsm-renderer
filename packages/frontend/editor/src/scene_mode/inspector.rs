//! Inspector (kind-editors.jsx): priority asset > node. M7 core delivers the
//! universal node inspector — name · prefab toggle · Transform (TRS) — plus the
//! batch panel for multi-select. Per-kind editors (Light/Camera/Geometry/
//! MaterialBlock/Shadows) extend this incrementally.

use std::sync::Arc;

use glam::{EulerRot, Quat};

use crate::engine::scene::mutate::find_by_id;
use crate::engine::scene::{Node, NodeId, NodeKind, Trs};
use crate::prelude::*;

pub fn render() -> Dom {
    let ctrl = controller();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("height", "100%")
        .style("background", "var(--bg-1)")
        .child(panel_header())
        .child(html!("div", {
            .style("flex", "1")
            .style("overflow-y", "auto")
            .child_signal(map_ref! {
                let _rev = ctrl.scene.revision.signal(),
                let sel = ctrl.selected.signal_cloned() => move {
                    Some(content(sel))
                }
            })
        }))
    })
}

fn panel_header() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("height", "38px")
        .style("padding", "0 14px")
        .style("border-bottom", "1px solid var(--line-soft)")
        .style("flex", "0 0 auto")
        .child(html!("span", {
            .style("font-size", "12.5px").style("font-weight", "620").style("color", "var(--text-0)")
            .text("Properties")
        }))
        .child(html!("div", { .style("margin-left", "auto") }))
    })
}

fn content(sel: &[NodeId]) -> Dom {
    match sel.len() {
        0 => nothing_selected(),
        1 => find_by_id(&controller().scene, sel[0])
            .map(single_node)
            .unwrap_or_else(nothing_selected),
        n => batch(n),
    }
}

fn nothing_selected() -> Dom {
    html!("div", {
        .style("padding", "16px")
        .style("font-size", "12.5px")
        .style("color", "var(--text-3)")
        .style("line-height", "1.5")
        .text("Nothing selected. Click a node in the Outliner to inspect its properties.")
    })
}

fn single_node(node: Arc<Node>) -> Dom {
    let id = node.id;

    // Name field (dispatch Rename; consecutive edits coalesce in the undo log).
    let name = Mutable::new(node.name.get_cloned());
    let name_field = TextInput::new(name.clone())
        .on_change(move |v| {
            spawn_local(async move {
                let _ = controller()
                    .dispatch(EditorCommand::Rename { id, name: v })
                    .await;
            });
        })
        .render();

    // Prefab toggle.
    let prefab = Mutable::new(node.prefab.get());
    spawn_local(clone!(prefab => async move {
        let mut first = true;
        prefab.signal().for_each(move |p| {
            let fire = !first;
            first = false;
            async move {
                if fire {
                    let _ = controller().dispatch(EditorCommand::SetPrefab { id, prefab: p }).await;
                }
            }
        }).await;
    }));

    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        // Header row: kind icon + name + kind label.
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "8px").style("padding", "12px 12px 8px")
            .child(Icon::new(super::outliner::kind_icon(&node.kind.get_cloned())).size(16.0).color("var(--accent-bright)").render())
            .child(html!("div", { .style("flex", "1").style("min-width", "0").child(name_field) }))
        }))
        .child(row("Prefab root", toggle(prefab)))
        .child(transform_section(&node))
        .child(Section::new(kind_label(&node.kind.get_cloned())).dense(true)
            .child(html!("div", {
                .style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
                .text("Per-kind properties (geometry · light · camera · material · shadows) land here.")
            }))
            .render())
    })
}

fn transform_section(node: &Arc<Node>) -> Dom {
    let id = node.id;
    let t = node.transform.get();

    // Position
    let n_pos = node.clone();
    let pos = row(
        "Position",
        vec3(f3(t.translation), 0.1, move |v| {
            let mut t = n_pos.transform.get();
            t.translation = [v[0] as f32, v[1] as f32, v[2] as f32];
            dispatch_transform(id, t);
        }),
    );

    // Rotation (Euler degrees)
    let (ex, ey, ez) = Quat::from_array(t.rotation).to_euler(EulerRot::XYZ);
    let n_rot = node.clone();
    let rot = row(
        "Rotation",
        vec3(
            [
                ex.to_degrees() as f64,
                ey.to_degrees() as f64,
                ez.to_degrees() as f64,
            ],
            1.0,
            move |v| {
                let mut t = n_rot.transform.get();
                t.rotation = Quat::from_euler(
                    EulerRot::XYZ,
                    (v[0] as f32).to_radians(),
                    (v[1] as f32).to_radians(),
                    (v[2] as f32).to_radians(),
                )
                .to_array();
                dispatch_transform(id, t);
            },
        ),
    );

    // Scale
    let n_scale = node.clone();
    let scale = row(
        "Scale",
        vec3(f3(t.scale), 0.1, move |v| {
            let mut t = n_scale.transform.get();
            t.scale = [v[0] as f32, v[1] as f32, v[2] as f32];
            dispatch_transform(id, t);
        }),
    );

    Section::new("Transform")
        .child(pos)
        .child(rot)
        .child(scale)
        .render()
}

fn dispatch_transform(id: NodeId, transform: Trs) {
    spawn_local(async move {
        let _ = controller()
            .dispatch(EditorCommand::SetTransform { id, transform })
            .await;
    });
}

fn f3(a: [f32; 3]) -> [f64; 3] {
    [a[0] as f64, a[1] as f64, a[2] as f64]
}

fn batch(count: usize) -> Dom {
    html!("div", {
        .style("padding", "14px")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "10px")
        .child(html!("div", {
            .style("font-size", "13px").style("font-weight", "600").style("color", "var(--text-0)")
            .text(&format!("{count} objects selected"))
        }))
        .child(Btn::new().label("Duplicate selected").icon("copy").variant(BtnVariant::Solid).full(true)
            .on_click(|| for_each_selected(|id| EditorCommand::Duplicate { id })).render())
        .child(Btn::new().label("Deselect all").variant(BtnVariant::Ghost).full(true)
            .on_click(|| spawn_local(async { let _ = controller().dispatch(EditorCommand::SetSelection { ids: vec![] }).await; })).render())
        .child(Btn::new().label("Delete selected").icon("trash").variant(BtnVariant::Ghost).full(true)
            .on_click(|| for_each_selected(|id| EditorCommand::Delete { id })).render())
    })
}

fn for_each_selected(make: fn(NodeId) -> EditorCommand) {
    spawn_local(async move {
        let ids = controller().selected.get_cloned();
        for id in ids {
            let _ = controller().dispatch(make(id)).await;
        }
    });
}

fn kind_label(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Group => "Group",
        NodeKind::Model(_) => "Model",
        NodeKind::Light(_) => "Light",
        NodeKind::Collider(_) => "Collider",
        NodeKind::Camera(_) => "Camera",
        NodeKind::Primitive { .. } => "Geometry",
        NodeKind::Mesh { .. } => "Mesh",
        NodeKind::Curve(_) => "Curve",
        NodeKind::SweepAlongCurve { .. } => "Sweep",
        NodeKind::InstancesAlongCurve(_) => "Instances",
        NodeKind::Line(_) => "Line",
        NodeKind::Sprite(_) => "Sprite",
        NodeKind::ParticleEmitter(_) => "Particle Emitter",
        NodeKind::Decal(_) => "Decal",
    }
}
