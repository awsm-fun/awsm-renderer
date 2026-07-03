//! Project (de)serialization — the TOML project format.
//!
//! A project is a directory: a `project.toml` (carrying the scene tree,
//! environment, shadows, asset table, and custom-material refs) plus the asset
//! and material files it references. This module builds the serializable
//! [`EditorProject`] from the live editor state and rebuilds the live scene from
//! a loaded one. The source-abstracted load entry is `LoadProjectFromUrl` (it
//! fetches over HTTP, gesture-free — the external/MCP + scriptable-test path);
//! the directory-handle Save (FS Access) writes the same bytes back.

use std::path::PathBuf;
use std::sync::Arc;

use awsm_renderer_editor_protocol::animation::CustomAnimationRef;
use awsm_renderer_editor_protocol::{
    asset_filename, mesh_asset_filename, AssetId, AssetSource, CapturedMesh, CustomMaterialRef,
    EditorProject, StoredMaterial, StoredSlot, TextureDef,
};
use awsm_renderer_web_shared::prelude::Mutable;

use super::animation::{stored_from_live, stored_to_live};
use super::custom_material::{AlphaMode, CustomMaterial, Slot};
use super::node_spec::{node_from_spec, spec_from_node, NodeSpec};
use super::EditorController;
use crate::engine::scene::node::Node;
use crate::error::{EditorError, EditorResult};

/// Snapshot a live custom material into its serializable form.
fn stored_from_material(m: &CustomMaterial) -> StoredMaterial {
    let slot = |s: &Slot| StoredSlot {
        name: s.name.clone(),
        ty: s.ty.clone(),
        val: s.val.clone(),
        debug: s.debug.clone(),
        color_kind: s.color_kind,
    };
    StoredMaterial {
        id: m.id,
        name: m.name.get_cloned(),
        builtin: m.builtin.get_cloned(),
        wgsl: m.wgsl.get_cloned(),
        alpha_wgsl: m.alpha_wgsl.get_cloned(),
        vertex_wgsl: m.vertex_wgsl.get_cloned(),
        alpha: m.alpha.get().key().to_string(),
        cutoff: m.cutoff.get() as f32,
        double_sided: m.double_sided.get(),
        color: m.color.get_cloned(),
        uniforms: m.uniforms.get_cloned().iter().map(slot).collect(),
        textures: m.textures.get_cloned().iter().map(slot).collect(),
        buffers: m.buffers.get_cloned().iter().map(slot).collect(),
        registered: m.registered.get(),
        shader_includes: m.shader_includes.get_cloned(),
        fragment_inputs: m.fragment_inputs.get_cloned(),
    }
}

/// Rebuild a live custom material from its serialized form (same id, so scene
/// nodes' material refs resolve).
fn material_from_stored(s: &StoredMaterial) -> Arc<CustomMaterial> {
    let slot = |x: &StoredSlot| Slot {
        name: x.name.clone(),
        ty: x.ty.clone(),
        val: x.val.clone(),
        debug: x.debug.clone(),
        color_kind: x.color_kind,
    };
    Arc::new(CustomMaterial {
        id: s.id,
        name: Mutable::new(s.name.clone()),
        builtin: Mutable::new(s.builtin.clone()),
        wgsl: Mutable::new(s.wgsl.clone()),
        alpha_wgsl: Mutable::new(s.alpha_wgsl.clone()),
        vertex_wgsl: Mutable::new(s.vertex_wgsl.clone()),
        alpha: Mutable::new(AlphaMode::from_key(&s.alpha)),
        cutoff: Mutable::new(s.cutoff as f64),
        double_sided: Mutable::new(s.double_sided),
        color: Mutable::new(if s.color.is_empty() {
            "#8aa0b8".to_string()
        } else {
            s.color.clone()
        }),
        uniforms: Mutable::new(s.uniforms.iter().map(slot).collect()),
        textures: Mutable::new(s.textures.iter().map(slot).collect()),
        buffers: Mutable::new(s.buffers.iter().map(slot).collect()),
        registered: Mutable::new(s.registered),
        last_diagnostics: Mutable::new(Vec::new()),
        shader_includes: Mutable::new(s.shader_includes.clone()),
        fragment_inputs: Mutable::new(s.fragment_inputs.clone()),
        recompile_rev: Mutable::new(0),
    })
}

/// Build the serializable project from the live editor state.
pub fn to_editor_project(ctrl: &EditorController) -> EditorProject {
    let nodes = ctrl
        .scene
        .nodes
        .lock_ref()
        .iter()
        .map(|n| spec_from_node(n).to_editor_node())
        .collect();

    // Only custom-WGSL materials get a ref: the ref's `folder` promises
    // `material.json` + `material.wgsl` files, and `material_files` only writes
    // those for non-builtin materials (a builtin round-trips via each node's
    // inline MaterialDef). Emitting builtin refs made every bundle advertise
    // phantom folders the player then fetched (dead URLs — or an SPA server's
    // HTML fallback) on every load.
    let custom_materials = ctrl
        .custom_materials
        .lock_ref()
        .iter()
        .filter(|m| m.builtin.get_cloned().is_none())
        .map(|m| {
            let name = m.name.get_cloned();
            let folder = material_folder_path(m.id, &name);
            CustomMaterialRef {
                id: m.id,
                name,
                folder: PathBuf::from(folder),
            }
        })
        .collect();

    let editor_materials = ctrl
        .custom_materials
        .lock_ref()
        .iter()
        .map(|m| stored_from_material(m))
        .collect();

    // Animation library: refs (name + side-file path) + the full authored model.
    let custom_animations = ctrl
        .custom_animations
        .lock_ref()
        .iter()
        .map(|c| {
            let name = c.name.get_cloned();
            let file = animation_file_path(c.id, &name);
            CustomAnimationRef {
                name,
                file: PathBuf::from(file),
            }
        })
        .collect();
    let editor_animations = ctrl
        .custom_animations
        .lock_ref()
        .iter()
        .map(|c| stored_from_live(c))
        .collect();

    EditorProject {
        name: ctrl.project_name.get_cloned(),
        environment: ctrl.scene.environment.get_cloned(),
        shadows: ctrl.scene.shadows.get_cloned(),
        post_process: ctrl.scene.post_process.get_cloned(),
        assets: ctrl.scene.assets.lock().unwrap().clone(),
        custom_materials,
        editor_materials,
        custom_animations,
        editor_animations,
        anim_mixer: ctrl.anim_mixer.get_cloned(),
        nodes,
    }
}

/// Serialize the live project to a `project.toml` string.
pub fn project_to_toml(ctrl: &EditorController) -> EditorResult<String> {
    toml::to_string_pretty(&to_editor_project(ctrl))
        .map_err(|e| EditorError::Msg(format!("serialize project: {e}")))
}

