//! Inspector (kind-editors.jsx): priority asset > node. M7 core delivers the
//! universal node inspector — name · prefab toggle · Transform (TRS) — plus the
//! batch panel for multi-select. Per-kind editors (Light/Camera/Geometry/
//! MaterialBlock/Shadows) extend this incrementally.

use std::sync::Arc;

use glam::{EulerRot, Quat};

use crate::engine::scene::mutate::find_by_id;
use crate::engine::scene::{LightConfig, Node, NodeId, NodeKind, Trs};
use crate::prelude::*;
use awsm_scene_schema::PrimitiveShape;

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
            // Rebuild on selection change ONLY (not every revision) so a
            // drag-scrub of a field isn't torn out mid-drag by its own
            // dispatched edits. External changes (undo) refresh on reselect.
            .child_signal(ctrl.selected.signal_cloned().map(|sel| Some(content(&sel))))
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
        .child(kind_editor(&node))
    })
}

/// Per-kind property editor (the kind-specific Section). M7 wires Light fully;
/// Geometry/Camera/Material/Shadows extend here.
fn kind_editor(node: &Arc<Node>) -> Dom {
    match node.kind.get_cloned() {
        NodeKind::Light(cfg) => light_editor(node, &cfg),
        NodeKind::Primitive { shape, .. } => geometry_editor(node, &shape),
        other => Section::new(kind_label(&other))
            .dense(true)
            .child(html!("div", {
                .style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
                .text("Properties for this kind land here (geometry · camera · material · shadows).")
            }))
            .render(),
    }
}

fn light_editor(node: &Arc<Node>, cfg: &LightConfig) -> Dom {
    let color = light_color(cfg);
    let intensity = light_intensity(cfg);

    // Color swatch — observe the picker's Mutable + dispatch SetKind on change.
    let col = Mutable::new(rgb_to_hex(color));
    spawn_local(clone!(col, node => async move {
        let mut first = true;
        col.signal_cloned().for_each(move |hex| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if fire {
                    if let (Some(rgb), Some(cur)) = (hex_to_rgb(&hex), current_light(&node)) {
                        dispatch_kind(node.id, NodeKind::Light(with_color(cur, rgb)));
                    }
                }
            })
        }).await;
    }));

    let n_int = node.clone();
    let intensity_field = NumField::new(intensity as f64)
        .min(0.0)
        .step(0.1)
        .on_change(move |v| {
            if let Some(cur) = current_light(&n_int) {
                dispatch_kind(n_int.id, NodeKind::Light(with_intensity(cur, v as f32)));
            }
        })
        .render();

    let mut sec = Section::new("Light")
        .child(row("Color", swatch(col, 22.0)))
        .child(row("Intensity", intensity_field));

    if let Some(range) = light_range(cfg) {
        let n_r = node.clone();
        sec = sec.child(row(
            "Range",
            NumField::new(range as f64)
                .min(0.0)
                .step(0.5)
                .on_change(move |v| {
                    if let Some(cur) = current_light(&n_r) {
                        dispatch_kind(n_r.id, NodeKind::Light(with_range(cur, v as f32)));
                    }
                })
                .render(),
        ));
    }
    sec.render()
}

fn current_light(node: &Arc<Node>) -> Option<LightConfig> {
    match node.kind.get_cloned() {
        NodeKind::Light(cfg) => Some(cfg),
        _ => None,
    }
}

