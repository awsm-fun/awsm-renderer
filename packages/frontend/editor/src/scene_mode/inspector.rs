//! Inspector (kind-editors.jsx): priority asset > node. M7 core delivers the
//! universal node inspector — name · prefab toggle · Transform (TRS) — plus the
//! batch panel for multi-select. Per-kind editors (Light/Camera/Geometry/
//! MaterialBlock/Shadows) extend this incrementally.

use std::sync::Arc;

use glam::{EulerRot, Quat};

use crate::engine::scene::mutate::find_by_id;
use crate::engine::scene::{
    AssetId, CameraConfig, CameraProjection, ColliderShape, LightConfig, Node, NodeId, NodeKind,
    Trs,
};
use crate::prelude::*;
use awsm_scene_schema::{
    AssetSource, MaterialAlphaMode, MaterialDef, MaterialShading, MeshShadowConfig, PrimitiveShape,
    ProceduralTextureDef, TextureDef,
};

/// The right rail shows the **Asset Inspector** when an asset is selected in the
/// Content Browser (priority asset > node), else the node inspector.
pub fn render() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("height", "100%")
        .style("background", "var(--bg-1)")
        .child_signal(controller().asset_selection.signal().map(|asset| {
            Some(match asset {
                Some(id) => asset_panel(id),
                None => node_panel(),
            })
        }))
    })
}