/// Per-custom-material side files (`<folder>/material.wgsl` + `material.toml`) —
/// the body the Studio authored. Each path is rooted at the same per-material
/// folder the `CustomMaterialRef` in `project.toml` declares (via the shared
/// [`material_folder_path`], so the writer can't drift from the ref), so the
/// directory-handle (FS Access) Save writer emits files exactly where the refs
/// point. The single-file Save downloads only `project.toml` today.
#[allow(dead_code)]
pub fn material_files(ctrl: &EditorController) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for m in ctrl.custom_materials.lock_ref().iter() {
        // Only custom-WGSL materials get a folder (wgsl + def). Built-in library
        // materials (`builtin = Some`) round-trip via each node's inline
        // MaterialDef, not a folder — emitting one would make the player try to
        // register them as custom shaders.
        if m.builtin.get_cloned().is_some() {
            continue;
        }
        let folder = material_folder_path(m.id, &m.name.get_cloned());
        out.push((format!("{folder}/material.wgsl"), m.wgsl.get_cloned()));
        // 2nd alpha-only WGSL window (masked cutouts) as a sidecar parallel to
        // material.wgsl — only when non-empty, so opaque/blend materials + the
        // common case keep clean bundles. The loader reads it back (absent →
        // no cutout). Closes the round-trip the player previously dropped.
        let alpha_wgsl = m.alpha_wgsl.get_cloned();
        if !alpha_wgsl.trim().is_empty() {
            out.push((format!("{folder}/material.alpha.wgsl"), alpha_wgsl));
        }
        // The full serde `MaterialDefinition` — the player parses this +
        // `material.wgsl` to rebuild the `MaterialRegistration`.
        let def = crate::engine::bridge::dynamic::material_definition(m);
        out.push((
            format!("{folder}/material.json"),
            serde_json::to_string_pretty(&def).unwrap_or_default(),
        ));
    }
    out
}

/// Per-captured-mesh side files (`assets/<id>.mesh.bin`) — the bitcode-encoded
/// [`CapturedMesh`] geometry for every `AssetSource::Mesh` entry whose bytes are
/// live in the [`mesh_cache`] store. Binary (not TOML), so this is the
/// `write_bytes` sibling of [`material_files`]. Closes the session-local-only
/// persistence gap: captured/editable meshes now survive Save → reload.
pub fn mesh_files(ctrl: &EditorController) -> Vec<(String, Vec<u8>)> {
    use crate::engine::bridge::mesh_cache;
    use awsm_renderer_editor_protocol::{MeshBase, MeshRef};
    let mesh_bin = |id: AssetId| -> Option<(String, Vec<u8>)> {
        let captured = mesh_cache::get_captured(id)?;
        let bytes = bitcode::serialize(&captured).ok()?;
        Some((format!("assets/{}", mesh_asset_filename(id)), bytes))
    };
    let mut out = Vec::new();
    let assets = ctrl.scene.assets.lock().unwrap();
    for (id, entry) in assets.entries.iter() {
        if let AssetSource::Mesh(def) = &entry.source {
            out.extend(mesh_bin(*id));
            // A collapsed/sculpted mesh's frozen snapshot lives under a distinct
            // id (see `captured_snapshot_id`) and is non-regenerable — save it too,
            // or post-reload editing would read empty bytes for the `Captured` base.
            if let MeshBase::Captured(MeshRef(snap)) = def.stack.base {
                if snap != *id {
                    out.extend(mesh_bin(snap));
                }
            }
        }
    }
    out
}

/// Per-imported-texture side files (`assets/<content_hash>.<ext>`) — the ENCODED
/// PNG/JPEG bytes for every `Texture(Raster)` asset whose bytes are live in the
/// [`texture_cache`](crate::engine::bridge::texture_cache) store. Closes the
/// texture half of the session-local-only persistence gap: imported textures now
/// survive Save → reload (the renderer only keeps decoded pixels). Content-hash
/// addressed so identical textures across models share one file.
pub fn texture_files(ctrl: &EditorController) -> Vec<(String, Vec<u8>)> {
    use crate::engine::bridge::texture_cache;
    let mut out = Vec::new();
    let assets = ctrl.scene.assets.lock().unwrap();
    for (id, entry) in assets.entries.iter() {
        if !matches!(
            &entry.source,
            AssetSource::Texture(TextureDef::Raster { .. })
        ) {
            continue;
        }
        // `asset_filename` is `Some` only when content_hash is set (a captured,
        // file-backed texture); procedural / un-captured textures return `None`.
        let Some(name) = asset_filename(*id, entry) else {
            continue;
        };
        if let Some((bytes, _mime)) = texture_cache::get(*id) {
            out.push((format!("assets/{name}"), bytes));
        }
    }
    out
}

/// Restore imported-texture bytes on LOAD: read each `Texture(Raster)` asset's
/// `assets/<hash>.<ext>` side file, re-seed the [`texture_cache`] (so a later
/// re-save persists it again), then decode + re-upload + re-register the GPU
/// texture via the material bridge. Called **before** [`apply_project`] so a
/// material resolves its texture slot the first time it materialises — a DECLARED
/// LOAD INPUT, not a post-hoc re-materialise. Missing files are skipped (older
/// projects / un-captured textures).
async fn restore_textures<F, Fut>(project: &EditorProject, mut read: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    use awsm_renderer_editor_protocol::TextureColorKind;
    use awsm_renderer_glb_export::ImageMime;
    let mut items: Vec<(AssetId, Vec<u8>, String, TextureColorKind)> = Vec::new();
    for (id, entry) in project.assets.entries.iter() {
        let AssetSource::Texture(TextureDef::Raster {
            display_name,
            color_kind,
        }) = &entry.source
        else {
            continue;
        };
        let Some(name) = asset_filename(*id, entry) else {
            continue;
        };
        let (mime_str, mime) = match display_name.rsplit_once('.').map(|(_, e)| e) {
            Some("png") => ("image/png", ImageMime::Png),
            Some("jpg") | Some("jpeg") => ("image/jpeg", ImageMime::Jpeg),
            _ => continue,
        };
        // The persisted semantic role IS the source of truth — its color space +
        // mipmap kind flow straight to the upload. `None` (never bound this
        // era) defaults to Albedo; the first slot bind re-materializes with the
        // slot's semantics AND writes the role back onto the asset
        // (`record_asset_color_kind`), so the next save persists it.
        let kind = color_kind.unwrap_or_default();
        if let Ok(bytes) = read(format!("assets/{name}")).await {
            crate::engine::bridge::texture_cache::store(*id, bytes.clone(), mime);
            items.push((*id, bytes, mime_str.to_string(), kind));
        }
    }
    crate::engine::bridge::material::restore_raster_textures(items).await;
}

/// Per-buffer-asset side files (`assets/<content_hash>.bin`) — the raw
/// little-endian u32 words bound to a custom-material buffer slot via
/// `set_material_buffer`, content-addressed exactly like an imported raster
/// texture. The renderer only keeps the words packed into its extras pool, so the
/// originals live session-locally in
/// [`buffer_cache`](crate::engine::bridge::buffer_cache) until Save reads them;
/// without this side file the slot binds to nothing on reload and the mesh renders
/// garbage. Mirrors [`texture_files`]. The player-bundle bake emits the same words
/// keyed by asset id (`export::emit_buffer_overrides`).
pub fn buffer_files(ctrl: &EditorController) -> Vec<(String, Vec<u8>)> {
    use crate::engine::bridge::buffer_cache;
    let mut out = Vec::new();
    let assets = ctrl.scene.assets.lock().unwrap();
    for (id, entry) in assets.entries.iter() {
        if !matches!(&entry.source, AssetSource::Buffer(_)) {
            continue;
        }
        let Some(name) = asset_filename(*id, entry) else {
            continue; // no content_hash → not addressable (shouldn't happen)
        };
        if let Some(words) = buffer_cache::get(*id) {
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            out.push((format!("assets/{name}"), bytes));
        }
    }
    out
}