fn light_color(cfg: &LightConfig) -> [f32; 3] {
    match cfg {
        LightConfig::Directional { color, .. }
        | LightConfig::Point { color, .. }
        | LightConfig::Spot { color, .. } => *color,
    }
}
fn light_intensity(cfg: &LightConfig) -> f32 {
    match cfg {
        LightConfig::Directional { intensity, .. }
        | LightConfig::Point { intensity, .. }
        | LightConfig::Spot { intensity, .. } => *intensity,
    }
}
fn light_range(cfg: &LightConfig) -> Option<f32> {
    match cfg {
        LightConfig::Point { range, .. } | LightConfig::Spot { range, .. } => Some(*range),
        LightConfig::Directional { .. } => None,
    }
}
fn with_color(cfg: LightConfig, color: [f32; 3]) -> LightConfig {
    match cfg {
        LightConfig::Directional {
            intensity, shadow, ..
        } => LightConfig::Directional {
            color,
            intensity,
            shadow,
        },
        LightConfig::Point {
            intensity,
            range,
            shadow,
            ..
        } => LightConfig::Point {
            color,
            intensity,
            range,
            shadow,
        },
        LightConfig::Spot {
            intensity,
            range,
            inner_angle,
            outer_angle,
            shadow,
            ..
        } => LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            shadow,
        },
    }
}
fn with_intensity(cfg: LightConfig, intensity: f32) -> LightConfig {
    match cfg {
        LightConfig::Directional { color, shadow, .. } => LightConfig::Directional {
            color,
            intensity,
            shadow,
        },
        LightConfig::Point {
            color,
            range,
            shadow,
            ..
        } => LightConfig::Point {
            color,
            intensity,
            range,
            shadow,
        },
        LightConfig::Spot {
            color,
            range,
            inner_angle,
            outer_angle,
            shadow,
            ..
        } => LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            shadow,
        },
    }
}
fn with_range(cfg: LightConfig, range: f32) -> LightConfig {
    match cfg {
        LightConfig::Point {
            color,
            intensity,
            shadow,
            ..
        } => LightConfig::Point {
            color,
            intensity,
            range,
            shadow,
        },
        LightConfig::Spot {
            color,
            intensity,
            inner_angle,
            outer_angle,
            shadow,
            ..
        } => LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            shadow,
        },
        other => other,
    }
}