fn node_panel() -> Dom {
    let ctrl = controller();
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("height", "100%")
        .child(panel_header())
        .child(html!("div", {
            .style("flex", "1")
            .style("overflow-y", "auto")
            // Rebuild on selection change OR a *structural* kind change
            // (`structure_rev` — a discrete PBR↔Unlit / Persp↔Ortho toggle that
            // changes which rows exist). A continuous numeric scrub keeps the
            // structure key constant, so the field being dragged is never torn
            // out mid-drag by its own dispatched edits.
            .child_signal(map_ref! {
                let sel = ctrl.selected.signal_cloned(),
                let _rev = ctrl.structure_rev.signal() =>
                Some(content(sel))
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
        .child(kind_editor(&node))
    })
}

/// Per-kind property editor (the kind-specific Sections). Light, Camera, and
/// Primitive (Geometry + Material + Shadows) are wired; other kinds show a
/// placeholder until their panels land.
fn kind_editor(node: &Arc<Node>) -> Dom {
    match node.kind.get_cloned() {
        NodeKind::Light(cfg) => light_editor(node, &cfg),
        NodeKind::Camera(cfg) => camera_editor(node, &cfg),
        NodeKind::Collider(shape) => collider_editor(node, &shape),
        NodeKind::Primitive {
            shape,
            inline_material,
            custom_material,
            shadow,
            ..
        } => html!("div", {
            .child(geometry_editor(node, &shape))
            .child(material_editor(node, &inline_material, custom_material.is_some()))
            .child(mesh_shadow_editor(node, shadow))
        }),
        other => Section::new(kind_label(&other))
            .dense(true)
            .child(html!("div", {
                .style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
                .text("Properties for this kind land here.")
            }))
            .render(),
    }
}

// ── Camera ──────────────────────────────────────────────────────────────────

fn current_camera(node: &Arc<Node>) -> Option<CameraConfig> {
    match node.kind.get_cloned() {
        NodeKind::Camera(cfg) => Some(cfg),
        _ => None,
    }
}

fn camera_editor(node: &Arc<Node>, cfg: &CameraConfig) -> Dom {
    let is_persp = matches!(cfg.projection, CameraProjection::Perspective { .. });

    // Projection segmented toggle (Persp / Ortho).
    let proj = Mutable::new(
        if is_persp {
            "perspective"
        } else {
            "orthographic"
        }
        .to_string(),
    );
    spawn_local(clone!(proj, node => async move {
        let mut first = true;
        proj.signal_cloned().for_each(move |p| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if !fire { return; }
                let Some(cur) = current_camera(&node) else { return; };
                let want_persp = p == "perspective";
                if want_persp == matches!(cur.projection, CameraProjection::Perspective { .. }) {
                    return; // no variant change
                }
                let projection = if want_persp {
                    CameraProjection::Perspective { fov_y_rad: std::f32::consts::FRAC_PI_3 }
                } else {
                    CameraProjection::Orthographic { half_height: 5.0 }
                };
                dispatch_kind(node.id, NodeKind::Camera(CameraConfig { projection, ..cur }));
            })
        }).await;
    }));

    let mut sec = Section::new("Camera").child(row(
        "Projection",
        segmented(
            proj,
            vec![
                SegOption::new("perspective", "Persp"),
                SegOption::new("orthographic", "Ortho"),
            ],
            true,
            true,
        ),
    ));

    match cfg.projection {
        CameraProjection::Perspective { fov_y_rad } => {
            let n = node.clone();
            sec = sec.child(row(
                "FOV (deg)",
                NumField::new(fov_y_rad.to_degrees() as f64)
                    .min(1.0)
                    .max(179.0)
                    .step(1.0)
                    .on_change(move |v| {
                        if let Some(cur) = current_camera(&n) {
                            dispatch_kind(
                                n.id,
                                NodeKind::Camera(CameraConfig {
                                    projection: CameraProjection::Perspective {
                                        fov_y_rad: (v as f32).to_radians(),
                                    },
                                    ..cur
                                }),
                            );
                        }
                    })
                    .render(),
            ));
        }
        CameraProjection::Orthographic { half_height } => {
            let n = node.clone();
            sec = sec.child(row(
                "Half height",
                NumField::new(half_height as f64)
                    .min(0.01)
                    .step(0.1)
                    .on_change(move |v| {
                        if let Some(cur) = current_camera(&n) {
                            dispatch_kind(
                                n.id,
                                NodeKind::Camera(CameraConfig {
                                    projection: CameraProjection::Orthographic {
                                        half_height: v as f32,
                                    },
                                    ..cur
                                }),
                            );
                        }
                    })
                    .render(),
            ));
        }
    }

    let n = node.clone();
    sec = sec.child(row(
        "Near",
        NumField::new(cfg.near as f64)
            .min(0.001)
            .step(0.05)
            .on_change(move |v| {
                if let Some(cur) = current_camera(&n) {
                    dispatch_kind(
                        n.id,
                        NodeKind::Camera(CameraConfig {
                            near: v as f32,
                            ..cur
                        }),
                    );
                }
            })
            .render(),
    ));
    let n = node.clone();
    sec = sec.child(row(
        "Far",
        NumField::new(cfg.far as f64)
            .min(0.1)
            .step(1.0)
            .on_change(move |v| {
                if let Some(cur) = current_camera(&n) {
                    dispatch_kind(
                        n.id,
                        NodeKind::Camera(CameraConfig {
                            far: v as f32,
                            ..cur
                        }),
                    );
                }
            })
            .render(),
    ));

    sec.render()
}

// ── Collider ──────────────────────────────────────────────────────────────────

fn set_collider(node: &Arc<Node>, shape: ColliderShape) {
    dispatch_kind(node.id, NodeKind::Collider(shape));
}