/// Restore buffer-asset bytes on LOAD: for each `AssetSource::Buffer` entry in the
/// project's asset table, read its `assets/<content_hash>.bin` side file, decode
/// the little-endian u32 words, and seed the
/// [`buffer_cache`](crate::engine::bridge::buffer_cache) (so a buffer override
/// resolves by asset id the first time the mesh materializes — and a later re-save
/// persists it again). Called **before** [`apply_project`] — a DECLARED LOAD
/// INPUT, like [`restore_textures`]. Returns the labels of buffer assets whose
/// backing file couldn't be resolved (the caller surfaces these in
/// `missing_assets`); previously such a slot failed silently and the mesh rendered
/// garbage with no warning.
async fn restore_buffers<F, Fut>(project: &EditorProject, mut read: F) -> Vec<String>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    let mut missing = Vec::new();
    for (id, entry) in project.assets.entries.iter() {
        if !matches!(&entry.source, AssetSource::Buffer(_)) {
            continue;
        }
        let Some(name) = asset_filename(*id, entry) else {
            continue;
        };
        match read(format!("assets/{name}")).await {
            Ok(bytes) if bytes.len() % 4 == 0 => {
                let words: Vec<u32> = bytes
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                crate::engine::bridge::buffer_cache::store(*id, words);
            }
            Ok(_) => {
                tracing::warn!("buffer {id}: {name} length not a multiple of 4");
                missing.push(format!("material buffer ({name})"));
            }
            Err(_) => {
                tracing::warn!("buffer {id}: missing side file {name}");
                missing.push(format!("material buffer ({name})"));
            }
        }
    }
    missing
}

/// Best-effort recovery of a texture's [`TextureColorKind`] for OLD projects that
/// didn't persist it: the editor names every imported texture `"<material> · <slot>"`
/// (see `ensure_import_texture` call sites in `state.rs`), so the slot suffix
/// recovers the role. New projects store the kind on the asset and never reach this.
/// Restore captured-mesh bytes into the [`mesh_cache`] store from a loaded
/// project's asset table, reading each `assets/<id>.mesh.bin` via `read`. Called
/// **before** [`apply_project`] rebuilds the scene so `NodeKind::Mesh` nodes
/// resolve their geometry the first time they materialize. Missing files are
/// skipped (older projects, or meshes captured but never saved).
async fn restore_mesh_bytes<F, Fut>(project: &EditorProject, mut read: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    use crate::engine::bridge::mesh_cache;
    use awsm_renderer_editor_protocol::{MeshBase, MeshRef};
    for (id, entry) in project.assets.entries.iter() {
        let AssetSource::Mesh(def) = &entry.source else {
            continue;
        };
        let path = format!("assets/{}", mesh_asset_filename(*id));
        if let Ok(bytes) = read(path).await {
            match bitcode::deserialize::<CapturedMesh>(&bytes) {
                Ok(captured) => mesh_cache::store_with_id(*id, captured),
                Err(e) => tracing::warn!("mesh {id}: bad .mesh.bin ({e})"),
            }
        }
        // Restore the frozen snapshot a collapsed/sculpted mesh's `Captured` base
        // points at (a distinct id; see `captured_snapshot_id`) — non-regenerable,
        // so without it post-reload editing reads empty bytes.
        if let MeshBase::Captured(MeshRef(snap)) = def.stack.base {
            if snap != *id {
                let p = format!("assets/{}", mesh_asset_filename(snap));
                if let Ok(bytes) = read(p).await {
                    match bitcode::deserialize::<CapturedMesh>(&bytes) {
                        Ok(captured) => mesh_cache::store_with_id(snap, captured),
                        Err(e) => tracing::warn!("snapshot {snap}: bad .mesh.bin ({e})"),
                    }
                }
            }
        }
    }
}

/// Filename for an imported skinned source's clean rig glb side file
/// (`assets/<id>.rig.glb`). Sibling of [`mesh_asset_filename`]'s `.mesh.bin`.
fn rig_glb_filename(id: AssetId) -> String {
    format!("{}.rig.glb", id.0)
}

/// The set of imported-skinned-model source ids referenced by the live scene's
/// `SkinnedMesh` nodes (one rig per source, shared across its instances).
fn skinned_sources(ctrl: &EditorController) -> std::collections::HashSet<AssetId> {
    use crate::engine::scene::NodeKind;
    fn walk(node: &Arc<Node>, out: &mut std::collections::HashSet<AssetId>) {
        if let NodeKind::SkinnedMesh { skin, .. } = node.kind.get_cloned() {
            out.insert(skin.source);
        }
        for c in node.children.lock_ref().iter() {
            walk(c, out);
        }
    }
    let mut out = std::collections::HashSet::new();
    for n in ctrl.scene.nodes.lock_ref().iter() {
        walk(n, &mut out);
    }
    out
}

/// The imported-skinned-model source ids referenced by a PARSED project's
/// `SkinnedMesh` nodes (before the scene is applied) — so their templates can be
/// rebuilt from the rig glb BEFORE the nodes materialize. Mirror of
/// [`skinned_sources`] but over the serialized `EditorNode` tree.
fn skinned_sources_from_project(project: &EditorProject) -> std::collections::HashSet<AssetId> {
    use crate::engine::scene::NodeKind;
    fn walk(
        node: &awsm_renderer_editor_protocol::EditorNode,
        out: &mut std::collections::HashSet<AssetId>,
    ) {
        if let NodeKind::SkinnedMesh { skin, .. } = &node.kind {
            out.insert(skin.source);
        }
        for c in &node.children {
            walk(c, out);
        }
    }
    let mut out = std::collections::HashSet::new();
    for n in &project.nodes {
        walk(n, &mut out);
    }
    out
}

/// Rebuild every skinned source's renderer template from its persisted rig glb
/// (slice-3), reading the rig bytes via `read`. Call BEFORE `apply_project` so
/// the SkinnedMesh nodes find their template when they materialize.
async fn restore_skinned_templates<F, Fut>(project: &EditorProject, mut read: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    for src in skinned_sources_from_project(project) {
        let path = format!("assets/{}", rig_glb_filename(src));
        if let Ok(bytes) = read(path).await {
            // Store the rig glb into the cache BEFORE `apply_project` (this runs
            // pre-apply). The MATERIALISER (`node_sync::raw_mesh_from_rig` →
            // `get_rig_node_decode` → `get_rig_glb`) is the our-format decode the
            // skinned drawable is rebuilt from; it must be available the moment the
            // SkinnedMesh nodes materialise. `restore_rig_glb` (post-apply) races
            // that materialisation, so the load INPUT could be missing → the node
            // fell back to the (broken-on-reload) template path. Storing it here —
            // where the bytes are already in hand — makes the input ready before the
            // operation. (`restore_rig_glb` stays as an idempotent post-apply refill
            // for the player-bundle export path.)
            crate::engine::bridge::skinned_bake_cache::store_rig_glb(src, bytes.clone());
            if let Err(e) = crate::engine::bridge::gltf::rebuild_skinned_template(src, bytes).await
            {
                tracing::warn!("reload: rebuild skinned template {src:?}: {e}");
            }
        }
    }
}

/// Per-imported-skinned-source side files (`assets/<id>.rig.glb`) — the clean rig
/// glb (skeleton + skin + morph, built at import via `reexport_clean_scene`) for
/// every source a live `SkinnedMesh` node references. Binary, like the
/// captured-mesh `.mesh.bin`. Closes a session-local persistence gap: the rig glb
/// lives only in the `skinned_bake_cache` thread-local, so without this a cold
/// reload couldn't ship a working player bundle for a skinned model
/// (`bake_player_bundle` re-reads `get_rig_glb` per source — see `export.rs`).
pub fn rig_glb_files(ctrl: &EditorController) -> Vec<(String, Vec<u8>)> {
    use crate::engine::bridge::skinned_bake_cache;
    let mut out = Vec::new();
    for src in skinned_sources(ctrl) {
        if let Some(bytes) = skinned_bake_cache::get_rig_glb(src) {
            out.push((format!("assets/{}", rig_glb_filename(src)), bytes));
        }
    }
    out
}

