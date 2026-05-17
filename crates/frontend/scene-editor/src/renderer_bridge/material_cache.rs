//! Editor-side material asset cache.
//!
//! When a node references a material via `Option<MaterialRef>`, the
//! materializer looks up the corresponding `AssetSource::Material(MaterialDef)`
//! in the live asset table and asks this cache for the renderer-side
//! `MaterialKey`. First call builds + uploads the `PbrMaterial`; subsequent
//! calls hit the cache. The cache is process-global (the bridge's
//! `with_renderer_mut` closures run deep in the async stack — threading
//! the map through every signature would balloon arities for no gain).

use std::collections::HashMap;
use std::sync::Mutex;

use awsm_renderer::{materials::MaterialKey, AwsmRenderer};
use awsm_scene_schema::{AssetId, AssetSource, MaterialDef, MaterialRef};

use crate::renderer_bridge::procedural_sync::material_def_to_renderer;
use crate::scene::Scene;

static CACHE: Mutex<Option<HashMap<AssetId, MaterialKey>>> = Mutex::new(None);

fn with_cache<R>(f: impl FnOnce(&mut HashMap<AssetId, MaterialKey>) -> R) -> R {
    let mut guard = CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Resolve a `MaterialRef` to a renderer-side `MaterialKey`, uploading +
/// caching on first lookup. Returns `None` if the asset is missing from
/// the table or isn't a `Material` source.
pub fn get_or_create(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    material_ref: MaterialRef,
) -> Option<MaterialKey> {
    let asset_id = material_ref.0;
    if let Some(key) = with_cache(|m| m.get(&asset_id).copied()) {
        return Some(key);
    }
    let def: MaterialDef = {
        let assets = scene.assets.lock().unwrap();
        let entry = assets.get(asset_id)?;
        match &entry.source {
            AssetSource::Material(def) => def.clone(),
            _ => return None,
        }
    };
    let material = material_def_to_renderer(renderer, &def);
    let key = renderer.materials.insert(material, &renderer.textures);
    with_cache(|m| {
        m.insert(asset_id, key);
    });
    Some(key)
}

/// Look up the existing `MaterialKey` for a `MaterialRef` and overwrite
/// its contents from the latest authored `MaterialDef`. No-op if the
/// asset hasn't been materialized yet (the next `resolve()` will build
/// it from the new def). Used by the asset inspector so live edits on
/// a shared `MaterialDef` propagate without re-materializing every
/// referencing node.
pub fn update_existing(renderer: &mut AwsmRenderer, scene: &Scene, material_ref: MaterialRef) {
    let asset_id = material_ref.0;
    let Some(key) = with_cache(|m| m.get(&asset_id).copied()) else {
        return;
    };
    let def: MaterialDef = {
        let assets = scene.assets.lock().unwrap();
        match assets.get(asset_id).map(|e| &e.source) {
            Some(AssetSource::Material(def)) => def.clone(),
            _ => return,
        }
    };
    let new_material = material_def_to_renderer(renderer, &def);
    renderer.update_material(key, |slot| {
        *slot = new_material.clone();
    });
}

/// Drop the cached `MaterialKey` for a single asset and return it,
/// so the caller can free the renderer-side slot via
/// `AwsmRenderer::remove_material`. Used by the asset-deletion
/// cascade.
pub fn invalidate(asset_id: AssetId) -> Option<MaterialKey> {
    with_cache(|m| m.remove(&asset_id))
}

/// Drain every cached entry on project switch (load / new) and return
/// the `MaterialKey`s so the caller can free their pool slots. Stale
/// cache entries from the prior project would otherwise bind new
/// nodes that happen to recycle an `AssetId`.
pub fn drain() -> Vec<MaterialKey> {
    with_cache(|m| m.drain().map(|(_, k)| k).collect())
}

/// Cascade a `MaterialAsset` deletion through the renderer:
///
/// 1. Drop the cached `MaterialKey` (so future `get_or_create` calls
///    for the deleted id fall back to inline material via `resolve`).
/// 2. For every editor node whose `material_ref` points at this
///    asset, invalidate the apply-kind identity cache + re-emit the
///    node's kind so the bridge re-resolves through `resolve`
///    (which now falls back to inline because the table no longer
///    has the entry).
/// 3. Free the old `MaterialKey` from the renderer pool. Done last,
///    after every referencing node has re-bound, so no live draw
///    still points at the freed slot.
///
/// Mirrors the structure of `texture_cache::update_existing` for the
/// delete case.
pub async fn cascade_after_delete(asset_id: AssetId) {
    use crate::context::with_renderer_mut;
    use crate::scene::{Node, NodeKind};
    use std::sync::Arc;

    let old_key = invalidate(asset_id);
    let scene = crate::state::app_state().scene.clone();

    // Walk every node whose material_ref points at the deleted asset.
    let nodes_lock = scene.nodes.lock_ref();
    let mut affected: Vec<Arc<Node>> = Vec::new();
    fn walk(nodes: &[Arc<Node>], asset_id: AssetId, out: &mut Vec<Arc<Node>>) {
        for n in nodes.iter() {
            let matches = match &*n.kind.lock_ref() {
                NodeKind::Primitive { material, .. }
                | NodeKind::SweepAlongCurve { material, .. }
                | NodeKind::Mesh { material, .. } => material.map(|r| r.0) == Some(asset_id),
                _ => false,
            };
            if matches {
                out.push(n.clone());
            }
            let children = n.children.lock_ref();
            walk(&children, asset_id, out);
        }
    }
    walk(&nodes_lock, asset_id, &mut affected);
    drop(nodes_lock);

    for n in affected {
        crate::renderer_bridge::RendererNode::invalidate_apply_kind_cache(n.id);
        let k = n.kind.get_cloned();
        n.kind.set(k);
    }

    if let Some(k) = old_key {
        with_renderer_mut(move |r| {
            r.remove_material(k);
        })
        .await;
    }
}

/// Outcome of `resolve`. `Shared` keys are owned by the cache and survive
/// across node teardowns; `Owned` keys were freshly inserted for one node
/// and must be freed when that node's mesh goes away (otherwise the
/// materials slotmap grows unbounded as nodes are re-materialized).
pub enum ResolvedMaterial {
    Shared(MaterialKey),
    Owned(MaterialKey),
}

impl ResolvedMaterial {
    pub fn key(&self) -> MaterialKey {
        match self {
            Self::Shared(k) | Self::Owned(k) => *k,
        }
    }
}

/// Convenience: dereference an `Option<MaterialRef>`, falling back to a
/// renderer-side `MaterialKey` built from the inline `MaterialDef`.
/// The variant tells the caller whether the key is shared (cache-owned,
/// don't free) or owned (insert was fresh, free on teardown).
pub fn resolve(
    renderer: &mut AwsmRenderer,
    scene: &Scene,
    material: Option<MaterialRef>,
    inline: &MaterialDef,
) -> ResolvedMaterial {
    if let Some(r) = material {
        if let Some(key) = get_or_create(renderer, scene, r) {
            return ResolvedMaterial::Shared(key);
        }
        tracing::warn!(
            "material_cache::resolve: MaterialRef {:?} missing or not a Material asset; falling back to inline",
            r
        );
    }
    let m = material_def_to_renderer(renderer, inline);
    ResolvedMaterial::Owned(renderer.materials.insert(m, &renderer.textures))
}