/// The two `(half_height, radius)` shapes (Capsule/Cylinder/Cone) share an
/// identical editor — `read` extracts the live pair, `make` rebuilds the variant.
/// Reading fresh inside each closure keeps an edit of one field from resetting the
/// other to a stale captured value.
fn hr_rows(
    sec: Section,
    node: &Arc<Node>,
    half_height: f32,
    radius: f32,
    make: fn(f32, f32) -> ColliderShape,
    read: fn(&ColliderShape) -> Option<(f32, f32)>,
) -> Section {
    let n = node.clone();
    let sec = sec.child(row(
        "Half height",
        NumField::new(half_height as f64)
            .min(0.0)
            .step(0.05)
            .on_change(move |v| {
                if let NodeKind::Collider(s) = n.kind.get_cloned() {
                    if let Some((_, r)) = read(&s) {
                        set_collider(&n, make(v as f32, r));
                    }
                }
            })
            .render(),
    ));
    let n = node.clone();
    sec.child(row(
        "Radius",
        NumField::new(radius as f64)
            .min(0.01)
            .step(0.05)
            .on_change(move |v| {
                if let NodeKind::Collider(s) = n.kind.get_cloned() {
                    if let Some((h, _)) = read(&s) {
                        set_collider(&n, make(h, v as f32));
                    }
                }
            })
            .render(),
    ))
}

fn collider_editor(node: &Arc<Node>, shape: &ColliderShape) -> Dom {
    let mut sec = Section::new("Collider");
    match shape {
        ColliderShape::Box { half_extents } => {
            let n = node.clone();
            sec = sec.child(row(
                "Half extents",
                vec3(f3(*half_extents), 0.05, move |v| {
                    set_collider(
                        &n,
                        ColliderShape::Box {
                            half_extents: [v[0] as f32, v[1] as f32, v[2] as f32],
                        },
                    );
                }),
            ));
        }
        ColliderShape::Ellipsoid { half_extents } => {
            let n = node.clone();
            sec = sec.child(row(
                "Half extents",
                vec3(f3(*half_extents), 0.05, move |v| {
                    set_collider(
                        &n,
                        ColliderShape::Ellipsoid {
                            half_extents: [v[0] as f32, v[1] as f32, v[2] as f32],
                        },
                    );
                }),
            ));
        }
        ColliderShape::Sphere { radius } => {
            let n = node.clone();
            sec = sec.child(row(
                "Radius",
                NumField::new(*radius as f64)
                    .min(0.01)
                    .step(0.05)
                    .on_change(move |v| {
                        set_collider(&n, ColliderShape::Sphere { radius: v as f32 });
                    })
                    .render(),
            ));
        }
        ColliderShape::Capsule {
            half_height,
            radius,
        } => {
            sec = hr_rows(
                sec,
                node,
                *half_height,
                *radius,
                |h, r| ColliderShape::Capsule {
                    half_height: h,
                    radius: r,
                },
                |s| match s {
                    ColliderShape::Capsule {
                        half_height,
                        radius,
                    } => Some((*half_height, *radius)),
                    _ => None,
                },
            );
        }
        ColliderShape::Cylinder {
            half_height,
            radius,
        } => {
            sec = hr_rows(
                sec,
                node,
                *half_height,
                *radius,
                |h, r| ColliderShape::Cylinder {
                    half_height: h,
                    radius: r,
                },
                |s| match s {
                    ColliderShape::Cylinder {
                        half_height,
                        radius,
                    } => Some((*half_height, *radius)),
                    _ => None,
                },
            );
        }
        ColliderShape::Cone {
            half_height,
            radius,
        } => {
            sec = hr_rows(
                sec,
                node,
                *half_height,
                *radius,
                |h, r| ColliderShape::Cone {
                    half_height: h,
                    radius: r,
                },
                |s| match s {
                    ColliderShape::Cone {
                        half_height,
                        radius,
                    } => Some((*half_height, *radius)),
                    _ => None,
                },
            );
        }
    }
    sec.render()
}

// ── Material (built-in inline_material) ───────────────────────────────────────

fn current_primitive_material(node: &Arc<Node>) -> Option<MaterialDef> {
    match node.kind.get_cloned() {
        NodeKind::Primitive {
            inline_material, ..
        } => Some(inline_material),
        _ => None,
    }
}