/// Restore each skinned source's rig glb (`assets/<id>.rig.glb`) into the
/// [`skinned_bake_cache`] thread-local from a loaded project, reading via `read`.
/// Walks the LIVE scene (so call AFTER [`apply_project`]) — the rig is keyed by
/// `skin.source`, which the just-applied `SkinnedMesh` nodes carry. Missing files
/// are skipped (older projects, or a non-skinned project). This makes a skinned
/// model's player-bundle export survive a cold project reload.
async fn restore_rig_glb<F, Fut>(ctrl: &EditorController, mut read: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    use crate::engine::bridge::skinned_bake_cache;
    for src in skinned_sources(ctrl) {
        let path = format!("assets/{}", rig_glb_filename(src));
        if let Ok(bytes) = read(path).await {
            skinned_bake_cache::store_rig_glb(src, bytes);
        }
    }
}

/// Filename for a skinned node's bind-pose bake side file
/// (`assets/<source>.<node>.<prim>.bake.bin`). `prim = None` (whole-node merge)
/// → `all`. Sibling of the captured-mesh `.mesh.bin`.
fn bind_pose_filename(source: AssetId, node_index: u32, prim: Option<u32>) -> String {
    let p = prim
        .map(|p| p.to_string())
        .unwrap_or_else(|| "all".to_string());
    format!("{}.{}.{}.bake.bin", source.0, node_index, p)
}

