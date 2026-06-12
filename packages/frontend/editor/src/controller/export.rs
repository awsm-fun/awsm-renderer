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
//! ## Imported-glTF geometry
//! Imported models are baked into captured `NodeKind::Mesh` nodes at import (their
//! geometry lives in the [`mesh_cache`] store, like every other procedural mesh),
//! so export reads them through the normal Mesh path with no special handling.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use awsm_editor_protocol::animation::{TrackTarget, TrackValue, TransformProp};
use awsm_editor_protocol::dynamic_material::MaterialInstance;
use awsm_editor_protocol::{
    AssetId, AssetSource, CameraConfig, CameraProjection, CrossSectionDef, LightConfig,
    MaterialAlphaMode, MaterialDef, MaterialShading, NodeId, NodeKind, SweepAlongCurveDef,
    SweepUvMode, TextureDef, TextureRef,
};
use awsm_glb_export::{
    write_glb, AlphaMode, AnimInterp, AnimPath, ExportAnimChannel, ExportAnimation, ExportCamera,
    ExportImage, ExportLight, ExportMaterial, ExportNode, GlbScene, ImageMime, MeshData,
    PbrMaterial, TexRef, Trs, UnlitMaterial,
};

use crate::engine::bridge::{material as bridge_material, mesh_cache};
use crate::engine::scene::{mutate, node::Node, Scene};

/// Maps a referenced texture asset → its index in `GlbScene::images`.
type TexIndex = HashMap<AssetId, usize>;

/// Bake the whole scene **including animations** (clips lowered to glTF TRS
/// channels) — the path behind `ExportGlb { node: None }` and the player bundle.
pub async fn export_scene_glb(ctrl: &super::EditorController) -> Result<Vec<u8>, String> {
    let scene = &ctrl.scene;
    let roots: Vec<Arc<Node>> = scene.nodes.lock_ref().iter().cloned().collect();
    let (images, tex_index) = resolve_images(scene, &roots).await;
    // Rig embedding (shared with export_glb): appended AFTER the scene part,
    // so the clip channels lowered against build_index_map's node indices
    // stay valid (appending never shifts existing DFS indices).
    let (rig_scenes, rig_embedded) = collect_rig_scenes(&roots);
    let nodes: Vec<ExportNode> = roots
        .iter()
        .map(|n| node_to_export(scene, n, &tex_index, &rig_embedded))
        .collect();
    let index_map = build_index_map(scene);
    let clips: Vec<_> = ctrl.custom_animations.lock_ref().iter().cloned().collect();
    let animations = lower_clips(&clips, &index_map);
    let mut glb = GlbScene {
        nodes,
        animations,
        images,
        ..Default::default()
    };
    append_rigs(&mut glb, rig_scenes);
    Ok(write_glb(&glb))
}

