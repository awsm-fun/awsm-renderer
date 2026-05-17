//! Save / Load / Build / Clear All.

use crate::fs::ProjectDir;
use crate::prelude::*;
use crate::scene::{AssetId, SceneSnapshot};
use crate::state::{
    app_state,
    project::{asset_disk_path, PROJECT_JSON_FILENAME},
};
use std::collections::HashSet;
use wasm_bindgen_futures::spawn_local;

pub fn save() {
    spawn_local(async move {
        match save_inner().await {
            Ok(()) => {
                crate::loading_modal::close();
                tracing::info!("action: project::save — done");
            }
            Err(err) => {
                // Modal::error replaces whichever modal is open
                // (loading or none), so no explicit close needed.
                tracing::error!("Save failed: {err}");
                Modal::error(format!("Save failed: {err}"));
            }
        }
    });
}

async fn save_inner() -> anyhow::Result<()> {
    let state = app_state();
    let dir = ensure_project_directory().await?;

    crate::loading_modal::open("Saving project", "Writing project.json…");
    let mut snapshot = state.snapshot_scene();
    // capture() leaves `name` blank — fold in the AppState project
    // name at save time so a rename through the header survives a
    // reload. Falling back to the directory name keeps existing
    // pre-rename projects' on-disk JSON unchanged.
    snapshot.name = state.project_name.get_cloned().unwrap_or_default();
    let json = serde_json::to_string_pretty(&snapshot)?;
    dir.write_text(PROJECT_JSON_FILENAME, &json).await?;

    // Flush any in-memory assets (inserted via `Insert Model` / KTX picker
    // this session) into the project directory. Only assets still
    // referenced by the scene make the trip — anything inserted then
    // undone / deleted is silently dropped.
    crate::loading_modal::set("Flushing pending assets…");
    let referenced = collect_referenced_asset_ids(&state.scene);
    let pending: Vec<(AssetId, Vec<u8>)> = {
        let mut map = state.pending_assets.lock().unwrap();
        let drained: Vec<(AssetId, Vec<u8>)> = map.drain().collect();
        // Re-insert anything we DON'T want to write so it stays cached for
        // a later save (e.g. an orphan that the user might re-reference
        // before saving).
        let mut keep_pending = Vec::new();
        for (id, bytes) in drained {
            if referenced.contains(&id) {
                keep_pending.push((id, bytes));
            }
            // Orphan: drop bytes from memory entirely.
        }
        keep_pending
    };

    let table = state.scene.assets.lock().unwrap().clone();
    for (id, bytes) in &pending {
        // Disk-path conventions:
        // - `AssetSource::Filename(name)` → `assets/<name>` (the
        //   user-picked filename round-trips into project.json).
        // - `AssetSource::Texture(TextureDef::Raster { filename })` →
        //   `assets/<filename>` (gltf-extracted images live here; the
        //   filename in the TextureDef is the stable identifier and
        //   the bytes in pending_assets are what gets written).
        // - `AssetSource::Mesh(_)` → `assets/<asset-id>.mesh.bin`
        //   (the captured-mesh side file; the AssetId is the stable
        //   filename, project.json only stores the MeshDef metadata).
        // Anything else with pending bytes is unexpected — log + skip.
        let disk_path = if let Some(filename) = table.filename(*id) {
            asset_disk_path(filename)
        } else if let Some(awsm_scene_schema::AssetSource::Texture(
            awsm_scene_schema::TextureDef::Raster { filename },
        )) = table.get(*id).map(|e| &e.source)
        {
            asset_disk_path(filename)
        } else if matches!(
            table.get(*id).map(|e| &e.source),
            Some(awsm_scene_schema::AssetSource::Mesh(_))
        ) {
            asset_disk_path(&awsm_scene_schema::mesh_asset_filename(*id))
        } else {
            tracing::warn!(
                "save: pending asset {id} has no disk filename in the asset table; \
                 skipping"
            );
            continue;
        };
        if !dir.file_exists(&disk_path).await {
            dir.write_bytes(&disk_path, bytes).await?;
        }
    }

    state.mark_clean();
    awsm_web_shared::prelude::Toast::info("Saved");
    Ok(())
}