/// The `(source, node_index, primitive_index)` bind-pose-bake keys every live
/// `SkinnedMesh` node carries (the same triple `skinned_bake_cache` is keyed by).
fn skinned_bake_keys(ctrl: &EditorController) -> Vec<(AssetId, u32, Option<u32>)> {
    use crate::engine::scene::NodeKind;
    fn walk(node: &Arc<Node>, out: &mut Vec<(AssetId, u32, Option<u32>)>) {
        if let NodeKind::SkinnedMesh { skin, .. } = node.kind.get_cloned() {
            out.push((skin.source, skin.node_index, skin.primitive_index));
        }
        for c in node.children.lock_ref().iter() {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    for n in ctrl.scene.nodes.lock_ref().iter() {
        walk(n, &mut out);
    }
    out
}

/// Per-skinned-node bind-pose bake side files (`assets/<source>.<node>.<prim>.bake.bin`)
/// — the no-JOINTS/WEIGHTS geometry `drop_skinning` bakes into a static editable
/// Mesh. Stored at import in the session-local `skinned_bake_cache` (MeshData),
/// serialized here as the bitcode `CapturedMesh` (reusing the `.mesh.bin` form).
/// Without this a cold reload loses the bind pose → `drop_skinning` errors.
pub fn bind_pose_files(ctrl: &EditorController) -> Vec<(String, Vec<u8>)> {
    use crate::engine::bridge::{mesh_cache, skinned_bake_cache};
    let mut out = Vec::new();
    for (src, node, prim) in skinned_bake_keys(ctrl) {
        if let Some(md) = skinned_bake_cache::get(src, node, prim) {
            let captured = mesh_cache::from_mesh_data(md);
            if let Ok(bytes) = bitcode::serialize(&captured) {
                out.push((
                    format!("assets/{}", bind_pose_filename(src, node, prim)),
                    bytes,
                ));
            }
        }
    }
    out
}

/// Restore each live `SkinnedMesh` node's bind-pose bake into the
/// [`skinned_bake_cache`] from a loaded project (via `read`). Call AFTER
/// [`apply_project`] (it walks the now-live SkinnedMesh nodes). Makes
/// `drop_skinning` survive a cold reload.
async fn restore_bind_poses<F, Fut>(ctrl: &EditorController, mut read: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    use crate::engine::bridge::{mesh_cache, skinned_bake_cache};
    for (src, node, prim) in skinned_bake_keys(ctrl) {
        let path = format!("assets/{}", bind_pose_filename(src, node, prim));
        if let Ok(bytes) = read(path).await {
            match bitcode::deserialize::<CapturedMesh>(&bytes) {
                Ok(c) => skinned_bake_cache::store(src, node, prim, mesh_cache::to_mesh_data(c)),
                Err(e) => tracing::warn!("skinned bind-pose {src:?}/{node}: bad .bake.bin ({e})"),
            }
        }
    }
}

/// Filename for a view-only cluster ("nanite") asset's baked DAG side file
/// (`<source>.clusters.bin`). The SAME name the runtime player fetches
/// (`scene-loader`'s `NodeKind::ClusterMesh` arm via `cluster_mesh_filename`), so
/// one written file serves BOTH editor reload AND the player bundle.
fn cluster_filename(source: AssetId) -> String {
    awsm_renderer_lod_bake::cluster_mesh_filename(&source.0.to_string())
}

/// The cluster ("nanite") source ids referenced by the LIVE scene's
/// `NodeKind::ClusterMesh` nodes (one baked DAG per source, shared across nodes).
fn cluster_sources(ctrl: &EditorController) -> std::collections::HashSet<AssetId> {
    use crate::engine::scene::NodeKind;
    fn walk(node: &Arc<Node>, out: &mut std::collections::HashSet<AssetId>) {
        if let NodeKind::ClusterMesh { cluster, .. } = node.kind.get_cloned() {
            out.insert(cluster.source);
        }
        for c in node.children.lock_ref().iter() {
            walk(c, out);
        }
    }
    let mut out = std::collections::HashSet::new();
    for n in ctrl.scene.nodes.lock_ref().iter() {
        walk(n, &mut out);
    }
    out
}

/// The cluster source ids referenced by a PARSED project's `ClusterMesh` nodes
/// (before the scene is applied) — so their DAGs can be re-read into the
/// `cluster_cache` BEFORE the nodes materialize. Mirror of [`cluster_sources`]
/// over the serialized `EditorNode` tree.
fn cluster_sources_from_project(project: &EditorProject) -> std::collections::HashSet<AssetId> {
    use crate::engine::scene::NodeKind;
    fn walk(
        node: &awsm_renderer_editor_protocol::EditorNode,
        out: &mut std::collections::HashSet<AssetId>,
    ) {
        if let NodeKind::ClusterMesh { cluster, .. } = &node.kind {
            out.insert(cluster.source);
        }
        for c in &node.children {
            walk(c, out);
        }
    }
    let mut out = std::collections::HashSet::new();
    for n in &project.nodes {
        walk(n, &mut out);
    }
    out
}

/// Per-cluster-source side files (`assets/<source>.clusters.bin`) — the serialized
/// `ClusterMesh` DAG for every live `ClusterMesh` node, read from the session-local
/// [`cluster_cache`](crate::engine::bridge::cluster_cache). JSON (the exact
/// `serde_json` form the `awsm-renderer-lod-bake` CLI writes + the runtime fetches), so one
/// file serves editor reload AND the player bundle. Closes the cluster half of the
/// session-local-only persistence gap: a view-only nanite import now survives
/// Save → reload (and ships in the player bundle).
pub fn cluster_files(ctrl: &EditorController) -> Vec<(String, Vec<u8>)> {
    use crate::engine::bridge::cluster_cache;
    let mut out = Vec::new();
    for src in cluster_sources(ctrl) {
        if let Some(cm) = cluster_cache::get(src) {
            if let Ok(bytes) = serde_json::to_vec(&*cm) {
                out.push((format!("assets/{}", cluster_filename(src)), bytes));
            }
        }
    }
    out
}

/// Restore each cluster source's baked DAG into the [`cluster_cache`] from a loaded
/// project (via `read`), reading `assets/<source>.clusters.bin`. Call BEFORE
/// [`apply_project`] so `ClusterMesh` nodes find their DAG the first time they
/// materialize (the bridge materializer reads `cluster_cache`) — a DECLARED LOAD
/// INPUT, not a post-hoc re-materialise. Missing files are skipped (older projects /
/// never-saved imports).
async fn restore_cluster_meshes<F, Fut>(project: &EditorProject, mut read: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    for src in cluster_sources_from_project(project) {
        let path = format!("assets/{}", cluster_filename(src));
        if let Ok(bytes) = read(path).await {
            match serde_json::from_slice::<awsm_renderer_lod_bake::ClusterMesh>(&bytes) {
                Ok(cm) => crate::engine::bridge::cluster_cache::insert(src, cm),
                Err(e) => tracing::warn!("cluster {src:?}: bad .clusters.bin ({e})"),
            }
        }
    }
}

/// Per-environment KTX2/HDR cubemap side files, at the shared
/// [`awsm_renderer_editor_protocol::env_ktx_path`] convention. The
/// `EnvironmentConfig` ids round-trip in `project.toml`, but the HDR BYTES live
/// session-only in `env_sync`'s stash — so without this an HDR skybox/IBL reverts to
/// the built-in default on reload. Mirrors `texture_files` for the env's KTX assets.
/// A stash miss is skipped here (the save census counts it — see [`save_census`]);
/// the bundle EXPORT path resolves strictly instead (`export::env_ktx_bundle_files`).
pub fn ktx_files(ctrl: &EditorController) -> Vec<(String, Vec<u8>)> {
    let env = ctrl.scene.environment.get_cloned();
    env.ktx_asset_ids()
        .into_iter()
        .filter_map(|id| {
            crate::engine::bridge::env_sync::ktx_bytes(id)
                .map(|bytes| (awsm_renderer_editor_protocol::env_ktx_path(id), bytes))
        })
        .collect()
}

/// Restore the env's KTX2 HDR bytes into `env_sync`'s stash from a loaded project
/// (via `read`, reading `assets/<id>.ktx2`). Call BEFORE [`apply_project`] sets the
/// environment, so the `env_sync` watcher resolves the skybox/IBL the first time it
/// applies — a DECLARED LOAD INPUT. Missing files are skipped (non-HDR / older).
async fn restore_ktx<F, Fut>(project: &EditorProject, mut read: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    for id in project.environment.ktx_asset_ids() {
        let path = awsm_renderer_editor_protocol::env_ktx_path(id);
        if let Ok(bytes) = read(path).await {
            crate::engine::bridge::env_sync::stash_ktx(id, bytes);
        }
    }
}

/// Per-clip animation side files — the full authored model serialized as TOML
/// (mirrors `material_files`). Each path matches the `CustomAnimationRef.file` in
/// `project.toml` via the shared [`animation_file_path`] (so the writer can't
/// drift from the ref), so the directory-handle Save writer emits files exactly
/// where the refs point.
#[allow(dead_code)]
pub fn animation_files(ctrl: &EditorController) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for c in ctrl.custom_animations.lock_ref().iter() {
        let stored = stored_from_live(c);
        if let Ok(body) = toml::to_string_pretty(&stored) {
            out.push((animation_file_path(c.id, &c.name.get_cloned()), body));
        }
    }
    out
}

/// Rebuild the live scene from a loaded project (replaces the current scene).
pub fn apply_project(ctrl: &EditorController, project: EditorProject) {
    ctrl.scene.environment.set(project.environment);
    ctrl.scene.shadows.set(project.shadows);
    ctrl.scene.post_process.set(project.post_process);
    *ctrl.scene.assets.lock().unwrap() = project.assets;
    if !project.name.is_empty() {
        ctrl.project_name.set(project.name);
    }

    // Restore the custom-material library FIRST (built-in variant defs + dynamic
    // WGSL), keyed by stable id, so the nodes that reference them resolve when
    // they materialize below. Re-arm each material's lifecycle: dynamics compile
    // (auto-register); built-ins re-sync assigned meshes on later variant edits.
    let mats: Vec<Arc<CustomMaterial>> = project
        .editor_materials
        .iter()
        .map(material_from_stored)
        .collect();
    ctrl.custom_materials
        .lock_mut()
        .replace_cloned(mats.clone());
    for m in mats {
        // Built-ins need no per-material observer: `UpdateBuiltinMaterial`
        // re-materializes assigned meshes directly now (the observer approach
        // had coverage holes — glTF-imported materials never spawned one).
        if !m.is_builtin() {
            super::spawn_auto_register(m);
        }
    }

    // Restore the animation library (the full authored model, keyed by stable id)
    // + the mixer doc. The `animation_sync` bridge re-lowers on the resulting
    // `custom_animations` change; node/material targets re-resolve as they
    // materialize (pending-skip in the bridge).
    let clips: Vec<Arc<super::animation::CustomAnimation>> = project
        .editor_animations
        .iter()
        .map(stored_to_live)
        .collect();
    ctrl.custom_animations.lock_mut().replace_cloned(clips);
    ctrl.anim_mixer.set(project.anim_mixer);
    ctrl.current_clip
        .set(ctrl.custom_animations.lock_ref().first().map(|c| c.id));
    ctrl.playhead.set_neq(0.0);
    ctrl.playing.set_neq(false);

    let new_nodes: Vec<Arc<Node>> = project
        .nodes
        .iter()
        .map(|n| node_from_spec(&NodeSpec::from_editor_node(n)))
        .collect();
    ctrl.scene.nodes.lock_mut().replace_cloned(new_nodes);

    // Re-bake any Mesh asset whose `.mesh.bin` cache wasn't restored (missing
    // side file, or a project authored without one — e.g. the tuning-scene
    // generator). Every `MeshDef` carries a `stack`, so its geometry is always
    // regenerable: evaluate it against the now-live scene (resolving Sweep curve
    // nodes / Captured refs) and store the bake so the `NodeKind::Mesh` node
    // materializes with geometry. Skips assets already in the cache (the common
    // path where `restore_mesh_bytes` loaded the saved bytes).
    {
        use crate::engine::bridge::mesh_cache;
        let defs: Vec<(AssetId, awsm_renderer_editor_protocol::MeshDef)> = {
            let assets = ctrl.scene.assets.lock().unwrap();
            assets
                .entries
                .iter()
                .filter_map(|(id, entry)| match &entry.source {
                    AssetSource::Mesh(def) if mesh_cache::get_captured(*id).is_none() => {
                        Some((*id, def.clone()))
                    }
                    _ => None,
                })
                .collect()
        };
        for (id, def) in defs {
            // Re-bake stack + overrides so a loaded project that carries authoring
            // overrides (but no saved `.mesh.bin`) reflects them.
            let baked = super::mesh_eval::evaluate_def(&ctrl.scene, &def);
            mesh_cache::store_with_id(id, mesh_cache::from_mesh_data(baked));
        }
    }

    ctrl.selected.set(Vec::new());
    ctrl.scene.bump_revision();
}

/// SAVE-COMPLETENESS INVARIANT — "a Save loses nothing, or fails loudly".
///
/// A project's `project.toml` carries everything serializable inline (scene tree,
/// material params/uniforms, animation, env, the asset *table*). But the BYTES of
/// imported geometry + textures live ONLY in session-local caches
/// ([`mesh_cache`](crate::engine::bridge::mesh_cache) /
/// [`texture_cache`](crate::engine::bridge::texture_cache)); Save streams them out
/// as `.mesh.bin` / `.png` side files. If a cache is incomplete at Save time (e.g.
/// an import's async populate hasn't settled), those side files silently go
/// missing — the reload then shows missing meshes / white materials, and re-saving
/// that broken project overwrites the good one. This check makes that condition a
/// hard, RETRYABLE error instead of silent data loss.
///
/// Only NON-regenerable assets are required: a captured/imported/edited mesh
/// ([`MeshBase::Captured`]) has no recipe — its `.mesh.bin` IS the source of
/// truth; primitive/lathe/sweep/SDF meshes are exempt (the loader re-bakes them
/// from their stack). An imported raster texture's encoded bytes are likewise the
/// only copy. Returns a human-readable summary of what's not yet persistable.
/// Census of persistable-byte completeness for the live project — the read-only
/// truth behind [`check_save_complete`] and the `SaveCensus` query (Phase 0.2 of
/// the roundtrip plan). `*_assets` = how many of that kind exist in the asset
/// table; `*_missing_cache` = how many lack their bytes in the session cache (so a
/// save would drop them); `texture_unhashed` = raster textures whose import never
/// captured a content_hash (can't be addressed/persisted at all).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SaveCensus {
    pub mesh_assets: usize,
    pub mesh_missing_cache: usize,
    pub texture_assets: usize,
    pub texture_missing_cache: usize,
    pub texture_unhashed: usize,
    pub buffer_assets: usize,
    pub buffer_missing_cache: usize,
    pub env_ktx_assets: usize,
    pub env_ktx_missing_stash: usize,
}