fn geometry_editor(node: &Arc<Node>, shape: &PrimitiveShape) -> Dom {
    let mut sec = Section::new("Geometry");
    let num = |label: &str, val: f64, step: f64, min: f64, on_change: Box<dyn FnMut(f64)>| -> Dom {
        row(
            label,
            NumField::new(val)
                .step(step)
                .min(min)
                .on_change(on_change)
                .render(),
        )
    };
    match shape {
        PrimitiveShape::Plane { width, depth, .. } => {
            let n = node.clone();
            sec = sec.child(num(
                "Width",
                *width as f64,
                0.1,
                0.01,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Plane {
                        depth,
                        segments_x,
                        segments_z,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Plane {
                                width: v as f32,
                                depth,
                                segments_x,
                                segments_z,
                            },
                        );
                    }
                }),
            ));
            let n = node.clone();
            sec = sec.child(num(
                "Depth",
                *depth as f64,
                0.1,
                0.01,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Plane {
                        width,
                        segments_x,
                        segments_z,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Plane {
                                width,
                                depth: v as f32,
                                segments_x,
                                segments_z,
                            },
                        );
                    }
                }),
            ));
        }
        PrimitiveShape::Box { dims } => {
            for (i, axis) in ["Width", "Height", "Depth"].iter().enumerate() {
                let n = node.clone();
                sec = sec.child(num(
                    axis,
                    dims[i] as f64,
                    0.1,
                    0.01,
                    Box::new(move |v| {
                        if let Some(PrimitiveShape::Box { mut dims }) = current_shape(&n) {
                            dims[i] = v as f32;
                            set_shape(&n, PrimitiveShape::Box { dims });
                        }
                    }),
                ));
            }
        }
        PrimitiveShape::Sphere {
            radius,
            segments_long,
            segments_lat,
        } => {
            let n = node.clone();
            sec = sec.child(num(
                "Radius",
                *radius as f64,
                0.05,
                0.01,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Sphere {
                        segments_long,
                        segments_lat,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Sphere {
                                radius: v as f32,
                                segments_long,
                                segments_lat,
                            },
                        );
                    }
                }),
            ));
            let n = node.clone();
            sec = sec.child(num(
                "Segments (long)",
                *segments_long as f64,
                1.0,
                3.0,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Sphere {
                        radius,
                        segments_lat,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Sphere {
                                radius,
                                segments_long: (v.round() as u32).max(3),
                                segments_lat,
                            },
                        );
                    }
                }),
            ));
            let n = node.clone();
            sec = sec.child(num(
                "Segments (lat)",
                *segments_lat as f64,
                1.0,
                2.0,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Sphere {
                        radius,
                        segments_long,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Sphere {
                                radius,
                                segments_long,
                                segments_lat: (v.round() as u32).max(2),
                            },
                        );
                    }
                }),
            ));
        }
        PrimitiveShape::Cylinder {
            radius,
            height,
            radial_segments,
        } => {
            let n = node.clone();
            sec = sec.child(num(
                "Radius",
                *radius as f64,
                0.05,
                0.01,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Cylinder {
                        height,
                        radial_segments,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Cylinder {
                                radius: v as f32,
                                height,
                                radial_segments,
                            },
                        );
                    }
                }),
            ));
            let n = node.clone();
            sec = sec.child(num(
                "Height",
                *height as f64,
                0.1,
                0.01,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Cylinder {
                        radius,
                        radial_segments,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Cylinder {
                                radius,
                                height: v as f32,
                                radial_segments,
                            },
                        );
                    }
                }),
            ));
            let n = node.clone();
            sec = sec.child(num(
                "Segments",
                *radial_segments as f64,
                1.0,
                3.0,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Cylinder { radius, height, .. }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Cylinder {
                                radius,
                                height,
                                radial_segments: (v.round() as u32).max(3),
                            },
                        );
                    }
                }),
            ));
        }
        PrimitiveShape::Cone { radius, height, .. } => {
            let n = node.clone();
            sec = sec.child(num(
                "Radius",
                *radius as f64,
                0.05,
                0.01,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Cone {
                        height,
                        radial_segments,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Cone {
                                radius: v as f32,
                                height,
                                radial_segments,
                            },
                        );
                    }
                }),
            ));
            let n = node.clone();
            sec = sec.child(num(
                "Height",
                *height as f64,
                0.1,
                0.01,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Cone {
                        radius,
                        radial_segments,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Cone {
                                radius,
                                height: v as f32,
                                radial_segments,
                            },
                        );
                    }
                }),
            ));
        }
        PrimitiveShape::Torus {
            radius, thickness, ..
        } => {
            let n = node.clone();
            sec = sec.child(num(
                "Radius",
                *radius as f64,
                0.05,
                0.01,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Torus {
                        thickness,
                        segments_major,
                        segments_minor,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Torus {
                                radius: v as f32,
                                thickness,
                                segments_major,
                                segments_minor,
                            },
                        );
                    }
                }),
            ));
            let n = node.clone();
            sec = sec.child(num(
                "Thickness",
                *thickness as f64,
                0.02,
                0.005,
                Box::new(move |v| {
                    if let Some(PrimitiveShape::Torus {
                        radius,
                        segments_major,
                        segments_minor,
                        ..
                    }) = current_shape(&n)
                    {
                        set_shape(
                            &n,
                            PrimitiveShape::Torus {
                                radius,
                                thickness: v as f32,
                                segments_major,
                                segments_minor,
                            },
                        );
                    }
                }),
            ));
        }
    }
    sec.render()
}

fn current_shape(node: &Arc<Node>) -> Option<PrimitiveShape> {
    match node.kind.get_cloned() {
        NodeKind::Primitive { shape, .. } => Some(shape),
        _ => None,
    }
}

fn set_shape(node: &Arc<Node>, shape: PrimitiveShape) {
    if let NodeKind::Primitive {
        material,
        inline_material,
        custom_material,
        shadow,
        ..
    } = node.kind.get_cloned()
    {
        dispatch_kind(
            node.id,
            NodeKind::Primitive {
                shape,
                material,
                inline_material,
                custom_material,
                shadow,
            },
        );
    }
}

fn dispatch_kind(id: NodeId, kind: NodeKind) {
    spawn_local(async move {
        let _ = controller()
            .dispatch(EditorCommand::SetKind {
                id,
                kind: Box::new(kind),
            })
            .await;
    });
}

fn rgb_to_hex(c: [f32; 3]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}
fn hex_to_rgb(hex: &str) -> Option<[f32; 3]> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
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
