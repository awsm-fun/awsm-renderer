//! Load + populate glb/gltf assets into the renderer.
//!
//! Concurrent inserts of the same `AssetId` share a single in-flight
//! load via `Shared`. Once loaded, the entry caches the `AssetTemplate`
//! (top-level transform keys + nested children/meshes) so subsequent
//! inserts can duplicate meshes via `duplicate_mesh_with_transform`
//! instead of re-parsing the gltf.

#![allow(clippy::type_complexity)]

use crate::context::{renderer_handle, with_renderer, worker_pool_handle};
use crate::fs::ProjectDir;
use crate::prelude::*;
use crate::scene::{AssetId, AssetSource, AssetStatus};
use crate::state::app_state;
use awsm_renderer::{
    meshes::MeshKey,
    textures::TextureKey,
    transforms::{Transform, TransformKey},
};
use awsm_renderer_gltf::{
    data::GltfDataHints,
    loader::{GltfFileType, GltfLoader},
    AwsmRendererGltfExt,
};
use futures::future::Shared;
use futures::FutureExt;
use std::collections::HashMap;
use std::pin::Pin;
use wasm_bindgen::JsCast;
use web_sys::{Blob, BlobPropertyBag, Url};

/// A populated glb's structure, captured after `populate_gltf`. Top-level
/// entries are those parented under `renderer.transforms.root_node`
/// immediately after populate. We never actually re-parent them — they
/// stay as a hidden template for future duplicates.
///
/// `Insert Model` walks this template and creates one editor `Node` per
/// `AssetTemplateNode`, mirroring the gltf hierarchy. Each editor `Node`
/// later instances *just its own* `mesh_keys` via
/// `duplicate_mesh_with_transform`, never recursing into children — child
/// gltf nodes are independent editor nodes that handle their own meshes.
#[derive(Clone)]
pub struct AssetTemplate {
    pub roots: Vec<AssetTemplateNode>,
}

impl AssetTemplate {
    /// Find a template node by its gltf node index (depth-first walk).
    /// Used by the bridge to look up which mesh-keys belong to a given
    /// `Model` editor node.
    pub fn find_by_node_index(&self, node_index: u32) -> Option<&AssetTemplateNode> {
        fn walk(nodes: &[AssetTemplateNode], node_index: u32) -> Option<&AssetTemplateNode> {
            for node in nodes {
                if node.gltf_node_index == node_index {
                    return Some(node);
                }
                if let Some(found) = walk(&node.children, node_index) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.roots, node_index)
    }
}

#[derive(Clone)]
pub struct AssetTemplateNode {
    /// Template transform key (parked in the renderer; not user-editable).
    #[allow(dead_code)]
    pub template_key: TransformKey,
    /// Original gltf node index. Used as the `node_index` on `ModelRef`
    /// so the editor `Node` can find its meshes in this template later.
    pub gltf_node_index: u32,
    /// gltf node label (`node.name`), if the gltf provided one.
    pub label: Option<String>,
    pub local: Transform,
    pub mesh_keys: Vec<MeshKey>,
    /// One entry per `mesh_keys[i]`: the originating glTF material
    /// index (`None` if the primitive had no material set, which glTF
    /// treats as the spec default material). Populated from
    /// `GltfKeyLookups::mesh_key_to_gltf_material_index` at template
    /// snapshot time; the editor uses it together with the gltf
    /// `AssetEntry::gltf_material_asset_ids` map to swap each
    /// duplicated mesh's material with an editable extraction.
    pub mesh_gltf_material_indices: Vec<Option<usize>>,
    pub children: Vec<AssetTemplateNode>,
}

#[derive(Clone)]
pub struct AssetEntry {
    #[allow(dead_code)]
    pub status: Mutable<AssetStatus>,
    result: Shared<Pin<Box<dyn std::future::Future<Output = Result<AssetTemplate, String>>>>>,
}

impl AssetEntry {
    pub async fn wait(&self) -> Result<AssetTemplate, String> {
        self.result.clone().await
    }
}

pub struct AssetCache {
    entries: Mutex<HashMap<AssetId, AssetEntry>>,
    /// Count of currently-loading entries. Powers the "N loading" line.
    pub loading_count: Mutable<usize>,
}

impl AssetCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            loading_count: Mutable::new(0),
        }
    }

    pub fn get_or_load(&self, asset_id: AssetId) -> AssetEntry {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get(&asset_id) {
            return entry.clone();
        }

        let status = Mutable::new(AssetStatus::Loading);
        let loading_count = self.loading_count.clone();
        loading_count.set(loading_count.get() + 1);

        let status_for_fut = status.clone();

        let fut: Pin<Box<dyn std::future::Future<Output = Result<AssetTemplate, String>>>> =
            Box::pin(async move {
                let t_start = web_sys::js_sys::Date::now();
                let result = load_and_populate(asset_id).await;
                let dt_ms = web_sys::js_sys::Date::now() - t_start;
                match &result {
                    Ok(_) => {
                        // Single, easily-greppable line so cold-boot
                        // success is visible at a glance — distinct
                        // from per-mesh tracing inside the load
                        // pipeline. Includes wall-clock so repeated
                        // loads can be eyeballed against baseline.
                        tracing::info!(
                            "[asset_cache] model loaded: asset_id={asset_id:?} ({dt_ms:.0}ms)"
                        );
                        status_for_fut.set(AssetStatus::Ready);
                    }
                    Err(err) => status_for_fut.set(AssetStatus::Failed(err.clone())),
                }
                loading_count.set(loading_count.get().saturating_sub(1));
                result
            });

        let entry = AssetEntry {
            status,
            result: fut.shared(),
        };
        entries.insert(asset_id, entry.clone());
        entry
    }

    #[allow(dead_code)]
    pub fn get(&self, asset_id: AssetId) -> Option<AssetEntry> {
        self.entries.lock().unwrap().get(&asset_id).cloned()
    }
}

