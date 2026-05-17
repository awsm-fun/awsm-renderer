//! Editor-side procedural-texture cache.
//!
//! Mirrors the player's `scene::texture_cache` for the editor. When a
//! `MaterialDef` references an `AssetSource::Texture(Procedural(...))`,
//! the corresponding RGBA bytes are generated via `awsm-meshgen` and
//! uploaded through `Textures::add_image_rgba_raw`; the resulting
//! `TextureKey` is cached here keyed by `AssetId` so subsequent material
//! lookups during procedural-node materialization can resolve without
//! re-uploading.
//!
//! The cache is process-global because the editor's reactive
//! materializers run deep in the async call stack (`with_renderer_mut`
//! closures); plumbing the map through every signature would balloon
//! arities for no gain. Cleared at editor startup; per-asset entries
//! are idempotent within a session.

use std::collections::HashMap;
use std::sync::Mutex;

use awsm_meshgen::{checker_rgba, gradient_rgba, noise_rgba};
use awsm_renderer::{
    textures::{SamplerCacheKey, TextureKey},
    AwsmRenderer,
};
use awsm_renderer_core::texture::{mipmap::MipmapTextureKind, texture_pool::TextureColorInfo};
use awsm_scene_schema::{AssetId, AssetSource, ProceduralTextureDef, TextureDef};

static CACHE: Mutex<Option<HashMap<AssetId, TextureKey>>> = Mutex::new(None);

fn with_cache<R>(f: impl FnOnce(&mut HashMap<AssetId, TextureKey>) -> R) -> R {
    let mut guard = CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Returns the cached `TextureKey` for a procedural texture asset, or
/// `None` if not uploaded yet.
pub fn lookup(asset_id: AssetId) -> Option<TextureKey> {
    with_cache(|m| m.get(&asset_id).copied())
}

/// How the renderer should interpret the pixel data of a raster
/// texture. Mirrors the glTF convention: `Srgb` for visible-color
/// channels (base color, emissive) — the bytes are gamma-encoded and
/// the GPU sampler decodes to linear on read — and `Linear` for data
/// channels (metallic-roughness, normal, occlusion) where the bytes
/// are already linear and any gamma round-trip would mangle the
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureColorRole {
    Srgb,
    Linear,
}

/// Upload a single texture asset on demand and cache the resulting key.
/// Returns the key (cached or newly inserted), or `None` if the asset
/// isn't a texture or its bytes aren't yet in `pending_assets` (for
/// raster textures hydrated from disk this happens during project
/// load — see `actions::project::load_inner`).
///
/// `role` only matters for first-upload of a raster texture; once
/// cached, subsequent `get_or_upload` calls return the same key
/// regardless of the role they pass. Different roles for the same
/// `AssetId` would need separate texture slots — not a real-world
/// case for now (a gltf texture is bound to a single MaterialDef
/// slot per material), and `cargo clippy` keeps the unused-arg lint
/// off this signature because callers always specify it.
pub fn get_or_upload(
    renderer: &mut AwsmRenderer,
    asset_id: AssetId,
    source: &AssetSource,
    role: TextureColorRole,
) -> Option<TextureKey> {
    if let Some(key) = lookup(asset_id) {
        return Some(key);
    }
    let texture_def = match source {
        AssetSource::Texture(t) => t,
        _ => return None,
    };

    let (rgba, width, height) = match texture_def {
        TextureDef::Procedural(proc_def) => procedural_rgba(proc_def),
        TextureDef::Raster { filename } => decode_raster_from_pending(asset_id, filename)?,
    };

    let sampler_key = renderer
        .textures
        .get_sampler_key(&renderer.gpu, SamplerCacheKey::default())
        .ok()?;
    let color = TextureColorInfo {
        mipmap_kind: MipmapTextureKind::Albedo,
        srgb_to_linear: matches!(role, TextureColorRole::Srgb),
        premultiplied_alpha: None,
    };
    let key = renderer
        .textures
        .add_image_rgba_raw(&rgba, width, height, sampler_key, color)
        .ok()?;
    with_cache(|m| {
        m.insert(asset_id, key);
    });
    Some(key)
}

/// Generate RGBA bytes for a procedural texture definition.
fn procedural_rgba(proc_def: &ProceduralTextureDef) -> (Vec<u8>, u32, u32) {
    match proc_def {
        ProceduralTextureDef::Checker {
            width,
            height,
            cells_x,
            cells_y,
            color_a,
            color_b,
        } => (
            checker_rgba(*width, *height, *cells_x, *cells_y, *color_a, *color_b),
            *width,
            *height,
        ),
        ProceduralTextureDef::Gradient {
            width,
            height,
            color_a,
            color_b,
            horizontal,
        } => (
            gradient_rgba(*width, *height, *color_a, *color_b, *horizontal),
            *width,
            *height,
        ),
        ProceduralTextureDef::Noise {
            width,
            height,
            seed,
            scale,
        } => (noise_rgba(*width, *height, *seed, *scale), *width, *height),
    }
}

/// Read raster bytes from `pending_assets` keyed by the texture's
/// `AssetId`, decode via the `image` crate, and return RGBA8 + dims.
/// Returns `None` if the bytes aren't in memory yet — the materializer
/// will silently skip the binding, matching the historical "missing
/// texture ⇒ untextured material" behaviour for procedural textures.
///
/// `_filename` is unused at runtime — the bytes are looked up by
/// AssetId — but it's part of the `TextureDef::Raster` shape and
/// useful for debug logging on decode failure.
fn decode_raster_from_pending(asset_id: AssetId, _filename: &str) -> Option<(Vec<u8>, u32, u32)> {
    let bytes = crate::state::app_state()
        .pending_assets
        .lock()
        .unwrap()
        .get(&asset_id)
        .cloned();
    let bytes = match bytes {
        Some(b) => b,
        None => {
            tracing::warn!(
                "texture_cache: raster {asset_id} ({_filename}) — no bytes in \
                 pending_assets; binding will be skipped"
            );
            return None;
        }
    };
    tracing::debug!(
        "texture_cache: decoding raster {asset_id} ({_filename}), {} bytes",
        bytes.len()
    );
    match image::load_from_memory(&bytes) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            Some((rgba.into_raw(), w, h))
        }
        Err(err) => {
            tracing::warn!("texture_cache: decode raster {asset_id} failed: {err}");
            None
        }
    }
}