/// Walk the live scene + environment, collect every `AssetId` that's still
/// in use. Used by Save (to filter `pending_assets`) and by
/// `cleanup_unused_assets`.
fn collect_referenced_asset_ids(scene: &crate::scene::Scene) -> HashSet<AssetId> {
    use crate::scene::{IblConfig, Node, NodeKind, SkyboxConfig};
    use awsm_scene_schema::{AssetSource, MaterialDef};

    // Pull every texture-asset reference out of a MaterialDef. Both
    // inline materials (on Primitive / Sweep / Mesh nodes) and shared
    // Material assets carry the same fields; this helper covers both.
    fn record_material_def(def: &MaterialDef, out: &mut HashSet<AssetId>) {
        for t in [
            def.base_color_texture,
            def.metallic_roughness_texture,
            def.emissive_texture,
            def.normal_texture,
            def.occlusion_texture,
        ]
        .into_iter()
        .flatten()
        {
            out.insert(t.0);
        }
    }

    fn walk(nodes: &[Arc<Node>], out: &mut HashSet<AssetId>) {
        for node in nodes {
            match &*node.kind.lock_ref() {
                NodeKind::Model(model_ref) => {
                    out.insert(model_ref.asset_id);
                }
                NodeKind::Mesh {
                    mesh,
                    material,
                    inline_material,
                } => {
                    // Captured-mesh side file — the bytes ride along on
                    // Save, and cleanup must not nuke the table entry
                    // while a node still references it.
                    out.insert(mesh.0);
                    if let Some(m) = material {
                        out.insert(m.0);
                    }
                    record_material_def(inline_material, out);
                }
                NodeKind::Primitive {
                    material,
                    inline_material,
                    ..
                }
                | NodeKind::SweepAlongCurve {
                    material,
                    inline_material,
                    ..
                } => {
                    if let Some(m) = material {
                        out.insert(m.0);
                    }
                    record_material_def(inline_material, out);
                }
                NodeKind::Sprite(def) => {
                    if let Some(t) = def.texture {
                        out.insert(t.0);
                    }
                }
                NodeKind::ParticleEmitter(def) => {
                    if let Some(t) = def.texture {
                        out.insert(t.0);
                    }
                }
                _ => {}
            }
            let children = node.children.lock_ref();
            walk(children.as_slice(), out);
        }
    }
    let mut ids = HashSet::new();
    let nodes = scene.nodes.lock_ref();
    walk(nodes.as_slice(), &mut ids);

    // Bring in indirect references. We do this in two passes because
    // both depend on the asset table itself, not the node tree:
    //
    // 1) For every in-use gltf (AssetSource::Filename referenced by a
    //    Model node above), mark every Material AssetId in its
    //    `gltf_material_asset_ids` map. These are the gltf-extracted
    //    editable materials — the link from Model → Material is
    //    indirected through the gltf's AssetEntry rather than living
    //    on the Model node, so the basic node walk doesn't see them.
    //
    // 2) For every Material asset, mark its texture refs. The Material
    //    itself was marked either by a Primitive/Sweep/Mesh node above
    //    or by step (1); the textures it embeds need to survive even
    //    though nothing else points at them directly.
    {
        let table = scene.assets.lock().unwrap();
        let gltf_link_ids: Vec<AssetId> = ids.iter().copied().collect();
        for id in gltf_link_ids {
            if let Some(entry) = table.entries.get(&id) {
                for material_id in &entry.gltf_material_asset_ids {
                    ids.insert(*material_id);
                }
            }
        }
        for entry in table.entries.values() {
            if let AssetSource::Material(def) = &entry.source {
                record_material_def(def, &mut ids);
            }
        }
    }

    let env = scene.environment.get_cloned();
    if let SkyboxConfig::Ktx { asset_id } = env.skybox {
        ids.insert(asset_id);
    }
    if let IblConfig::Ktx {
        prefiltered_asset_id,
        irradiance_asset_id,
    } = env.ibl
    {
        ids.insert(prefiltered_asset_id);
        ids.insert(irradiance_asset_id);
    }

    ids
}