impl SaveCensus {
    pub fn is_complete(&self) -> bool {
        self.mesh_missing_cache == 0
            && self.texture_missing_cache == 0
            && self.texture_unhashed == 0
            && self.buffer_missing_cache == 0
            && self.env_ktx_missing_stash == 0
    }
}

/// Compute the [`SaveCensus`] for the live project (read-only; no mutation, no I/O).
pub fn save_census(ctrl: &EditorController) -> SaveCensus {
    use crate::engine::bridge::{buffer_cache, mesh_cache, texture_cache};
    use awsm_renderer_editor_protocol::{MeshBase, MeshRef};
    let assets = ctrl.scene.assets.lock().unwrap();
    let mut c = SaveCensus::default();
    for (id, entry) in assets.entries.iter() {
        match &entry.source {
            // Captured-base meshes (imported / collapsed / raw-edited) are
            // non-regenerable: their bytes must be live in the cache to persist.
            AssetSource::Mesh(def) => {
                c.mesh_assets += 1;
                if let MeshBase::Captured(MeshRef(snap)) = &def.stack.base {
                    let snap = *snap;
                    if mesh_cache::get_captured(*id).is_none()
                        || (snap != *id && mesh_cache::get_captured(snap).is_none())
                    {
                        c.mesh_missing_cache += 1;
                    }
                }
            }
            // Imported raster textures: the encoded bytes are the only copy. An
            // empty content_hash means the import never captured them (so they
            // can't be addressed/persisted at all) — also a loss.
            AssetSource::Texture(TextureDef::Raster { .. }) => {
                c.texture_assets += 1;
                if entry.content_hash.is_empty() {
                    c.texture_unhashed += 1;
                } else if texture_cache::get(*id).is_none() {
                    c.texture_missing_cache += 1;
                }
            }
            // Custom-material buffer data: the words are the only copy (the
            // renderer keeps only the packed extras-pool slice), so they must be
            // live in the cache to persist — same contract as a captured mesh.
            AssetSource::Buffer(_) => {
                c.buffer_assets += 1;
                if !buffer_cache::contains(*id) {
                    c.buffer_missing_cache += 1;
                }
            }
            _ => {}
        }
    }
    drop(assets);
    // Environment KTX cubemaps: the bytes live only in `env_sync`'s session
    // stash (written to `assets/<id>.ktx2` on save). A referenced id with no
    // stashed bytes would silently revert the env to the built-in default on
    // reload — the same loss class as a missing mesh/texture cache.
    for id in ctrl.scene.environment.get_cloned().ktx_asset_ids() {
        c.env_ktx_assets += 1;
        if !crate::engine::bridge::env_sync::has_ktx(id) {
            c.env_ktx_missing_stash += 1;
        }
    }
    c
}

pub fn check_save_complete(ctrl: &EditorController) -> Result<(), String> {
    let c = save_census(ctrl);
    // Census on every save — surfaces a 38→N drop even when the guard passes.
    tracing::info!(
        "save census: mesh_assets={} (missing_cache={}) texture_assets={} \
         (missing_cache={} unhashed={}) buffer_assets={} (missing_cache={}) \
         env_ktx_assets={} (missing_stash={})",
        c.mesh_assets,
        c.mesh_missing_cache,
        c.texture_assets,
        c.texture_missing_cache,
        c.texture_unhashed,
        c.buffer_assets,
        c.buffer_missing_cache,
        c.env_ktx_assets,
        c.env_ktx_missing_stash
    );
    if c.is_complete() {
        return Ok(());
    }
    let (missing_mesh, missing_tex, unhashed_tex, missing_buf) = (
        c.mesh_missing_cache,
        c.texture_missing_cache,
        c.texture_unhashed,
        c.buffer_missing_cache,
    );
    let mut parts = Vec::new();
    if missing_mesh > 0 {
        parts.push(format!("{missing_mesh} mesh(es)"));
    }
    if missing_tex > 0 {
        parts.push(format!("{missing_tex} texture(s)"));
    }
    if unhashed_tex > 0 {
        parts.push(format!("{unhashed_tex} texture(s) with no captured bytes"));
    }
    if missing_buf > 0 {
        parts.push(format!("{missing_buf} material buffer(s)"));
    }
    if c.env_ktx_missing_stash > 0 {
        parts.push(format!(
            "{} environment cubemap(s)",
            c.env_ktx_missing_stash
        ));
    }
    Err(format!(
        "refusing to save — {} have no persistable bytes yet (import not fully \
         settled). Nothing was written, so your existing save is untouched. Wait \
         for the import to finish (or re-import the model / re-apply the \
         environment) and Save again.",
        parts.join(", ")
    ))
}

/// Save the project to a picked directory (File System Access): writes
/// `project.toml` at the root plus each custom material's and clip's side files
/// under `assets/` — material bodies in `assets/materials/<slug>-<id>/` and clips
/// as `assets/animations/animation-<slug>-<id>.toml` (the stable id keeps
/// same-named entries from colliding), matching the ref paths recorded in
/// `project.toml`. `write_text` creates the subdirectories as it writes.
///
/// Refuses up-front (writing NOTHING) if the save would drop imported geometry /
/// textures — see [`check_save_complete`]. This is the "lose nothing" backstop: a
/// partial save must never silently clobber a good one.
pub async fn save_to_dir(ctrl: &EditorController, dir: &crate::fs::ProjectDir) -> EditorResult<()> {
    check_save_complete(ctrl).map_err(EditorError::Msg)?;
    // Gather every side file up-front (counts are known before any I/O), then write
    // with a per-phase tracing breadcrumb + a final "wrote N/N" summary. If the save
    // ever stops mid-loop (a backend hang or a future abort), the LAST breadcrumb
    // pinpoints exactly which phase/file it died on instead of leaving a silent
    // partial project — pairs with `write_bytes`'s per-file write-verify.
    let text_files: Vec<(String, String)> =
        std::iter::once(("project.toml".to_string(), project_to_toml(ctrl)?))
            .chain(material_files(ctrl))
            .chain(animation_files(ctrl))
            .collect();
    let byte_files: Vec<(String, Vec<u8>)> = mesh_files(ctrl)
        .into_iter()
        .chain(rig_glb_files(ctrl))
        .chain(bind_pose_files(ctrl))
        .chain(texture_files(ctrl))
        .chain(cluster_files(ctrl))
        .chain(ktx_files(ctrl))
        .chain(buffer_files(ctrl))
        .collect();
    let total = text_files.len() + byte_files.len();
    let mut written = 0usize;
    for (name, content) in &text_files {
        tracing::debug!("save: writing {name} ({}/{total})", written + 1);
        dir.write_text(name, content)
            .await
            .map_err(|e| EditorError::Msg(format!("save {name}: {e}")))?;
        written += 1;
    }
    for (name, bytes) in &byte_files {
        tracing::debug!("save: writing {name} ({}/{total})", written + 1);
        dir.write_bytes(name, bytes)
            .await
            .map_err(|e| EditorError::Msg(format!("save {name}: {e}")))?;
        written += 1;
    }
    tracing::info!(
        "save complete: wrote {written}/{total} files to {}",
        dir.name()
    );
    Ok(())
}

