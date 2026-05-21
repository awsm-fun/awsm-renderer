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
use awsm_renderer_core::image::{bitmap as core_bitmap, ImageData};
use awsm_renderer_core::texture::{
    mipmap::MipmapTextureKind, texture_pool::TextureColorInfo, TextureFormat,
};
use awsm_scene_schema::{AssetId, AssetSource, ProceduralTextureDef, TextureDef};

static CACHE: Mutex<Option<HashMap<AssetId, TextureKey>>> = Mutex::new(None);

/// `AssetId → ImageBitmap` for raster textures. Populated by
/// [`ensure_raster_bitmap`] (which decodes asynchronously via the
/// browser's native `createImageBitmap`), drained by [`get_or_upload`]
/// when it sees a raster `TextureDef`. The bitmap path is orders of
/// magnitude faster than the pure-Rust `image::load_from_memory`
/// fallback for 4K PNGs/JPEGs: the browser decodes off the main thread,
/// the Wasm decoder runs single-threaded inside the wasm sandbox.
///
/// `ImageBitmap` is a JS handle; cloning is cheap (refcount only).
static BITMAP_CACHE: Mutex<Option<HashMap<AssetId, web_sys::ImageBitmap>>> = Mutex::new(None);

fn with_cache<R>(f: impl FnOnce(&mut HashMap<AssetId, TextureKey>) -> R) -> R {
    let mut guard = CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

fn with_bitmap_cache<R>(f: impl FnOnce(&mut HashMap<AssetId, web_sys::ImageBitmap>) -> R) -> R {
    let mut guard = BITMAP_CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Sync lookup into the pre-decoded raster bitmap cache. Returns
/// `None` if the asset hasn't been pre-decoded yet (the caller falls
/// back to the slow `image` crate path so renderers still work in
/// codepaths that don't run [`ensure_raster_bitmap`] first).
fn lookup_raster_bitmap(asset_id: AssetId) -> Option<web_sys::ImageBitmap> {
    with_bitmap_cache(|m| m.get(&asset_id).cloned())
}

/// Drop a single bitmap-cache entry. Called from
/// [`get_or_upload`]'s fast path once the bitmap has been handed off
/// to the renderer's texture pool — the pool now owns the underlying
/// `ImageBitmap`, so keeping a second reference here just wastes JS
/// heap on every model load.
fn drop_raster_bitmap(asset_id: AssetId) {
    with_bitmap_cache(|m| {
        m.remove(&asset_id);
    });
}

/// Decode the raw PNG/JPG bytes for a raster `TextureDef` asset into an
/// `ImageBitmap` via the browser's `createImageBitmap` (off-thread
/// decode), and cache it for the next sync [`get_or_upload`] call.
/// Idempotent + safe to call in parallel for distinct asset ids via
/// `futures::future::join_all`. Returns `false` if the bytes aren't in
/// `pending_assets` yet or `createImageBitmap` rejected the blob — the
/// caller's sync path will then fall through to the slow `image` crate
/// decode (or skip the binding entirely, matching the historical
/// "missing raster ⇒ untextured material" behaviour).
///
/// This is the *only* async step in the editor's raster-texture upload
/// pipeline; running it before [`instance_template`] acquires the
/// renderer lock keeps the lock held for the strict minimum (GPU
/// allocation only, no software image decode), which avoids the
/// pathological `try_lock` contention spam on the render-loop side.
pub async fn ensure_raster_bitmap(asset_id: AssetId) -> bool {
    if lookup_raster_bitmap(asset_id).is_some() {
        return true;
    }
    let bytes = crate::state::app_state()
        .pending_assets
        .lock()
        .unwrap()
        .get(&asset_id)
        .cloned();
    let Some(bytes) = bytes else {
        return false;
    };
    let mime = detect_image_mime(&bytes);
    match core_bitmap::load_u8(&bytes[..], mime, None).await {
        Ok(image) => {
            with_bitmap_cache(|m| {
                m.insert(asset_id, image);
            });
            true
        }
        Err(err) => {
            tracing::warn!(
                "texture_cache: createImageBitmap failed for {asset_id} (mime={mime}): {err}"
            );
            false
        }
    }
}

/// Sniff the leading magic bytes so we can hand `createImageBitmap` a
/// `Blob` with an accurate MIME type. Browsers fall back to internal
/// sniffing too, but a correct MIME silences the spec's "unknown
/// type" warning path on Chromium and is essentially free.
fn detect_image_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8
        && bytes[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    {
        "image/png"
    } else if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        "image/jpeg"
    } else if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        // The browser will sniff anyway; this keeps the Blob constructor happy.
        "application/octet-stream"
    }
}

