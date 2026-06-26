//! Inspector: priority asset > node. The universal node inspector — name ·
//! prefab toggle · Transform (TRS) — plus the batch panel for multi-select.
//! Per-kind editors (Light/Camera/Geometry/MaterialBlock/Shadows) extend this
//! incrementally.

use std::sync::Arc;

use glam::{EulerRot, Quat};

use crate::engine::scene::mutate::find_by_id;
use crate::engine::scene::{
    AssetId, CameraConfig, CameraProjection, ColliderShape, LightConfig, Node, NodeId, NodeKind,
    Trs,
};
use crate::prelude::*;
use awsm_renderer_editor_protocol::{
    AssetSource, BillboardMode, CubeFaceUpdateRate, CurveDef, DecalConfig, EvsmCutoff,
    FarCascadeUpdateRate, LightShadowConfig, LightShadowHardness, LineDef, MaterialAlphaMode,
    MaterialDef, MaterialShading, MeshLodConfig, MeshShadowConfig, ParticleEmitterDef,
    PrimitiveShape, ProceduralTextureDef, SpriteAlphaMode, SpriteDef, TextureDef,
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
        .child(export_node_section(&node))
    })
}

/// "Export GLB" for the selected node's subtree (geometry-bearing kinds +
/// Group containers). Downloads a binary glTF; re-import is up to the user.
fn export_node_section(node: &Arc<Node>) -> Dom {
    let exportable = matches!(
        node.kind.get_cloned(),
        NodeKind::Mesh { .. } | NodeKind::SkinnedMesh { .. } | NodeKind::Group
    );
    if !exportable {
        return html!("div");
    }
    let node_id = node.id;
    let raw_name = node.name.get_cloned();
    html!("div", {
        .style("margin", "10px 12px 0")
        .child(Btn::new()
            .label("Export GLB")
            .icon("mesh")
            .variant(BtnVariant::Ghost)
            .full(true)
            .on_click(move || {
                let file = {
                    let n = raw_name.trim();
                    if n.is_empty() { "node.glb".to_string() } else { format!("{n}.glb") }
                };
                spawn_local(async move {
                    match crate::controller::export::export_glb(&controller().scene, Some(node_id)).await {
                        Ok(bytes) => {
                            crate::app::download_bytes(&file, &bytes);
                            Toast::info(format!("Exported {file} ({} KB)", bytes.len() / 1024));
                        }
                        Err(e) => Toast::error(format!("Export failed: {e}")),
                    }
                });
            })
            .render())
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
        NodeKind::InstancesAlongCurve(_) => instances_editor(node),
        NodeKind::Curve(def) => curve_editor(node, &def),
        NodeKind::Sprite(def) => sprite_editor(node, &def),
        NodeKind::Line(def) => line_editor(node, &def),
        NodeKind::Decal(cfg) => decal_editor(node, &cfg),
        NodeKind::ParticleEmitter(def) => particle_editor(node, &def),
        // The sole procedural-geometry node. Shows an informational geometry
        // summary (the stack base + modifier count — editing is via MCP) plus the
        // per-mesh surface: material (built-in/dynamic + per-mesh uniforms) and
        // shadow flags.
        NodeKind::Mesh {
            mesh,
            material,
            shadow,
            lod,
        } => html!("div", {
            .child(mesh_geometry_section(mesh.0))
            .child(material_editor(node, &inline_of(&material), material.is_some()))
            .child(mesh_shadow_editor(node, shadow))
            .child(mesh_lod_editor(node, lod))
        }),
        // A skinned glTF import: not editable geometry (its per-vertex skin
        // weights can't survive topology edits). Shows the material + shadow
        // surface plus a "Drop Skinning" action that bakes the bind pose into a
        // static editable Mesh (terminal).
        NodeKind::SkinnedMesh {
            material,
            shadow,
            lod,
            ..
        } => {
            let node_id = node.id;
            html!("div", {
                .child(info_section(
                    "Skinned Mesh",
                    "An imported, rigged mesh deformed by the renderer's skin path \
                     (driven by its bones + animation clips). It is NOT editable — \
                     per-vertex skin weights can't survive topology-changing edits. \
                     Drop skinning to bake the bind pose into a static, editable mesh.",
                ))
                .child(material_editor(node, &inline_of(&material), material.is_some()))
                .child(mesh_shadow_editor(node, shadow))
                .child(mesh_lod_editor(node, lod))
                .child(html!("div", {
                    .style("margin", "8px 0")
                    .child(Btn::new()
                        .label("Drop Skinning")
                        .icon("cube")
                        .variant(BtnVariant::Ghost)
                        .full(true)
                        .on_click(move || {
                            spawn_local(async move {
                                let _ = controller()
                                    .dispatch(EditorCommand::DropSkinning { node: node_id })
                                    .await;
                            });
                        })
                        .render())
                }))
            })
        }
        // A pre-baked cluster ("nanite") mesh: view-only, rendered via the bounded
        // cluster pipeline from its baked DAG (same path the player uses). Not
        // editable — no modifier stack / per-vertex ops; it IS the LOD. Shows the
        // material + shadow surface only.
        NodeKind::ClusterMesh {
            material, shadow, ..
        } => html!("div", {
            .child(info_section(
                "Nanite Mesh",
                "A pre-baked cluster-LOD mesh, rendered as nanite (bounded draw + VRAM) \
                 via the renderer's cluster pipeline — the same path the player uses, no \
                 in-editor re-baking. View-only: it has no editable geometry stack (bake \
                 it offline with the awsm-renderer-lod-bake CLI). Move/scale it like any node and \
                 assign a material below.",
            ))
            .child(material_editor(node, &inline_of(&material), material.is_some()))
            .child(mesh_shadow_editor(node, shadow))
        }),
        // A Group is purely an organisational transform parent — name + transform
        // (above) are its full property set.
        NodeKind::Group => info_section("Group", "An organizational parent. Its children inherit this node's transform; it has no geometry of its own."),
    }
}

/// Geometry editor for a `NodeKind::Mesh`. When the backing `MeshDef`'s stack
/// base is a built-in **primitive**, expose live editable parameters (box dims,
/// sphere/cylinder/cone radius + segments, plane subdivisions, torus, …) — each
/// rewrites the stack base via `SetMeshModifiers`, which re-bakes the mesh + the
/// bridge re-materializes referencing nodes. Non-primitive bases (lathe /
/// superquadric / sweep / SDF / captured) + the modifier stack stay MCP-authored,
/// so those show a read-only summary. Falls back to a "missing asset" note.
fn mesh_geometry_section(mesh: awsm_renderer_editor_protocol::AssetId) -> Dom {
    use awsm_renderer_editor_protocol::MeshBase;

    let (prim, base_name, mod_count) = {
        let ctrl = controller();
        let assets = ctrl.scene.assets.lock().unwrap();
        match assets.get(mesh).map(|e| &e.source) {
            Some(AssetSource::Mesh(def)) => {
                let n = def.stack.modifiers.len();
                match &def.stack.base {
                    MeshBase::Primitive(s) => (Some(s.clone()), None, n),
                    MeshBase::Lathe { .. } => (None, Some("lathe"), n),
                    MeshBase::Superquadric { .. } => (None, Some("superquadric"), n),
                    MeshBase::Sweep(_) => (None, Some("sweep along curve"), n),
                    MeshBase::Captured(_) => (None, Some("captured"), n),
                    MeshBase::Sdf { .. } => (None, Some("SDF/CSG"), n),
                }
            }
            _ => (None, Some("geometry asset missing"), 0),
        }
    };

    let mut sec = Section::new("Geometry").dense(true);
    match prim {
        Some(shape) => {
            sec = primitive_fields(sec, mesh, &shape);
            if mod_count > 0 {
                let mods = if mod_count == 1 {
                    "1 modifier".to_string()
                } else {
                    format!("{mod_count} modifiers")
                };
                sec = sec.child(html!("div", {
                    .style("font-size", "11px").style("color", "var(--text-4)").style("margin-top", "6px")
                    .text(&format!("+ {mods} on the stack (edit via the MCP mesh tools)"))
                }));
            }
        }
        None => {
            sec = sec
                .child(html!("div", {
                    .style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
                    .text(&format!("base: {}", base_name.unwrap_or("")))
                }))
                .child(html!("div", {
                    .style("font-size", "11px").style("color", "var(--text-4)").style("margin-top", "4px")
                    .text("Edit the modifier stack + vertices via the MCP mesh tools.")
                }));
        }
    }
    sec.render()
}

/// Replace the mesh's stack base with `shape` (preserving its modifiers) and
/// dispatch `SetMeshModifiers` so the geometry re-bakes live.
fn apply_primitive(mesh: awsm_renderer_editor_protocol::AssetId, shape: PrimitiveShape) {
    use awsm_renderer_editor_protocol::MeshBase;
    let stack = {
        let ctrl = controller();
        let assets = ctrl.scene.assets.lock().unwrap();
        match assets.get(mesh).map(|e| &e.source) {
            Some(AssetSource::Mesh(def)) => Some(def.stack.clone()),
            _ => None,
        }
    };
    if let Some(mut stack) = stack {
        stack.base = MeshBase::Primitive(shape);
        spawn_local(async move {
            let _ = controller()
                .dispatch(EditorCommand::SetMeshModifiers { mesh, stack })
                .await;
        });
    }
}

/// Append the per-variant editable NumFields for a primitive base. Each field's
/// `on_change` rebuilds the full `PrimitiveShape` (the other params captured from
/// the current snapshot) and calls [`apply_primitive`]. `u32` segment counts are
/// floored to their topological minimum.
fn primitive_fields(
    sec: Section,
    mesh: awsm_renderer_editor_protocol::AssetId,
    shape: &PrimitiveShape,
) -> Section {
    use PrimitiveShape as P;
    // `mesh` is Copy; build an f32 field whose change maps to a new shape.
    let f = move |val: f32, min: f64, step: f64, build: Box<dyn Fn(f64) -> P>| {
        NumField::new(val as f64)
            .min(min)
            .step(step)
            .on_change(move |v| apply_primitive(mesh, build(v)))
            .render()
    };
    match shape {
        P::Plane {
            width,
            depth,
            segments_x,
            segments_z,
        } => {
            let (w, d, sx, sz) = (*width, *depth, *segments_x, *segments_z);
            sec.child(row(
                "Width",
                f(
                    w,
                    0.0,
                    0.1,
                    Box::new(move |v| P::Plane {
                        width: v as f32,
                        depth: d,
                        segments_x: sx,
                        segments_z: sz,
                    }),
                ),
            ))
            .child(row(
                "Depth",
                f(
                    d,
                    0.0,
                    0.1,
                    Box::new(move |v| P::Plane {
                        width: w,
                        depth: v as f32,
                        segments_x: sx,
                        segments_z: sz,
                    }),
                ),
            ))
            .child(row(
                "Segments X",
                f(
                    sx as f32,
                    1.0,
                    1.0,
                    Box::new(move |v| P::Plane {
                        width: w,
                        depth: d,
                        segments_x: (v as u32).max(1),
                        segments_z: sz,
                    }),
                ),
            ))
            .child(row(
                "Segments Z",
                f(
                    sz as f32,
                    1.0,
                    1.0,
                    Box::new(move |v| P::Plane {
                        width: w,
                        depth: d,
                        segments_x: sx,
                        segments_z: (v as u32).max(1),
                    }),
                ),
            ))
        }
        P::Box { dims } => {
            let dm = *dims;
            sec.child(row(
                "Width",
                f(
                    dm[0],
                    0.0,
                    0.1,
                    Box::new(move |v| P::Box {
                        dims: [v as f32, dm[1], dm[2]],
                    }),
                ),
            ))
            .child(row(
                "Height",
                f(
                    dm[1],
                    0.0,
                    0.1,
                    Box::new(move |v| P::Box {
                        dims: [dm[0], v as f32, dm[2]],
                    }),
                ),
            ))
            .child(row(
                "Depth",
                f(
                    dm[2],
                    0.0,
                    0.1,
                    Box::new(move |v| P::Box {
                        dims: [dm[0], dm[1], v as f32],
                    }),
                ),
            ))
        }
        P::Sphere {
            radius,
            segments_long,
            segments_lat,
        } => {
            let (r, sl, sa) = (*radius, *segments_long, *segments_lat);
            sec.child(row(
                "Radius",
                f(
                    r,
                    0.0,
                    0.1,
                    Box::new(move |v| P::Sphere {
                        radius: v as f32,
                        segments_long: sl,
                        segments_lat: sa,
                    }),
                ),
            ))
            .child(row(
                "Segments Long",
                f(
                    sl as f32,
                    3.0,
                    1.0,
                    Box::new(move |v| P::Sphere {
                        radius: r,
                        segments_long: (v as u32).max(3),
                        segments_lat: sa,
                    }),
                ),
            ))
            .child(row(
                "Segments Lat",
                f(
                    sa as f32,
                    2.0,
                    1.0,
                    Box::new(move |v| P::Sphere {
                        radius: r,
                        segments_long: sl,
                        segments_lat: (v as u32).max(2),
                    }),
                ),
            ))
        }
        P::Cylinder {
            radius,
            height,
            radial_segments,
        } => {
            let (r, h, rs) = (*radius, *height, *radial_segments);
            sec.child(row(
                "Radius",
                f(
                    r,
                    0.0,
                    0.1,
                    Box::new(move |v| P::Cylinder {
                        radius: v as f32,
                        height: h,
                        radial_segments: rs,
                    }),
                ),
            ))
            .child(row(
                "Height",
                f(
                    h,
                    0.0,
                    0.1,
                    Box::new(move |v| P::Cylinder {
                        radius: r,
                        height: v as f32,
                        radial_segments: rs,
                    }),
                ),
            ))
            .child(row(
                "Radial Segments",
                f(
                    rs as f32,
                    3.0,
                    1.0,
                    Box::new(move |v| P::Cylinder {
                        radius: r,
                        height: h,
                        radial_segments: (v as u32).max(3),
                    }),
                ),
            ))
        }
        P::Cone {
            radius,
            height,
            radial_segments,
        } => {
            let (r, h, rs) = (*radius, *height, *radial_segments);
            sec.child(row(
                "Radius",
                f(
                    r,
                    0.0,
                    0.1,
                    Box::new(move |v| P::Cone {
                        radius: v as f32,
                        height: h,
                        radial_segments: rs,
                    }),
                ),
            ))
            .child(row(
                "Height",
                f(
                    h,
                    0.0,
                    0.1,
                    Box::new(move |v| P::Cone {
                        radius: r,
                        height: v as f32,
                        radial_segments: rs,
                    }),
                ),
            ))
            .child(row(
                "Radial Segments",
                f(
                    rs as f32,
                    3.0,
                    1.0,
                    Box::new(move |v| P::Cone {
                        radius: r,
                        height: h,
                        radial_segments: (v as u32).max(3),
                    }),
                ),
            ))
        }
        P::Torus {
            radius,
            thickness,
            segments_major,
            segments_minor,
        } => {
            let (r, t, sj, sn) = (*radius, *thickness, *segments_major, *segments_minor);
            sec.child(row(
                "Radius",
                f(
                    r,
                    0.0,
                    0.1,
                    Box::new(move |v| P::Torus {
                        radius: v as f32,
                        thickness: t,
                        segments_major: sj,
                        segments_minor: sn,
                    }),
                ),
            ))
            .child(row(
                "Thickness",
                f(
                    t,
                    0.0,
                    0.05,
                    Box::new(move |v| P::Torus {
                        radius: r,
                        thickness: v as f32,
                        segments_major: sj,
                        segments_minor: sn,
                    }),
                ),
            ))
            .child(row(
                "Segments Major",
                f(
                    sj as f32,
                    3.0,
                    1.0,
                    Box::new(move |v| P::Torus {
                        radius: r,
                        thickness: t,
                        segments_major: (v as u32).max(3),
                        segments_minor: sn,
                    }),
                ),
            ))
            .child(row(
                "Segments Minor",
                f(
                    sn as f32,
                    3.0,
                    1.0,
                    Box::new(move |v| P::Torus {
                        radius: r,
                        thickness: t,
                        segments_major: sj,
                        segments_minor: (v as u32).max(3),
                    }),
                ),
            ))
        }
    }
}

/// A small read-only info Section (used for kinds whose only settable properties
/// are the universal name/transform/visibility above).
fn info_section(title: &str, body: &str) -> Dom {
    Section::new(title)
        .dense(true)
        .child(html!("div", {
            .style("font-size", "12px").style("color", "var(--text-3)").style("line-height", "1.5")
            .text(body)
        }))
        .render()
}

// ── Sweep / Instances curve-reference pickers ───────────────────────────────

/// Collect `(id, name)` of every scene node whose kind matches `pred`, for a
/// reference dropdown.
fn collect_kind_nodes(pred: impl Fn(&NodeKind) -> bool) -> Vec<(NodeId, String)> {
    fn walk(
        nodes: &[Arc<Node>],
        pred: &dyn Fn(&NodeKind) -> bool,
        out: &mut Vec<(NodeId, String)>,
    ) {
        for n in nodes {
            if pred(&n.kind.get_cloned()) {
                out.push((n.id, n.name.get_cloned()));
            }
            walk(&n.children.lock_ref(), pred, out);
        }
    }
    let mut out = Vec::new();
    walk(&controller().scene.nodes.lock_ref(), &pred, &mut out);
    out
}

/// A labelled node-reference dropdown: options are the eligible nodes (plus a
/// "— none —" entry); selecting one runs `on_pick(node_id)`.
fn ref_picker(
    label: &str,
    eligible: Vec<(NodeId, String)>,
    current: NodeId,
    on_pick: impl Fn(NodeId) + 'static,
) -> Dom {
    // The "— none —" entry uses the stable nil sentinel (all-zeros), NOT
    // `NodeId::default()` — that mints a *fresh random* id each call, so it could
    // never round-trip as a real "unset" marker (and picking it wrote garbage).
    let mut options: Vec<(String, String)> = vec![(NodeId::nil().to_string(), "— none —".into())];
    options.extend(
        eligible
            .iter()
            .map(|(id, name)| (id.to_string(), name.clone())),
    );
    let sel = Mutable::new(current.to_string());
    let lookup: Vec<(String, NodeId)> = eligible
        .iter()
        .map(|(id, _)| (id.to_string(), *id))
        .collect();
    spawn_local(clone!(sel => async move {
        let mut first = true;
        sel.signal_cloned()
            .for_each(move |val| {
                let fire = !first;
                first = false;
                let picked = lookup
                    .iter()
                    .find(|(s, _)| *s == val)
                    .map(|(_, id)| *id)
                    .unwrap_or(NodeId::nil());
                if fire {
                    on_pick(picked);
                }
                async {}
            })
            .await;
    }));
    row(label, select(sel, options))
}

fn instances_editor(node: &Arc<Node>) -> Dom {
    let id = node.id;
    let def = match node.kind.get_cloned() {
        NodeKind::InstancesAlongCurve(def) => def,
        _ => return html!("div", {}),
    };
    let curves = collect_kind_nodes(|k| matches!(k, NodeKind::Curve(_)));
    let sources = collect_kind_nodes(|k| matches!(k, NodeKind::Mesh { .. }));
    let n_curve = node.clone();
    let n_src = node.clone();
    Section::new("Instances")
        .child(ref_picker("Curve", curves, def.curve_node, move |picked| {
            if let NodeKind::InstancesAlongCurve(mut def) = n_curve.kind.get_cloned() {
                def.curve_node = picked;
                dispatch_kind(id, NodeKind::InstancesAlongCurve(def));
            }
        }))
        .child(ref_picker(
            "Source",
            sources,
            def.source_node,
            move |picked| {
                if let NodeKind::InstancesAlongCurve(mut def) = n_src.kind.get_cloned() {
                    def.source_node = picked;
                    dispatch_kind(id, NodeKind::InstancesAlongCurve(def));
                }
            },
        ))
        .render()
}

// ── Passive-kind editors (Curve / Sprite / Line / Decal / Particle) ──────────

/// A toggle row that dispatches a `SetKind` (rebuilt by `apply`) when flipped.
fn kind_toggle_row(
    node: &Arc<Node>,
    label: &str,
    value: bool,
    apply: impl Fn(NodeKind, bool) -> Option<NodeKind> + 'static,
) -> Dom {
    let state = Mutable::new(value);
    let node = node.clone();
    let apply = std::rc::Rc::new(apply);
    spawn_local(clone!(state => async move {
        let mut first = true;
        state.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            let node = node.clone();
            let apply = apply.clone();
            async move {
                if fire {
                    if let Some(k) = apply(node.kind.get_cloned(), on) {
                        dispatch_kind(node.id, k);
                    }
                }
            }
        }).await;
    }));
    row(label, toggle(state))
}

