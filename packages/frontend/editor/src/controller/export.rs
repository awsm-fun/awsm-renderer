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
//! ## Model nodes
//! An imported-glTF `Model` node references one node inside a source file (by
//! node index, optionally one primitive). Export re-reads that node's geometry
//! from the source bytes cached at import (`model_source_cache`, consulted via the
//! [`resolve_model_meshes`] pre-pass) and emits it as baked triangles, with the
//! node's assigned library material. Models whose source bytes aren't cached
//! (e.g. after a project reload — see the `model_source_cache` TODO) still export
//! as empty transform nodes (their children recurse).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use awsm_glb_export::{
    write_glb, AlphaMode, AnimInterp, AnimPath, ExportAnimChannel, ExportAnimation, ExportCamera,
    ExportImage, ExportLight, ExportMaterial, ExportNode, GlbScene, ImageMime, MeshData,
    PbrMaterial, TexRef, Trs, UnlitMaterial,
};
use awsm_scene_schema::animation::{TrackTarget, TrackValue, TransformProp};
use awsm_scene_schema::dynamic_material::MaterialInstance;
use awsm_scene_schema::{
    AssetId, AssetSource, CameraConfig, CameraProjection, CrossSectionDef, LightConfig,
    MaterialAlphaMode, MaterialDef, MaterialShading, NodeId, NodeKind, SweepAlongCurveDef,
    SweepUvMode, TextureDef, TextureRef,
};

use crate::engine::bridge::{
    material as bridge_material, mesh_cache, model_source_cache, node_sync,
};
use crate::engine::scene::{mutate, node::Node, Scene};

/// Maps a referenced texture asset → its index in `GlbScene::images`.
type TexIndex = HashMap<AssetId, usize>;

/// Maps a `Model` node's id → its re-read source geometry (the [`resolve_model_meshes`]
/// pre-pass). A node absent from the map had no cached source / no extractable mesh.
type ModelMeshes = HashMap<NodeId, MeshData>;

