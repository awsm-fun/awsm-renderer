//! GLB export — lower the live editor scene (or one subtree) to a baked
//! [`awsm_glb_export::GlbScene`] and serialize it to a `.glb`.
//!
//! This is the standalone "get geometry out" path behind
//! [`EditorQuery::ExportGlb`](awsm_editor_protocol::query::EditorQuery::ExportGlb)
//! / the `export_scene_glb` + `export_node_glb` MCP tools. The whole-runtime
//! player publish (Phase 6) reuses the same `GlbScene` IR + `write_glb`.
//!
//! ## Material policy (locked)
//! - assigned/inline **PBR** → glTF PBR; **Unlit** → `KHR_materials_unlit`;
//! - custom-WGSL or **Toon** → `AWSM_materials_none` (no embedded material; the
//!   scene/player re-binds the real material on import via the carried id).
//!
//! ## Known gaps (tracked in docs/plans/mesh-editing-STATUS.md, verified in-browser)
//! - **Textures**: emitted referenced-only, but reading the raster bytes off
//!   `ProjectDir` is async/browser-only, so texture *factors* export today and the
//!   image pool is left empty. The lightweighting use-case (reassign a no-texture
//!   PBR) already works.
//! - **Model nodes**: exporting an imported glTF requires re-reading its source
//!   blob (`GltfLoader::load`), which is browser-only; Model subtrees export as
//!   empty transform nodes for now.

use awsm_glb_export::{
    write_glb, AlphaMode, ExportCamera, ExportLight, ExportMaterial, ExportNode, GlbScene,
    MeshData, PbrMaterial, Trs, UnlitMaterial,
};
use awsm_scene_schema::{
    AssetSource, CameraConfig, CameraProjection, CrossSectionDef, CustomMaterialInstance,
    LightConfig, MaterialAlphaMode, MaterialDef, MaterialRef, MaterialShading, NodeId, NodeKind,
    SweepAlongCurveDef, SweepUvMode,
};

use crate::engine::bridge::{mesh_cache, node_sync};
use crate::engine::scene::{mutate, node::Node, Scene};

/// Bake `node` (or the whole scene when `None`) to a binary glTF byte vector.
pub fn export_glb(scene: &Scene, node: Option<NodeId>) -> Result<Vec<u8>, String> {
    let nodes = match node {
        Some(id) => {
            let n = mutate::find_by_id(scene, id).ok_or_else(|| format!("no node {id}"))?;
            vec![node_to_export(scene, &n)]
        }
        None => scene
            .nodes
            .lock_ref()
            .iter()
            .map(|n| node_to_export(scene, n))
            .collect(),
    };
    let glb = GlbScene {
        nodes,
        ..Default::default()
    };
    Ok(write_glb(&glb))
}

fn node_to_export(scene: &Scene, node: &Node) -> ExportNode {
    let trs = node.transform.get();
    let mut out = ExportNode {
        name: node.name.get_cloned(),
        transform: Trs {
            translation: trs.translation,
            rotation: trs.rotation,
            scale: trs.scale,
        },
        ..Default::default()
    };

    match &node.kind.get_cloned() {
        NodeKind::Primitive {
            shape,
            material,
            inline_material,
            custom_material,
            ..
        } => {
            out.mesh = Some(node_sync::primitive_to_mesh(shape));
            out.material = Some(map_material(
                scene,
                material,
                inline_material,
                custom_material,
            ));
        }
        NodeKind::Mesh {
            mesh,
            material,
            inline_material,
            custom_material,
            ..
        } => {
            if let Some(raw) = mesh_cache::get_raw(mesh.0) {
                out.mesh = Some(MeshData {
                    positions: raw.positions,
                    normals: raw.normals,
                    uvs: raw.uvs,
                    colors: raw.colors,
                    indices: raw.indices,
                });
                out.material = Some(map_material(
                    scene,
                    material,
                    inline_material,
                    custom_material,
                ));
            }
        }
        NodeKind::SweepAlongCurve {
            def,
            material,
            inline_material,
            custom_material,
            ..
        } => {
            if let Some(mesh) = sweep_mesh(scene, def) {
                out.mesh = Some(mesh);
                out.material = Some(map_material(
                    scene,
                    material,
                    inline_material,
                    custom_material,
                ));
            }
        }
        NodeKind::Light(cfg) => out.light = Some(map_light(cfg)),
        NodeKind::Camera(cfg) => out.camera = Some(map_camera(cfg)),
        // Group + (deferred) Model + non-geometry leaves export as plain
        // transform nodes; their children still recurse below.
        _ => {}
    }

    out.children = node
        .children
        .lock_ref()
        .iter()
        .map(|c| node_to_export(scene, c))
        .collect();
    out
}