/// What we actually need to fetch + parse a gltf asset. `Filename` resolves
/// against `pending_assets` first, then the project directory; `Url`
/// fetches from the network (used by build artifacts at runtime).
struct ResolvedAsset {
    filename: String,
    bytes: Vec<u8>,
}

async fn load_and_populate(asset_id: AssetId) -> Result<AssetTemplate, String> {
    let state = app_state();
    let resolved = resolve_asset(&state, asset_id).await?;
    let mime = mime_for(&resolved.filename);

    let blob = make_blob(&resolved.bytes, mime).map_err(|e| format!("blob: {e:?}"))?;
    let url = Url::create_object_url_with_blob(&blob).map_err(|e| format!("url: {e:?}"))?;

    // Worker-mode is the editor default — `maybe_build_worker_pool`
    // pre-warms the pool at `create_context` time, so by the time any
    // asset load reaches here the dispatch is a direct `pool.dispatch`
    // call (no on-demand pool-build tax on the first load). When the
    // bootstrap failed (CSP, blob-URL restriction, `?gltf-worker=off`
    // opt-out) `pool_handle.is_none()` and we transparently route
    // through the canonical inline `GltfLoader::load` path. The
    // decision is sticky for the session — see `WorkerPoolHandle`
    // doc in `context.rs`.
    let pool_handle = worker_pool_handle();
    let loader_result = if let Some(pool) = pool_handle.as_ref() {
        use awsm_renderer_gltf::worker_job::{FileTypeHint, GltfParseInput, GltfParseJob};
        let hint = detect_file_type(&resolved.filename)
            .as_ref()
            .map(FileTypeHint::from);
        let input = GltfParseInput {
            url: url.clone(),
            file_type: hint,
        };
        match pool.dispatch::<GltfParseJob>(input).await {
            Ok(out) => match out.into_loader().await {
                Ok(loader) => Ok(loader),
                Err(e) => Err(format!("gltf worker into_loader: {e}")),
            },
            Err(e) => Err(format!("gltf worker dispatch: {e}")),
        }
    } else {
        GltfLoader::load(&url, detect_file_type(&resolved.filename))
            .await
            .map_err(|e| format!("gltf load: {e}"))
    };
    let _ = Url::revoke_object_url(&url);
    let loader = loader_result?;

    // gltf loading spans are sub-frame-level detail (per-primitive
    // staging), so only emit them when the renderer is configured
    // for sub-frame timing — the cheaper `Frame` tier shouldn't
    // explode into hundreds of mesh-load marks per scene load.
    let render_timings = with_renderer(|r| r.logging.render_timings.sub_frame()).await;
    let gltf_data = loader
        .into_data(Some(
            GltfDataHints::default().with_render_timings(render_timings),
        ))
        .map_err(|e| format!("gltf decode: {e}"))?;

    // Walk the editable material assets the user already stamped onto
    // this glb (via `extract_gltf_materials_into` at insert time) and
    // pre-decode their raster textures into `ImageBitmap` via the
    // browser's native off-main-thread `createImageBitmap`. We run
    // this *in parallel with* `populate_gltf` — the two paths produce
    // independent bitmaps (renderer-gltf has its own internal decode
    // for the baked materials), so doing them concurrently halves
    // the wall-clock cost on a multi-MB textured model.
    //
    // Doing this once here (instead of per-Model-node inside
    // `instance_template`) matters because `load_and_populate` is
    // returned via `fut.shared()` — N reactive Model nodes all see
    // the same `Shared` future, so the prefetch fires exactly once
    // per glb. Hoisting it into `instance_template` made N parallel
    // `createImageBitmap` calls per texture race against each other.
    let material_asset_ids: Vec<AssetId> = {
        let assets = state.scene.assets.lock().unwrap();
        assets
            .get(asset_id)
            .map(|e| e.gltf_material_asset_ids.clone())
            .unwrap_or_default()
    };

    // Populate holds the renderer lock across its own internal awaits.
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;

    // Drive populate_gltf and the editor's raster bitmap prefetch
    // concurrently. The prefetch doesn't touch the renderer, so
    // holding the renderer lock across both `await`s is fine.
    use futures::FutureExt;
    let populate_fut = renderer
        .populate_gltf(gltf_data, None)
        .map(|r| r.map_err(|e| format!("populate_gltf: {e}")));
    let prefetch_fut =
        super::texture_cache::prefetch_raster_bitmaps_for_materials(&material_asset_ids);
    let (ctx, ()) = futures::future::join(populate_fut, prefetch_fut).await;
    let ctx = ctx?;

    // Phase 4.1 — seed the editor's texture_cache with the
    // `TextureKey`s renderer-gltf just uploaded so the override-path
    // `texture_cache::get_or_upload(asset_id, ...)` returns the
    // existing slot instead of re-decoding + re-uploading the same
    // image. The renderer-gltf side keys by `(gltf_texture_index,
    // color)`; the editor's cache keys by editor `AssetId`. We
    // resolve the join via the gltf doc's `texture → image` map and
    // the editor's per-image `gltf_image_asset_ids` (stashed at
    // insert time by `extract_gltf_materials_into`). Multiple
    // (texture_index, color) pairs can map to the same image — the
    // first one to land wins; the editor cache is keyed by AssetId
    // only (it doesn't track srgb/linear role per entry), so this
    // matches the existing semantics.
    seed_texture_cache_from_populate(&state, asset_id, &ctx);

    let root = renderer.transforms.root_node;
    let (all_keys, key_to_node_index, key_to_label): (
        Vec<TransformKey>,
        HashMap<TransformKey, u32>,
        HashMap<TransformKey, String>,
    ) = {
        let lookups = ctx.key_lookups.lock().unwrap();
        let key_to_node_index: HashMap<TransformKey, u32> = lookups
            .node_index_to_transform
            .iter()
            .map(|(idx, key)| (*key, *idx as u32))
            .collect();
        let key_to_label: HashMap<TransformKey, String> = lookups
            .node_transforms
            .iter()
            .map(|(label, key)| (*key, label.clone()))
            .collect();
        let all_keys = lookups.node_index_to_transform.values().copied().collect();
        (all_keys, key_to_node_index, key_to_label)
    };

    let top_level: Vec<TransformKey> = all_keys
        .iter()
        .copied()
        .filter(|k| renderer.transforms.get_parent(*k).ok() == Some(root))
        .collect();

    let mesh_key_to_gltf_material_index: HashMap<awsm_renderer::meshes::MeshKey, Option<usize>> = {
        let lookups = ctx.key_lookups.lock().unwrap();
        lookups.mesh_key_to_gltf_material_index.clone()
    };

    let roots: Vec<AssetTemplateNode> = top_level
        .into_iter()
        .map(|k| {
            snapshot_template(
                &renderer,
                k,
                &key_to_node_index,
                &key_to_label,
                &mesh_key_to_gltf_material_index,
            )
        })
        .collect();

    // Hide every mesh in the template so the populated originals don't
    // render as ghostly duplicates at the world origin. Future instances
    // still spawn visible meshes via `duplicate_mesh_with_transform`.
    hide_template_meshes(&mut renderer, &roots);

    Ok(AssetTemplate { roots })
}