/// Walk a slate of editor `MaterialAsset` ids and pre-decode every
/// raster texture they (transitively) reference into the
/// bitmap cache, in parallel. Designed to run *before* the renderer
/// lock is taken so the subsequent sync [`get_or_upload`] calls take
/// the fast bitmap path without blocking the render loop on a wasm-
/// `image`-crate decode of a 10 MB PNG.
///
/// No-op for material ids that aren't `AssetSource::Material`, and
/// silently skips texture refs whose bytes haven't arrived in
/// `pending_assets` yet — the materializer treats those as the
/// `untextured` fallback, same as before.
pub async fn prefetch_raster_bitmaps_for_materials(material_asset_ids: &[AssetId]) {
    let scene = crate::state::app_state().scene.clone();
    let mut texture_ids: indexmap::IndexSet<AssetId> = indexmap::IndexSet::new();
    {
        let assets = scene.assets.lock().unwrap();
        for mat_id in material_asset_ids.iter().copied() {
            let Some(entry) = assets.get(mat_id) else {
                continue;
            };
            let AssetSource::Material(def) = &entry.source else {
                continue;
            };
            collect_raster_texture_ids(def, &assets, &mut texture_ids);
        }
    }

    if texture_ids.is_empty() {
        return;
    }

    // Decode in parallel. `createImageBitmap` is off-main-thread on
    // every browser that ships WebGPU, so this scales with the
    // browser's image-decoder thread pool rather than serializing on
    // ours. The IndexSet dedup above means a glb whose materials all
    // share one base-color texture only pays one decode, not N.
    use futures::future::join_all;
    let futs: Vec<_> = texture_ids
        .into_iter()
        .map(|id| async move {
            let _ = ensure_raster_bitmap(id).await;
        })
        .collect();
    join_all(futs).await;
}

fn collect_raster_texture_ids(
    def: &awsm_scene_schema::MaterialDef,
    assets: &awsm_scene_schema::AssetTable,
    out: &mut indexmap::IndexSet<AssetId>,
) {
    for tex_ref in [
        def.base_color_texture,
        def.metallic_roughness_texture,
        def.normal_texture,
        def.occlusion_texture,
        def.emissive_texture,
    ]
    .into_iter()
    .flatten()
    {
        let id = tex_ref.0;
        // Only ensure raster (i.e. external PNG/JPG) — procedural defs
        // are generated synchronously by the meshgen helpers and don't
        // need a browser decode round-trip.
        if let Some(entry) = assets.get(id) {
            if matches!(
                entry.source,
                AssetSource::Texture(TextureDef::Raster { .. })
            ) {
                out.insert(id);
            }
        }
    }
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

    let sampler_key = renderer
        .textures
        .get_sampler_key(&renderer.gpu, SamplerCacheKey::default())
        .ok()?;
    let color = TextureColorInfo {
        mipmap_kind: MipmapTextureKind::Albedo,
        srgb_to_linear: matches!(role, TextureColorRole::Srgb),
        premultiplied_alpha: None,
    };

    // Three upload paths, in fastest-first order:
    //
    //  1. Raster + pre-decoded bitmap in `BITMAP_CACHE`: hand the
    //     `ImageBitmap` straight to `add_image` — no software decode,
    //     no canvas round-trip. This is the path
    //     `prefetch_raster_bitmaps_for_materials` is designed to enable.
    //  2. Procedural: synthesise RGBA via meshgen and route through
    //     `add_image_rgba_raw` (canvas-wraps the bytes into an
    //     `ImageBitmap`). The decode is "free" for the few-KB
    //     procedural sizes, so the canvas hop is the cheaper option
    //     than a second helper for the literal-bytes case.
    //  3. Raster + no pre-decoded bitmap: fall back to
    //     `image::load_from_memory` in pure Rust/wasm. Correct, but
    //     pathologically slow for ≥4K PNGs — every caller in the slow
    //     path should be queueing a `prefetch_raster_bitmaps_for_materials`
    //     pass before reaching here.
    let key = match texture_def {
        TextureDef::Raster { display_name } => {
            if let Some(image) = lookup_raster_bitmap(asset_id) {
                let image_data = ImageData::Bitmap {
                    image,
                    options: None,
                };
                let result = renderer.textures.add_image(
                    image_data,
                    TextureFormat::Rgba8unorm,
                    sampler_key,
                    color,
                );
                // Hand-off completed; the renderer's pool now owns the
                // bitmap. Drop our cache entry so future re-uploads
                // (e.g. project-load → drain) don't accidentally use a
                // stale bitmap that the pool has already mutated.
                drop_raster_bitmap(asset_id);
                result.ok()?
            } else {
                let (rgba, width, height) =
                    decode_raster_from_pending(asset_id, display_name)?;
                renderer
                    .textures
                    .add_image_rgba_raw(&rgba, width, height, sampler_key, color)
                    .ok()?
            }
        }
        TextureDef::Procedural(proc_def) => {
            let (rgba, width, height) = procedural_rgba(proc_def);
            renderer
                .textures
                .add_image_rgba_raw(&rgba, width, height, sampler_key, color)
                .ok()?
        }
    };
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