/// Resolve a node's effective material to the export representation.
fn map_material(
    scene: &Scene,
    material: &Option<MaterialRef>,
    inline: &MaterialDef,
    custom: &Option<CustomMaterialInstance>,
) -> ExportMaterial {
    // A custom-WGSL material is not glTF-representable → none + carried id.
    if let Some(inst) = custom {
        return ExportMaterial::None {
            id: Some(inst.material.to_string()),
        };
    }
    // Prefer an assigned library MaterialDef asset; fall back to the inline one.
    let assigned = material
        .as_ref()
        .and_then(|r| {
            let assets = scene.assets.lock().unwrap();
            match assets.get(r.0).map(|e| &e.source) {
                Some(AssetSource::Material(def)) => Some(def.clone()),
                _ => None,
            }
        })
        .unwrap_or_else(|| inline.clone());
    map_material_def(&assigned, material.as_ref())
}

fn map_material_def(def: &MaterialDef, assigned: Option<&MaterialRef>) -> ExportMaterial {
    match def.shading {
        MaterialShading::Pbr => ExportMaterial::Pbr(PbrMaterial {
            name: def.label.clone(),
            base_color: def.base_color,
            metallic: def.metallic,
            roughness: def.roughness,
            emissive: def.emissive,
            alpha_mode: map_alpha(&def.alpha_mode),
            double_sided: def.double_sided,
            ..Default::default()
        }),
        MaterialShading::Unlit => ExportMaterial::Unlit(UnlitMaterial {
            name: def.label.clone(),
            base_color: def.base_color,
            alpha_mode: map_alpha(&def.alpha_mode),
            double_sided: def.double_sided,
            ..Default::default()
        }),
        // Toon isn't glTF-representable → none + the assigned id (if any) for
        // scene-level re-resolution on import.
        MaterialShading::Toon { .. } => ExportMaterial::None {
            id: assigned.map(|r| r.0.to_string()),
        },
    }
}

fn map_alpha(m: &MaterialAlphaMode) -> AlphaMode {
    match m {
        MaterialAlphaMode::Opaque => AlphaMode::Opaque,
        MaterialAlphaMode::Mask { cutoff } => AlphaMode::Mask { cutoff: *cutoff },
        MaterialAlphaMode::Blend => AlphaMode::Blend,
    }
}

fn map_light(cfg: &LightConfig) -> ExportLight {
    match *cfg {
        LightConfig::Directional {
            color, intensity, ..
        } => ExportLight::Directional { color, intensity },
        LightConfig::Point {
            color,
            intensity,
            range,
            ..
        } => ExportLight::Point {
            color,
            intensity,
            range: Some(range),
        },
        LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            ..
        } => ExportLight::Spot {
            color,
            intensity,
            range: Some(range),
            inner_cone_angle: inner_angle,
            outer_cone_angle: outer_angle,
        },
    }
}

fn map_camera(cfg: &CameraConfig) -> ExportCamera {
    match cfg.projection {
        CameraProjection::Perspective { fov_y_rad } => ExportCamera::Perspective {
            yfov: fov_y_rad,
            aspect_ratio: None,
            znear: cfg.near,
            zfar: Some(cfg.far),
        },
        CameraProjection::Orthographic { half_height } => ExportCamera::Orthographic {
            xmag: half_height,
            ymag: half_height,
            znear: cfg.near,
            zfar: cfg.far,
        },
    }
}

/// Bake a `SweepAlongCurve` to triangles by resolving its referenced curve node
/// from the scene tree (mirrors the renderer-bridge `materialize_sweep`).
fn sweep_mesh(scene: &Scene, def: &SweepAlongCurveDef) -> Option<MeshData> {
    use awsm_curves::CatmullRomCurve;
    use awsm_meshgen::{sweep_along_curve, CrossSection, SweepOpts, UvMode};
    use glam::Vec3;

    if def.curve_node.is_nil() {
        return None;
    }
    let curve_node = mutate::find_by_id(scene, def.curve_node)?;
    let curve_def = match curve_node.kind.get_cloned() {
        NodeKind::Curve(c) => c,
        _ => return None,
    };
    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    let cs = match def.cross_section.clone() {
        CrossSectionDef::Strip { width, y_offset } => CrossSection::Strip { width, y_offset },
        CrossSectionDef::Tube {
            radius,
            radial_segments,
        } => CrossSection::Tube {
            radius,
            radial_segments,
        },
        CrossSectionDef::Wall { width, height } => CrossSection::Wall { width, height },
        CrossSectionDef::Profile { points, closed } => CrossSection::Profile { points, closed },
    };
    let opts = SweepOpts {
        samples: def.samples,
        uv_mode: match def.uv_mode {
            SweepUvMode::StretchOnce => UvMode::StretchOnce,
            SweepUvMode::RepeatByLength {
                u_repeat,
                v_repeat_per_unit,
            } => UvMode::RepeatByLength {
                u_repeat,
                v_repeat_per_unit,
            },
        },
        up_hint: def.up_hint,
    };
    Some(sweep_along_curve(&curve, &cs, &opts))
}
