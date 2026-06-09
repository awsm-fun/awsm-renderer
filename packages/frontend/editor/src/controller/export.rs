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
//! ## Textures (referenced-only)
//! Export embeds exactly the images the *assigned* materials reference: procedural
//! textures are regenerated + PNG-encoded on the spot; raster textures are read
//! back from the GPU (`texture_png_bytes`). Unreferenced textures are never
//! carried — so reassigning a lighter material drops the heavy ones with no flag.
//! Raster textures not yet uploaded to the GPU are skipped (the material keeps its
//! factors). This is why export is **async**.
//!
//! ## Known gap
//! - **Model nodes**: exporting an imported glTF requires re-reading its source
//!   blob (`GltfLoader::load`); Model subtrees export as empty transform nodes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use awsm_glb_export::{
    write_glb, AlphaMode, AnimInterp, AnimPath, ExportAnimChannel, ExportAnimation, ExportCamera,
    ExportImage, ExportLight, ExportMaterial, ExportNode, GlbScene, ImageMime, MeshData,
    PbrMaterial, TexRef, Trs, UnlitMaterial,
};
use awsm_scene_schema::animation::{TrackTarget, TrackValue, TransformProp};
use awsm_scene_schema::{
    AssetId, AssetSource, CameraConfig, CameraProjection, CrossSectionDef, CustomMaterialInstance,
    LightConfig, MaterialAlphaMode, MaterialDef, MaterialRef, MaterialShading, NodeId, NodeKind,
    SweepAlongCurveDef, SweepUvMode, TextureDef, TextureRef,
};

use crate::engine::bridge::{material as bridge_material, mesh_cache, node_sync};
use crate::engine::scene::{mutate, node::Node, Scene};

/// Maps a referenced texture asset → its index in `GlbScene::images`.
type TexIndex = HashMap<AssetId, usize>;

/// Bake the whole scene **including animations** (clips lowered to glTF TRS
/// channels) — the path behind `ExportGlb { node: None }` and the player bundle.
pub async fn export_scene_glb(ctrl: &super::EditorController) -> Result<Vec<u8>, String> {
    let scene = &ctrl.scene;
    let roots: Vec<Arc<Node>> = scene.nodes.lock_ref().iter().cloned().collect();
    let (images, tex_index) = resolve_images(scene, &roots).await;
    let nodes: Vec<ExportNode> = roots
        .iter()
        .map(|n| node_to_export(scene, n, &tex_index))
        .collect();
    let index_map = build_index_map(scene);
    let clips: Vec<_> = ctrl.custom_animations.lock_ref().iter().cloned().collect();
    let animations = lower_clips(&clips, &index_map);
    let glb = GlbScene {
        nodes,
        animations,
        images,
        ..Default::default()
    };
    Ok(write_glb(&glb))
}

/// `node.id → depth-first index`, matching `write_glb`'s node flattening (so
/// animation channels reference the right glTF node).
fn build_index_map(scene: &Scene) -> HashMap<NodeId, usize> {
    fn walk(nodes: &[std::sync::Arc<Node>], map: &mut HashMap<NodeId, usize>, next: &mut usize) {
        for n in nodes {
            map.insert(n.id, *next);
            *next += 1;
            walk(&n.children.lock_ref(), map, next);
        }
    }
    let mut map = HashMap::new();
    let mut next = 0;
    walk(&scene.nodes.lock_ref(), &mut map, &mut next);
    map
}