/// A select row that dispatches a `SetKind` (rebuilt by `apply`) on change.
fn kind_select_row(
    node: &Arc<Node>,
    label: &str,
    current: &str,
    options: Vec<(&str, &str)>,
    apply: impl Fn(NodeKind, &str) -> Option<NodeKind> + 'static,
) -> Dom {
    let state = Mutable::new(current.to_string());
    let node = node.clone();
    let apply = std::rc::Rc::new(apply);
    spawn_local(clone!(state => async move {
        let mut first = true;
        state.signal_cloned().for_each(move |v| {
            let fire = !first;
            first = false;
            let node = node.clone();
            let apply = apply.clone();
            async move {
                if fire {
                    if let Some(k) = apply(node.kind.get_cloned(), &v) {
                        dispatch_kind(node.id, k);
                    }
                }
            }
        }).await;
    }));
    let options = options
        .into_iter()
        .map(|(v, l)| (v.to_string(), l.to_string()))
        .collect();
    row(label, select(state, options))
}

/// A read-only informational line (counts / status that aren't directly editable yet).
fn info_text(text: impl Into<String>) -> Dom {
    html!("div", {
        .style("font-size", "11.5px").style("color", "var(--text-3)")
        .style("line-height", "1.5").style("padding", "2px 0")
        .text(&text.into())
    })
}

fn curve_editor(node: &Arc<Node>, def: &CurveDef) -> Dom {
    let n = node.clone();
    let tension = NumField::new(def.tension as f64)
        .min(0.0)
        .step(0.05)
        .on_change(move |v| {
            if let NodeKind::Curve(mut d) = n.kind.get_cloned() {
                d.tension = v.clamp(0.0, 1.0) as f32;
                dispatch_kind(n.id, NodeKind::Curve(d));
            }
        })
        .render();
    let n = node.clone();
    let samples = NumField::new(def.sample_count as f64)
        .min(2.0)
        .step(1.0)
        .on_change(move |v| {
            if let NodeKind::Curve(mut d) = n.kind.get_cloned() {
                d.sample_count = v.max(2.0) as u32;
                dispatch_kind(n.id, NodeKind::Curve(d));
            }
        })
        .render();
    // The control-point list rebuilds only when the point COUNT changes
    // (add/delete) — keyed on `control_points.len()` + `dedupe()` — so editing a
    // single coordinate doesn't tear down the field being typed in. Every edit
    // dispatches a fresh `CurveDef`, which the bridge's kind subscription picks
    // up to re-materialize the curve polyline live.
    let node_for_pts = node.clone();
    let points_signal = node
        .kind
        .signal_ref(|k| match k {
            NodeKind::Curve(d) => d.control_points.len(),
            _ => 0,
        })
        .dedupe()
        .map(move |_count| Some(curve_points_list(&node_for_pts)));
    let points = html!("div", {
        .child_signal(points_signal)
    });
    Section::new("Curve")
        .child(row("Tension", tension))
        .child(row("Samples", samples))
        .child(kind_toggle_row(node, "Closed", def.closed, |k, on| {
            if let NodeKind::Curve(mut d) = k {
                d.closed = on;
                Some(NodeKind::Curve(d))
            } else {
                None
            }
        }))
        .child(points)
        .render()
}

/// The editable control-point list for the curve's *current* kind: a labelled
/// row per point (X/Y/Z fields + delete) followed by an "Add point" button.
/// Rebuilt by [`curve_editor`] whenever the point count changes.
fn curve_points_list(node: &Arc<Node>) -> Dom {
    let pts = match node.kind.get_cloned() {
        NodeKind::Curve(d) => d.control_points,
        _ => return html!("div", {}),
    };
    let n_points = pts.len();
    let mut kids: Vec<Dom> = Vec::with_capacity(n_points + 2);
    kids.push(html!("div", {
        .style("margin", "10px 0 2px").style("font-size", "11px").style("font-weight", "600")
        .style("letter-spacing", ".04em").style("text-transform", "uppercase")
        .style("color", "var(--text-3)")
        .text(&format!("Control points ({n_points})"))
    }));
    for i in 0..n_points {
        kids.push(curve_point_row(node, i, n_points));
    }
    let n = node.clone();
    let add_btn = Btn::new()
        .label("Add point")
        .icon("plus")
        .variant(BtnVariant::Ghost)
        .size(BtnSize::Sm)
        .on_click(move || {
            if let NodeKind::Curve(mut d) = n.kind.get_cloned() {
                let p = &d.control_points;
                // Extrapolate one segment past the last point so a new point
                // naturally *elongates* the curve in its current direction
                // (rather than stacking at the origin).
                let new_p = match p.len() {
                    0 => [0.0, 0.0, 0.0],
                    1 => [p[0][0] + 1.0, p[0][1], p[0][2]],
                    len => {
                        let a = p[len - 2];
                        let b = p[len - 1];
                        [2.0 * b[0] - a[0], 2.0 * b[1] - a[1], 2.0 * b[2] - a[2]]
                    }
                };
                d.control_points.push(new_p);
                dispatch_kind(n.id, NodeKind::Curve(d));
            }
        })
        .render();
    kids.push(html!("div", {
        .style("margin-top", "6px")
        .child(add_btn)
    }));
    html!("div", { .children(kids) })
}