/// Bake the whole scene **including animations** (clips lowered to glTF TRS
/// channels) — the path behind `ExportGlb { node: None }` and the player bundle.
pub async fn export_scene_glb(ctrl: &super::EditorController) -> Result<Vec<u8>, String> {
    let scene = &ctrl.scene;
    let roots: Vec<Arc<Node>> = scene.nodes.lock_ref().iter().cloned().collect();
    let (images, tex_index) = resolve_images(scene, &roots).await;
    let model_meshes = resolve_model_meshes(scene, &roots);
    let nodes: Vec<ExportNode> = roots
        .iter()
        .map(|n| node_to_export(scene, n, &tex_index, &model_meshes))
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

/// Assemble a player bundle for the live scene, reusing the native
/// [`awsm_glb_export::assemble_bundle`] layout so the editor and the
/// natively-tested directory layout can never drift.
///
/// Pieces:
/// - `scene.glb`: the whole-scene baked glTF (with embedded **PBR/built-in**
///   textures — referenced-only — and animations) from [`export_scene_glb`].
/// - `materials`: the custom-material `.wgsl`/`.toml` side-files at their
///   bundle-relative paths (from `persistence::material_files`).
/// - `textures`: only the textures referenced by **custom-WGSL** material
///   instances in the scene (their `texture_overrides`). Built-in/PBR textures
///   are NOT gathered here — they already travel embedded inside `scene_glb`.
/// - `env.json`: the serialized environment descriptor.
pub async fn assemble_player_bundle(
    ctrl: &super::EditorController,
    name: &str,
) -> Result<awsm_glb_export::PlayerBundle, String> {
    use awsm_glb_export::BundleInputs;

    let scene_glb = export_scene_glb(ctrl).await?;
    let materials = crate::controller::persistence::material_files(ctrl);
    let env_json = serde_json::to_string(&ctrl.scene.environment.get_cloned()).ok();

    // Gather the (deduped, ordered) texture asset ids referenced by custom-WGSL
    // material instances in the scene.
    let mut ids: Vec<AssetId> = Vec::new();
    let mut seen: HashSet<AssetId> = HashSet::new();
    let roots: Vec<Arc<Node>> = ctrl.scene.nodes.lock_ref().iter().cloned().collect();
    for n in &roots {
        collect_custom_texture_assets(ctrl, n, &mut ids, &mut seen);
    }

    let mut textures: Vec<(String, Vec<u8>)> = Vec::new();
    for id in ids {
        if let Some((tex_name, png)) = resolve_one_texture(&ctrl.scene, id).await {
            textures.push((format!("{}.png", sanitize_filename(&tex_name)), png));
        }
    }

    Ok(awsm_glb_export::assemble_bundle(
        name,
        BundleInputs {
            scene_glb,
            materials,
            textures,
            env_json,
        },
    ))
}

/// Walk a subtree collecting the (unique, ordered) texture asset ids referenced
/// by **custom-WGSL** material instances' `texture_overrides`. Built-in/PBR
/// material textures are skipped — those are embedded in `scene_glb` already.
//
// TODO: this only gathers `texture_overrides`; a custom material whose declared
// texture slot uses its default (un-overridden) texture is not covered here.
// Resolving declared-slot defaults is a follow-on.
fn collect_custom_texture_assets(
    ctrl: &super::EditorController,
    node: &Node,
    ids: &mut Vec<AssetId>,
    seen: &mut HashSet<AssetId>,
) {
    let kind = node.kind.get_cloned();
    if let Some(Some(inst)) = material_slot(&kind) {
        // A custom-WGSL material is one whose asset resolves to a custom-material
        // entry with NO built-in variant (the inverse of `collect_texture_assets`).
        let is_builtin =
            crate::controller::custom_material::find_material(&ctrl.custom_materials, inst.asset)
                .map(|m| m.builtin.get_cloned().is_some())
                .unwrap_or(false);
        if !is_builtin {
            for t in inst.texture_overrides.values() {
                if seen.insert(t.asset) {
                    ids.push(t.asset);
                }
            }
        }
    }
    for c in node.children.lock_ref().iter() {
        collect_custom_texture_assets(ctrl, c, ids, seen);
    }
}

/// Keep a filename simple/safe: alphanumeric, dash, underscore, dot are kept;
/// everything else becomes `_`.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
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
    let model_meshes = resolve_model_meshes(scene, &roots);
    let nodes: Vec<ExportNode> = roots
        .iter()
        .map(|n| node_to_export(scene, n, &tex_index, &model_meshes))
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
        collect_texture_assets(n, &mut ids, &mut seen);
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

/// Pre-pass mirroring [`resolve_images`]: re-read every `Model` node's geometry
/// from its source glTF/GLB and key it by `NodeId` for [`node_to_export`].
///
/// Source bytes come from [`model_source_cache`] (stashed at import), loaded
/// **once per `asset_id`** (deduped) and parsed into a `gltf::Document` reused
/// across every Model node referencing that asset. A node is omitted from the map
/// (and exports as an empty transform node) when its source isn't cached or its
/// `node_index` / mesh can't be read — each logged with a `tracing::warn!`.
///
/// Synchronous: parsing + accessor reads need no GPU/await (unlike texture
/// readback). It's a free-standing pre-pass only to keep the per-asset parse
/// deduped and out of the recursive `node_to_export` hot path.
fn resolve_model_meshes(scene: &Scene, roots: &[Arc<Node>]) -> ModelMeshes {
    // Collect (node id, model ref) for every Model node in the forest.
    let mut models: Vec<(NodeId, awsm_scene_schema::ModelRef)> = Vec::new();
    fn walk(node: &Node, out: &mut Vec<(NodeId, awsm_scene_schema::ModelRef)>) {
        if let NodeKind::Model(m) = node.kind.get_cloned() {
            out.push((node.id, m));
        }
        for c in node.children.lock_ref().iter() {
            walk(c, out);
        }
    }
    for r in roots {
        walk(r, &mut models);
    }

    // Parse each referenced source glTF/GLB once (deduped by asset_id). `None`
    // marks an asset whose bytes weren't cached or didn't parse, so we warn once.
    let mut parsed: HashMap<AssetId, Option<Arc<ParsedGltf>>> = HashMap::new();
    let mut out = ModelMeshes::new();
    for (node_id, model) in models {
        let entry = parsed
            .entry(model.asset_id)
            .or_insert_with(|| load_model_source(scene, model.asset_id));
        let Some(gltf) = entry.clone() else {
            continue; // already warned in load_model_source
        };
        match awsm_glb_export::extract_node_mesh(
            &gltf.doc,
            &gltf.buffers,
            model.node_index,
            model.primitive_index,
        ) {
            Some(mesh) => {
                out.insert(node_id, mesh);
            }
            None => tracing::warn!(
                "model export: node {} (asset {}, gltf node {}) has no extractable mesh; \
                 exporting as empty transform node",
                node_id,
                model.asset_id,
                model.node_index
            ),
        }
    }
    out
}

/// A parsed source glTF/GLB held for the duration of a single export (its
/// `Document` + buffer blobs are reused across every Model node that references
/// the same asset).
struct ParsedGltf {
    doc: gltf::Document,
    buffers: Vec<Vec<u8>>,
}

/// Load + parse a model's cached source bytes for `asset_id`. `None` (with a
/// warn) when the bytes aren't cached (e.g. post-reload) or don't parse.
fn load_model_source(scene: &Scene, asset_id: AssetId) -> Option<Arc<ParsedGltf>> {
    let Some(bytes) = model_source_cache::get(asset_id) else {
        let name = scene
            .assets
            .lock()
            .unwrap()
            .display_name(asset_id)
            .map(str::to_owned)
            .unwrap_or_default();
        tracing::warn!(
            "model export: no cached source bytes for asset {asset_id} ({name}); \
             Model nodes from it export as empty transform nodes \
             (model source bytes don't survive a project reload yet)"
        );
        return None;
    };
    match gltf::import_slice(bytes.as_slice()) {
        Ok((doc, buffers, _images)) => Some(Arc::new(ParsedGltf {
            doc,
            buffers: buffers.into_iter().map(|b| b.0).collect(),
        })),
        Err(e) => {
            tracing::warn!("model export: asset {asset_id} source failed to parse: {e}");
            None
        }
    }
}

/// Walk a subtree collecting the (unique, ordered) texture asset ids that the
/// nodes' effective materials reference.
fn collect_texture_assets(node: &Node, ids: &mut Vec<AssetId>, seen: &mut HashSet<AssetId>) {
    let kind = node.kind.get_cloned();
    if let Some(Some(inst)) = material_slot(&kind) {
        // Only a built-in assignment exports glTF textures (its per-mesh `inline`
        // carries the slots); custom-WGSL materials export as AWSM_materials_none.
        let is_builtin = crate::controller::custom_material::find_material(
            &crate::controller::controller().custom_materials,
            inst.asset,
        )
        .map(|m| m.builtin.get_cloned().is_some())
        .unwrap_or(false);
        if is_builtin {
            for t in material_texture_refs(&inst.inline) {
                if seen.insert(t.asset) {
                    ids.push(t.asset);
                }
            }
        }
    }
    for c in node.children.lock_ref().iter() {
        collect_texture_assets(c, ids, seen);
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

fn node_to_export(
    scene: &Scene,
    node: &Node,
    tex_index: &TexIndex,
    model_meshes: &ModelMeshes,
) -> ExportNode {
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
    // Model geometry comes from the pre-pass map (keyed by node id, re-read from
    // the source file); every other geometry kind bakes inline via `node_mesh`.
    // The mesh is the source node's RAW accessor geometry (its own local space) —
    // `out.transform` (above, the editor node's transform, mirrored from the glTF
    // node's local at import) already places it, so applying any extra matrix here
    // would double-transform.
    let mesh = match &kind {
        NodeKind::Model(_) => model_meshes.get(&node.id).cloned(),
        _ => node_mesh(scene, &kind),
    };
    if let Some(mesh) = mesh {
        out.mesh = Some(mesh);
        if let Some(material) = material_slot(&kind) {
            out.material = Some(map_material(material, tex_index));
        }
    }
    match &kind {
        NodeKind::Light(cfg) => out.light = Some(map_light(cfg)),
        NodeKind::Camera(cfg) => out.camera = Some(map_camera(cfg)),
        // Group + non-geometry leaves (and Models with no cached source) export as
        // plain transform nodes; their children still recurse below.
        _ => {}
    }

    out.children = node
        .children
        .lock_ref()
        .iter()
        .map(|c| node_to_export(scene, c, tex_index, model_meshes))
        .collect();
    out
}

/// Resolve any geometry node to baked triangles: Primitive → generated, Mesh →
/// the captured-mesh store, Sweep → the curve baked along the scene curve, Model
/// → its source node's geometry re-read from the import-cached glTF bytes (raw
/// accessor positions in the node's local space — see [`node_to_export`] on the
/// no-double-transform rule). `None` for non-geometry kinds and for Models whose
/// source bytes aren't cached. Shared by GLB export and the
/// `MeshStats`/`MeshCrossSection` introspection queries + vertex-highlight.
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
        NodeKind::Model(m) => {
            let gltf = load_model_source(scene, m.asset_id)?;
            awsm_glb_export::extract_node_mesh(
                &gltf.doc,
                &gltf.buffers,
                m.node_index,
                m.primitive_index,
            )
        }
        _ => None,
    }
}

/// Borrow the single material assignment of a geometry node kind (the `Model`
/// slot lives inside its `ModelRef`, but it's the same one-material-per-node
/// model as every other geometry kind).
fn material_slot(kind: &NodeKind) -> Option<&Option<MaterialInstance>> {
    match kind {
        NodeKind::Primitive { material, .. }
        | NodeKind::Mesh { material, .. }
        | NodeKind::SweepAlongCurve { material, .. } => Some(material),
        NodeKind::Model(m) => Some(&m.material),
        _ => None,
    }
}

/// Resolve a node's material assignment to the export representation.
///
/// - Unassigned (`None`) → [`ExportMaterial::None`] with no id.
/// - A **built-in** assignment (the asset resolves to a built-in library
///   material) → its per-mesh `inline` def mapped to glTF (built-ins ARE
///   glTF-representable).
/// - A **custom-WGSL** assignment (or one that doesn't resolve to a built-in) →
///   [`ExportMaterial::None`] carrying the assigned id for scene-level
///   re-resolution on import.
fn map_material(material: &Option<MaterialInstance>, tex_index: &TexIndex) -> ExportMaterial {
    let Some(inst) = material else {
        return ExportMaterial::None { id: None };
    };
    let is_builtin = crate::controller::custom_material::find_material(
        &crate::controller::controller().custom_materials,
        inst.asset,
    )
    .map(|m| m.builtin.get_cloned().is_some())
    .unwrap_or(false);
    if is_builtin {
        map_material_def(&inst.inline, Some(inst.asset), tex_index)
    } else {
        ExportMaterial::None {
            id: Some(inst.asset.to_string()),
        }
    }
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
    assigned: Option<AssetId>,
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
            id: assigned.map(|a| a.to_string()),
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