/// Replace a Primitive's `inline_material`, preserving shape/material/custom/shadow.
fn set_inline_material(node: &Arc<Node>, mat: MaterialDef) {
    if let NodeKind::Primitive {
        shape,
        material,
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
                inline_material: mat,
                custom_material,
                shadow,
            },
        );
    }
}

fn material_editor(node: &Arc<Node>, mat: &MaterialDef, has_custom: bool) -> Dom {
    // A custom (Studio) material overrides the built-in palette — surface a
    // link to Material mode rather than the built-in knobs (decision 3).
    if has_custom {
        return Section::new("Material")
            .child(html!("div", {
                .style("display", "flex").style("flex-direction", "column").style("gap", "8px")
                .child(html!("div", {
                    .style("font-size", "12px").style("color", "var(--text-2)").style("line-height", "1.5")
                    .text("Driven by a custom Studio material. Edit its graph in Material mode.")
                }))
                .child(Btn::new().label("Open in Material mode").icon("edit").variant(BtnVariant::Ghost).full(true)
                    .on_click(|| spawn_local(async {
                        let _ = controller().dispatch(EditorCommand::SwitchMode { mode: EditorMode::Material }).await;
                    })).render())
            }))
            .render();
    }

    let mut sec = Section::new("Material");

    // Shading model (PBR / Unlit / Toon).
    let shading_key = match mat.shading {
        MaterialShading::Pbr => "pbr",
        MaterialShading::Unlit => "unlit",
        MaterialShading::Toon { .. } => "toon",
    };
    let shading = Mutable::new(shading_key.to_string());
    spawn_local(clone!(shading, node => async move {
        let mut first = true;
        shading.signal_cloned().for_each(move |s| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if !fire { return; }
                let Some(cur) = current_primitive_material(&node) else { return; };
                let shading = match s.as_str() {
                    "unlit" => MaterialShading::Unlit,
                    "toon" => match cur.shading {
                        MaterialShading::Toon { .. } => cur.shading,
                        _ => MaterialShading::Toon { diffuse_bands: 4, rim_strength: 0.5 },
                    },
                    _ => MaterialShading::Pbr,
                };
                if shading != cur.shading {
                    set_inline_material(&node, MaterialDef { shading, ..cur });
                }
            })
        }).await;
    }));
    sec = sec.child(row(
        "Shading",
        segmented(
            shading,
            vec![
                SegOption::new("pbr", "PBR"),
                SegOption::new("unlit", "Unlit"),
                SegOption::new("toon", "Toon"),
            ],
            true,
            true,
        ),
    ));

    // Base color (RGB swatch) + alpha.
    let col = Mutable::new(rgb_to_hex([
        mat.base_color[0],
        mat.base_color[1],
        mat.base_color[2],
    ]));
    spawn_local(clone!(col, node => async move {
        let mut first = true;
        col.signal_cloned().for_each(move |hex| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if !fire { return; }
                if let (Some(rgb), Some(cur)) = (hex_to_rgb(&hex), current_primitive_material(&node)) {
                    let base_color = [rgb[0], rgb[1], rgb[2], cur.base_color[3]];
                    set_inline_material(&node, MaterialDef { base_color, ..cur });
                }
            })
        }).await;
    }));
    sec = sec.child(row("Base color", swatch(col, 22.0)));

    let n = node.clone();
    sec = sec.child(row(
        "Opacity",
        NumField::new(mat.base_color[3] as f64)
            .min(0.0)
            .max(1.0)
            .step(0.05)
            .on_change(move |v| {
                if let Some(cur) = current_primitive_material(&n) {
                    let mut base_color = cur.base_color;
                    base_color[3] = v as f32;
                    // Opacity < 1 implies a blended material.
                    let alpha_mode = if v < 1.0 {
                        MaterialAlphaMode::Blend
                    } else {
                        MaterialAlphaMode::Opaque
                    };
                    set_inline_material(
                        &n,
                        MaterialDef {
                            base_color,
                            alpha_mode,
                            ..cur
                        },
                    );
                }
            })
            .render(),
    ));

    // PBR-only knobs.
    if matches!(mat.shading, MaterialShading::Pbr) {
        let n = node.clone();
        sec = sec.child(row(
            "Metallic",
            NumField::new(mat.metallic as f64)
                .min(0.0)
                .max(1.0)
                .step(0.05)
                .on_change(move |v| {
                    if let Some(cur) = current_primitive_material(&n) {
                        set_inline_material(
                            &n,
                            MaterialDef {
                                metallic: v as f32,
                                ..cur
                            },
                        );
                    }
                })
                .render(),
        ));
        let n = node.clone();
        sec = sec.child(row(
            "Roughness",
            NumField::new(mat.roughness as f64)
                .min(0.0)
                .max(1.0)
                .step(0.05)
                .on_change(move |v| {
                    if let Some(cur) = current_primitive_material(&n) {
                        set_inline_material(
                            &n,
                            MaterialDef {
                                roughness: v as f32,
                                ..cur
                            },
                        );
                    }
                })
                .render(),
        ));
    }

    // Toon-only knobs.
    if let MaterialShading::Toon {
        diffuse_bands,
        rim_strength,
    } = mat.shading
    {
        let n = node.clone();
        sec = sec.child(row(
            "Diffuse bands",
            NumField::new(diffuse_bands as f64)
                .min(1.0)
                .step(1.0)
                .on_change(move |v| {
                    if let Some(cur) = current_primitive_material(&n) {
                        if let MaterialShading::Toon { rim_strength, .. } = cur.shading {
                            set_inline_material(
                                &n,
                                MaterialDef {
                                    shading: MaterialShading::Toon {
                                        diffuse_bands: (v.round() as u32).max(1),
                                        rim_strength,
                                    },
                                    ..cur
                                },
                            );
                        }
                    }
                })
                .render(),
        ));
        let n = node.clone();
        sec = sec.child(row(
            "Rim strength",
            NumField::new(rim_strength as f64)
                .min(0.0)
                .max(2.0)
                .step(0.05)
                .on_change(move |v| {
                    if let Some(cur) = current_primitive_material(&n) {
                        if let MaterialShading::Toon { diffuse_bands, .. } = cur.shading {
                            set_inline_material(
                                &n,
                                MaterialDef {
                                    shading: MaterialShading::Toon {
                                        diffuse_bands,
                                        rim_strength: v as f32,
                                    },
                                    ..cur
                                },
                            );
                        }
                    }
                })
                .render(),
        ));
    }

    // Emissive color.
    let emi = Mutable::new(rgb_to_hex(mat.emissive));
    spawn_local(clone!(emi, node => async move {
        let mut first = true;
        emi.signal_cloned().for_each(move |hex| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if !fire { return; }
                if let (Some(rgb), Some(cur)) = (hex_to_rgb(&hex), current_primitive_material(&node)) {
                    set_inline_material(&node, MaterialDef { emissive: rgb, ..cur });
                }
            })
        }).await;
    }));
    sec = sec.child(row("Emissive", swatch(emi, 22.0)));

    // Double-sided toggle.
    let ds = Mutable::new(mat.double_sided);
    spawn_local(clone!(ds, node => async move {
        let mut first = true;
        ds.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if !fire { return; }
                if let Some(cur) = current_primitive_material(&node) {
                    if cur.double_sided != on {
                        set_inline_material(&node, MaterialDef { double_sided: on, ..cur });
                    }
                }
            })
        }).await;
    }));
    sec = sec.child(row("Double-sided", check(ds)));

    sec.render()
}

