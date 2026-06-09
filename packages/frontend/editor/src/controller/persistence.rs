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

use awsm_scene_schema::animation::CustomAnimationRef;
use awsm_scene_schema::{
    mesh_asset_filename, AssetId, AssetSource, CapturedMesh, CustomMaterialRef, EditorProject,
    StoredMaterial, StoredSlot,
};
use awsm_web_shared::prelude::Mutable;

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
    };
    StoredMaterial {
        id: m.id,
        name: m.name.get_cloned(),
        builtin: m.builtin.get_cloned(),
        wgsl: m.wgsl.get_cloned(),
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
    };
    Arc::new(CustomMaterial {
        id: s.id,
        name: Mutable::new(s.name.clone()),
        builtin: Mutable::new(s.builtin.clone()),
        wgsl: Mutable::new(s.wgsl.clone()),
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

    let custom_materials = ctrl
        .custom_materials
        .lock_ref()
        .iter()
        .map(|m| {
            let name = m.name.get_cloned();
            let folder = material_folder_path(m.id, &name);
            CustomMaterialRef {
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
        let folder = material_folder_path(m.id, &m.name.get_cloned());
        out.push((format!("{folder}/material.wgsl"), m.wgsl.get_cloned()));
        // A compact TOML sidecar of the surface + declared slots.
        let meta = format!(
            "name = \"{}\"\nalpha = \"{}\"\ndouble_sided = {}\nregistered = {}\n",
            m.name.get_cloned(),
            m.alpha.get().key(),
            m.double_sided.get(),
            m.registered.get(),
        );
        out.push((format!("{folder}/material.toml"), meta));
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
    let mut out = Vec::new();
    let assets = ctrl.scene.assets.lock().unwrap();
    for (id, entry) in assets.entries.iter() {
        if matches!(entry.source, AssetSource::Mesh(_)) {
            if let Some(captured) = mesh_cache::get_captured(*id) {
                if let Ok(bytes) = bitcode::serialize(&captured) {
                    out.push((format!("assets/{}", mesh_asset_filename(*id)), bytes));
                }
            }
        }
    }
    out
}

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
    for (id, entry) in project.assets.entries.iter() {
        if !matches!(entry.source, AssetSource::Mesh(_)) {
            continue;
        }
        let path = format!("assets/{}", mesh_asset_filename(*id));
        if let Ok(bytes) = read(path).await {
            match bitcode::deserialize::<CapturedMesh>(&bytes) {
                Ok(captured) => mesh_cache::store_with_id(*id, captured),
                Err(e) => tracing::warn!("mesh {id}: bad .mesh.bin ({e})"),
            }
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
        if m.is_builtin() {
            super::spawn_builtin_resync(m);
        } else {
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
        let defs: Vec<(AssetId, awsm_scene_schema::MeshDef)> = {
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

/// Save the project to a picked directory (File System Access): writes
/// `project.toml` at the root plus each custom material's and clip's side files
/// under `assets/` — material bodies in `assets/materials/<slug>-<id>/` and clips
/// as `assets/animations/animation-<slug>-<id>.toml` (the stable id keeps
/// same-named entries from colliding), matching the ref paths recorded in
/// `project.toml`. `write_text` creates the subdirectories as it writes.
pub async fn save_to_dir(ctrl: &EditorController, dir: &crate::fs::ProjectDir) -> EditorResult<()> {
    dir.write_text("project.toml", &project_to_toml(ctrl)?)
        .await
        .map_err(|e| EditorError::Msg(e.to_string()))?;
    for (name, content) in material_files(ctrl) {
        dir.write_text(&name, &content)
            .await
            .map_err(|e| EditorError::Msg(e.to_string()))?;
    }
    for (name, content) in animation_files(ctrl) {
        dir.write_text(&name, &content)
            .await
            .map_err(|e| EditorError::Msg(e.to_string()))?;
    }
    for (name, bytes) in mesh_files(ctrl) {
        dir.write_bytes(&name, &bytes)
            .await
            .map_err(|e| EditorError::Msg(e.to_string()))?;
    }
    Ok(())
}

/// Load a project from a picked directory: reads `project.toml` + rebuilds the
/// live scene. (Reloading custom-material bodies into the Studio is the follow-on.)
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
    apply_project(ctrl, project);
    ctrl.reset_history();
    ctrl.dirty.set_neq(false);
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
    apply_project(ctrl, project);
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