/// Bake the live scene to a **player bundle directory** (`scene.toml` + an
/// `assets/` directory) — the runtime form per the glb-mesh design, replacing the
/// old single-`scene.glb` bundle.
///
/// Emits: `scene.toml` (the runtime `Scene` from `project_to_scene` — nodes /
/// transforms / material-instances / lights / cameras / our-clips / env, meshes
/// by id); `assets/<id>.glb` (one geometry-only glb per mesh that lowered to
/// `RuntimeMesh::Glb` — bare primitives stay procedural in `scene.toml`; no
/// materials/animations in the glb, those are ours); `assets/materials/<name>/…`
/// (custom-material wgsl + sidecars); `assets/<id>.png` (referenced textures).
///
/// Skinned/morph meshes re-export a clean rig glb from their source (skeleton +
/// mesh + skin + morph, built at import via `reexport_clean_scene`); the
/// `scene.toml` SkinnedMesh nodes reference it by `skin.source` → `assets/<source>.glb`.
pub async fn bake_player_bundle(
    ctrl: &super::EditorController,
) -> Result<Vec<awsm_editor_protocol::BundleFile>, String> {
    use awsm_editor_protocol::{assemble_bundle, mesh_glb_filename, BundleFile, RuntimeMesh};
    use awsm_editor_protocol::{lower_mesh, project_to_scene};

    let project = crate::controller::persistence::to_editor_project(ctrl);
    let mut scene = project_to_scene(&project);
    let mut files: Vec<BundleFile> = Vec::new();

    // 0. Custom-material BUFFER overrides: the per-mesh `buffer_overrides` carry a
    //    session path into the editor's in-memory word store. Emit each as
    //    `assets/buffer-<id>.bin` and rewrite the path to that bundle-relative
    //    location so the player can read it back (the words don't otherwise
    //    survive the bake — they're not in the asset table like textures).
    rewrite_buffer_overrides(&mut scene.nodes, &mut files);

    // 1. One geometry-only glb per Glb-lowered mesh asset.
    for (id, entry) in &project.assets.entries {
        if let AssetSource::Mesh(def) = &entry.source {
            if matches!(lower_mesh(def), RuntimeMesh::Glb) {
                if let Some(raw) = mesh_cache::get_raw(*id) {
                    let mesh = MeshData {
                        positions: raw.positions,
                        normals: raw.normals,
                        uvs: raw.uvs,
                        colors: raw.colors,
                        indices: raw.indices,
                    };
                    let glb = write_glb(&GlbScene {
                        nodes: vec![ExportNode::new("mesh").with_mesh(mesh)],
                        ..Default::default()
                    });
                    files.push(BundleFile::asset(mesh_glb_filename(*id), glb));
                }
            }
        }
    }

    // 2. Custom-material folders. `material_files` already returns paths rooted
    //    under `assets/` (e.g. `assets/materials/<slug>-<id>/material.wgsl`), so
    //    they go in verbatim — prepending `ASSETS_DIR` here would double it to
    //    `assets/assets/materials/…`.
    for (path, contents) in crate::controller::persistence::material_files(ctrl) {
        files.push(BundleFile::new(path, contents.into_bytes()));
    }

    // 3. Textures the materials reference → assets/<id>.png (built-in + custom-WGSL).
    let roots: Vec<Arc<Node>> = ctrl.scene.nodes.lock_ref().iter().cloned().collect();
    let mut ids: Vec<AssetId> = Vec::new();
    let mut seen: HashSet<AssetId> = HashSet::new();
    for n in &roots {
        collect_texture_assets(n, &mut ids, &mut seen);
        collect_custom_texture_assets(ctrl, n, &mut ids, &mut seen);
    }
    for id in ids {
        if let Some((_name, png)) = resolve_one_texture(&ctrl.scene, id).await {
            files.push(BundleFile::asset(format!("{id}.png"), png));
        }
    }

    // 4. Skinned meshes: one clean rig glb (skeleton + mesh + skin + morph, built
    // at import via reexport_clean_scene) per imported source. The scene.toml
    // SkinnedMesh nodes reference `skin.source` → `assets/<source>.glb`.
    fn collect_skinned(node: &Node, out: &mut HashSet<AssetId>) {
        if let NodeKind::SkinnedMesh { skin, .. } = &node.kind.get_cloned() {
            out.insert(skin.source);
        }
        for c in node.children.lock_ref().iter() {
            collect_skinned(c, out);
        }
    }
    let mut skinned_sources: HashSet<AssetId> = HashSet::new();
    for n in &roots {
        collect_skinned(n, &mut skinned_sources);
    }
    for src in skinned_sources {
        if let Some(glb) = crate::engine::bridge::skinned_bake_cache::get_rig_glb(src) {
            files.push(BundleFile::asset(
                awsm_editor_protocol::mesh_glb_filename(src),
                glb,
            ));
        }
    }

    assemble_bundle(&scene, files).map_err(|e| e.to_string())
}

/// Emit each custom-material BUFFER override's words as `assets/buffer-<id>.bin`
/// and rewrite its `BufferRef` path to that bundle location. The editor stores
/// override words in an in-memory session map keyed by a `session://buffer/<id>`
/// path; that path means nothing to the player, and (unlike textures) the words
/// aren't in the asset table — so the bake must materialize them. Recurses the
/// whole tree (operates on the baked `Scene`'s plain nodes, pre-serialization).
fn rewrite_buffer_overrides(
    nodes: &mut [awsm_editor_protocol::EditorNode],
    files: &mut Vec<awsm_editor_protocol::BundleFile>,
) {
    use awsm_editor_protocol::{BundleFile, NodeKind, ASSETS_DIR};
    for node in nodes {
        let material = match &mut node.kind {
            NodeKind::Mesh { material, .. } | NodeKind::SkinnedMesh { material, .. } => {
                material.as_mut()
            }
            _ => None,
        };
        if let Some(inst) = material {
            for bref in inst.buffer_overrides.values_mut() {
                let path = bref.path.to_string_lossy().to_string();
                if let Some(words) = crate::engine::bridge::dynamic::buffer_words_for(&path) {
                    let leaf = format!("buffer-{}.bin", AssetId::new().0);
                    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
                    files.push(BundleFile::asset(leaf.clone(), bytes));
                    bref.path = std::path::PathBuf::from(format!("{ASSETS_DIR}/{leaf}"));
                }
            }
        }
        rewrite_buffer_overrides(&mut node.children, files);
    }
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
    use awsm_editor_protocol::animation::SamplerKind;
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

    let (rig_scenes, rig_embedded) = collect_rig_scenes(&roots);
    let nodes: Vec<ExportNode> = roots
        .iter()
        .map(|n| node_to_export(scene, n, &tex_index, &rig_embedded))
        .collect();
    let mut glb = GlbScene {
        nodes,
        images,
        ..Default::default()
    };
    append_rigs(&mut glb, rig_scenes);
    Ok(write_glb(&glb))
}

