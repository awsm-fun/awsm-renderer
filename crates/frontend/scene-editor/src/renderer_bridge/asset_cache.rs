//! Load + populate glb/gltf assets into the renderer.
//!
//! Concurrent inserts of the same `AssetId` share a single in-flight
//! load via `Shared`. Once loaded, the entry caches the `AssetTemplate`
//! (top-level transform keys + nested children/meshes) so subsequent
//! inserts can duplicate meshes via `duplicate_mesh_with_transform`
//! instead of re-parsing the gltf.

#![allow(clippy::type_complexity)]

use crate::context::{renderer_handle, with_renderer};
use crate::fs::ProjectDir;
use crate::prelude::*;
use crate::scene::{AssetId, AssetSource, AssetStatus};
use crate::state::{app_state, project::asset_disk_path};
use awsm_renderer::{
    meshes::MeshKey,
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
                let result = load_and_populate(asset_id).await;
                match &result {
                    Ok(_) => status_for_fut.set(AssetStatus::Ready),
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

    let loader_result = GltfLoader::load(&url, detect_file_type(&resolved.filename))
        .await
        .map_err(|e| format!("gltf load: {e}"));
    let _ = Url::revoke_object_url(&url);
    let loader = loader_result?;

    let render_timings = with_renderer(|r| r.logging.render_timings).await;
    let gltf_data = loader
        .into_data(Some(
            GltfDataHints::default().with_render_timings(render_timings),
        ))
        .map_err(|e| format!("gltf decode: {e}"))?;

    // Populate holds the renderer lock across its own internal awaits.
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;

    let ctx = renderer
        .populate_gltf(gltf_data, None)
        .await
        .map_err(|e| format!("populate_gltf: {e}"))?;

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

    let roots: Vec<AssetTemplateNode> = top_level
        .into_iter()
        .map(|k| snapshot_template(&renderer, k, &key_to_node_index, &key_to_label))
        .collect();

    // Hide every mesh in the template so the populated originals don't
    // render as ghostly duplicates at the world origin. Future instances
    // still spawn visible meshes via `duplicate_mesh_with_transform`.
    hide_template_meshes(&mut renderer, &roots);

    Ok(AssetTemplate { roots })
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
) -> AssetTemplateNode {
    let local = renderer
        .transforms
        .get_local(key)
        .cloned()
        .unwrap_or(Transform::IDENTITY);
    let mesh_keys = renderer
        .meshes
        .keys_by_transform_key(key)
        .cloned()
        .unwrap_or_default();
    let children: Vec<AssetTemplateNode> = renderer
        .transforms
        .get_children(key)
        .map(|kids| {
            kids.iter()
                .map(|c| snapshot_template(renderer, *c, key_to_node_index, key_to_label))
                .collect()
        })
        .unwrap_or_default();
    AssetTemplateNode {
        template_key: key,
        gltf_node_index: key_to_node_index.get(&key).copied().unwrap_or(0),
        label: key_to_label.get(&key).cloned(),
        local,
        mesh_keys,
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
    let source = state
        .scene
        .assets
        .lock()
        .unwrap()
        .get(asset_id)
        .map(|e| e.source.clone())
        .ok_or_else(|| format!("asset id {asset_id} not in the project asset table"))?;

    match source {
        AssetSource::Filename(filename) => {
            // First check the in-memory pending bytes.
            if let Some(bytes) = state.pending_assets.lock().unwrap().get(&asset_id).cloned() {
                return Ok(ResolvedAsset { filename, bytes });
            }
            // Fall back to disk via the project directory.
            let dir: Option<ProjectDir> = state.project.lock().unwrap().directory.clone();
            match dir {
                Some(dir) => {
                    let disk_path = asset_disk_path(&filename);
                    let bytes = dir
                        .read_bytes(&disk_path)
                        .await
                        .map_err(|e| format!("read {filename}: {e}"))?;
                    Ok(ResolvedAsset { filename, bytes })
                }
                None => Err(format!(
                    "asset '{filename}' is not in memory and no project directory is set"
                )),
            }
        }
        AssetSource::Url(url) => {
            let bytes = gloo_net::http::Request::get(&url)
                .send()
                .await
                .map_err(|e| format!("fetch {url}: {e}"))?
                .binary()
                .await
                .map_err(|e| format!("fetch {url} body: {e}"))?;
            // Prefer the URL's tail for filename-based detection (mime,
            // file type). The full URL is fine as a fallback.
            let filename = url.rsplit('/').next().unwrap_or(&url).to_string();
            Ok(ResolvedAsset { filename, bytes })
        }
        AssetSource::Material(_) | AssetSource::Texture(_) | AssetSource::Mesh(_) => {
            // Non-file-backed asset sources (authored materials, procedural textures,
            // and procedural mesh placeholders) don't have bytes to fetch — the
            // renderer materializes them directly from their `*Def` parameters.
            Err(format!(
                "asset id {asset_id} is a non-file source ({:?}); call the appropriate \
                 procedural materialization path instead of `resolve`",
                source
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