/// Number of asset-table entries that aren't referenced anywhere in the
/// scene or environment. Drives the "Clean unused assets (N)" overflow
/// item + the Build-modal hint.
pub fn unused_asset_count(scene: &crate::scene::Scene) -> usize {
    let referenced = collect_referenced_asset_ids(scene);
    let table = scene.assets.lock().unwrap();
    table
        .entries
        .keys()
        .filter(|id| !referenced.contains(id))
        .count()
}

/// Drop unreferenced entries from the asset table. Returns the number of
/// entries removed. Commits a single history entry when something
/// actually changes.
pub fn cleanup_unused_assets() -> usize {
    let state = app_state();
    let scene = state.scene.clone();

    let referenced = collect_referenced_asset_ids(&scene);
    // Partition by source kind BEFORE table removal so the cascade
    // dispatch later knows which cache cleanup each id needs.
    let (texture_ids, material_ids, other_ids) = {
        let table = scene.assets.lock().unwrap();
        let mut textures = Vec::new();
        let mut materials = Vec::new();
        let mut others = Vec::new();
        for (id, entry) in &table.entries {
            if referenced.contains(id) {
                continue;
            }
            match &entry.source {
                awsm_scene_schema::AssetSource::Texture(_) => {
                    textures.push(*id);
                }
                awsm_scene_schema::AssetSource::Material(_) => {
                    materials.push(*id);
                }
                _ => others.push(*id),
            }
        }
        (textures, materials, others)
    };
    let to_remove: Vec<AssetId> = texture_ids
        .iter()
        .chain(material_ids.iter())
        .chain(other_ids.iter())
        .copied()
        .collect();
    if to_remove.is_empty() {
        return 0;
    }

    let previous = state.snapshot_scene();
    {
        let mut table = scene.assets.lock().unwrap();
        for id in &to_remove {
            table.remove(*id);
        }
    }
    // Also forget any pending bytes attached to those orphans — they're
    // never going to be referenced again.
    {
        let mut pending = state.pending_assets.lock().unwrap();
        for id in &to_remove {
            pending.remove(id);
        }
    }
    scene.bump_revision();
    state.commit_history(previous);

    // Free renderer-side resources for the removed Texture + Material
    // assets. Async, so spawned; the table mutation above already took
    // effect synchronously, so the cascade observes the post-delete
    // state when it runs.
    spawn_local(async move {
        for id in texture_ids {
            crate::renderer_bridge::texture_cache::update_existing(id).await;
        }
        for id in material_ids {
            crate::renderer_bridge::material_cache::cascade_after_delete(id).await;
        }
    });

    let removed = to_remove.len();
    tracing::info!("action: project::cleanup_unused_assets — removed {removed} entry(s)");
    removed
}

/// Bulk-delete a set of `AssetId`s from the project's asset table.
/// One history entry covers the whole batch. After deletion the
/// `selected_assets` set is cleared so the right-sidebar inspector
/// falls back to the node view. Pending bytes attached to the deleted
/// assets are dropped from memory; the on-disk side files (if any)
/// stay until the next Save runs (cleanup-on-save would handle them
/// if they're still in the assets/ dir, but a precise drop would
/// need a separate fs delete pass).
pub fn delete_asset_entries(ids: &[AssetId]) {
    if ids.is_empty() {
        return;
    }
    let state = app_state();

    // Partition by source kind before mutating the table so we know
    // which cache cleanup each id needs after the fact.
    let (texture_ids, material_ids): (Vec<AssetId>, Vec<AssetId>) = {
        let table = state.scene.assets.lock().unwrap();
        let mut textures = Vec::new();
        let mut materials = Vec::new();
        for id in ids {
            match table.entries.get(id).map(|e| &e.source) {
                Some(awsm_scene_schema::AssetSource::Texture(_)) => {
                    textures.push(*id);
                }
                Some(awsm_scene_schema::AssetSource::Material(_)) => {
                    materials.push(*id);
                }
                _ => {}
            }
        }
        (textures, materials)
    };

    let previous = state.snapshot_scene();
    {
        let mut table = state.scene.assets.lock().unwrap();
        for id in ids {
            table.remove(*id);
        }
    }
    {
        let mut pending = state.pending_assets.lock().unwrap();
        for id in ids {
            pending.remove(id);
        }
    }
    state.selected_assets.set(indexmap::IndexSet::new());
    state.scene.bump_revision();
    state.commit_history(previous);

    // Free renderer-side resources for the deleted Texture + Material
    // assets. See `cleanup_unused_assets` for the same pattern.
    spawn_local(async move {
        for id in texture_ids {
            crate::renderer_bridge::texture_cache::update_existing(id).await;
        }
        for id in material_ids {
            crate::renderer_bridge::material_cache::cascade_after_delete(id).await;
        }
    });

    tracing::info!(
        "action: project::delete_asset_entries — removed {} entry(s)",
        ids.len()
    );
    awsm_web_shared::prelude::Toast::info(format!("Deleted {} asset(s)", ids.len()));
}