/// Lower editor clips → glTF TRS animations. First cut: **Transform** tracks
/// only (translation/rotation/scale); morph-weight, material-uniform, light, and
/// camera tracks need `KHR_animation_pointer`/morph wiring (follow-on). Cubic
/// tracks are emitted as Linear (glTF CubicSpline needs in/out tangents).
fn lower_clips(
    clips: &[std::sync::Arc<crate::controller::animation::CustomAnimation>],
    index_map: &HashMap<NodeId, usize>,
) -> Vec<ExportAnimation> {
    use awsm_scene_schema::animation::SamplerKind;
    let mut out = Vec::new();
    for clip in clips {
        let mut channels = Vec::new();
        for track in clip.tracks.lock_ref().iter() {
            let TrackTarget::Transform { node, prop } = &track.target else {
                continue; // non-TRS targets: follow-on
            };
            let Some(&node_index) = index_map.get(node) else {
                continue;
            };
            let times: Vec<f32> = track.times.get_cloned().iter().map(|t| *t as f32).collect();
            let keys = track.keys.get_cloned();
            if times.is_empty() || keys.len() != times.len() {
                continue;
            }
            let path = match prop {
                TransformProp::Translation => AnimPath::Translation,
                TransformProp::Rotation => AnimPath::Rotation,
                TransformProp::Scale => AnimPath::Scale,
            };
            let mut values = Vec::new();
            let mut ok = true;
            for k in &keys {
                match (prop, &k.value) {
                    (TransformProp::Translation | TransformProp::Scale, TrackValue::Vec3(v)) => {
                        values.extend_from_slice(v)
                    }
                    (TransformProp::Rotation, TrackValue::Quat(q)) => values.extend_from_slice(q),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let interpolation = match track.sampler.get() {
                SamplerKind::Step => AnimInterp::Step,
                SamplerKind::Linear | SamplerKind::Cubic => AnimInterp::Linear,
            };
            channels.push(ExportAnimChannel {
                node_index,
                path,
                interpolation,
                times,
                values,
            });
        }
        if !channels.is_empty() {
            out.push(ExportAnimation {
                name: clip.name.get_cloned(),
                channels,
            });
        }
    }
    out
}

/// Bake `node` (or the whole scene when `None`) to a binary glTF byte vector.
/// Single-node export carries no animations (channels are scene-flat-indexed);
/// use [`export_scene_glb`] for the whole scene with animations.
pub async fn export_glb(scene: &Scene, node: Option<NodeId>) -> Result<Vec<u8>, String> {
    let roots: Vec<Arc<Node>> = match node {
        Some(id) => vec![mutate::find_by_id(scene, id).ok_or_else(|| format!("no node {id}"))?],
        None => scene.nodes.lock_ref().iter().cloned().collect(),
    };
    let (images, tex_index) = resolve_images(scene, &roots).await;
    let nodes: Vec<ExportNode> = roots
        .iter()
        .map(|n| node_to_export(scene, n, &tex_index))
        .collect();
    let glb = GlbScene {
        nodes,
        images,
        ..Default::default()
    };
    Ok(write_glb(&glb))
}

/// Resolve every texture referenced by the exported subtree(s) to embedded PNG
/// images (referenced-only): procedural textures are regenerated + encoded;
/// raster textures are read back from the GPU. Returns the image pool + an
/// `AssetId → image index` map. Textures that can't be resolved (e.g. a raster
/// not yet uploaded) are skipped.
async fn resolve_images(scene: &Scene, roots: &[Arc<Node>]) -> (Vec<ExportImage>, TexIndex) {
    let mut ids: Vec<AssetId> = Vec::new();
    let mut seen: HashSet<AssetId> = HashSet::new();
    for n in roots {
        collect_texture_assets(scene, n, &mut ids, &mut seen);
    }
    let mut images = Vec::new();
    let mut index = TexIndex::new();
    for id in ids {
        if let Some((name, bytes)) = resolve_one_texture(scene, id).await {
            index.insert(id, images.len());
            images.push(ExportImage {
                name,
                bytes,
                mime: ImageMime::Png,
            });
        }
    }
    (images, index)
}

/// Walk a subtree collecting the (unique, ordered) texture asset ids that the
/// nodes' effective materials reference.
fn collect_texture_assets(
    scene: &Scene,
    node: &Node,
    ids: &mut Vec<AssetId>,
    seen: &mut HashSet<AssetId>,
) {
    let kind = node.kind.get_cloned();
    if let Some((material, inline_material, custom_material)) = material_slots(&kind) {
        // Custom-WGSL materials export as AWSM_materials_none (no glTF textures).
        if custom_material.is_none() {
            if let Some(def) = effective_material_def(scene, material, inline_material) {
                for t in material_texture_refs(&def) {
                    if seen.insert(t.asset) {
                        ids.push(t.asset);
                    }
                }
            }
        }
    }
    for c in node.children.lock_ref().iter() {
        collect_texture_assets(scene, c, ids, seen);
    }
}

/// The texture refs a PBR/Unlit `MaterialDef` carries (the five glTF slots).
fn material_texture_refs(def: &MaterialDef) -> Vec<TextureRef> {
    [
        &def.base_color_texture,
        &def.metallic_roughness_texture,
        &def.normal_texture,
        &def.occlusion_texture,
        &def.emissive_texture,
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect()
}

/// Resolve one texture asset to `(name, png_bytes)`. Procedural → regenerate +
/// encode (sync); raster → GPU readback (async). `None` if missing/unavailable.
async fn resolve_one_texture(scene: &Scene, id: AssetId) -> Option<(String, Vec<u8>)> {
    let def = {
        let assets = scene.assets.lock().unwrap();
        match assets.get(id).map(|e| &e.source) {
            Some(AssetSource::Texture(d)) => d.clone(),
            _ => return None,
        }
    };
    match def {
        TextureDef::Procedural(p) => {
            let (rgba, w, h) = bridge_material::procedural_rgba(&p);
            rgba_to_png(&rgba, w, h).map(|png| (format!("texture-{id}"), png))
        }
        TextureDef::Raster { display_name } => {
            // Only available once uploaded to the GPU (assign the material / its
            // model first). Skipped otherwise — referenced-only.
            let key = bridge_material::texture_key_for(id)?;
            let handle = crate::engine::context::renderer_handle();
            let r = handle.lock().await;
            let png = r.texture_png_bytes(key).await.ok()?;
            Some((display_name, png))
        }
    }
}

/// Encode tightly-packed RGBA8 to PNG bytes (via the `image` crate).
fn rgba_to_png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    Some(bytes)
}

fn node_to_export(scene: &Scene, node: &Node, tex_index: &TexIndex) -> ExportNode {
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

    let kind = node.kind.get_cloned();
    if let Some(mesh) = node_mesh(scene, &kind) {
        out.mesh = Some(mesh);
        if let Some((material, inline_material, custom_material)) = material_slots(&kind) {
            out.material = Some(map_material(
                scene,
                material,
                inline_material,
                custom_material,
                tex_index,
            ));
        }
    }
    match &kind {
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
        .map(|c| node_to_export(scene, c, tex_index))
        .collect();
    out
}

/// Resolve any geometry node to baked triangles: Primitive → generated, Mesh →
/// the captured-mesh store, Sweep → the curve baked along the scene curve. `None`
/// for non-geometry kinds (Model is deferred — needs source-blob re-read). Shared
/// by GLB export and the `MeshStats`/`MeshCrossSection` introspection queries.
pub(crate) fn node_mesh(scene: &Scene, kind: &NodeKind) -> Option<MeshData> {
    match kind {
        NodeKind::Primitive { shape, .. } => Some(node_sync::primitive_to_mesh(shape)),
        NodeKind::Mesh { mesh, .. } => mesh_cache::get_raw(mesh.0).map(|r| MeshData {
            positions: r.positions,
            normals: r.normals,
            uvs: r.uvs,
            colors: r.colors,
            indices: r.indices,
        }),
        NodeKind::SweepAlongCurve { def, .. } => sweep_mesh(scene, def),
        _ => None,
    }
}

/// Borrow the (assigned, inline, custom) material slots of a geometry node kind.
#[allow(clippy::type_complexity)]
fn material_slots(
    kind: &NodeKind,
) -> Option<(
    &Option<MaterialRef>,
    &MaterialDef,
    &Option<CustomMaterialInstance>,
)> {
    match kind {
        NodeKind::Primitive {
            material,
            inline_material,
            custom_material,
            ..
        }
        | NodeKind::Mesh {
            material,
            inline_material,
            custom_material,
            ..
        }
        | NodeKind::SweepAlongCurve {
            material,
            inline_material,
            custom_material,
            ..
        } => Some((material, inline_material, custom_material)),
        _ => None,
    }
}

/// The effective `MaterialDef` for a geometry node: the assigned library material
/// asset if present + resolvable, else the inline material.
fn effective_material_def(
    scene: &Scene,
    material: &Option<MaterialRef>,
    inline: &MaterialDef,
) -> Option<MaterialDef> {
    let assigned = material.as_ref().and_then(|r| {
        let assets = scene.assets.lock().unwrap();
        match assets.get(r.0).map(|e| &e.source) {
            Some(AssetSource::Material(def)) => Some(def.clone()),
            _ => None,
        }
    });
    Some(assigned.unwrap_or_else(|| inline.clone()))
}

/// Resolve a node's effective material to the export representation.
fn map_material(
    scene: &Scene,
    material: &Option<MaterialRef>,
    inline: &MaterialDef,
    custom: &Option<CustomMaterialInstance>,
    tex_index: &TexIndex,
) -> ExportMaterial {
    // A custom-WGSL material is not glTF-representable → none + carried id.
    if let Some(inst) = custom {
        return ExportMaterial::None {
            id: Some(inst.material.to_string()),
        };
    }
    let def = effective_material_def(scene, material, inline).unwrap_or_else(|| inline.clone());
    map_material_def(&def, material.as_ref(), tex_index)
}

/// Resolve a `TextureRef` to an export `TexRef` (image index + uv set), if the
/// referenced texture was embedded.
fn tex_ref(t: &Option<TextureRef>, tex_index: &TexIndex) -> Option<TexRef> {
    let t = t.as_ref()?;
    let image = *tex_index.get(&t.asset)?;
    Some(TexRef {
        image,
        tex_coord: t.uv_index,
    })
}

fn map_material_def(
    def: &MaterialDef,
    assigned: Option<&MaterialRef>,
    tex_index: &TexIndex,
) -> ExportMaterial {
    match def.shading {
        MaterialShading::Pbr => ExportMaterial::Pbr(PbrMaterial {
            name: def.label.clone(),
            base_color: def.base_color,
            metallic: def.metallic,
            roughness: def.roughness,
            emissive: def.emissive,
            alpha_mode: map_alpha(&def.alpha_mode),
            double_sided: def.double_sided,
            base_color_texture: tex_ref(&def.base_color_texture, tex_index),
            metallic_roughness_texture: tex_ref(&def.metallic_roughness_texture, tex_index),
            normal_texture: tex_ref(&def.normal_texture, tex_index),
            occlusion_texture: tex_ref(&def.occlusion_texture, tex_index),
            emissive_texture: tex_ref(&def.emissive_texture, tex_index),
        }),
        MaterialShading::Unlit => ExportMaterial::Unlit(UnlitMaterial {
            name: def.label.clone(),
            base_color: def.base_color,
            alpha_mode: map_alpha(&def.alpha_mode),
            double_sided: def.double_sided,
            base_color_texture: tex_ref(&def.base_color_texture, tex_index),
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
/// from the scene tree (mirrors the renderer-bridge `materialize_sweep`). Shared
/// with `ConvertToEditableMesh` (which bakes a sweep into a captured mesh).
pub(crate) fn sweep_mesh(scene: &Scene, def: &SweepAlongCurveDef) -> Option<MeshData> {
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