/// A single control point: index label, axis-tinted X/Y/Z fields (color-coded +
/// drag-to-scrub, like the Transform editor), and a delete button. The fields
/// track the live kind via a signal, so a viewport handle drag updates them in
/// real time. Delete is omitted at the 2-point minimum a Catmull-Rom curve needs.
fn curve_point_row(node: &Arc<Node>, index: usize, n_points: usize) -> Dom {
    let n = node.clone();
    let fields = vec3_signal(
        node.kind.signal_ref(move |k| match k {
            NodeKind::Curve(d) => d
                .control_points
                .get(index)
                .map(|p| f3(*p))
                .unwrap_or([0.0; 3]),
            _ => [0.0; 3],
        }),
        0.1,
        move |v| {
            if let NodeKind::Curve(mut d) = n.kind.get_cloned() {
                if let Some(pt) = d.control_points.get_mut(index) {
                    *pt = [v[0] as f32, v[1] as f32, v[2] as f32];
                    dispatch_kind(n.id, NodeKind::Curve(d));
                }
            }
        },
    );
    let fields_wrap = html!("div", { .style("flex", "1").style("min-width", "0").child(fields) });
    let control = if n_points > 2 {
        let n = node.clone();
        let del = IconBtn::new("trash")
            .size(15.0)
            .on_click(move || {
                if let NodeKind::Curve(mut d) = n.kind.get_cloned() {
                    if d.control_points.len() > 2 && index < d.control_points.len() {
                        d.control_points.remove(index);
                        dispatch_kind(n.id, NodeKind::Curve(d));
                    }
                }
            })
            .render();
        html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "6px")
            .child(fields_wrap)
            .child(del)
        })
    } else {
        html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "6px")
            .child(fields_wrap)
        })
    };
    row(format!("P{index}"), control)
}

fn line_editor(node: &Arc<Node>, def: &LineDef) -> Dom {
    let n = node.clone();
    let width = NumField::new(def.width_px as f64)
        .min(0.5)
        .step(0.5)
        .on_change(move |v| {
            if let NodeKind::Line(mut d) = n.kind.get_cloned() {
                d.width_px = v.max(0.5) as f32;
                dispatch_kind(n.id, NodeKind::Line(d));
            }
        })
        .render();
    Section::new("Line")
        .child(row("Width (px)", width))
        .child(kind_toggle_row(
            node,
            "Always on top",
            def.depth_test_always,
            |k, on| {
                if let NodeKind::Line(mut d) = k {
                    d.depth_test_always = on;
                    Some(NodeKind::Line(d))
                } else {
                    None
                }
            },
        ))
        .child(info_text(format!("{} points", def.points.len())))
        .render()
}

fn sprite_editor(node: &Arc<Node>, def: &SpriteDef) -> Dom {
    let n = node.clone();
    let w = NumField::new(def.size[0] as f64)
        .min(0.001)
        .step(0.1)
        .on_change(move |v| {
            if let NodeKind::Sprite(mut d) = n.kind.get_cloned() {
                d.size[0] = v as f32;
                dispatch_kind(n.id, NodeKind::Sprite(d));
            }
        })
        .render();
    let n = node.clone();
    let h = NumField::new(def.size[1] as f64)
        .min(0.001)
        .step(0.1)
        .on_change(move |v| {
            if let NodeKind::Sprite(mut d) = n.kind.get_cloned() {
                d.size[1] = v as f32;
                dispatch_kind(n.id, NodeKind::Sprite(d));
            }
        })
        .render();
    // Tint RGB swatch (alpha edited separately).
    let col = Mutable::new(rgb_to_hex([def.tint[0], def.tint[1], def.tint[2]]));
    spawn_local(clone!(col, node => async move {
        let mut first = true;
        col.signal_cloned().for_each(move |hex| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if fire {
                    if let (Some(rgb), NodeKind::Sprite(mut d)) = (hex_to_rgb(&hex), node.kind.get_cloned()) {
                        d.tint = [rgb[0], rgb[1], rgb[2], d.tint[3]];
                        dispatch_kind(node.id, NodeKind::Sprite(d));
                    }
                }
            })
        }).await;
    }));
    let n = node.clone();
    let alpha = NumField::new(def.tint[3] as f64)
        .min(0.0)
        .step(0.05)
        .on_change(move |v| {
            if let NodeKind::Sprite(mut d) = n.kind.get_cloned() {
                d.tint[3] = v.clamp(0.0, 1.0) as f32;
                dispatch_kind(n.id, NodeKind::Sprite(d));
            }
        })
        .render();
    let bb = match def.billboard {
        BillboardMode::None => "none",
        BillboardMode::YAxis => "yaxis",
        BillboardMode::Full => "full",
    };
    let am = match def.alpha_mode {
        SpriteAlphaMode::Opaque => "opaque",
        SpriteAlphaMode::Mask { .. } => "mask",
        SpriteAlphaMode::Blend => "blend",
    };
    Section::new("Sprite")
        .child(row("Width", w))
        .child(row("Height", h))
        .child(row("Tint", swatch(col, 22.0)))
        .child(row("Opacity", alpha))
        .child(kind_select_row(
            node,
            "Billboard",
            bb,
            vec![("none", "None"), ("yaxis", "Y-axis"), ("full", "Full")],
            |k, v| {
                if let NodeKind::Sprite(mut d) = k {
                    d.billboard = match v {
                        "none" => BillboardMode::None,
                        "yaxis" => BillboardMode::YAxis,
                        _ => BillboardMode::Full,
                    };
                    Some(NodeKind::Sprite(d))
                } else {
                    None
                }
            },
        ))
        .child(kind_select_row(
            node,
            "Alpha",
            am,
            vec![("opaque", "Opaque"), ("mask", "Mask"), ("blend", "Blend")],
            |k, v| {
                if let NodeKind::Sprite(mut d) = k {
                    d.alpha_mode = match v {
                        "opaque" => SpriteAlphaMode::Opaque,
                        "mask" => SpriteAlphaMode::Mask { cutoff_x1000: 500 },
                        _ => SpriteAlphaMode::Blend,
                    };
                    Some(NodeKind::Sprite(d))
                } else {
                    None
                }
            },
        ))
        .render()
}

fn decal_editor(node: &Arc<Node>, cfg: &DecalConfig) -> Dom {
    let n = node.clone();
    let alpha = NumField::new(cfg.alpha as f64)
        .min(0.0)
        .step(0.05)
        .on_change(move |v| {
            if let NodeKind::Decal(mut c) = n.kind.get_cloned() {
                c.alpha = v.clamp(0.0, 1.0) as f32;
                dispatch_kind(n.id, NodeKind::Decal(c));
            }
        })
        .render();
    Section::new("Decal")
        .child(row("Opacity", alpha))
        .child(info_text(if cfg.texture.is_some() {
            "Texture: assigned"
        } else {
            "No texture assigned (decal inert until one is wired)"
        }))
        .render()
}

fn particle_editor(node: &Arc<Node>, def: &ParticleEmitterDef) -> Dom {
    let n = node.clone();
    let rate = NumField::new(def.spawn_rate as f64)
        .min(0.0)
        .step(1.0)
        .on_change(move |v| {
            if let NodeKind::ParticleEmitter(mut d) = n.kind.get_cloned() {
                d.spawn_rate = v.max(0.0) as f32;
                dispatch_kind(n.id, NodeKind::ParticleEmitter(d));
            }
        })
        .render();
    let n = node.clone();
    let max_alive = NumField::new(def.max_alive as f64)
        .min(1.0)
        .step(1.0)
        .on_change(move |v| {
            if let NodeKind::ParticleEmitter(mut d) = n.kind.get_cloned() {
                d.max_alive = v.max(1.0) as u32;
                dispatch_kind(n.id, NodeKind::ParticleEmitter(d));
            }
        })
        .render();
    let n = node.clone();
    let life_min = NumField::new(def.lifetime[0] as f64)
        .min(0.0)
        .step(0.1)
        .on_change(move |v| {
            if let NodeKind::ParticleEmitter(mut d) = n.kind.get_cloned() {
                d.lifetime[0] = v.max(0.0) as f32;
                dispatch_kind(n.id, NodeKind::ParticleEmitter(d));
            }
        })
        .render();
    let n = node.clone();
    let life_max = NumField::new(def.lifetime[1] as f64)
        .min(0.0)
        .step(0.1)
        .on_change(move |v| {
            if let NodeKind::ParticleEmitter(mut d) = n.kind.get_cloned() {
                d.lifetime[1] = v.max(0.0) as f32;
                dispatch_kind(n.id, NodeKind::ParticleEmitter(d));
            }
        })
        .render();
    Section::new("Particle Emitter")
        .child(row("Spawn rate", rate))
        .child(row("Max alive", max_alive))
        .child(row("Lifetime min", life_min))
        .child(row("Lifetime max", life_max))
        .child(kind_toggle_row(node, "One shot", def.one_shot, |k, on| {
            if let NodeKind::ParticleEmitter(mut d) = k {
                d.one_shot = on;
                Some(NodeKind::ParticleEmitter(d))
            } else {
                None
            }
        }))
        .child(kind_toggle_row(
            node,
            "Transparent blend",
            def.blend,
            |k, on| {
                if let NodeKind::ParticleEmitter(mut d) = k {
                    d.blend = on;
                    Some(NodeKind::ParticleEmitter(d))
                } else {
                    None
                }
            },
        ))
        .render()
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

/// The per-mesh inline material of a Primitive **or** a captured Mesh node (both
/// share the same material surface in the inspector). `None` when the node is
/// unassigned (nothing to edit on a magenta node).
fn current_primitive_material(node: &Arc<Node>) -> Option<MaterialDef> {
    node_material_instance(node).map(|inst| inst.inline)
}

/// Replace the node's per-mesh inline uniform store, preserving the rest of the
/// assigned instance + kind. A no-op when the node is unassigned.
fn set_inline_material(node: &Arc<Node>, mat: MaterialDef) {
    let Some(mut inst) = node_material_instance(node) else {
        return;
    };
    inst.inline = mat;
    set_node_material(node, inst);
}

/// Reactive material-assignment dropdown — rebuilds whenever the custom-material
/// library changes, so a material created *after* this mesh was selected appears
/// immediately (previously the picker snapshotted the list once and went stale).
fn material_picker(node: &Arc<Node>) -> Dom {
    let node = node.clone();
    let sig = controller()
        .custom_materials
        .signal_vec_cloned()
        .to_signal_cloned()
        .map(move |_mats| build_material_select(&node));
    html!("div", {
        .child_signal(sig)
    })
}

/// Build the assignment dropdown from the current library snapshot — "Built-in
/// (inline)" plus each custom (Studio) material. Dispatches `AssignMaterial`
/// (id-keyed). Returns `None` when there are no custom materials and none is
/// assigned (nothing to pick).
fn dispatch_assign(node: NodeId, material: Option<AssetId>) {
    spawn_local(async move {
        let _ = controller()
            .dispatch(EditorCommand::AssignMaterial { node, material })
            .await;
    });
}

fn build_material_select(node: &Arc<Node>) -> Option<Dom> {
    let ctrl = controller();
    let mats: Vec<(AssetId, String)> = ctrl
        .custom_materials
        .lock_ref()
        .iter()
        .map(|m| (m.id, m.name.get_cloned()))
        .collect();
    let current: Option<AssetId> = node_material_instance(node).map(|i| i.asset);
    // A DropButton whose items dispatch `AssignMaterial` directly on click —
    // robust against the reactive rebuild (no Mutable-observer race). The button
    // label reflects the current assignment; the inspector rebuilds on assign.
    // "None" = no material (renders magenta); it is NOT a real material.
    let current_label = match current {
        None => "None".to_string(),
        Some(id) => mats
            .iter()
            .find(|(mid, _)| *mid == id)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| "None".to_string()),
    };
    let node_id = node.id;
    let items = move |close: Close| -> Vec<Dom> {
        let mut rows = vec![MenuItem::new("None")
            .checked(current.is_none())
            .on_click({
                let close = close.clone();
                move || {
                    dispatch_assign(node_id, None);
                    (close.borrow_mut())();
                }
            })
            .render()];
        for (id, name) in mats.iter() {
            let id = *id;
            rows.push(
                MenuItem::new(name.clone())
                    .checked(current == Some(id))
                    .on_click({
                        let close = close.clone();
                        move || {
                            dispatch_assign(node_id, Some(id));
                            (close.borrow_mut())();
                        }
                    })
                    .render(),
            );
        }
        rows
    };
    Some(row(
        "Material",
        DropButton::new()
            .label(current_label)
            .variant(BtnVariant::Ghost)
            .size(BtnSize::Sm)
            .items(items)
            .render(),
    ))
}

/// What library material (if any) a mesh has assigned.
enum Assigned {
    None,
    Builtin,
    Dynamic,
}

fn assigned_material(node: &Arc<Node>) -> Assigned {
    let Some(id) = node_material_instance(node).map(|i| i.asset) else {
        return Assigned::None;
    };
    match crate::controller::custom_material::find_material(&controller().custom_materials, id) {
        Some(m) if m.is_builtin() => Assigned::Builtin,
        Some(_) => Assigned::Dynamic,
        None => Assigned::None,
    }
}

/// The shading model of the **assigned** library material (which decides whether to
/// show the PBR factor knobs), or `None` when nothing resolvable is assigned. Note:
/// the *mesh's* `inline.shading` is irrelevant — shading is a variant setting that
/// lives on the material.
fn assigned_shading(node: &Arc<Node>) -> Option<MaterialShading> {
    let id = node_material_instance(node).map(|i| i.asset)?;
    crate::controller::custom_material::find_material(&controller().custom_materials, id)
        .and_then(|m| m.builtin.get_cloned())
        .map(|def| def.shading)
}