/// Load a project from a picked directory: reads `project.toml` + rebuilds the
/// live scene. Custom-material bodies (wgsl / alpha / vertex / uniforms / textures /
/// includes) ride the inline `StoredMaterial` in `project.toml` and are restored into
/// `custom_materials` + re-registered by `apply_project` (`material_from_stored` +
/// `spawn_auto_register`), so the Studio shows them on reload.
pub async fn load_from_dir(
    ctrl: &EditorController,
    dir: &crate::fs::ProjectDir,
) -> EditorResult<()> {
    let body = dir
        .read_text("project.toml")
        .await
        .map_err(|e| EditorError::Msg(e.to_string()))?;
    let project: EditorProject =
        toml::from_str(&body).map_err(|e| EditorError::Msg(format!("parse project.toml: {e}")))?;
    // Populate the mesh store before nodes materialize (see `restore_mesh_bytes`).
    restore_mesh_bytes(&project, |path| async move {
        dir.read_bytes(&path).await.map_err(|e| e.to_string())
    })
    .await;
    // Slice 3: rebuild skinned templates from the persisted rig glb BEFORE
    // apply_project, so SkinnedMesh nodes render after a cold reload.
    restore_skinned_templates(&project, |path| async move {
        dir.read_bytes(&path).await.map_err(|e| e.to_string())
    })
    .await;
    // Re-upload imported textures BEFORE the scene materialises (declared input).
    restore_textures(&project, |path| async move {
        dir.read_bytes(&path).await.map_err(|e| e.to_string())
    })
    .await;
    // Re-read view-only cluster ("nanite") DAGs into the cluster_cache BEFORE the
    // scene materialises, so ClusterMesh nodes render after a cold reload.
    restore_cluster_meshes(&project, |path| async move {
        dir.read_bytes(&path).await.map_err(|e| e.to_string())
    })
    .await;
    restore_ktx(&project, |path| async move {
        dir.read_bytes(&path).await.map_err(|e| e.to_string())
    })
    .await;
    // Rehydrate custom-material buffer assets BEFORE apply_project (their words
    // must be in the buffer cache the first time a mesh materializes).
    let missing_buffers = restore_buffers(&project, |path| async move {
        dir.read_bytes(&path).await.map_err(|e| e.to_string())
    })
    .await;
    apply_project(ctrl, project);
    ctrl.missing_assets.set(missing_buffers);
    // Restore each skinned source's rig glb AFTER apply_project (walks the
    // now-live SkinnedMesh nodes for `skin.source`), so a skinned model's
    // player-bundle export survives a cold reload.
    restore_rig_glb(ctrl, |path| async move {
        dir.read_bytes(&path).await.map_err(|e| e.to_string())
    })
    .await;
    restore_bind_poses(ctrl, |path| async move {
        dir.read_bytes(&path).await.map_err(|e| e.to_string())
    })
    .await;
    ctrl.reset_history();
    ctrl.dirty.set_neq(false);
    ctrl.env_saved_baseline
        .set(ctrl.scene.environment.get_cloned());
    Ok(())
}

/// Serialize the current project to its in-memory persisted form — exactly what
/// [`save_to_dir`] writes that [`load_from_dir`] reads back: `project.toml`
/// (materials/clips/nodes inline) + the captured-mesh `.mesh.bin` side files
/// keyed by path. The editor-path round-trip half of `ReloadProjectInMemory`;
/// call BEFORE clearing any session state, then feed the result to
/// [`apply_inmem`]. (Player-bundle side files — material.wgsl etc. — are NOT
/// needed: `apply_project` rebuilds from the inline `EditorProject`.)
pub fn serialize_inmem(
    ctrl: &EditorController,
) -> EditorResult<(String, std::collections::HashMap<String, Vec<u8>>)> {
    let toml = project_to_toml(ctrl)?;
    // Both captured-mesh `.mesh.bin` and skinned-rig `.rig.glb` side files live
    // under `assets/` with distinct names, so they share one byte map.
    let mut byte_files: std::collections::HashMap<String, Vec<u8>> =
        mesh_files(ctrl).into_iter().collect();
    byte_files.extend(rig_glb_files(ctrl));
    byte_files.extend(bind_pose_files(ctrl));
    byte_files.extend(texture_files(ctrl));
    byte_files.extend(cluster_files(ctrl));
    byte_files.extend(ktx_files(ctrl));
    byte_files.extend(buffer_files(ctrl));
    Ok((toml, byte_files))
}

/// Reload a project from its in-memory persisted form (the output of
/// [`serialize_inmem`]) through the SAME path as [`load_from_dir`]
/// (`restore_mesh_bytes` + [`apply_project`]) — but reading captured-mesh bytes
/// from the map instead of a directory. Rebuilds the editor scene tree (unlike
/// `LoadPlayerBundle`'s runtime `populate_awsm_scene` path, which leaves the tree
/// empty), so a driver can verify over MCP exactly what a project save→reload
/// preserves. The caller drops session-local caches (templates / skinned bakes /
/// skin joints) between serialize + apply to faithfully model a cold load.
pub async fn apply_inmem(
    ctrl: &EditorController,
    toml: String,
    byte_files: std::collections::HashMap<String, Vec<u8>>,
) -> EditorResult<()> {
    let project: EditorProject =
        toml::from_str(&toml).map_err(|e| EditorError::Msg(format!("parse project.toml: {e}")))?;
    restore_mesh_bytes(&project, |path| {
        let bytes = byte_files.get(&path).cloned();
        async move { bytes.ok_or_else(|| format!("missing in-memory mesh file: {path}")) }
    })
    .await;
    // Slice 3: rebuild skinned templates from the persisted rig glb BEFORE
    // apply_project, so SkinnedMesh nodes render after the round-trip reload.
    restore_skinned_templates(&project, |path| {
        let bytes = byte_files.get(&path).cloned();
        async move { bytes.ok_or_else(|| format!("missing in-memory rig glb: {path}")) }
    })
    .await;
    // Re-upload imported textures BEFORE the scene materialises, so materials bind
    // their texture slots the first time they resolve (declared load input).
    restore_textures(&project, |path| {
        let bytes = byte_files.get(&path).cloned();
        async move { bytes.ok_or_else(|| format!("missing in-memory texture: {path}")) }
    })
    .await;
    // Re-read view-only cluster ("nanite") DAGs into the cluster_cache BEFORE the
    // scene materialises (declared input), so ClusterMesh nodes render after the
    // round-trip reload.
    restore_cluster_meshes(&project, |path| {
        let bytes = byte_files.get(&path).cloned();
        async move { bytes.ok_or_else(|| format!("missing in-memory cluster DAG: {path}")) }
    })
    .await;
    restore_ktx(&project, |path| {
        let bytes = byte_files.get(&path).cloned();
        async move { bytes.ok_or_else(|| format!("missing in-memory ktx: {path}")) }
    })
    .await;
    // Rehydrate custom-material buffer assets BEFORE apply_project (their words
    // must be in the buffer cache the first time a mesh materializes).
    let missing_buffers = restore_buffers(&project, |path| {
        let bytes = byte_files.get(&path).cloned();
        async move { bytes.ok_or_else(|| format!("missing in-memory buffer: {path}")) }
    })
    .await;
    apply_project(ctrl, project);
    ctrl.missing_assets.set(missing_buffers);
    // Restore each skinned source's rig glb AFTER apply_project (walks the
    // now-live SkinnedMesh nodes), so a skinned model's player-bundle export
    // survives the round-trip.
    restore_rig_glb(ctrl, |path| {
        let bytes = byte_files.get(&path).cloned();
        async move { bytes.ok_or_else(|| format!("missing in-memory rig glb: {path}")) }
    })
    .await;
    restore_bind_poses(ctrl, |path| {
        let bytes = byte_files.get(&path).cloned();
        async move { bytes.ok_or_else(|| format!("missing in-memory bind-pose: {path}")) }
    })
    .await;
    ctrl.reset_history();
    ctrl.dirty.set_neq(false);
    ctrl.env_saved_baseline
        .set(ctrl.scene.environment.get_cloned());
    Ok(())
}