// ── Shadows (per-mesh cast / receive) ─────────────────────────────────────────

/// Replace a Primitive's `shadow`, preserving the rest of the kind.
fn set_mesh_shadow(node: &Arc<Node>, shadow: MeshShadowConfig) {
    if let NodeKind::Primitive {
        shape,
        material,
        inline_material,
        custom_material,
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

fn mesh_shadow_editor(node: &Arc<Node>, shadow: MeshShadowConfig) -> Dom {
    let cast = Mutable::new(shadow.cast);
    spawn_local(clone!(cast, node => async move {
        let mut first = true;
        cast.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if !fire { return; }
                if let NodeKind::Primitive { shadow, .. } = node.kind.get_cloned() {
                    if shadow.cast != on {
                        set_mesh_shadow(&node, MeshShadowConfig { cast: on, ..shadow });
                    }
                }
            })
        }).await;
    }));
    let receive = Mutable::new(shadow.receive);
    spawn_local(clone!(receive, node => async move {
        let mut first = true;
        receive.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if !fire { return; }
                if let NodeKind::Primitive { shadow, .. } = node.kind.get_cloned() {
                    if shadow.receive != on {
                        set_mesh_shadow(&node, MeshShadowConfig { receive: on, ..shadow });
                    }
                }
            })
        }).await;
    }));

    Section::new("Shadows")
        .child(row("Cast", toggle(cast)))
        .child(row("Receive", toggle(receive)))
        .render()
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