/// Every texture asset in the project, as `(id, label)` for a picker. Shared with
/// the Material pane (material-side texture-slot pickers).
pub(crate) fn collect_texture_assets() -> Vec<(AssetId, String)> {
    let ctrl = controller();
    let assets = ctrl.scene.assets.lock().unwrap();
    assets
        .entries
        .iter()
        .filter_map(|(id, e)| match &e.source {
            awsm_renderer_editor_protocol::AssetSource::Texture(def) => {
                let label = match def {
                    awsm_renderer_editor_protocol::TextureDef::Procedural(p) => match p {
                        awsm_renderer_editor_protocol::ProceduralTextureDef::Checker { .. } => {
                            "Checker"
                        }
                        awsm_renderer_editor_protocol::ProceduralTextureDef::Gradient {
                            ..
                        } => "Gradient",
                        awsm_renderer_editor_protocol::ProceduralTextureDef::Noise { .. } => {
                            "Noise"
                        }
                    }
                    .to_string(),
                    awsm_renderer_editor_protocol::TextureDef::Raster { display_name } => {
                        display_name.clone()
                    }
                };
                Some((*id, label))
            }
            _ => None,
        })
        .collect()
}

/// Per-mesh material editor.
///
/// ── MATERIAL MODEL (keep this split intact) ──────────────────────────────────
/// See also `material_mode::builtin_definition` (the material-side editor) and
/// `bridge::node_sync::builtin_merged` (the merge), and the renderer's
/// `PbrFeatures` doc (`materials/src/pbr.rs`) for which fields are variant bits.
///
///  • A mesh with **no** assigned material renders flat MAGENTA — "none" is not a
///    real material and has no settings.
///  • VARIANT settings — anything that changes the compiled shader/pipeline:
///    shading model, alpha mode, double-sided, vertex colours, texture *slots*,
///    KHR extension *enables*, Toon knobs — live ONLY on the material (Material
///    pane). They must NOT appear in this per-mesh editor.
///  • UNIFORM settings — values that don't recompile: base colour, opacity,
///    metallic, roughness, emissive — are per-mesh and live ONLY here.
fn material_editor(node: &Arc<Node>, mat: &MaterialDef, _has_custom: bool) -> Dom {
    let mut sec = Section::new("Material").child(material_picker(node));

    match assigned_material(node) {
        Assigned::None => {
            // No material → magenta. No per-mesh settings.
            return sec
                .child(html!("div", {
                    .style("margin-top", "8px").style("font-size", "12px")
                    .style("color", "var(--text-2)").style("line-height", "1.5")
                    .text("No material — this mesh renders magenta. Pick a material above, or create one in the Material pane.")
                }))
                .render();
        }
        Assigned::Dynamic => {
            // A dynamic Studio material drives the look. Per-mesh uniform/texture
            // overrides land below once it declares slots; the link edits the graph.
            return sec
                .child(dynamic_overrides(node))
                .child(html!("div", {
                    .style("display", "flex").style("flex-direction", "column").style("gap", "8px").style("margin-top", "8px")
                    .child(Btn::new().label("Open in Material mode").icon("edit").variant(BtnVariant::Ghost).full(true)
                        .on_click(|| spawn_local(async {
                            let _ = controller().dispatch(EditorCommand::SwitchMode { mode: EditorMode::Material }).await;
                        })).render())
                }))
                .render();
        }
        Assigned::Builtin => {}
    }

    // ── Built-in material assigned → per-mesh UNIFORM factors only ───────────────
    // (Shading comes from the assigned material, NOT the mesh's inline_material.)
    let shading = assigned_shading(node).unwrap_or(MaterialShading::Pbr);

    // Base color (RGB swatch) + opacity.
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
                    // alpha_MODE (opaque/mask/blend) is a variant setting on the
                    // material; opacity is just the per-mesh alpha factor. The
                    // bridge's alpha_mode_of heuristic still blends when a < 1.
                    set_inline_material(&n, MaterialDef { base_color, ..cur });
                }
            })
            .render(),
    ));

    // PBR-only knobs.
    if matches!(shading, MaterialShading::Pbr) {
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
        // Normal-map scale + occlusion strength — per-mesh uniforms, shown only
        // when the assigned material declares the corresponding texture slot.
        if let Some(variant) = assigned_builtin_def(node) {
            if variant.normal_texture.is_some() {
                let n = node.clone();
                sec = sec.child(row(
                    "Normal scale",
                    NumField::new(mat.normal_scale as f64)
                        .min(0.0)
                        .max(4.0)
                        .step(0.05)
                        .on_change(move |v| {
                            if let Some(cur) = current_primitive_material(&n) {
                                set_inline_material(
                                    &n,
                                    MaterialDef {
                                        normal_scale: v as f32,
                                        ..cur
                                    },
                                );
                            }
                        })
                        .render(),
                ));
            }
            if variant.occlusion_texture.is_some() {
                let n = node.clone();
                sec = sec.child(row(
                    "Occlusion",
                    NumField::new(mat.occlusion_strength as f64)
                        .min(0.0)
                        .max(1.0)
                        .step(0.05)
                        .on_change(move |v| {
                            if let Some(cur) = current_primitive_material(&n) {
                                set_inline_material(
                                    &n,
                                    MaterialDef {
                                        occlusion_strength: v as f32,
                                        ..cur
                                    },
                                );
                            }
                        })
                        .render(),
                ));
            }
        }
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

    // The full uniform long-tail — everything that does NOT recompile is per-mesh
    // overridable: Toon knobs, mask cutoff, and every enabled KHR extension's
    // parameters. Read from the mesh's inline store; enablement from the variant.
    if let Some(variant) = assigned_builtin_def(node) {
        for extra in builtin_uniform_extras(node, mat, &variant) {
            sec = sec.child(extra);
        }
    }

    // Per-mesh TEXTURE overrides. Slot *presence* (does this material sample a
    // base-color / normal / … map?) is a variant bit — adding or removing a slot
    // recompiles, so that stays on the material. But binding a *different* image
    // to an existing slot does NOT recompile, so per the override rule it's
    // per-mesh overridable: for each slot the material declares, let this mesh
    // swap which texture fills it (clearing falls back to the material default).
    if let Some(def) = assigned_builtin_def(node) {
        // Core PBR slots the material declares (default = its shared TextureRef,
        // which carries the imported UV set / transform / sampler).
        let core: [(
            &'static str,
            &'static str,
            Option<awsm_renderer_editor_protocol::TextureRef>,
        ); 5] = [
            ("base_color_texture", "Base color", def.base_color_texture),
            (
                "metallic_roughness_texture",
                "Metal/rough",
                def.metallic_roughness_texture,
            ),
            ("normal_texture", "Normal", def.normal_texture),
            ("occlusion_texture", "Occlusion", def.occlusion_texture),
            ("emissive_texture", "Emissive map", def.emissive_texture),
        ];
        // KHR-extension texture slots the material declares.
        let ext_slots: [(&'static str, &'static str); 14] = [
            ("specular.tex", "Specular"),
            ("specular.color_tex", "Specular color"),
            ("transmission.tex", "Transmission"),
            ("diffuse_transmission.tex", "Diffuse trans."),
            ("diffuse_transmission.color_tex", "Diffuse trans. color"),
            ("volume.thickness_tex", "Volume thickness"),
            ("clearcoat.tex", "Clearcoat"),
            ("clearcoat.roughness_tex", "Clearcoat rough"),
            ("clearcoat.normal_tex", "Clearcoat normal"),
            ("sheen.color_tex", "Sheen color"),
            ("sheen.roughness_tex", "Sheen rough"),
            ("anisotropy.tex", "Anisotropy"),
            ("iridescence.tex", "Iridescence"),
            ("iridescence.thickness_tex", "Iridescence thick."),
        ];
        let mut entries: Vec<(
            &'static str,
            &'static str,
            Option<awsm_renderer_editor_protocol::TextureRef>,
            bool,
        )> = Vec::new();
        for (slot, label, d) in core {
            if d.is_some() {
                entries.push((slot, label, d, false));
            }
        }
        for (slot, label) in ext_slots {
            if let Some(t) = crate::controller::get_ext_texture(&def.extensions, slot) {
                entries.push((slot, label, Some(t), true));
            }
        }
        if !entries.is_empty() {
            sec = sec.child(uniform_subhead("Textures"));
            let assets = collect_texture_assets();
            for (slot, label, default_ref, is_ext) in entries {
                for r in texture_slot_rows(node, label, slot, default_ref, is_ext, &assets) {
                    sec = sec.child(r);
                }
            }
        }
    }

    // NOTE: only VARIANT settings stay on the material (Material pane): the
    // shading-model *choice*, alpha *mode*, double-sided, vertex colours, texture
    // *slot presence*, and KHR extension *enables*. Everything else — every
    // uniform-class value, including extension parameters + Toon knobs + the bound
    // texture per slot — is per-mesh, above.

    sec.render()
}

/// The assigned **built-in** library material's variant [`MaterialDef`] for this
/// node (its shared defaults — texture slots, shading, factors), or `None` when
/// the node has no built-in material assigned.
fn assigned_builtin_def(node: &Arc<Node>) -> Option<MaterialDef> {
    let inst = node_material_instance(node)?;
    crate::controller::custom_material::find_material(&controller().custom_materials, inst.asset)?
        .builtin
        .get_cloned()
}

/// A small uppercase subsection header row (for grouping uniform overrides).
fn uniform_subhead(text: &str) -> Dom {
    html!("div", {
        .style("margin", "10px 0 2px").style("font-size", "11px").style("font-weight", "600")
        .style("letter-spacing", ".04em").style("text-transform", "uppercase")
        .style("color", "var(--text-3)")
        .text(text)
    })
}

/// A numeric per-mesh control for a KHR-extension parameter. Reads the *current*
/// inline def at change time (so successive edits compose) and seeds the
/// extension struct if absent before applying.
fn ext_num_row(
    node: &Arc<Node>,
    label: &str,
    value: f32,
    min: f64,
    max: f64,
    step: f64,
    apply: impl Fn(&mut awsm_renderer_editor_protocol::material::PbrExtensions, f32) + 'static,
) -> Dom {
    let node = node.clone();
    row(
        label,
        NumField::new(value as f64)
            .min(min)
            .max(max)
            .step(step)
            .on_change(move |v| {
                if let Some(mut m) = current_primitive_material(&node) {
                    apply(&mut m.extensions, v as f32);
                    set_inline_material(&node, m);
                }
            })
            .render(),
    )
}

/// An RGB-swatch per-mesh control for a KHR-extension colour parameter.
fn ext_color_row(
    node: &Arc<Node>,
    label: &str,
    current: [f32; 3],
    apply: impl Fn(&mut awsm_renderer_editor_protocol::material::PbrExtensions, [f32; 3]) + 'static,
) -> Dom {
    let m = Mutable::new(rgb_to_hex(current));
    let apply = std::rc::Rc::new(apply);
    spawn_local(clone!(m, node => async move {
        let mut first = true;
        m.signal_cloned().for_each(move |hex| {
            let fire = !first;
            first = false;
            clone!(node, apply => async move {
                if !fire { return; }
                if let (Some(rgb), Some(mut cur)) = (hex_to_rgb(&hex), current_primitive_material(&node)) {
                    apply(&mut cur.extensions, rgb);
                    set_inline_material(&node, cur);
                }
            })
        }).await;
    }));
    row(label, swatch(m, 22.0))
}

/// Rebuild this mesh's inline `MaterialShading::Toon` with one knob changed,
/// reading the current values at change time. No-op if inline isn't Toon.
fn update_toon(node: &Arc<Node>, f: impl FnOnce(u32, f32, u32, f32, f32) -> MaterialShading) {
    if let Some(mut m) = current_primitive_material(node) {
        if let MaterialShading::Toon {
            diffuse_bands,
            rim_strength,
            specular_steps,
            shininess,
            rim_power,
        } = m.shading
        {
            m.shading = f(
                diffuse_bands,
                rim_strength,
                specular_steps,
                shininess,
                rim_power,
            );
            set_inline_material(node, m);
        }
    }
}