pub fn load() {
    spawn_local(async move {
        match load_inner().await {
            Ok(true) => {
                crate::loading_modal::close();
                tracing::info!("action: project::load — done");
            }
            Ok(false) => {
                // User cancelled the picker — modal was never opened.
            }
            Err(err) => {
                // Modal::error replaces the loading modal.
                tracing::error!("Load failed: {err}");
                Modal::error(format!("Load failed: {err}"));
            }
        }
    });
}

async fn load_inner() -> anyhow::Result<bool> {
    let state = app_state();
    let dir = match ProjectDir::pick().await {
        Ok(dir) => dir,
        Err(crate::fs::FsError::Cancelled) => return Ok(false),
        Err(err) => return Err(err.into()),
    };

    if !dir.file_exists(PROJECT_JSON_FILENAME).await {
        anyhow::bail!(
            "No {PROJECT_JSON_FILENAME} found in the picked directory. Pick a project folder or \
             start a new project and use Save."
        );
    }

    crate::loading_modal::open("Loading project", "Reading project.json…");
    let text = dir.read_text(PROJECT_JSON_FILENAME).await?;
    let mut snapshot: SceneSnapshot = serde_json::from_str(&text)?;

    // Drop the prior project's caches before we point the scene at
    // the new project — otherwise a fresh project that recycles an
    // AssetId would silently reuse the wrong geometry / texture /
    // material. Drains the editor-side caches and frees the
    // corresponding renderer-side pool slots in one lock.
    crate::renderer_bridge::mesh_cache::clear();
    drop_renderer_caches().await;

    // Switching projects: drop anything the previous session staged but
    // never saved. Assets referenced by the loaded project live on disk.
    // Done BEFORE the gltf-material extraction below so the extracted
    // texture bytes land in a fresh pending_assets map.
    state.pending_assets.lock().unwrap().clear();

    // Auto-extract editable materials + textures from every glb/gltf
    // asset that hasn't already been processed. This is what makes
    // gltf-imported materials show up in the Assets library; we mutate
    // the snapshot rather than the live scene so apply_to picks up the
    // extracted state in one shot and the materializer reads the final
    // override map straight away (no race with reactive observers).
    let gltfs_to_extract: Vec<(awsm_scene_schema::AssetId, String)> = snapshot
        .assets
        .entries
        .iter()
        .filter_map(|(id, entry)| {
            if !entry.gltf_material_asset_ids.is_empty() {
                return None;
            }
            match &entry.source {
                awsm_scene_schema::AssetSource::Filename(name)
                    if name.ends_with(".glb") || name.ends_with(".gltf") =>
                {
                    Some((*id, name.clone()))
                }
                _ => None,
            }
        })
        .collect();
    if !gltfs_to_extract.is_empty() {
        crate::loading_modal::set("Extracting glTF materials + textures…");
    }
    for (gltf_id, filename) in gltfs_to_extract {
        let disk_path = asset_disk_path(&filename);
        match dir.read_bytes(&disk_path).await {
            Ok(bytes) => {
                let display = filename
                    .rsplit_once('.')
                    .map(|(s, _)| s.to_string())
                    .unwrap_or_else(|| filename.clone());
                crate::actions::insert::extract_gltf_materials_into(
                    &mut snapshot.assets,
                    &state.pending_assets,
                    gltf_id,
                    &display,
                    &bytes,
                );
            }
            Err(err) => {
                tracing::warn!(
                    "load: auto-extract gltf {gltf_id} ({filename}) — read bytes \
                     failed: {err}; renderer-baked materials will continue to be \
                     used for this asset"
                );
            }
        }
    }

    crate::loading_modal::set("Materializing scene…");
    crate::scene::snapshot::apply_to(&snapshot, &state.scene);
    state.scene.bump_revision();

    state.clear_selection();

    state.history.lock().unwrap().clear();
    state.refresh_history_signals();

    {
        let mut project = state.project.lock().unwrap();
        project.directory = Some(dir.clone());
        project.dirty = false;
    }

    // Pre-hydrate `pending_assets` with the bytes for every
    // `TextureDef::Raster` entry — the texture cache's sync upload
    // path reads from this map and has no async hook, so we have to
    // page the files in before any materializer runs. Silently skip
    // textures whose file isn't on disk (the inspector surfaces
    // those via the existing "missing assets" modal).
    let raster_filenames: Vec<(awsm_scene_schema::AssetId, String)> = {
        let table = state.scene.assets.lock().unwrap();
        table
            .entries
            .iter()
            .filter_map(|(id, entry)| match &entry.source {
                awsm_scene_schema::AssetSource::Texture(
                    awsm_scene_schema::TextureDef::Raster { filename },
                ) => Some((*id, filename.clone())),
                _ => None,
            })
            .collect()
    };
    if !raster_filenames.is_empty() {
        crate::loading_modal::set("Loading texture files…");
    }
    for (texture_id, filename) in raster_filenames {
        let disk_path = asset_disk_path(&filename);
        match dir.read_bytes(&disk_path).await {
            Ok(bytes) => {
                state
                    .pending_assets
                    .lock()
                    .unwrap()
                    .insert(texture_id, bytes);
            }
            Err(err) => {
                tracing::warn!(
                    "load: raster texture asset {texture_id} ({filename}) \
                     could not be read from disk: {err}"
                );
            }
        }
    }
    // Prefer the project's stored name if the user has renamed it
    // through the header; fall back to the directory name otherwise.
    let display_name = if snapshot.name.is_empty() {
        dir.name()
    } else {
        snapshot.name.clone()
    };
    state.project_name.set(Some(display_name));
    state.mark_clean();

    // Hold the loading modal up until the bridge has actually
    // instantiated every Model node on the GPU — otherwise the
    // modal closes while the scene is still half-empty and the
    // user watches geometry pop in piece by piece.
    crate::loading_modal::set("Materializing on GPU…");
    let roots: Vec<Arc<crate::scene::Node>> =
        state.scene.nodes.lock_ref().iter().cloned().collect();
    crate::loading_modal::wait_for_models_ready(&roots).await;

    // Warn if any model references point at missing files.
    let missing = collect_missing_assets(&state.scene, &dir).await;
    if !missing.is_empty() {
        tracing::warn!(
            "Loaded project, but {} asset file(s) are missing: {:?}",
            missing.len(),
            missing
        );
        Modal::error(format!(
            "Loaded {PROJECT_JSON_FILENAME}, but {} asset file(s) are missing:\n\n{}\n\n\
             Nodes referencing these files will still appear in the tree.",
            missing.len(),
            missing.join("\n")
        ));
    }

    Ok(true)
}