// ── Asset Inspector (content-browser.jsx AssetInspector) ──────────────────────

fn close_asset() {
    spawn_local(async {
        let _ = controller()
            .dispatch(EditorCommand::SetAssetSelection { id: None })
            .await;
    });
}

/// The right-rail inspector for a Content Browser asset selection. Reads the
/// project [`AssetTable`] entry for `id`; if it's gone (e.g. just deleted), the
/// selection is cleared back to the node inspector.
fn asset_panel(id: AssetId) -> Dom {
    let ctrl = controller();
    let source = ctrl
        .scene
        .assets
        .lock()
        .unwrap()
        .get(id)
        .map(|e| e.source.clone());
    let Some(source) = source else {
        close_asset();
        return node_panel();
    };

    let (kind_label, icon) = match &source {
        AssetSource::Material(_) => ("Material", "material"),
        AssetSource::Texture(_) => ("Texture", "texture"),
        AssetSource::Mesh(_) => ("Mesh", "cube"),
        _ => ("Asset", "folder"),
    };

    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("height", "100%")
        // Header: kind icon + "{Kind} Asset" + back-to-Properties.
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "8px")
            .style("height", "38px").style("padding", "0 8px 0 14px")
            .style("border-bottom", "1px solid var(--line-soft)").style("flex", "0 0 auto")
            .child(Icon::new(icon).size(15.0).color("var(--accent-bright)").render())
            .child(html!("span", { .style("font-size", "12.5px").style("font-weight", "620").text(&format!("{kind_label} Asset")) }))
            .child(html!("button", {
                .class("t").style("margin-left", "auto")
                .style("display", "flex").style("align-items", "center").style("gap", "4px")
                .style("background", "transparent").style("border-style", "none").style("cursor", "pointer")
                .style("color", "var(--text-2)").style("font-size", "11.5px")
                .attr("title", "Back to node properties")
                .child(Icon::new("chevron").size(13.0).render())
                .child(html!("span", { .text("Properties") }))
                .event(|_: events::Click| close_asset())
            }))
        }))
        // Body.
        .child(html!("div", {
            .style("flex", "1").style("overflow-y", "auto")
            .child(html!("div", {
                .style("padding", "14px")
                .child(html!("div", {
                    .style("height", "110px").style("border-radius", "var(--r3)")
                    .style("background", &asset_swatch_css(&source))
                    .style("border", "1px solid var(--line-strong)")
                    .style("box-shadow", "inset 0 0 0 1px oklch(1 0 0 / .08)")
                }))
            }))
            .child(asset_identity(id, &source))
            .apply(|b| match &source {
                AssetSource::Material(_) => b.child(asset_authoring()),
                AssetSource::Texture(TextureDef::Procedural(p)) => b.child(asset_procedural(p)),
                _ => b,
            })
        }))
        // Footer: delete.
        .child(html!("div", {
            .style("padding", "10px").style("border-top", "1px solid var(--line-soft)")
            .style("display", "flex").style("gap", "8px")
            .child(Btn::new().label("Delete asset").icon("trash").variant(BtnVariant::Ghost).full(true)
                .on_click(move || {
                    spawn_local(async move {
                        let _ = controller().dispatch(EditorCommand::DeleteAsset { id }).await;
                    });
                }).render())
        }))
    })
}