/// The full per-mesh **uniform long-tail** for a built-in material: Toon knobs,
/// mask cutoff, and every enabled KHR extension's parameters. `inline` is the
/// mesh's own store (current values); `variant` is the shared material (decides
/// which controls appear — Toon vs not, which extensions are enabled). None of
/// these recompile, so all are per-mesh overridable.
fn builtin_uniform_extras(
    node: &Arc<Node>,
    inline: &MaterialDef,
    variant: &MaterialDef,
) -> Vec<Dom> {
    let mut rows: Vec<Dom> = Vec::new();

    // ── Toon knobs ──
    if matches!(variant.shading, MaterialShading::Toon { .. }) {
        let pick = |s: MaterialShading| match s {
            MaterialShading::Toon {
                diffuse_bands,
                rim_strength,
                specular_steps,
                shininess,
                rim_power,
            } => Some((
                diffuse_bands,
                rim_strength,
                specular_steps,
                shininess,
                rim_power,
            )),
            _ => None,
        };
        let (bands, rim, steps, shin, power) = pick(inline.shading)
            .or_else(|| pick(variant.shading))
            .unwrap_or((3, 0.5, 2, 32.0, 2.0));
        rows.push(uniform_subhead("Toon"));
        {
            let n = node.clone();
            rows.push(row(
                "Diffuse bands",
                NumField::new(bands as f64)
                    .min(1.0)
                    .max(16.0)
                    .step(1.0)
                    .on_change(move |v| {
                        update_toon(&n, |_b, r, s, sh, p| MaterialShading::Toon {
                            diffuse_bands: (v as u32).max(1),
                            rim_strength: r,
                            specular_steps: s,
                            shininess: sh,
                            rim_power: p,
                        })
                    })
                    .render(),
            ));
        }
        {
            let n = node.clone();
            rows.push(row(
                "Rim strength",
                NumField::new(rim as f64)
                    .min(0.0)
                    .max(4.0)
                    .step(0.05)
                    .on_change(move |v| {
                        update_toon(&n, |b, _r, s, sh, p| MaterialShading::Toon {
                            diffuse_bands: b,
                            rim_strength: v as f32,
                            specular_steps: s,
                            shininess: sh,
                            rim_power: p,
                        })
                    })
                    .render(),
            ));
        }
        {
            let n = node.clone();
            rows.push(row(
                "Specular steps",
                NumField::new(steps as f64)
                    .min(1.0)
                    .max(16.0)
                    .step(1.0)
                    .on_change(move |v| {
                        update_toon(&n, |b, r, _s, sh, p| MaterialShading::Toon {
                            diffuse_bands: b,
                            rim_strength: r,
                            specular_steps: (v as u32).max(1),
                            shininess: sh,
                            rim_power: p,
                        })
                    })
                    .render(),
            ));
        }
        {
            let n = node.clone();
            rows.push(row(
                "Shininess",
                NumField::new(shin as f64)
                    .min(1.0)
                    .max(256.0)
                    .step(1.0)
                    .on_change(move |v| {
                        update_toon(&n, |b, r, s, _sh, p| MaterialShading::Toon {
                            diffuse_bands: b,
                            rim_strength: r,
                            specular_steps: s,
                            shininess: v as f32,
                            rim_power: p,
                        })
                    })
                    .render(),
            ));
        }
        {
            let n = node.clone();
            rows.push(row(
                "Rim power",
                NumField::new(power as f64)
                    .min(0.1)
                    .max(16.0)
                    .step(0.1)
                    .on_change(move |v| {
                        update_toon(&n, |b, r, s, sh, _p| MaterialShading::Toon {
                            diffuse_bands: b,
                            rim_strength: r,
                            specular_steps: s,
                            shininess: sh,
                            rim_power: v as f32,
                        })
                    })
                    .render(),
            ));
        }
    }

    // ── Mask cutoff ──
    if matches!(variant.alpha_mode, MaterialAlphaMode::Mask { .. }) {
        let cutoff = match &inline.alpha_mode {
            MaterialAlphaMode::Mask { cutoff } => *cutoff,
            _ => 0.5,
        };
        let n = node.clone();
        rows.push(row(
            "Alpha cutoff",
            NumField::new(cutoff as f64)
                .min(0.0)
                .max(1.0)
                .step(0.01)
                .on_change(move |v| {
                    if let Some(mut m) = current_primitive_material(&n) {
                        m.alpha_mode = MaterialAlphaMode::Mask { cutoff: v as f32 };
                        set_inline_material(&n, m);
                    }
                })
                .render(),
        ));
    }

    // ── KHR extension parameters (per enabled extension) ──
    let e = &variant.extensions;
    let ie = &inline.extensions;
    let any = e.emissive_strength.is_some()
        || e.ior.is_some()
        || e.specular.is_some()
        || e.transmission.is_some()
        || e.diffuse_transmission.is_some()
        || e.volume.is_some()
        || e.clearcoat.is_some()
        || e.sheen.is_some()
        || e.dispersion.is_some()
        || e.anisotropy.is_some()
        || e.iridescence.is_some();
    if any {
        rows.push(uniform_subhead("Extensions"));
    }
    if e.emissive_strength.is_some() {
        let v = ie.emissive_strength.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Emissive strength",
            v.strength,
            0.0,
            100.0,
            0.1,
            |x, val| {
                x.emissive_strength
                    .get_or_insert_with(Default::default)
                    .strength = val
            },
        ));
    }
    if e.ior.is_some() {
        let v = ie.ior.unwrap_or_default();
        rows.push(ext_num_row(node, "IOR", v.ior, 1.0, 3.0, 0.01, |x, val| {
            x.ior.get_or_insert_with(Default::default).ior = val
        }));
    }
    if e.specular.is_some() {
        let v = ie.specular.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Specular",
            v.factor,
            0.0,
            1.0,
            0.05,
            |x, val| x.specular.get_or_insert_with(Default::default).factor = val,
        ));
        rows.push(ext_color_row(
            node,
            "Specular color",
            v.color_factor,
            |x, c| x.specular.get_or_insert_with(Default::default).color_factor = c,
        ));
    }
    if e.transmission.is_some() {
        let v = ie.transmission.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Transmission",
            v.factor,
            0.0,
            1.0,
            0.05,
            |x, val| x.transmission.get_or_insert_with(Default::default).factor = val,
        ));
    }
    if e.diffuse_transmission.is_some() {
        let v = ie.diffuse_transmission.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Diffuse transmission",
            v.factor,
            0.0,
            1.0,
            0.05,
            |x, val| {
                x.diffuse_transmission
                    .get_or_insert_with(Default::default)
                    .factor = val
            },
        ));
        rows.push(ext_color_row(
            node,
            "Diffuse trans. color",
            v.color_factor,
            |x, c| {
                x.diffuse_transmission
                    .get_or_insert_with(Default::default)
                    .color_factor = c
            },
        ));
    }
    if e.volume.is_some() {
        let v = ie.volume.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Thickness",
            v.thickness_factor,
            0.0,
            10.0,
            0.05,
            |x, val| {
                x.volume
                    .get_or_insert_with(Default::default)
                    .thickness_factor = val
            },
        ));
        rows.push(ext_num_row(
            node,
            "Attenuation dist",
            v.attenuation_distance,
            0.0,
            100.0,
            0.1,
            |x, val| {
                x.volume
                    .get_or_insert_with(Default::default)
                    .attenuation_distance = val
            },
        ));
        rows.push(ext_color_row(
            node,
            "Attenuation color",
            v.attenuation_color,
            |x, c| {
                x.volume
                    .get_or_insert_with(Default::default)
                    .attenuation_color = c
            },
        ));
    }
    if e.clearcoat.is_some() {
        let v = ie.clearcoat.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Clearcoat",
            v.factor,
            0.0,
            1.0,
            0.05,
            |x, val| x.clearcoat.get_or_insert_with(Default::default).factor = val,
        ));
        rows.push(ext_num_row(
            node,
            "Clearcoat rough",
            v.roughness_factor,
            0.0,
            1.0,
            0.05,
            |x, val| {
                x.clearcoat
                    .get_or_insert_with(Default::default)
                    .roughness_factor = val
            },
        ));
    }
    if e.sheen.is_some() {
        let v = ie.sheen.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Sheen rough",
            v.roughness_factor,
            0.0,
            1.0,
            0.05,
            |x, val| {
                x.sheen
                    .get_or_insert_with(Default::default)
                    .roughness_factor = val
            },
        ));
        rows.push(ext_color_row(
            node,
            "Sheen color",
            v.color_factor,
            |x, c| x.sheen.get_or_insert_with(Default::default).color_factor = c,
        ));
    }
    if e.dispersion.is_some() {
        let v = ie.dispersion.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Dispersion",
            v.dispersion,
            0.0,
            2.0,
            0.01,
            |x, val| x.dispersion.get_or_insert_with(Default::default).dispersion = val,
        ));
    }
    if e.anisotropy.is_some() {
        let v = ie.anisotropy.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Anisotropy",
            v.strength,
            0.0,
            1.0,
            0.05,
            |x, val| x.anisotropy.get_or_insert_with(Default::default).strength = val,
        ));
        rows.push(ext_num_row(
            node,
            "Anisotropy rot",
            v.rotation,
            -std::f64::consts::PI,
            std::f64::consts::PI,
            0.01,
            |x, val| x.anisotropy.get_or_insert_with(Default::default).rotation = val,
        ));
    }
    if e.iridescence.is_some() {
        let v = ie.iridescence.unwrap_or_default();
        rows.push(ext_num_row(
            node,
            "Iridescence",
            v.factor,
            0.0,
            1.0,
            0.05,
            |x, val| x.iridescence.get_or_insert_with(Default::default).factor = val,
        ));
        rows.push(ext_num_row(
            node,
            "Iridescence IOR",
            v.ior,
            1.0,
            3.0,
            0.01,
            |x, val| x.iridescence.get_or_insert_with(Default::default).ior = val,
        ));
        rows.push(ext_num_row(
            node,
            "Thickness min",
            v.thickness_min,
            0.0,
            1000.0,
            1.0,
            |x, val| {
                x.iridescence
                    .get_or_insert_with(Default::default)
                    .thickness_min = val
            },
        ));
        rows.push(ext_num_row(
            node,
            "Thickness max",
            v.thickness_max,
            0.0,
            2000.0,
            1.0,
            |x, val| {
                x.iridescence
                    .get_or_insert_with(Default::default)
                    .thickness_max = val
            },
        ));
    }

    rows
}

/// Per-mesh override editor for an assigned **dynamic** material's declared
/// uniform slots (#4.2). Each uniform the material declares is shown here with a
/// control seeded from its default (or this mesh's existing override); editing
/// it writes a per-mesh entry into `MaterialInstance::uniform_overrides`,
/// which `dynamic::insert_custom` applies when materializing the mesh. Texture /
/// buffer slot overrides are a follow-on; uniforms are the common case the user
/// hit (declared uniforms weren't exposed on the mesh at all).
fn dynamic_overrides(node: &Arc<Node>) -> Dom {
    use awsm_renderer_editor_protocol::dynamic_material::UniformValue as UV;

    let Some(inst) = node_material_instance(node) else {
        return html!("div", {});
    };
    let Some(mat) = crate::controller::custom_material::find_material(
        &controller().custom_materials,
        inst.asset,
    ) else {
        return html!("div", {});
    };
    if mat.is_builtin() {
        return html!("div", {});
    }
    let slots = mat.uniforms.get_cloned();
    if slots.is_empty() && mat.textures.lock_ref().is_empty() && mat.buffers.lock_ref().is_empty() {
        return html!("div", {
            .style("margin-top", "8px").style("font-size", "12px")
            .style("color", "var(--text-2)").style("line-height", "1.5")
            .text("This material declares no uniforms, textures, or buffers. Add slots in the Material pane to expose per-mesh overrides here.")
        });
    }

    let mut rows: Vec<Dom> = Vec::new();
    if !slots.is_empty() {
        rows.push(html!("div", {
            .style("margin", "8px 0 2px").style("font-size", "11px").style("font-weight", "600")
            .style("letter-spacing", ".04em").style("text-transform", "uppercase")
            .style("color", "var(--text-3)")
            .text("Uniform overrides")
        }));
    }
    for slot in &slots {
        let cur = inst
            .uniform_overrides
            .get(&slot.name)
            .cloned()
            .unwrap_or_else(|| crate::engine::bridge::dynamic::slot_default_value(slot));
        let control: Dom = match cur {
            UV::F32(v) => uniform_num(node, &slot.name, v as f64, 0.05, |x| UV::F32(x as f32)),
            UV::U32(v) => uniform_num(node, &slot.name, v as f64, 1.0, |x| {
                UV::U32(x.max(0.0) as u32)
            }),
            UV::Vec2(a) => uniform_vec(node, &slot.name, &a, |c| UV::Vec2([c[0], c[1]])),
            UV::Vec3(a) => uniform_vec(node, &slot.name, &a, |c| UV::Vec3([c[0], c[1], c[2]])),
            UV::Vec4(a) => {
                uniform_vec(node, &slot.name, &a, |c| UV::Vec4([c[0], c[1], c[2], c[3]]))
            }
            UV::Color3(a) => uniform_color(node, &slot.name, [a[0], a[1], a[2]], None),
            UV::Color4(a) => uniform_color(node, &slot.name, [a[0], a[1], a[2]], Some(a[3])),
            UV::Bool(b) => uniform_bool(node, &slot.name, b),
            _ => html!("span", {
                .style("font-size", "11.5px").style("color", "var(--text-3)")
                .text("(edit in Material pane)")
            }),
        };
        rows.push(row(&slot.name, control));
    }

    // Per-mesh **texture** overrides (#4.2): one picker per declared texture
    // slot, choosing among the project's texture assets.
    let tex_slots = mat.textures.get_cloned();
    if !tex_slots.is_empty() {
        rows.push(html!("div", {
            .style("margin", "10px 0 2px").style("font-size", "11px").style("font-weight", "600")
            .style("letter-spacing", ".04em").style("text-transform", "uppercase")
            .style("color", "var(--text-3)")
            .text("Texture overrides")
        }));
        let assets = collect_texture_assets();
        for slot in &tex_slots {
            let cur = inst.texture_overrides.get(&slot.name).map(|t| t.asset);
            rows.push(row(
                &slot.name,
                texture_override_picker(node, &slot.name, cur, assets.clone()),
            ));
        }
    }

    // Per-mesh **buffer** overrides (#4.2): load a `.bin` per declared data-buffer
    // slot.
    let buf_slots = mat.buffers.get_cloned();
    if !buf_slots.is_empty() {
        rows.push(html!("div", {
            .style("margin", "10px 0 2px").style("font-size", "11px").style("font-weight", "600")
            .style("letter-spacing", ".04em").style("text-transform", "uppercase")
            .style("color", "var(--text-3)")
            .text("Buffer overrides")
        }));
        for slot in &buf_slots {
            let loaded = inst.buffer_overrides.contains_key(&slot.name);
            rows.push(row(
                &slot.name,
                buffer_override_control(node, &slot.name, loaded),
            ));
        }
    }

    html!("div", {
        .style("display", "flex").style("flex-direction", "column").style("gap", "2px")
        .children(rows)
    })
}