async fn collect_missing_assets(scene: &crate::scene::Scene, dir: &ProjectDir) -> Vec<String> {
    let referenced = collect_referenced_asset_ids(scene);
    let table = scene.assets.lock().unwrap().clone();

    let mut filenames: Vec<String> = referenced
        .iter()
        .filter_map(|id| table.filename(*id).map(|f| f.to_string()))
        .collect();
    filenames.sort();
    filenames.dedup();

    let mut missing = Vec::new();
    for filename in filenames {
        let disk_path = asset_disk_path(&filename);
        if !dir.file_exists(&disk_path).await {
            missing.push(filename);
        }
    }
    missing
}

/// Reset the editor to an empty, project-less state. If there are unsaved
/// changes, prompts the user first; otherwise proceeds immediately.
pub fn new_project() {
    let state = app_state();
    if state.dirty.get() {
        open_new_project_confirm();
    } else {
        spawn_local(reset_to_empty());
    }
}

/// Drain the editor-side texture + material caches and free the
/// corresponding renderer-side pool slots. Called on project switch
/// (load / new) so the next project starts from a clean pool — stale
/// cached keys binding new entries would either silently reuse the
/// wrong texture or grow the pool unbounded across reloads.
async fn drop_renderer_caches() {
    let texture_keys = crate::renderer_bridge::texture_cache::drain();
    let material_keys = crate::renderer_bridge::material_cache::drain();
    if texture_keys.is_empty() && material_keys.is_empty() {
        return;
    }
    crate::context::with_renderer_mut(move |r| {
        for k in texture_keys {
            r.remove_texture(k);
        }
        for k in material_keys {
            r.remove_material(k);
        }
    })
    .await;
}