fn asset_identity(id: AssetId, source: &AssetSource) -> Dom {
    let name = asset_name(id, source);
    let mut sec =
        Section::new("Identity").child(row("Name", TextInput::new(Mutable::new(name)).render()));
    match source {
        AssetSource::Material(def) => {
            let (label, tone) = material_badge(def);
            let alpha = match def.alpha_mode {
                MaterialAlphaMode::Opaque => "Opaque",
                MaterialAlphaMode::Mask { .. } => "Mask",
                MaterialAlphaMode::Blend => "Blend",
            };
            sec = sec.child(row(
                "Status",
                html!("div", {
                    .style("display", "flex").style("gap", "6px").style("align-items", "center")
                    .child(badge(label, tone))
                    .child(badge(alpha, Tone::Neutral))
                }),
            ));
        }
        AssetSource::Texture(def) => {
            let kind = match def {
                TextureDef::Raster { .. } => "raster",
                TextureDef::Procedural(ProceduralTextureDef::Checker { .. }) => "checker",
                TextureDef::Procedural(ProceduralTextureDef::Gradient { .. }) => "gradient",
                TextureDef::Procedural(ProceduralTextureDef::Noise { .. }) => "noise",
            };
            sec = sec.child(row("Kind", badge(kind, Tone::Neutral)));
            if let Some((w, h)) = texture_size(def) {
                sec = sec.child(row(
                    "Size",
                    html!("span", { .class("mono").style("font-size", "12px").style("color", "var(--text-1)")
                        .text(&format!("{w} \u{00d7} {h}")) }),
                ));
            }
        }
        _ => {}
    }
    sec.child(row(
        "Used by",
        html!("span", { .style("font-size", "12px").style("color", "var(--text-3)").text("0 objects") }),
    ))
    .render()
}

fn asset_authoring() -> Dom {
    Section::new("Authoring")
        .child(
            Btn::new()
                .label("Edit in Material editor")
                .icon("code")
                .variant(BtnVariant::Primary)
                .full(true)
                .on_click(|| {
                    spawn_local(async {
                        let _ = controller()
                            .dispatch(EditorCommand::SwitchMode {
                                mode: EditorMode::Material,
                            })
                            .await;
                    });
                })
                .render(),
        )
        .child(html!("div", {
            .style("font-size", "11px").style("color", "var(--text-3)").style("line-height", "1.45").style("margin-top", "4px")
            .text("Opens this asset in the Material workspace \u{2014} WGSL, uniforms, preview & registration.")
        }))
        .render()
}

fn asset_procedural(p: &ProceduralTextureDef) -> Dom {
    let (title, rows): (String, Vec<Dom>) = match p {
        ProceduralTextureDef::Checker {
            cells_x, cells_y, ..
        } => (
            "Procedural \u{00b7} checker".to_string(),
            vec![ro_row("Cells", &format!("{cells_x} \u{00d7} {cells_y}"))],
        ),
        ProceduralTextureDef::Gradient { horizontal, .. } => (
            "Procedural \u{00b7} gradient".to_string(),
            vec![ro_row(
                "Direction",
                if *horizontal {
                    "horizontal"
                } else {
                    "vertical"
                },
            )],
        ),
        ProceduralTextureDef::Noise { seed, scale, .. } => (
            "Procedural \u{00b7} noise".to_string(),
            vec![
                ro_row("Seed", &seed.to_string()),
                ro_row("Scale", &fmt_num(*scale as f64)),
            ],
        ),
    };
    let mut sec = Section::new(title);
    for r in rows {
        sec = sec.child(r);
    }
    sec.render()
}