/// Control for a buffer-slot override: a "Load .bin" button (+ Clear when set).
fn buffer_override_control(node: &Arc<Node>, name: &str, loaded: bool) -> Dom {
    let n1 = node.clone();
    let nm1 = name.to_string();
    html!("div", {
        .style("display", "flex").style("gap", "6px").style("align-items", "center")
        .child(Btn::new()
            .label(if loaded { "Replace .bin\u{2026}" } else { "Load .bin\u{2026}" })
            .variant(BtnVariant::Ghost).size(BtnSize::Sm)
            .on_click(move || pick_buffer_bin(&n1, &nm1)).render())
        .apply(|b| if loaded {
            let n2 = node.clone();
            let nm2 = name.to_string();
            b.child(html!("span", { .style("font-size", "11px").style("color", "var(--ok)").text("loaded") }))
                .child(Btn::new().label("Clear").variant(BtnVariant::Ghost).size(BtnSize::Sm)
                    .on_click(move || set_buffer_override(&n2, &nm2, None)).render())
        } else {
            b
        })
    })
}

/// Open a native file dialog for a `.bin`, load its bytes as little-endian u32
/// words, stash them, and set this mesh's buffer override to reference them.
fn pick_buffer_bin(node: &Arc<Node>, name: &str) {
    use wasm_bindgen::{closure::Closure, JsCast};
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(input) = document
        .create_element("input")
        .ok()
        .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
        return;
    };
    input.set_type("file");
    let _ = input.set_attribute("accept", ".bin,application/octet-stream");
    let node = node.clone();
    let name = name.to_string();
    let input_ref = input.clone();
    let cb = Closure::<dyn FnMut()>::new(move || {
        let Some(file) = input_ref.files().and_then(|f| f.get(0)) else {
            return;
        };
        let node = node.clone();
        let name = name.to_string();
        spawn_local(async move {
            let Ok(buf) = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await else {
                return;
            };
            let bytes = js_sys::Uint8Array::new(&buf).to_vec();
            let words: Vec<u32> = bytes
                .chunks(4)
                .map(|c| {
                    let mut b = [0u8; 4];
                    b[..c.len()].copy_from_slice(c);
                    u32::from_le_bytes(b)
                })
                .collect();
            let path = crate::engine::bridge::dynamic::store_buffer_words(words);
            set_buffer_override(
                &node,
                &name,
                Some(awsm_renderer_editor_protocol::dynamic_material::BufferRef {
                    path: path.into(),
                }),
            );
        });
    });
    input.set_onchange(Some(cb.as_ref().unchecked_ref()));
    cb.forget();
    input.click();
}

/// Write (or clear) one per-mesh buffer override and re-materialize.
fn set_buffer_override(
    node: &Arc<Node>,
    name: &str,
    value: Option<awsm_renderer_editor_protocol::dynamic_material::BufferRef>,
) {
    if let Some(mut inst) = node_material_instance(node) {
        match value {
            Some(b) => {
                inst.buffer_overrides.insert(name.to_string(), b);
            }
            None => {
                inst.buffer_overrides.remove(name);
            }
        }
        set_node_material(node, inst);
    }
}

/// A texture-slot override picker: pick a project texture asset (or None) for a
/// dynamic material's declared texture slot on this mesh.
fn texture_override_picker(
    node: &Arc<Node>,
    name: &str,
    cur: Option<AssetId>,
    assets: Vec<(AssetId, String)>,
) -> Dom {
    let current_label = match cur {
        None => "None".to_string(),
        Some(id) => assets
            .iter()
            .find(|(a, _)| *a == id)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| "None".to_string()),
    };
    let node = node.clone();
    let name = name.to_string();
    let items = move |close: Close| -> Vec<Dom> {
        let mut rows = vec![MenuItem::new("None")
            .checked(cur.is_none())
            .on_click({
                let (node, name, close) = (node.clone(), name.clone(), close.clone());
                move || {
                    set_texture_override(&node, &name, None);
                    (close.borrow_mut())();
                }
            })
            .render()];
        for (id, label) in assets.iter() {
            let id = *id;
            rows.push(
                MenuItem::new(label.clone())
                    .checked(cur == Some(id))
                    .on_click({
                        let (node, name, close) = (node.clone(), name.clone(), close.clone());
                        move || {
                            set_texture_override(
                                &node,
                                &name,
                                Some(awsm_renderer_editor_protocol::TextureRef::new(id)),
                            );
                            (close.borrow_mut())();
                        }
                    })
                    .render(),
            );
        }
        rows
    };
    DropButton::new()
        .label(current_label)
        .variant(BtnVariant::Ghost)
        .size(BtnSize::Sm)
        .items(items)
        .render()
}

/// Write (or clear) one per-mesh texture override and re-materialize.
fn set_texture_override(
    node: &Arc<Node>,
    name: &str,
    value: Option<awsm_renderer_editor_protocol::TextureRef>,
) {
    if let Some(mut inst) = node_material_instance(node) {
        match value {
            Some(t) => {
                inst.texture_overrides.insert(name.to_string(), t);
            }
            None => {
                inst.texture_overrides.remove(name);
            }
        }
        set_node_material(node, inst);
    }
}

// ── Unified per-mesh texture-slot editor (core PBR slots + KHR extension slots) ─
// Edits the FULL `TextureRef` per mesh: bound image + UV set + KHR_texture_
// transform — all non-recompiling, so all overridable. Core slots route through
// the instance's `texture_overrides` map; extension slots edit the inline
// extension struct (which `builtin_merged` copies per mesh). Always preserves the
// other binding fields when one changes (read-modify-write at edit time).

/// The currently-bound `TextureRef` for a slot (`is_ext` picks the storage).
fn read_slot(
    node: &Arc<Node>,
    slot: &str,
    is_ext: bool,
) -> Option<awsm_renderer_editor_protocol::TextureRef> {
    if is_ext {
        current_primitive_material(node)
            .and_then(|m| crate::controller::get_ext_texture(&m.extensions, slot))
    } else {
        node_material_instance(node).and_then(|i| i.texture_overrides.get(slot).copied())
    }
}

/// Write (or clear) a slot's `TextureRef`.
fn write_slot(
    node: &Arc<Node>,
    slot: &str,
    is_ext: bool,
    tref: Option<awsm_renderer_editor_protocol::TextureRef>,
) {
    if is_ext {
        if let Some(mut m) = current_primitive_material(node) {
            crate::controller::set_ext_texture(&mut m.extensions, slot, tref);
            set_inline_material(node, m);
        }
    } else {
        set_texture_override(node, slot, tref);
    }
}

/// Read-modify-write one slot's `TextureRef` (seeding from the material's default
/// `TextureRef` — full image + UV + transform + sampler — if the mesh has no
/// override yet), so editing one field keeps every other imported field.
fn edit_slot(
    node: &Arc<Node>,
    slot: &str,
    is_ext: bool,
    default_ref: Option<awsm_renderer_editor_protocol::TextureRef>,
    f: impl FnOnce(&mut awsm_renderer_editor_protocol::TextureRef),
) {
    if let Some(mut tr) = read_slot(node, slot, is_ext).or(default_ref) {
        f(&mut tr);
        write_slot(node, slot, is_ext, Some(tr));
    }
}

/// A labelled enum dropdown that fires `on_pick(value)` on change.
fn enum_select_row(
    label: &str,
    current: &str,
    options: Vec<(String, String)>,
    on_pick: impl Fn(String) + 'static,
) -> Dom {
    let sel = Mutable::new(current.to_string());
    let on_pick = std::rc::Rc::new(on_pick);
    spawn_local(clone!(sel => async move {
        let mut first = true;
        sel.signal_cloned().for_each(move |val| {
            let fire = !first;
            first = false;
            clone!(on_pick => async move { if fire { on_pick(val); } })
        }).await;
    }));
    row(label, select(sel, options))
}

fn wrap_str(w: awsm_renderer_editor_protocol::TextureWrap) -> &'static str {
    use awsm_renderer_editor_protocol::TextureWrap as W;
    match w {
        W::Repeat => "repeat",
        W::ClampToEdge => "clamp_to_edge",
        W::MirroredRepeat => "mirrored_repeat",
    }
}
fn wrap_from(s: &str) -> awsm_renderer_editor_protocol::TextureWrap {
    use awsm_renderer_editor_protocol::TextureWrap as W;
    match s {
        "clamp_to_edge" => W::ClampToEdge,
        "mirrored_repeat" => W::MirroredRepeat,
        _ => W::Repeat,
    }
}
fn filt_str(f: awsm_renderer_editor_protocol::TextureFilter) -> &'static str {
    match f {
        awsm_renderer_editor_protocol::TextureFilter::Nearest => "nearest",
        awsm_renderer_editor_protocol::TextureFilter::Linear => "linear",
    }
}
fn filt_from(s: &str) -> awsm_renderer_editor_protocol::TextureFilter {
    match s {
        "nearest" => awsm_renderer_editor_protocol::TextureFilter::Nearest,
        _ => awsm_renderer_editor_protocol::TextureFilter::Linear,
    }
}
fn wrap_opts() -> Vec<(String, String)> {
    vec![
        ("repeat".into(), "Repeat".into()),
        ("clamp_to_edge".into(), "Clamp".into()),
        ("mirrored_repeat".into(), "Mirror".into()),
    ]
}
fn filt_opts() -> Vec<(String, String)> {
    vec![
        ("linear".into(), "Linear".into()),
        ("nearest".into(), "Nearest".into()),
    ]
}