fn open_new_project_confirm() {
    Modal::open(|| {
        html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "0.85rem")
            .style("color", ColorText::SidebarHeader.value())
            .style("min-width", "360px")
            .child(html!("h2", { .style("margin", "0") .text("Start a new project?") }))
            .child(html!("div", {
                .style("font-size", "0.9rem")
                .style("line-height", "1.4")
                .text("You have unsaved changes. Starting a new project will discard them.")
            }))
            .child(html!("div", {
                .style("display", "flex")
                .style("justify-content", "flex-end")
                .style("gap", "0.5rem")
                .child(Button::new()
                    .with_text("Cancel")
                    .with_style(ButtonStyle::Outline)
                    .with_on_click(Modal::close)
                    .render())
                .child(Button::new()
                    .with_text("Discard and Start New")
                    .with_color(ButtonColor::Red)
                    .with_on_click(|| {
                        Modal::close();
                        spawn_local(reset_to_empty());
                    })
                    .render())
            }))
        })
    });
}

async fn reset_to_empty() {
    let state = app_state();
    state.scene.nodes.lock_mut().clear();
    state
        .scene
        .environment
        .set(crate::scene::EnvironmentConfig::default());
    *state.scene.assets.lock().unwrap() = crate::scene::AssetTable::new();
    state.scene.bump_revision();
    state.clear_selection();
    state.history.lock().unwrap().clear();
    state.refresh_history_signals();
    state.pending_assets.lock().unwrap().clear();
    crate::renderer_bridge::mesh_cache::clear();
    drop_renderer_caches().await;
    {
        let mut p = state.project.lock().unwrap();
        p.directory = None;
        p.dirty = false;
    }
    state.project_name.set(None);
    state.mark_clean();
    tracing::info!("action: project::new_project — reset to empty");
}

/// Update the project's display name. Trimmed; empty input falls
/// back to the project directory's on-disk name on the next reload.
/// Marks the project dirty so Save persists the change. Not undoable
/// — name lives outside the scene snapshot ring on purpose.
pub fn rename(new_name: String) {
    let state = app_state();
    let trimmed = new_name.trim();
    let current = state.project_name.get_cloned().unwrap_or_default();
    if trimmed == current {
        return;
    }
    state.project_name.set(Some(trimmed.to_string()));
    state.mark_dirty();
    tracing::info!("action: project::rename — {trimmed:?}");
}

pub fn clear_all() {
    let state = app_state();
    if state.scene.is_empty() {
        return;
    }
    let previous = state.snapshot_scene();
    state.scene.nodes.lock_mut().clear();
    state.clear_selection();
    state.scene.bump_revision();
    state.commit_history(previous);
    tracing::info!("action: project::clear_all — done");
}

/// Return the current project directory, prompting the user if none has
/// been chosen yet. The picker may cancel, in which case this returns an
/// error.
pub async fn ensure_project_directory() -> Result<ProjectDir, crate::fs::FsError> {
    let state = app_state();

    // Fast path: already picked.
    if let Some(dir) = state.project.lock().unwrap().directory.clone() {
        return Ok(dir);
    }

    let dir = ProjectDir::pick().await?;
    {
        let mut project = state.project.lock().unwrap();
        project.directory = Some(dir.clone());
    }
    state.project_name.set(Some(dir.name()));
    Ok(dir)
}