fn seed_texture_cache_from_populate(
    state: &crate::state::AppState,
    gltf_asset_id: AssetId,
    ctx: &awsm_renderer_gltf::populate::GltfPopulateContext,
) {
    let image_asset_ids = {
        let table = state.scene.assets.lock().unwrap();
        match table.get(gltf_asset_id) {
            Some(e) if !e.gltf_image_asset_ids.is_empty() => e.gltf_image_asset_ids.clone(),
            _ => return,
        }
    };
    let textures = ctx.textures.lock().unwrap();
    // One source image can back several `GltfTextureKey`s: the key
    // carries the gltf *texture* index plus `color`, so the same image
    // referenced srgb as a base-color in one material AND linear as a
    // normal map in another (or via two distinct texture entries) lands
    // under multiple keys, each with its own renderer `TextureKey`. The
    // editor's texture cache keys purely on `AssetId` — one slot per
    // image — so a multi-variant image has no single "correct" key to
    // seed, and HashMap iteration order would otherwise pick the variant
    // arbitrarily, silently overriding the role-specific upload that
    // `texture_cache::get_or_upload` would have done.
    //
    // Resolve every renderer texture to its backing image first, then
    // seed only images that map to exactly one renderer `TextureKey`.
    // Ambiguous images fall back to the (correct, role-aware) on-demand
    // upload — losing the dedup only for the rare multi-variant case,
    // never binding the wrong color. `Some(None)` marks "ambiguous".
    let mut per_image: HashMap<AssetId, Option<TextureKey>> = HashMap::new();
    for (gltf_texture_key, texture_key) in textures.iter() {
        // gltf::Document::textures() is ordered by texture index; we
        // pull the image's index off the matching texture entry. If
        // the gltf doc was malformed or trimmed between insert and
        // populate (shouldn't happen — same bytes), skip silently.
        let Some(gltf_texture) = ctx.data.doc.textures().nth(gltf_texture_key.index) else {
            continue;
        };
        let image_index = gltf_texture.source().index();
        let Some(asset_id) = image_asset_ids.get(image_index).copied() else {
            continue;
        };
        // `AssetId::default()` is the "no extracted asset for this
        // image" sentinel from extract_gltf_materials_into (e.g.
        // URI-sourced images get skipped). Don't seed those.
        if asset_id == AssetId::default() {
            continue;
        }
        per_image
            .entry(asset_id)
            .and_modify(|slot| {
                // A second *distinct* renderer key for this image ⇒
                // ambiguous variant; clear the slot so we skip seeding.
                if *slot != Some(*texture_key) {
                    *slot = None;
                }
            })
            .or_insert(Some(*texture_key));
    }
    for (asset_id, key) in per_image {
        if let Some(texture_key) = key {
            super::texture_cache::seed(asset_id, texture_key);
        }
    }
}