/// Build the rows for one texture slot: image picker + UV set + transform.
fn texture_slot_rows(
    node: &Arc<Node>,
    label: &str,
    slot: &'static str,
    default_ref: Option<awsm_renderer_editor_protocol::TextureRef>,
    is_ext: bool,
    assets: &[(AssetId, String)],
) -> Vec<Dom> {
    use awsm_renderer_editor_protocol::TextureTransform;
    // Effective binding = this mesh's override, else the material's imported
    // default (which carries the UV set / transform / sampler read from glTF).
    let cur = read_slot(node, slot, is_ext).or(default_ref);
    let cur_asset = cur.map(|t| t.asset);
    let xf = cur.and_then(|t| t.transform).unwrap_or_default();
    let uv = cur.map(|t| t.uv_index).unwrap_or(0);
    let mut rows: Vec<Dom> = Vec::new();

    // Image picker — swaps the asset, preserving UV/transform.
    let cur_label = match cur_asset {
        None => "None".to_string(),
        Some(id) => assets
            .iter()
            .find(|(a, _)| *a == id)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| "None".to_string()),
    };
    let items = {
        let (node, assets) = (node.clone(), assets.to_vec());
        move |close: Close| -> Vec<Dom> {
            let mut v = vec![MenuItem::new("None")
                .checked(cur_asset.is_none())
                .on_click({
                    let (node, close) = (node.clone(), close.clone());
                    move || {
                        write_slot(&node, slot, is_ext, None);
                        (close.borrow_mut())();
                    }
                })
                .render()];
            for (id, name) in assets.iter() {
                let id = *id;
                v.push(
                    MenuItem::new(name.clone())
                        .checked(cur_asset == Some(id))
                        .on_click({
                            let (node, close) = (node.clone(), close.clone());
                            move || {
                                edit_slot(&node, slot, is_ext, default_ref, |t| t.asset = id);
                                (close.borrow_mut())();
                            }
                        })
                        .render(),
                );
            }
            v
        }
    };
    rows.push(row(
        label,
        DropButton::new()
            .label(cur_label)
            .variant(BtnVariant::Ghost)
            .size(BtnSize::Sm)
            .items(items)
            .render(),
    ));

    // UV set + KHR_texture_transform — only when a texture is bound.
    if cur_asset.is_some() {
        {
            let n = node.clone();
            rows.push(row(
                "· UV set",
                NumField::new(uv as f64)
                    .min(0.0)
                    .max(7.0)
                    .step(1.0)
                    .on_change(move |v| {
                        edit_slot(&n, slot, is_ext, default_ref, |t| t.uv_index = v as u32)
                    })
                    .render(),
            ));
        }
        // A transform component setter that preserves the rest of the transform.
        let set_xf = move |node: &Arc<Node>, g: fn(&mut TextureTransform, f32), v: f32| {
            edit_slot(node, slot, is_ext, default_ref, |t| {
                let mut x = t.transform.unwrap_or_default();
                g(&mut x, v);
                t.transform = Some(x);
            });
        };
        let num = |val: f64, step: f64, n: Arc<Node>, g: fn(&mut TextureTransform, f32)| {
            NumField::new(val)
                .step(step)
                .on_change(move |v| set_xf(&n, g, v as f32))
                .render()
        };
        rows.push(row(
            "· Offset X",
            num(xf.offset[0] as f64, 0.01, node.clone(), |x, v| {
                x.offset[0] = v
            }),
        ));
        rows.push(row(
            "· Offset Y",
            num(xf.offset[1] as f64, 0.01, node.clone(), |x, v| {
                x.offset[1] = v
            }),
        ));
        rows.push(row(
            "· Rotation",
            num(xf.rotation as f64, 0.01, node.clone(), |x, v| {
                x.rotation = v
            }),
        ));
        rows.push(row(
            "· Scale X",
            num(xf.scale[0] as f64, 0.01, node.clone(), |x, v| {
                x.scale[0] = v
            }),
        ));
        rows.push(row(
            "· Scale Y",
            num(xf.scale[1] as f64, 0.01, node.clone(), |x, v| {
                x.scale[1] = v
            }),
        ));

        // UV flow (auto-scroll): a U/V velocity (units/sec) the runtime adds to the
        // offset each frame — B3. Both zero clears it (back to a static offset).
        let flow = cur.and_then(|t| t.flow).unwrap_or([0.0, 0.0]);
        let set_flow = move |node: &Arc<Node>, idx: usize, v: f32| {
            edit_slot(node, slot, is_ext, default_ref, move |t| {
                let mut f = t.flow.unwrap_or([0.0, 0.0]);
                f[idx] = v;
                t.flow = if f == [0.0, 0.0] { None } else { Some(f) };
            });
        };
        rows.push(row(
            "· Flow U/s",
            NumField::new(flow[0] as f64)
                .step(0.01)
                .on_change({
                    let n = node.clone();
                    move |v| set_flow(&n, 0, v as f32)
                })
                .render(),
        ));
        rows.push(row(
            "· Flow V/s",
            NumField::new(flow[1] as f64)
                .step(0.01)
                .on_change({
                    let n = node.clone();
                    move |v| set_flow(&n, 1, v as f32)
                })
                .render(),
        ));
        // B3-extra — surface flow's UV-continuity requirement at the point of use.
        // Shown only while a flow is active: scrolling walks the UVs, so the mesh
        // needs a continuous / tiling UV axis along the scroll direction. Atlas /
        // baked UVs smear (the scroll crosses into neighbouring cells), and a mesh
        // with no UV set won't move at all. (A non-fuzzy advisory rather than a
        // false-positive-prone automatic atlas-detection heuristic.)
        if flow != [0.0, 0.0] {
            rows.push(html!("div", {
                .style("padding", "5px 8px")
                .style("margin", "3px 0 1px")
                .style("font-size", "11px")
                .style("line-height", "1.45")
                .style("color", "var(--text-3)")
                .style("border-left", "2px solid var(--accent-bright)")
                .style("border-radius", "3px")
                .text("\u{2139} Flow scrolls this slot\u{2019}s UVs over time — it needs a continuous / \
                       tiling UV axis along the scroll direction. Atlas or baked UVs will smear, and a \
                       mesh with no UV set won\u{2019}t move at all.")
            }));
        }

        // Sampler: wrap modes + filtering (imported from the glTF sampler).
        let smp = cur.and_then(|t| t.sampler).unwrap_or_default();
        // Set one sampler field, preserving the rest (seeds a sampler if absent).
        let set_smp = move |node: &Arc<Node>,
                            g: fn(&mut awsm_renderer_editor_protocol::TextureSampler, &str),
                            v: String| {
            edit_slot(node, slot, is_ext, default_ref, move |t| {
                let mut s = t.sampler.unwrap_or_default();
                g(&mut s, &v);
                t.sampler = Some(s);
            });
        };
        {
            let n = node.clone();
            rows.push(enum_select_row(
                "· Wrap U",
                wrap_str(smp.wrap_u),
                wrap_opts(),
                move |v| set_smp(&n, |s, x| s.wrap_u = wrap_from(x), v),
            ));
        }
        {
            let n = node.clone();
            rows.push(enum_select_row(
                "· Wrap V",
                wrap_str(smp.wrap_v),
                wrap_opts(),
                move |v| set_smp(&n, |s, x| s.wrap_v = wrap_from(x), v),
            ));
        }
        {
            let n = node.clone();
            rows.push(enum_select_row(
                "· Mag filter",
                filt_str(smp.mag_filter),
                filt_opts(),
                move |v| set_smp(&n, |s, x| s.mag_filter = filt_from(x), v),
            ));
        }
        {
            let n = node.clone();
            rows.push(enum_select_row(
                "· Min filter",
                filt_str(smp.min_filter),
                filt_opts(),
                move |v| set_smp(&n, |s, x| s.min_filter = filt_from(x), v),
            ));
        }
        {
            let n = node.clone();
            rows.push(enum_select_row(
                "· Mipmap filter",
                filt_str(smp.mipmap_filter),
                filt_opts(),
                move |v| set_smp(&n, |s, x| s.mipmap_filter = filt_from(x), v),
            ));
        }
    }
    rows
}

/// The single per-mesh `MaterialInstance` on a geometry node, if assigned.
fn node_material_instance(
    node: &Arc<Node>,
) -> Option<awsm_renderer_editor_protocol::dynamic_material::MaterialInstance> {
    match node.kind.get_cloned() {
        NodeKind::Mesh { material, .. } => material,
        NodeKind::SkinnedMesh { material, .. } => material,
        _ => None,
    }
}

/// Replace the node's `material` instance, preserving the rest of the kind.
fn set_node_material(
    node: &Arc<Node>,
    inst: awsm_renderer_editor_protocol::dynamic_material::MaterialInstance,
) {
    match node.kind.get_cloned() {
        NodeKind::Mesh {
            mesh, shadow, lod, ..
        } => {
            dispatch_kind(
                node.id,
                NodeKind::Mesh {
                    mesh,
                    material: Some(inst),
                    shadow,
                    lod,
                },
            );
        }
        NodeKind::SkinnedMesh {
            skin, shadow, lod, ..
        } => {
            dispatch_kind(
                node.id,
                NodeKind::SkinnedMesh {
                    skin,
                    material: Some(inst),
                    shadow,
                    lod,
                },
            );
        }
        _ => {}
    }
}

/// The per-mesh inline uniform store for a (possibly `None`) assignment — the
/// built-in's per-mesh values, or a default for an unassigned node. Used as the
/// `material_editor`'s read-only `MaterialDef` view.
fn inline_of(
    material: &Option<awsm_renderer_editor_protocol::dynamic_material::MaterialInstance>,
) -> MaterialDef {
    material
        .as_ref()
        .map(|i| i.inline.clone())
        .unwrap_or_default()
}

/// Write one per-mesh uniform override and re-materialize.
fn set_uniform_override(
    node: &Arc<Node>,
    name: &str,
    value: awsm_renderer_editor_protocol::dynamic_material::UniformValue,
) {
    if let Some(mut inst) = node_material_instance(node) {
        inst.uniform_overrides.insert(name.to_string(), value);
        set_node_material(node, inst);
    }
}

/// A single scalar (f32 / u32) override control.
fn uniform_num(
    node: &Arc<Node>,
    name: &str,
    value: f64,
    step: f64,
    to_val: impl Fn(f64) -> awsm_renderer_editor_protocol::dynamic_material::UniformValue + 'static,
) -> Dom {
    let node = node.clone();
    let name = name.to_string();
    NumField::new(value)
        .step(step)
        .on_change(move |x| set_uniform_override(&node, &name, to_val(x)))
        .render()
}

/// A multi-component (vec2/3/4) override: one NumField per channel, sharing a
/// per-row buffer so editing one channel doesn't clobber the others.
fn uniform_vec(
    node: &Arc<Node>,
    name: &str,
    comps: &[f32],
    build: impl Fn(&[f32]) -> awsm_renderer_editor_protocol::dynamic_material::UniformValue + 'static,
) -> Dom {
    let state = std::rc::Rc::new(std::cell::RefCell::new(comps.to_vec()));
    let build = std::rc::Rc::new(build);
    let labels = ["X", "Y", "Z", "W"];
    let fields: Vec<Dom> = (0..comps.len())
        .map(|i| {
            let node = node.clone();
            let name = name.to_string();
            let state = state.clone();
            let build = build.clone();
            html!("div", {
                .style("display", "flex").style("align-items", "center").style("gap", "3px")
                .child(html!("span", {
                    .style("font-size", "10px").style("color", "var(--text-3)").text(labels[i])
                }))
                .child(NumField::new(comps[i] as f64).step(0.05).on_change(move |x| {
                    state.borrow_mut()[i] = x as f32;
                    let v = build(&state.borrow());
                    set_uniform_override(&node, &name, v);
                }).render())
            })
        })
        .collect();
    html!("div", {
        .style("display", "flex").style("gap", "6px").style("flex-wrap", "wrap")
        .children(fields)
    })
}

/// A color (color3 / color4) override as an RGB swatch; color4 keeps its alpha.
fn uniform_color(node: &Arc<Node>, name: &str, rgb: [f32; 3], alpha: Option<f32>) -> Dom {
    use awsm_renderer_editor_protocol::dynamic_material::UniformValue as UV;
    let hexm = Mutable::new(rgb_to_hex(rgb));
    let node = node.clone();
    let name = name.to_string();
    spawn_local(clone!(hexm => async move {
        let mut first = true;
        hexm.signal_cloned().for_each(move |hex| {
            let fire = !first;
            first = false;
            clone!(node, name => async move {
                if !fire { return; }
                if let Some(c) = hex_to_rgb(&hex) {
                    let v = match alpha {
                        Some(a) => UV::Color4([c[0], c[1], c[2], a]),
                        None => UV::Color3(c),
                    };
                    set_uniform_override(&node, &name, v);
                }
            })
        }).await;
    }));
    swatch(hexm, 22.0)
}

/// A boolean override toggle.
fn uniform_bool(node: &Arc<Node>, name: &str, value: bool) -> Dom {
    use awsm_renderer_editor_protocol::dynamic_material::UniformValue as UV;
    let m = Mutable::new(value);
    let node = node.clone();
    let name = name.to_string();
    spawn_local(clone!(m => async move {
        let mut first = true;
        m.signal().for_each(move |val| {
            let fire = !first;
            first = false;
            clone!(node, name => async move {
                if !fire { return; }
                set_uniform_override(&node, &name, UV::Bool(val));
            })
        }).await;
    }));
    toggle(m)
}

// ── Shadows (per-mesh cast / receive) ─────────────────────────────────────────

/// Replace a Mesh / SkinnedMesh's `shadow`, preserving the rest of the kind.
fn set_mesh_shadow(node: &Arc<Node>, shadow: MeshShadowConfig) {
    match node.kind.get_cloned() {
        NodeKind::Mesh {
            mesh,
            material,
            lod,
            ..
        } => {
            dispatch_kind(
                node.id,
                NodeKind::Mesh {
                    mesh,
                    material,
                    shadow,
                    lod,
                },
            );
        }
        NodeKind::SkinnedMesh {
            skin,
            material,
            lod,
            ..
        } => {
            dispatch_kind(
                node.id,
                NodeKind::SkinnedMesh {
                    skin,
                    material,
                    shadow,
                    lod,
                },
            );
        }
        _ => {}
    }
}

// ── LOD (per-mesh export bake opt-out) ────────────────────────────────────────

/// Replace a Mesh / SkinnedMesh's `lod`, preserving the rest of the kind.
fn set_mesh_lod(node: &Arc<Node>, lod: MeshLodConfig) {
    match node.kind.get_cloned() {
        NodeKind::Mesh {
            mesh,
            material,
            shadow,
            ..
        } => {
            dispatch_kind(
                node.id,
                NodeKind::Mesh {
                    mesh,
                    material,
                    shadow,
                    lod,
                },
            );
        }
        NodeKind::SkinnedMesh {
            skin,
            material,
            shadow,
            ..
        } => {
            dispatch_kind(
                node.id,
                NodeKind::SkinnedMesh {
                    skin,
                    material,
                    shadow,
                    lod,
                },
            );
        }
        _ => {}
    }
}