/// Rig embedding: SkinnedMesh sources whose clean rig glb is cached (built at
/// import by `reexport_clean_scene`) get the WHOLE rig — skeleton nodes, skin
/// (joints/IBMs/JOINTS_0/WEIGHTS_0) and morph targets — appended to the
/// export, so a scene glb round-trips rigs instead of flattening them to
/// bind-pose statics. The editor-side SkinnedMesh nodes skip their static
/// bake (see `node_to_export`). v1 limitations (logged): the rig embeds at
/// its source placement (edits to the mirror hierarchy don't retarget into
/// the rig), and rig materials are the source defaults (the bundle path
/// re-applies ours from scene.toml).
fn collect_rig_scenes(roots: &[Arc<Node>]) -> (Vec<awsm_glb_export::GlbScene>, HashSet<AssetId>) {
    fn collect(node: &Node, out: &mut Vec<AssetId>, seen: &mut HashSet<AssetId>) {
        if let NodeKind::SkinnedMesh { skin, .. } = &node.kind.get_cloned() {
            if seen.insert(skin.source) {
                out.push(skin.source);
            }
        }
        for c in node.children.lock_ref().iter() {
            collect(c, out, seen);
        }
    }
    let mut sources = Vec::new();
    let mut seen = HashSet::new();
    for n in roots {
        collect(n, &mut sources, &mut seen);
    }
    let mut rig_scenes = Vec::new();
    let mut rig_embedded = HashSet::new();
    for src in sources {
        let Some(bytes) = crate::engine::bridge::skinned_bake_cache::get_rig_glb(src) else {
            tracing::warn!(
                "glb export: no cached rig glb for source {src} — its skinned \
                 nodes export as bind-pose statics"
            );
            continue;
        };
        match awsm_glb_export::reexport_clean(&bytes) {
            Some(rig) => {
                rig_embedded.insert(src);
                rig_scenes.push(rig);
            }
            None => tracing::warn!(
                "glb export: cached rig glb for {src} failed to re-parse — \
                 exporting bind-pose statics"
            ),
        }
    }
    (rig_scenes, rig_embedded)
}