fn hide_template_meshes(renderer: &mut awsm_renderer::AwsmRenderer, roots: &[AssetTemplateNode]) {
    fn walk(renderer: &mut awsm_renderer::AwsmRenderer, node: &AssetTemplateNode) {
        for mesh in &node.mesh_keys {
            let _ = renderer.set_mesh_hidden(*mesh, true);
        }
        for child in &node.children {
            walk(renderer, child);
        }
    }
    for root in roots {
        walk(renderer, root);
    }
}

fn snapshot_template(
    renderer: &awsm_renderer::AwsmRenderer,
    key: TransformKey,
    key_to_node_index: &HashMap<TransformKey, u32>,
    key_to_label: &HashMap<TransformKey, String>,
    mesh_key_to_gltf_material_index: &HashMap<awsm_renderer::meshes::MeshKey, Option<usize>>,
) -> AssetTemplateNode {
    let local = renderer
        .transforms
        .get_local(key)
        .cloned()
        .unwrap_or(Transform::IDENTITY);
    let mesh_keys: Vec<MeshKey> = renderer
        .meshes
        .keys_by_transform_key(key)
        .cloned()
        .unwrap_or_default();
    let mesh_gltf_material_indices: Vec<Option<usize>> = mesh_keys
        .iter()
        .map(|mk| {
            mesh_key_to_gltf_material_index
                .get(mk)
                .copied()
                .unwrap_or(None)
        })
        .collect();
    let children: Vec<AssetTemplateNode> = renderer
        .transforms
        .get_children(key)
        .map(|kids| {
            kids.iter()
                .map(|c| {
                    snapshot_template(
                        renderer,
                        *c,
                        key_to_node_index,
                        key_to_label,
                        mesh_key_to_gltf_material_index,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    AssetTemplateNode {
        template_key: key,
        gltf_node_index: key_to_node_index.get(&key).copied().unwrap_or(0),
        label: key_to_label.get(&key).cloned(),
        local,
        mesh_keys,
        mesh_gltf_material_indices,
        children,
    }
}

/// Resolve an `AssetId` to filename + bytes. Looks up the source on the
/// scene's asset table; for `Filename`, prefers `pending_assets` over disk;
/// for `Url`, fetches via `gloo_net`.
async fn resolve_asset(
    state: &crate::state::AppState,
    asset_id: AssetId,
) -> Result<ResolvedAsset, String> {
    let entry = state
        .scene
        .assets
        .lock()
        .unwrap()
        .get(asset_id)
        .cloned()
        .ok_or_else(|| format!("asset id {asset_id} not in the project asset table"))?;

    match &entry.source {
        AssetSource::Filename(filename) => {
            // First check the in-memory pending bytes.
            if let Some(bytes) = state.pending_assets.lock().unwrap().get(&asset_id).cloned() {
                return Ok(ResolvedAsset {
                    filename: filename.clone(),
                    bytes,
                });
            }
            // Fall back to disk via the project directory + hash-derived path.
            let dir: Option<ProjectDir> = state.project.lock().unwrap().directory.clone();
            let disk_path =
                awsm_scene_schema::asset_disk_path(asset_id, &entry).ok_or_else(|| {
                    format!(
                        "asset '{filename}' has no resolvable disk path \
                         (missing content hash on entry {asset_id})"
                    )
                })?;
            match dir {
                Some(dir) => {
                    let bytes = dir
                        .read_bytes(&disk_path)
                        .await
                        .map_err(|e| format!("read {filename}: {e}"))?;
                    Ok(ResolvedAsset {
                        filename: filename.clone(),
                        bytes,
                    })
                }
                None => Err(format!(
                    "asset '{filename}' is not in memory and no project directory is set"
                )),
            }
        }
        AssetSource::Url(url) => {
            let bytes = gloo_net::http::Request::get(url)
                .send()
                .await
                .map_err(|e| format!("fetch {url}: {e}"))?
                .binary()
                .await
                .map_err(|e| format!("fetch {url} body: {e}"))?;
            // Prefer the URL's tail for filename-based detection (mime,
            // file type). The full URL is fine as a fallback.
            let filename = url.rsplit('/').next().unwrap_or(url.as_str()).to_string();
            Ok(ResolvedAsset { filename, bytes })
        }
        AssetSource::Material(_) | AssetSource::Texture(_) | AssetSource::Mesh(_) => {
            // Non-file-backed asset sources (authored materials, procedural textures,
            // and procedural mesh placeholders) don't have bytes to fetch — the
            // renderer materializes them directly from their `*Def` parameters.
            Err(format!(
                "asset id {asset_id} is a non-file source ({:?}); call the appropriate \
                 procedural materialization path instead of `resolve`",
                entry.source
            ))
        }
    }
}

fn detect_file_type(filename: &str) -> Option<GltfFileType> {
    match filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
    {
        Some(ref s) if s == "glb" => Some(GltfFileType::Glb),
        Some(ref s) if s == "gltf" => Some(GltfFileType::Json),
        _ => None,
    }
}

fn make_blob(bytes: &[u8], mime: &str) -> Result<Blob, wasm_bindgen::JsValue> {
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&array);

    let options = BlobPropertyBag::new();
    options.set_type(mime);
    let obj = Blob::new_with_u8_array_sequence_and_options(&parts, &options)?;
    Ok(obj.dyn_into().unwrap())
}

fn mime_for(filename: &str) -> &'static str {
    match filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
    {
        Some(ref s) if s == "glb" => "model/gltf-binary",
        Some(ref s) if s == "gltf" => "model/gltf+json",
        _ => "application/octet-stream",
    }
}