fn mesh_lod_editor(node: &Arc<Node>, lod: MeshLodConfig) -> Dom {
    let enabled = Mutable::new(lod.enabled);
    spawn_local(clone!(enabled, node => async move {
        let mut first = true;
        enabled.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if !fire { return; }
                if let Some(lod) = node.kind.get_cloned().mesh_lod().copied() {
                    if lod.enabled != on {
                        set_mesh_lod(&node, MeshLodConfig { enabled: on });
                    }
                }
            })
        }).await;
    }));

    Section::new("LOD")
        .child(row("Enabled", toggle(enabled)))
        .render()
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
                if let Some(shadow) = node.kind.get_cloned().mesh_shadow().copied() {
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
                if let Some(shadow) = node.kind.get_cloned().mesh_shadow().copied() {
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
    // Light params (Color/Intensity/Range) + the per-light Shadows section
    // (cast / hardness-PCSS / cascades / biases / update rates).
    html!("div", {
        .child(sec.render())
        .child(light_shadow_editor(node, cfg))
    })
}

/// Per-light shadow settings: cast toggle, filter hardness (Hard / Soft-PCF /
/// PCSS), map resolution, depth/normal bias, max distance, and — for the kinds
/// that use them — directional cascade controls (count / split-λ / EVSM cutoff /
/// far-cascade update rate) and the point-light cube-face update rate. Each
/// control rewrites the light's `LightShadowConfig` via `SetKind`, which the
/// node-sync bridge re-applies to the renderer live (`set_light_shadow_params`).
fn light_shadow_editor(node: &Arc<Node>, cfg: &LightConfig) -> Dom {
    let shadow = light_shadow(cfg);

    let cast = Mutable::new(shadow.cast);
    spawn_local(clone!(cast, node => async move {
        let mut first = true;
        cast.signal().for_each(move |on| {
            let fire = !first;
            first = false;
            clone!(node => async move {
                if fire { update_shadow(&node, |s| s.cast = on); }
            })
        }).await;
    }));

    let mut sec = Section::new("Shadows").child(row("Cast", toggle(cast)));

    let n = node.clone();
    sec = sec.child(enum_select_row(
        "Hardness",
        hardness_str(shadow.hardness),
        vec![
            ("hard".into(), "Hard".into()),
            ("soft".into(), "Soft (PCF)".into()),
            ("pcss".into(), "PCSS".into()),
        ],
        move |v| update_shadow(&n, |s| s.hardness = hardness_from(&v)),
    ));

    // Governs both Soft (scales the fixed PCF disc) and PCSS (scales the
    // virtual light-disc radius), so it's labelled generically and shown for
    // every hardness except Hard, where it has no effect.
    let n = node.clone();
    sec = sec.child(row(
        "Softness",
        NumField::new(shadow.pcss_penumbra_scale as f64)
            .min(0.0)
            .step(0.1)
            .on_change(move |v| update_shadow(&n, |s| s.pcss_penumbra_scale = v as f32))
            .render(),
    ));

    let n = node.clone();
    sec = sec.child(row(
        "Resolution",
        NumField::new(shadow.resolution as f64)
            .min(16.0)
            .step(256.0)
            .on_change(move |v| update_shadow(&n, |s| s.resolution = (v as u32).max(16)))
            .render(),
    ));

    let n = node.clone();
    sec = sec.child(row(
        "Depth Bias",
        NumField::new(shadow.depth_bias as f64)
            .step(0.0005)
            .on_change(move |v| update_shadow(&n, |s| s.depth_bias = v as f32))
            .render(),
    ));

    let n = node.clone();
    sec = sec.child(row(
        "Normal Bias",
        NumField::new(shadow.normal_bias as f64)
            .min(0.0)
            .step(0.01)
            .on_change(move |v| update_shadow(&n, |s| s.normal_bias = v as f32))
            .render(),
    ));

    let n = node.clone();
    sec = sec.child(row(
        "Max Distance",
        NumField::new(shadow.max_distance as f64)
            .min(0.0)
            .step(5.0)
            .on_change(move |v| update_shadow(&n, |s| s.max_distance = v as f32))
            .render(),
    ));

    // Directional cascades.
    if matches!(cfg, LightConfig::Directional { .. }) {
        let n = node.clone();
        sec = sec.child(row(
            "Cascades",
            NumField::new(shadow.cascade_count as f64)
                .min(1.0)
                .max(4.0)
                .step(1.0)
                .on_change(move |v| update_shadow(&n, |s| s.cascade_count = (v as u8).clamp(1, 4)))
                .render(),
        ));
        let n = node.clone();
        sec = sec.child(row(
            "Split \u{03bb}",
            NumField::new(shadow.cascade_split_lambda as f64)
                .min(0.0)
                .max(1.0)
                .step(0.05)
                .on_change(move |v| {
                    update_shadow(&n, |s| s.cascade_split_lambda = (v as f32).clamp(0.0, 1.0))
                })
                .render(),
        ));
        let n = node.clone();
        sec = sec.child(enum_select_row(
            "EVSM",
            evsm_str(shadow.evsm_cutoff),
            vec![
                ("off".into(), "Off".into()),
                ("last_cascade".into(), "Last cascade".into()),
                ("last_two_cascades".into(), "Last 2 cascades".into()),
            ],
            move |v| update_shadow(&n, |s| s.evsm_cutoff = evsm_from(&v)),
        ));
        let n = node.clone();
        sec = sec.child(enum_select_row(
            "Far Update",
            far_rate_str(shadow.far_cascade_update_rate),
            rate_opts(),
            move |v| update_shadow(&n, |s| s.far_cascade_update_rate = far_rate_from(&v)),
        ));
    }

    // Point-light cube faces.
    if matches!(cfg, LightConfig::Point { .. }) {
        let n = node.clone();
        sec = sec.child(enum_select_row(
            "Cube Update",
            cube_rate_str(shadow.cube_face_update_rate),
            rate_opts(),
            move |v| update_shadow(&n, |s| s.cube_face_update_rate = cube_rate_from(&v)),
        ));
    }

    sec.render()
}

/// Read the inline shadow config off any light kind.
fn light_shadow(cfg: &LightConfig) -> LightShadowConfig {
    match cfg {
        LightConfig::Directional { shadow, .. }
        | LightConfig::Point { shadow, .. }
        | LightConfig::Spot { shadow, .. } => shadow.clone(),
    }
}

/// Mutate the selected light's shadow config in place + dispatch the SetKind.
fn update_shadow(node: &Arc<Node>, f: impl FnOnce(&mut LightShadowConfig)) {
    if let Some(cur) = current_light(node) {
        let mut shadow = light_shadow(&cur);
        f(&mut shadow);
        dispatch_kind(node.id, NodeKind::Light(with_shadow(cur, shadow)));
    }
}

fn with_shadow(cfg: LightConfig, shadow: LightShadowConfig) -> LightConfig {
    match cfg {
        LightConfig::Directional {
            color, intensity, ..
        } => LightConfig::Directional {
            color,
            intensity,
            shadow,
        },
        LightConfig::Point {
            color,
            intensity,
            range,
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
            range,
            inner_angle,
            outer_angle,
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

fn hardness_str(h: LightShadowHardness) -> &'static str {
    match h {
        LightShadowHardness::Hard => "hard",
        LightShadowHardness::Soft => "soft",
        LightShadowHardness::Pcss => "pcss",
    }
}
fn hardness_from(s: &str) -> LightShadowHardness {
    match s {
        "hard" => LightShadowHardness::Hard,
        "pcss" => LightShadowHardness::Pcss,
        _ => LightShadowHardness::Soft,
    }
}
fn evsm_str(c: EvsmCutoff) -> &'static str {
    match c {
        EvsmCutoff::Off => "off",
        EvsmCutoff::LastCascade => "last_cascade",
        EvsmCutoff::LastTwoCascades => "last_two_cascades",
    }
}
fn evsm_from(s: &str) -> EvsmCutoff {
    match s {
        "off" => EvsmCutoff::Off,
        "last_two_cascades" => EvsmCutoff::LastTwoCascades,
        _ => EvsmCutoff::LastCascade,
    }
}
fn rate_opts() -> Vec<(String, String)> {
    vec![
        ("every_frame".into(), "Every frame".into()),
        ("every2_frames".into(), "Every 2".into()),
        ("every4_frames".into(), "Every 4".into()),
        ("every8_frames".into(), "Every 8".into()),
    ]
}
fn far_rate_str(r: FarCascadeUpdateRate) -> &'static str {
    match r {
        FarCascadeUpdateRate::EveryFrame => "every_frame",
        FarCascadeUpdateRate::Every2Frames => "every2_frames",
        FarCascadeUpdateRate::Every4Frames => "every4_frames",
        FarCascadeUpdateRate::Every8Frames => "every8_frames",
    }
}
fn far_rate_from(s: &str) -> FarCascadeUpdateRate {
    match s {
        "every_frame" => FarCascadeUpdateRate::EveryFrame,
        "every2_frames" => FarCascadeUpdateRate::Every2Frames,
        "every8_frames" => FarCascadeUpdateRate::Every8Frames,
        _ => FarCascadeUpdateRate::Every4Frames,
    }
}
fn cube_rate_str(r: CubeFaceUpdateRate) -> &'static str {
    match r {
        CubeFaceUpdateRate::EveryFrame => "every_frame",
        CubeFaceUpdateRate::Every2Frames => "every2_frames",
        CubeFaceUpdateRate::Every4Frames => "every4_frames",
        CubeFaceUpdateRate::Every8Frames => "every8_frames",
    }
}
fn cube_rate_from(s: &str) -> CubeFaceUpdateRate {
    match s {
        "every2_frames" => CubeFaceUpdateRate::Every2Frames,
        "every4_frames" => CubeFaceUpdateRate::Every4Frames,
        "every8_frames" => CubeFaceUpdateRate::Every8Frames,
        _ => CubeFaceUpdateRate::EveryFrame,
    }
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

thread_local! {
    /// Rotation display mode — "euler" (degrees) or "quat" (x,y,z,w). A pure
    /// display preference (not a command); persists across selections so the
    /// toggle doesn't reset every time you pick a different node.
    static ROT_MODE: Mutable<String> = Mutable::new("euler".to_string());
}

fn transform_section(node: &Arc<Node>) -> Dom {
    let id = node.id;

    // Position — the field values track `node.transform` live, so a gizmo drag
    // (or any other source) updates them in real time; user edits still commit
    // via `on_change` and aren't clobbered while focused/scrubbed.
    let n_pos = node.clone();
    let pos = row(
        "Position",
        vec3_signal(
            node.transform.signal_ref(|t| f3(t.translation)),
            0.1,
            move |v| {
                let mut t = n_pos.transform.get();
                t.translation = [v[0] as f32, v[1] as f32, v[2] as f32];
                dispatch_transform(id, t);
            },
        ),
    );

    // Rotation — two lines: "Rotation" + an Euler/Quat toggle, then the fields
    // (3 Euler degrees, or 4 quaternion components), both live-tracking.
    let rot_mode = ROT_MODE.with(|m| m.clone());
    let rot_header = html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "space-between")
        .style("min-height", "var(--row-h)")
        .child(html!("span", {
            .style("font-size", "12px")
            .style("color", "var(--text-1)")
            .text("Rotation")
        }))
        .child(segmented(
            rot_mode.clone(),
            vec![SegOption::new("euler", "Euler"), SegOption::new("quat", "Quat")],
            true,
            false,
        ))
    });
    let rot = html!("div", {
        .style("margin-bottom", "var(--gap)")
        .child(rot_header)
        .child(html!("div", {
            .style("margin-top", "5px")
            .child_signal(rot_mode.signal_cloned().map(clone!(node => move |mode| {
                Some(if mode == "quat" {
                    quat_fields(node.clone(), id)
                } else {
                    euler_fields(node.clone(), id)
                })
            })))
        }))
    });

    // Scale
    let n_scale = node.clone();
    let scale = row(
        "Scale",
        vec3_signal(node.transform.signal_ref(|t| f3(t.scale)), 0.1, move |v| {
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

/// Euler-degree rotation fields (XYZ), live-tracking `node.transform`.
fn euler_fields(node: Arc<Node>, id: NodeId) -> Dom {
    let n = node.clone();
    vec3_signal(
        node.transform.signal_ref(|t| {
            let (ex, ey, ez) = Quat::from_array(t.rotation).to_euler(EulerRot::XYZ);
            [
                ex.to_degrees() as f64,
                ey.to_degrees() as f64,
                ez.to_degrees() as f64,
            ]
        }),
        1.0,
        move |v| {
            let mut t = n.transform.get();
            t.rotation = Quat::from_euler(
                EulerRot::XYZ,
                (v[0] as f32).to_radians(),
                (v[1] as f32).to_radians(),
                (v[2] as f32).to_radians(),
            )
            .to_array();
            dispatch_transform(id, t);
        },
    )
}

/// Quaternion rotation fields (XYZW), live-tracking `node.transform`. Edits are
/// re-normalized so the quaternion stays unit-length.
fn quat_fields(node: Arc<Node>, id: NodeId) -> Dom {
    let n = node.clone();
    vec4_signal(
        node.transform.signal_ref(|t| {
            [
                t.rotation[0] as f64,
                t.rotation[1] as f64,
                t.rotation[2] as f64,
                t.rotation[3] as f64,
            ]
        }),
        0.01,
        move |v| {
            let mut t = n.transform.get();
            t.rotation = Quat::from_xyzw(v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32)
                .normalize()
                .to_array();
            dispatch_transform(id, t);
        },
    )
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
            .on_click(|| for_each_selected(|id| EditorCommand::Duplicate { id, new_id: None })).render())
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

// ── Asset Inspector ───────────────────────────────────────────────────────────

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
                .attr("title", "Close the asset view (back to node properties)")
                .child(html!("span", { .style("font-size", "13px").style("line-height", "1").text("\u{2715}") }))
                .child(html!("span", { .text("Close") }))
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
        MaterialShading::FlipBook { .. } => ("FlipBook".to_string(), Tone::Ok),
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