/// A read-only labelled value row for the asset inspector's procedural params.
fn ro_row(label: &str, value: &str) -> Dom {
    row(
        label,
        html!("span", { .class("mono").style("font-size", "12px").style("color", "var(--text-1)").text(value) }),
    )
}

fn fmt_num(n: f64) -> String {
    if n == n.trunc() {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn asset_name(id: AssetId, source: &AssetSource) -> String {
    match source {
        AssetSource::Material(def) if !def.label.is_empty() => def.label.clone(),
        AssetSource::Material(_) => "Material".to_string(),
        AssetSource::Mesh(def) if !def.label.is_empty() => def.label.clone(),
        AssetSource::Mesh(_) => "Mesh".to_string(),
        AssetSource::Texture(TextureDef::Raster { display_name }) => display_name.clone(),
        AssetSource::Texture(TextureDef::Procedural(ProceduralTextureDef::Checker { .. })) => {
            "Checker".to_string()
        }
        AssetSource::Texture(TextureDef::Procedural(ProceduralTextureDef::Gradient { .. })) => {
            "Gradient".to_string()
        }
        AssetSource::Texture(TextureDef::Procedural(ProceduralTextureDef::Noise { .. })) => {
            "Noise".to_string()
        }
        _ => format!("Asset {}", &id.to_string()[..8]),
    }
}

fn material_badge(def: &MaterialDef) -> (String, Tone) {
    match def.shading {
        MaterialShading::Pbr => ("PBR".to_string(), Tone::Accent),
        MaterialShading::Unlit => ("Unlit".to_string(), Tone::Warn),
        MaterialShading::Toon { .. } => ("Toon".to_string(), Tone::Ok),
    }
}

fn texture_size(def: &TextureDef) -> Option<(u32, u32)> {
    match def {
        TextureDef::Procedural(ProceduralTextureDef::Checker { width, height, .. })
        | TextureDef::Procedural(ProceduralTextureDef::Gradient { width, height, .. })
        | TextureDef::Procedural(ProceduralTextureDef::Noise { width, height, .. }) => {
            Some((*width, *height))
        }
        TextureDef::Raster { .. } => None,
    }
}

fn asset_swatch_css(source: &AssetSource) -> String {
    match source {
        AssetSource::Material(def) => {
            let c = def.base_color;
            let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            format!("rgb({}, {}, {})", b(c[0]), b(c[1]), b(c[2]))
        }
        AssetSource::Texture(TextureDef::Procedural(ProceduralTextureDef::Checker {
            color_a,
            color_b,
            ..
        })) => format!(
            "repeating-conic-gradient({} 0% 25%, {} 0% 50%) 50% / 26px 26px",
            rgba_css(*color_a),
            rgba_css(*color_b)
        ),
        AssetSource::Texture(TextureDef::Procedural(ProceduralTextureDef::Gradient {
            color_a,
            color_b,
            ..
        })) => format!(
            "linear-gradient(135deg, {}, {})",
            rgba_css(*color_a),
            rgba_css(*color_b)
        ),
        AssetSource::Texture(TextureDef::Procedural(ProceduralTextureDef::Noise { .. })) => {
            "repeating-linear-gradient(45deg, oklch(0.5 0 0) 0 3px, oklch(0.3 0 0) 3px 6px)"
                .to_string()
        }
        _ => "linear-gradient(135deg, oklch(0.35 0.01 255), oklch(0.22 0.01 255))".to_string(),
    }
}

fn rgba_css(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("rgb({}, {}, {})", b(c[0]), b(c[1]), b(c[2]))
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