/// Append each rig with index fixups: skin joints are DFS-flattened node
/// indices (the writer flattens roots in pre-order), so appending rig roots
/// after everything flattened so far shifts them by a uniform offset;
/// node→skin bindings shift by the skins appended so far. Appending never
/// shifts EXISTING node indices, so animation channels lowered against the
/// scene part stay valid.
fn append_rigs(glb: &mut GlbScene, rigs: Vec<awsm_glb_export::GlbScene>) {
    fn count_nodes(nodes: &[ExportNode]) -> usize {
        nodes.iter().map(|n| 1 + count_nodes(&n.children)).sum()
    }
    fn bump_skin_refs(nodes: &mut [ExportNode], skin_base: usize) {
        for n in nodes {
            if let Some(s) = n.skin.as_mut() {
                *s += skin_base;
            }
            bump_skin_refs(&mut n.children, skin_base);
        }
    }
    for mut rig in rigs {
        let node_offset = count_nodes(&glb.nodes);
        let skin_base = glb.skins.len();
        for skin in &mut rig.skins {
            for j in &mut skin.joints {
                *j += node_offset;
            }
        }
        bump_skin_refs(&mut rig.nodes, skin_base);
        glb.skins.extend(rig.skins);
        glb.nodes.extend(rig.nodes);
    }
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

/// The texture refs a PBR/Unlit `MaterialDef` carries: the five standard glTF
/// slots plus every KHR-extension texture slot (so the player can bind them —
/// mirrors the loader's `bind_extension_textures`).
fn material_texture_refs(def: &MaterialDef) -> Vec<TextureRef> {
    let mut refs: Vec<TextureRef> = [
        &def.base_color_texture,
        &def.metallic_roughness_texture,
        &def.normal_texture,
        &def.occlusion_texture,
        &def.emissive_texture,
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect();
    let ext = &def.extensions;
    for t in [
        ext.specular.as_ref().and_then(|e| e.tex.as_ref()),
        ext.specular.as_ref().and_then(|e| e.color_tex.as_ref()),
        ext.transmission.as_ref().and_then(|e| e.tex.as_ref()),
        ext.diffuse_transmission
            .as_ref()
            .and_then(|e| e.tex.as_ref()),
        ext.diffuse_transmission
            .as_ref()
            .and_then(|e| e.color_tex.as_ref()),
        ext.volume.as_ref().and_then(|e| e.thickness_tex.as_ref()),
        ext.clearcoat.as_ref().and_then(|e| e.tex.as_ref()),
        ext.clearcoat
            .as_ref()
            .and_then(|e| e.roughness_tex.as_ref()),
        ext.clearcoat.as_ref().and_then(|e| e.normal_tex.as_ref()),
        ext.sheen.as_ref().and_then(|e| e.color_tex.as_ref()),
        ext.sheen.as_ref().and_then(|e| e.roughness_tex.as_ref()),
        ext.anisotropy.as_ref().and_then(|e| e.tex.as_ref()),
        ext.iridescence.as_ref().and_then(|e| e.tex.as_ref()),
        ext.iridescence
            .as_ref()
            .and_then(|e| e.thickness_tex.as_ref()),
    ]
    .into_iter()
    .flatten()
    {
        refs.push(*t);
    }
    refs
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
    rig_embedded: &HashSet<AssetId>,
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
    // Every geometry kind — including imported models, now baked into captured
    // Mesh nodes — bakes its triangles inline via `node_mesh` (from the
    // captured-mesh store). The mesh is the node's RAW local-space geometry;
    // `out.transform` (the editor node's transform, mirrored from the glTF node's
    // local at import) already places it, so applying any extra matrix here would
    // double-transform.
    // A SkinnedMesh whose source rig is embedded wholesale (see export_glb's
    // rig-embedding pass) must NOT also bake its static bind-pose copy —
    // geometry would double.
    let rig_covers_this = matches!(
        &kind,
        NodeKind::SkinnedMesh { skin, .. } if rig_embedded.contains(&skin.source)
    );
    if !rig_covers_this {
        if let Some(mesh) = node_mesh(scene, &kind) {
            out.mesh = Some(mesh);
            if let Some(material) = material_slot(&kind) {
                out.material = Some(map_material(material, tex_index));
            }
        }
    }
    match &kind {
        NodeKind::Light(cfg) => out.light = Some(map_light(cfg)),
        NodeKind::Camera(cfg) => out.camera = Some(map_camera(cfg)),
        // Group + non-geometry leaves export as plain transform nodes; their
        // children still recurse below.
        _ => {}
    }

    out.children = node
        .children
        .lock_ref()
        .iter()
        .map(|c| node_to_export(scene, c, tex_index, rig_embedded))
        .collect();
    out
}

/// Resolve any geometry node to baked triangles: Mesh → the captured-mesh store
/// (every geometry node — primitive / sweep / lathe / SDF / imported-glTF — is a
/// Mesh backed by a baked `ModifierStack`). `None` for non-geometry kinds. Shared
/// by GLB export and the `MeshStats`/`MeshCrossSection` introspection queries +
/// vertex-highlight. (`scene` is unused now that all geometry resolves from the
/// store, but kept for signature stability with the introspection callers.)
pub(crate) fn node_mesh(_scene: &Scene, kind: &NodeKind) -> Option<MeshData> {
    match kind {
        NodeKind::Mesh { mesh, .. } => mesh_cache::get_raw(mesh.0).map(|r| MeshData {
            positions: r.positions,
            normals: r.normals,
            uvs: r.uvs,
            colors: r.colors,
            indices: r.indices,
        }),
        // A skinned mesh exports its **bind-pose** geometry (the simplest correct
        // path: GLB export is static, and the bind pose is what `drop_skinning`
        // would bake). Resolved from the session-local bind-pose bake cache;
        // `None` after a cold reload (no cached bake) — flagged as a limitation.
        NodeKind::SkinnedMesh { skin, .. } => crate::engine::bridge::skinned_bake_cache::get(
            skin.source,
            skin.node_index,
            skin.primitive_index,
        ),
        _ => None,
    }
}

/// Borrow the single material assignment of a geometry node kind.
fn material_slot(kind: &NodeKind) -> Option<&Option<MaterialInstance>> {
    match kind {
        NodeKind::Mesh { material, .. } => Some(material),
        NodeKind::SkinnedMesh { material, .. } => Some(material),
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