/// Look up an asset's `AssetSource` in the editor scene's asset table.
pub fn asset_source(asset_id: AssetId) -> Option<AssetSource> {
    let scene = crate::state::app_state().scene.clone();
    let assets = scene.assets.lock().unwrap();
    assets.get(asset_id).map(|e| e.source.clone())
}

/// Drop the cached `TextureKey` for `asset_id` and return it (the
/// caller is responsible for freeing the corresponding pool slot via
/// `AwsmRenderer::remove_texture` once every live material has been
/// rebound — typically through [`update_existing`]).
pub fn invalidate(asset_id: AssetId) -> Option<TextureKey> {
    with_cache(|m| m.remove(&asset_id))
}

/// Drain every cached entry and return the `TextureKey`s. Called on
/// project switch (load / new) so the next project doesn't bind
/// stale entries. Pool-side cleanup is the caller's job — pair this
/// with `AwsmRenderer::remove_texture` for each returned key inside
/// a renderer lock.
pub fn drain() -> Vec<TextureKey> {
    with_cache(|m| m.drain().map(|(_, k)| k).collect())
}

/// Cascade a procedural-texture edit through every binding:
///
/// 1. Drop the cached `TextureKey` for `asset_id`.
/// 2. Inside one renderer lock, remove the old `TextureKey` from the
///    pool *before* the material cascade so the upcoming re-upload
///    recycles the freed layer slot instead of pushing a new layer
///    onto the GPU array. Then rebuild every Material asset whose
///    `MaterialDef.base_color_texture` points at `asset_id`.
/// 3. For every editor node whose *inline* material / sprite / particle
///    references this texture, invalidate the identity cache + re-emit
///    the kind so the bridge re-materializes via the standard path.
///    The async re-uploads triggered here also recycle the slot.
pub async fn update_existing(asset_id: AssetId) {
    use crate::context::with_renderer_mut;
    use crate::scene::{Node, NodeKind};
    use awsm_scene_schema::{AssetSource, MaterialRef};

    let old_key = invalidate(asset_id);
    let scene = crate::state::app_state().scene.clone();

    // Step 1: gather every Material asset whose MaterialDef references
    // this texture. We do the bookkeeping outside the renderer lock —
    // the table is a Mutex on AppState, not the renderer.
    let material_refs: Vec<MaterialRef> = {
        let table = scene.assets.lock().unwrap();
        table
            .entries
            .iter()
            .filter_map(|(mid, e)| match &e.source {
                AssetSource::Material(def)
                    if def.base_color_texture.map(|t| t.0) == Some(asset_id) =>
                {
                    Some(MaterialRef(*mid))
                }
                _ => None,
            })
            .collect()
    };

    // Step 2: free the old pool slot first, then re-resolve every
    // affected Material asset's MaterialKey. Doing the remove BEFORE
    // the material updates means the cascade's call to
    // `get_or_upload` lands its fresh upload in the recycled slot
    // (see `TexturePool::add_image`).
    if old_key.is_some() || !material_refs.is_empty() {
        let scene_for_renderer = scene.clone();
        with_renderer_mut(move |r| {
            if let Some(k) = old_key {
                r.remove_texture(k);
            }
            for mr in material_refs {
                crate::renderer_bridge::material_cache::update_existing(r, &scene_for_renderer, mr);
            }
        })
        .await;
    }

    // Step 3: every node whose inline material / sprite / particle
    // references the texture needs a full re-materialize so the renderer
    // picks up the new TextureKey. The kind value itself is unchanged,
    // so we invalidate the identity cache before re-emitting.
    let nodes = scene.nodes.lock_ref();
    let mut affected: Vec<std::sync::Arc<Node>> = Vec::new();
    fn walk(
        nodes: &[std::sync::Arc<Node>],
        asset_id: AssetId,
        out: &mut Vec<std::sync::Arc<Node>>,
    ) {
        for n in nodes.iter() {
            let needs = match &*n.kind.lock_ref() {
                NodeKind::Primitive {
                    inline_material, ..
                }
                | NodeKind::SweepAlongCurve {
                    inline_material, ..
                }
                | NodeKind::Mesh {
                    inline_material, ..
                } => inline_material.base_color_texture.map(|t| t.0) == Some(asset_id),
                NodeKind::Sprite(def) => def.texture.map(|t| t.0) == Some(asset_id),
                NodeKind::ParticleEmitter(def) => def.texture.map(|t| t.0) == Some(asset_id),
                _ => false,
            };
            if needs {
                out.push(n.clone());
            }
            let children = n.children.lock_ref();
            walk(&children, asset_id, out);
        }
    }
    walk(&nodes, asset_id, &mut affected);
    drop(nodes);
    for n in affected {
        crate::renderer_bridge::RendererNode::invalidate_apply_kind_cache(n.id);
        let k = n.kind.get_cloned();
        n.kind.set(k);
    }
}