/// Fetch + parse a `project.toml` from `<base_url>/project.toml`.
pub async fn load_project_from_url(ctrl: &EditorController, base_url: &str) -> EditorResult<()> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/project.toml");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| EditorError::Msg(format!("fetch {url}: {e}")))?;
    if !resp.ok() {
        return Err(EditorError::Msg(format!(
            "fetch {url}: HTTP {}",
            resp.status()
        )));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| EditorError::Msg(format!("read {url}: {e}")))?;
    let project: EditorProject =
        toml::from_str(&body).map_err(|e| EditorError::Msg(format!("parse {url}: {e}")))?;
    // Fetch captured-mesh side files over HTTP before nodes materialize.
    restore_mesh_bytes(&project, |path| {
        let file_url = format!("{base}/{path}");
        async move {
            let resp = gloo_net::http::Request::get(&file_url)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.ok() {
                return Err(format!("HTTP {}", resp.status()));
            }
            resp.binary().await.map_err(|e| e.to_string())
        }
    })
    .await;
    // Fetch a binary side file over HTTP (rig glb / bind pose). Inlined per call
    // since each `restore_*` consumes its reader.
    macro_rules! http_bytes {
        () => {
            |path: String| {
                let file_url = format!("{base}/{path}");
                async move {
                    let resp = gloo_net::http::Request::get(&file_url)
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;
                    if !resp.ok() {
                        return Err(format!("HTTP {}", resp.status()));
                    }
                    resp.binary().await.map_err(|e| e.to_string())
                }
            }
        };
    }
    // Rebuild skinned templates BEFORE apply_project (so SkinnedMesh nodes render).
    restore_skinned_templates(&project, http_bytes!()).await;
    // Re-upload imported textures BEFORE apply_project (declared load input).
    restore_textures(&project, http_bytes!()).await;
    // Re-read cluster ("nanite") DAGs BEFORE apply_project (so ClusterMesh nodes render).
    restore_cluster_meshes(&project, http_bytes!()).await;
    restore_ktx(&project, http_bytes!()).await;
    // Rehydrate custom-material buffer assets BEFORE apply_project.
    let missing_buffers = restore_buffers(&project, http_bytes!()).await;
    apply_project(ctrl, project);
    ctrl.missing_assets.set(missing_buffers);
    // Restore rig glb (bundle export) + bind poses (drop_skinning) AFTER apply.
    restore_rig_glb(ctrl, http_bytes!()).await;
    restore_bind_poses(ctrl, http_bytes!()).await;
    Ok(())
}

/// The per-material side-file folder: a readable slug **plus the stable id**.
/// Names can collide (duplicates, or empty → `slugify` returns `"material"`), so
/// the id (a UUID) guarantees uniqueness. The ref builder and the side-file
/// writer both call this, so their paths can't drift apart.
fn material_folder_path(id: AssetId, name: &str) -> String {
    format!("assets/materials/{}-{}", slugify(name), id)
}

/// The per-clip side-file path: a readable slug **plus the stable id**, for the
/// same collision-safety reason as [`material_folder_path`].
fn animation_file_path(id: AssetId, name: &str) -> String {
    format!("assets/animations/animation-{}-{}.toml", slugify(name), id)
}

/// A filesystem-safe slug for a material name (`"Holo Grid"` → `holo-grid`).
fn slugify(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "material".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod cluster_persistence_tests {
    use super::*;
    use crate::engine::scene::{NodeId, NodeKind, Trs};
    use awsm_renderer_editor_protocol::{ClusterMeshRef, EditorNode};

    fn cluster_node(source: AssetId, children: Vec<EditorNode>) -> EditorNode {
        EditorNode {
            id: NodeId::new(),
            name: "nanite".into(),
            transform: Trs::default(),
            kind: NodeKind::ClusterMesh {
                cluster: ClusterMeshRef { source },
                material: None,
                shadow: Default::default(),
            },
            locked: false,
            visible: true,
            prefab: false,
            children,
        }
    }

    /// A project with several `ClusterMesh` nodes (incl. a nested one) yields every
    /// distinct source — so `cluster_files` writes them all on Save and
    /// `restore_cluster_meshes` re-reads them all on Load. This is the persistence
    /// contract that lets MULTIPLE nanite meshes survive Save→reload (A3); the
    /// writer/restorer both iterate exactly this set.
    #[test]
    fn cluster_sources_from_project_collects_every_mesh() {
        let a = AssetId::new();
        let b = AssetId::new();
        let project = EditorProject {
            // `b` nested under `a` also exercises the recursive walk.
            nodes: vec![cluster_node(a, vec![cluster_node(b, vec![])])],
            ..Default::default()
        };
        let sources = cluster_sources_from_project(&project);
        assert_eq!(sources.len(), 2, "every cluster source must be collected");
        assert!(sources.contains(&a) && sources.contains(&b));
    }

    /// A buffer asset is content-addressed as `<content_hash>.bin`. Save
    /// (`buffer_files`) and Load (`restore_buffers`) both derive the side-file name
    /// via the shared [`asset_filename`], so they can't drift — this pins that
    /// contract (and that a hash-less buffer entry is unaddressable → `None`).
    #[test]
    fn buffer_asset_filename_is_content_hash_bin() {
        use awsm_renderer_editor_protocol::{AssetEntry, AssetSource, BufferDef};
        let id = AssetId::new();
        let hashed = AssetEntry::new_with_hash(
            AssetSource::Buffer(BufferDef { word_len: 8 }),
            "deadbeef".to_string(),
        );
        assert_eq!(
            asset_filename(id, &hashed),
            Some("deadbeef.bin".to_string())
        );

        let unhashed = AssetEntry::new(AssetSource::Buffer(BufferDef { word_len: 8 }));
        assert_eq!(asset_filename(id, &unhashed), None);
    }
}
