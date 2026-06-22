//! `MaterialDef` → renderer `Material` conversion. Dispatches on the authored
//! shading model (PBR / Unlit / Toon) so a built-in material's *variant* (its
//! shader-generation choice) actually renders. Texture binding resolution is the
//! follow-on; factors/alpha/double-sided/vertex-colors are wired here.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_editor_protocol::{
    AssetSource, MaterialDef, MaterialShading, ProceduralTextureDef, TextureDef, TextureRef,
};
use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey, MaterialTexture};
use awsm_renderer::textures::{SamplerCacheKey, SamplerKey, TextureKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::sampler::{AddressMode, FilterMode, MipmapFilterMode};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_core::texture::texture_pool::TextureColorInfo;

use crate::engine::scene::AssetId;

// The pure `MaterialDef` → renderer `Material` conversion is shared with the
// runtime-bundle loader (`populate_awsm_scene`) so the editor's live render and
// the player lower a material identically — the precondition for a meaningful
// round-trip. Re-exported at the old paths so `material::material_to_renderer`
// (and the thumbnail renderer's `bmat::material_to_renderer`) keep resolving.
pub(crate) use awsm_scene_loader::material::{
    alpha_mode_of, material_to_pbr, material_to_renderer,
};

thread_local! {
    /// Maps a texture-asset id → its uploaded renderer `TextureKey`, so a texture
    /// is generated + uploaded once and reused across every mesh/material that
    /// binds it (and across re-materializations). Session-scoped — a full page
    /// reload rebuilds it (acceptable; project reset within a session is rare).
    static TEXTURE_KEYS: RefCell<HashMap<AssetId, TextureKey>> = RefCell::new(HashMap::new());
}

/// Resolve a texture ref → `(pooled texture key, sampler key)` for a dynamic
/// material slot, uploading a procedural texture once / reusing a
/// pre-registered key and pooling its sampler so the binding is valid. Returns
/// the sampler too so the dynamic packer can encode the slot's `uv_and_sampler`
/// word (see `DynamicMaterialContext::resolve_texture_index`). Used for
/// per-mesh texture overrides on dynamic materials.
pub(crate) fn resolve_texture_binding(
    r: &mut AwsmRenderer,
    tref: &TextureRef,
) -> Option<(TextureKey, SamplerKey)> {
    let mt = resolve_texture(r, tref, true, MipmapTextureKind::Albedo)?;
    Some((mt.key, mt.sampler_key?))
}

/// The renderer [`TextureKey`] a texture asset resolves to, if it's been
/// materialized/registered this session. Used by the image-query seam to read a
/// raster/file texture back from the GPU.
pub(crate) fn texture_key_for(asset_id: AssetId) -> Option<TextureKey> {
    TEXTURE_KEYS.with(|c| c.borrow().get(&asset_id).copied())
}

/// Pre-register a texture-asset id against a renderer [`TextureKey`] that's
/// already uploaded (e.g. one `populate_gltf` baked for an imported model), so
/// `resolve_texture` returns it on the cache-hit path instead of re-decoding.
/// Used by glTF import to wire extracted materials to their original textures.
pub(crate) fn register_texture_key(asset_id: AssetId, key: TextureKey) {
    TEXTURE_KEYS.with(|c| c.borrow_mut().insert(asset_id, key));
}

/// Fetch + decode an image URL to RGBA8 bytes (via the browser's
/// `createImageBitmap` + a 2D canvas readback). Cross-origin URLs need CORS
/// headers (same constraint as glTF image loads).
async fn fetch_rgba(url: &str) -> Result<(Vec<u8>, u32, u32), String> {
    let bitmap = awsm_renderer_core::image::bitmap::load(url.to_string(), None)
        .await
        .map_err(|e| format!("load image: {e}"))?;
    bitmap_to_rgba(bitmap)
}

/// Decode ENCODED image bytes (PNG/JPEG) to RGBA8 via the browser, for restoring
/// persisted textures on load. `mime` is the source mime (`image/png` etc.).
async fn decode_rgba_from_bytes(bytes: &[u8], mime: &str) -> Result<(Vec<u8>, u32, u32), String> {
    let bitmap = awsm_renderer_core::image::bitmap::load_u8(bytes, mime, None)
        .await
        .map_err(|e| format!("decode image: {e}"))?;
    bitmap_to_rgba(bitmap)
}

/// Decode an agent texture **payload string** (a `data:` URI or bare base64
/// PNG/JPEG, or a raw-RGBA descriptor) to RGBA8 + dims (§18 equirect upload).
/// Mirrors the `CreateTexture` decode but with no caller-supplied dims/format.
pub(crate) async fn decode_rgba_from_payload(data: &str) -> Result<(Vec<u8>, u32, u32), String> {
    use awsm_editor_protocol::TexturePayload;
    let payload = awsm_editor_protocol::decode_texture_payload(data, None, None, None)
        .map_err(|e| e.to_string())?;
    match payload {
        TexturePayload::RawRgba8 {
            bytes,
            width,
            height,
        } => Ok((bytes, width, height)),
        TexturePayload::Encoded { bytes, mime } => {
            decode_rgba_from_bytes(&bytes, mime.as_deref().unwrap_or("image/png")).await
        }
    }
}

/// Read an `ImageBitmap` back to RGBA8 bytes via a 2D canvas (shared by the
/// URL-fetch + the encoded-bytes decode paths).
fn bitmap_to_rgba(bitmap: web_sys::ImageBitmap) -> Result<(Vec<u8>, u32, u32), String> {
    use wasm_bindgen::JsCast;
    let (w, h) = (bitmap.width().max(1), bitmap.height().max(1));
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let canvas: web_sys::HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(|_| "create canvas")?
        .dyn_into()
        .map_err(|_| "canvas cast")?;
    canvas.set_width(w);
    canvas.set_height(h);
    let ctx: web_sys::CanvasRenderingContext2d = canvas
        .get_context("2d")
        .map_err(|_| "get 2d context")?
        .ok_or("no 2d context")?
        .dyn_into()
        .map_err(|_| "2d context cast")?;
    ctx.draw_image_with_image_bitmap(&bitmap, 0.0, 0.0)
        .map_err(|_| "drawImage")?;
    let image_data = ctx
        .get_image_data(0, 0, w as i32, h as i32)
        .map_err(|_| "getImageData (image cross-origin without CORS?)")?;
    Ok((image_data.data().to_vec(), w, h))
}

/// Import a raster texture from a URL: fetch + decode, upload to the GPU texture
/// pool, and register the asset id against the resulting [`TextureKey`] so it
/// resolves for material binding + `screenshot_texture`. The caller creates the
/// `TextureDef::Raster` asset entry.
pub(crate) async fn import_texture_url(id: AssetId, url: &str) -> Result<(), String> {
    // Fetch/decode WITHOUT holding the renderer lock (network wait).
    let (rgba, w, h) = fetch_rgba(url).await?;
    let handle = crate::engine::context::renderer_handle();
    let mut r = handle.lock().await;
    let sampler_key = sampler_for(&mut r, None).ok_or("sampler")?;
    let color = TextureColorInfo {
        mipmap_kind: MipmapTextureKind::Albedo,
        srgb_to_linear: true,
        premultiplied_alpha: None,
    };
    let key = r
        .textures
        .add_image_rgba_raw(&rgba, w, h, sampler_key, color)
        .map_err(|e| format!("upload: {e}"))?;
    // Live texture add: a pool grow invalidates the opaque/classify/edge
    // pipeline shaders (they bake in `texture_pool_arrays_len`), so route
    // through the one compile path — `commit_load` finalizes the pool AND
    // recompiles against it (the render preamble no longer does).
    r.commit_load(crate::engine::activity::commit_phase_handler())
        .await
        .map_err(|e| format!("commit_load: {e}"))?;
    register_texture_key(id, key);
    Ok(())
}

/// Create a texture from **agent-authored** data (the ★ raw-upload primitive):
/// raw RGBA8 pixels, or an encoded PNG/JPEG/WebP the browser decodes — uploaded
/// to the GPU texture pool + registered under `id` so it resolves for material
/// binding + `screenshot_texture`. Mirrors [`import_texture_url`], but the bytes
/// come from the agent (not a URL). `linear` uploads as linear data
/// (normal/roughness/height maps) instead of sRGB albedo. The caller creates the
/// `TextureDef::Raster` asset entry.
pub(crate) async fn create_texture(
    id: AssetId,
    payload: awsm_editor_protocol::TexturePayload,
    linear: bool,
) -> Result<(), String> {
    use awsm_editor_protocol::TexturePayload;
    // Decode WITHOUT holding the renderer lock (the encoded path is async).
    let (rgba, w, h) = match payload {
        TexturePayload::RawRgba8 {
            bytes,
            width,
            height,
        } => (bytes, width, height),
        TexturePayload::Encoded { bytes, mime } => {
            decode_rgba_from_bytes(&bytes, mime.as_deref().unwrap_or("image/png")).await?
        }
    };
    let handle = crate::engine::context::renderer_handle();
    let mut r = handle.lock().await;
    let sampler_key = sampler_for(&mut r, None).ok_or("sampler")?;
    let color = TextureColorInfo {
        mipmap_kind: MipmapTextureKind::Albedo,
        // sRGB albedo by default; `linear` opts data/normal maps out of the
        // sRGB→linear conversion so their values are sampled verbatim.
        srgb_to_linear: !linear,
        premultiplied_alpha: None,
    };
    let key = r
        .textures
        .add_image_rgba_raw(&rgba, w, h, sampler_key, color)
        .map_err(|e| format!("upload: {e}"))?;
    // Live texture add: a pool grow invalidates the texture-array-len-baked
    // shaders, so route through `commit_load` (finalize pool + recompile once).
    r.commit_load(crate::engine::activity::commit_phase_handler())
        .await
        .map_err(|e| format!("commit_load: {e}"))?;
    register_texture_key(id, key);
    Ok(())
}

/// Restore persisted raster textures on LOAD: decode each `(asset id, encoded
/// bytes, mime)` and re-upload it to the GPU, registering the asset id against
/// the new [`TextureKey`] so materials resolve their texture slots. This is a
/// DECLARED LOAD INPUT — call it BEFORE the scene's materials/geometry
/// materialise, so the slot is bound the first time a material resolves (NOT a
/// post-hoc re-materialise). Decodes happen WITHOUT the renderer lock; all
/// uploads + the single pool-finalising `commit_load` happen under one lock in
/// ONE batch (not per-texture — transaction-aligned).
///
/// Color space: re-uploaded as sRGB albedo (the visible base-color case is
/// correct). The per-slot linear-vs-sRGB classification isn't persisted on the
/// texture asset yet, so normal/metallic-roughness/occlusion maps restore in the
/// albedo color space — a follow-up (store the `TextureColorInfo` kind per
/// `TextureDef::Raster`).
pub(crate) async fn restore_raster_textures(items: Vec<(AssetId, Vec<u8>, String)>) {
    if items.is_empty() {
        return;
    }
    // Decode all (async, network/codec) BEFORE taking the renderer lock.
    let mut decoded: Vec<(AssetId, Vec<u8>, u32, u32)> = Vec::with_capacity(items.len());
    for (id, bytes, mime) in &items {
        match decode_rgba_from_bytes(bytes, mime).await {
            Ok((rgba, w, h)) => decoded.push((*id, rgba, w, h)),
            Err(e) => tracing::warn!("restore texture {id}: {e}"),
        }
    }
    if decoded.is_empty() {
        return;
    }
    let handle = crate::engine::context::renderer_handle();
    let mut r = handle.lock().await;
    let Some(sampler_key) = sampler_for(&mut r, None) else {
        tracing::warn!("restore textures: no sampler");
        return;
    };
    let color = TextureColorInfo {
        mipmap_kind: MipmapTextureKind::Albedo,
        srgb_to_linear: true,
        premultiplied_alpha: None,
    };
    for (id, rgba, w, h) in &decoded {
        match r
            .textures
            .add_image_rgba_raw(rgba, *w, *h, sampler_key, color)
        {
            Ok(key) => register_texture_key(*id, key),
            Err(e) => tracing::warn!("restore texture {id}: upload {e}"),
        }
    }
    // ONE pool-finalising commit for the whole batch (the grow invalidates the
    // texture-array-len-baked shaders; commit recompiles once).
    if let Err(e) = r
        .commit_load(crate::engine::activity::commit_phase_handler())
        .await
    {
        tracing::warn!("restore textures commit_load: {e}");
    }
}

/// The "missing material" colour: flat, unlit magenta. A mesh with **no** assigned
/// material renders this (the classic engine sentinel) — it is deliberately NOT a
/// real material with editable settings (see the material model note in
/// `inspector.rs::material_editor`).
pub fn insert_magenta(renderer: &mut AwsmRenderer) -> MaterialKey {
    let mut m = UnlitMaterial::new(MaterialAlphaMode::Opaque, false);
    m.base_color_factor = [1.0, 0.0, 1.0, 1.0];
    renderer.materials.insert(
        Material::Unlit(m),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    )
}

/// Wrap an authored `MaterialDef` into the renderer's `Material` enum + insert
/// it, returning the `MaterialKey`. For the PBR path this also resolves the
/// material's texture refs (procedural → uploaded once, cached) and binds them —
/// each bound slot flips a `PbrFeatures` bit, so a textured material specializes
/// to its own shader. The caller commits the uploads via `finalize_gpu_textures`.
pub fn insert_material(renderer: &mut AwsmRenderer, def: &MaterialDef) -> MaterialKey {
    insert_material_vc(renderer, def, None)
}

/// Like [`insert_material`], but binds vertex colours to a specific geometry
/// COLOR set (glTF `COLOR_n`). The editor's model bridge passes the set index it
/// detected from the mesh geometry so `COLOR_1+` meshes sample the right set
/// rather than always set 0.
pub fn insert_material_vc(
    renderer: &mut AwsmRenderer,
    def: &MaterialDef,
    vertex_color_set: Option<u32>,
) -> MaterialKey {
    let material = match def.shading {
        MaterialShading::Pbr => {
            let alpha_mode = alpha_mode_of(def);
            let mut pbr = material_to_pbr(def, alpha_mode, vertex_color_set);
            apply_textures(renderer, &mut pbr, def);
            apply_extension_textures(renderer, &mut pbr, def);
            Material::Pbr(Box::new(pbr))
        }
        MaterialShading::FlipBook { .. } => {
            // The atlas rides the BASE-COLOR texture slot (sRGB albedo).
            let mut material = material_to_renderer(def);
            if let Material::FlipBook(m) = &mut material {
                if let Some(t) = &def.base_color_texture {
                    m.atlas_tex = resolve_texture(renderer, t, true, MipmapTextureKind::Albedo);
                }
            }
            material
        }
        // Unlit / Toon don't carry texture slots in the editor yet.
        _ => material_to_renderer(def),
    };
    renderer.materials.insert(
        material,
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    )
}

/// Resolve + bind each enabled standard PBR texture slot onto `pbr`.
fn apply_textures(r: &mut AwsmRenderer, pbr: &mut PbrMaterial, def: &MaterialDef) {
    if let Some(t) = &def.base_color_texture {
        pbr.base_color_tex = resolve_texture(r, t, true, MipmapTextureKind::Albedo);
    }
    if let Some(t) = &def.metallic_roughness_texture {
        pbr.metallic_roughness_tex =
            resolve_texture(r, t, false, MipmapTextureKind::MetallicRoughness);
    }
    if let Some(t) = &def.normal_texture {
        pbr.normal_tex = resolve_texture(r, t, false, MipmapTextureKind::Normal);
    }
    if let Some(t) = &def.occlusion_texture {
        pbr.occlusion_tex = resolve_texture(r, t, false, MipmapTextureKind::Occlusion);
    }
    if let Some(t) = &def.emissive_texture {
        pbr.emissive_tex = resolve_texture(r, t, true, MipmapTextureKind::Emissive);
    }
}

/// Resolve an optional texture ref (no-op when `None`).
fn resolve_opt(
    r: &mut AwsmRenderer,
    t: &Option<TextureRef>,
    srgb: bool,
    kind: MipmapTextureKind,
) -> Option<MaterialTexture> {
    t.as_ref().and_then(|t| resolve_texture(r, t, srgb, kind))
}

/// Resolve + bind each enabled KHR extension's texture slots onto the renderer's
/// already-populated `PbrMaterial` extension structs. Mirrors `apply_textures`
/// but for the extension maps (clearcoat normal map, specular colour map, …).
fn apply_extension_textures(r: &mut AwsmRenderer, pbr: &mut PbrMaterial, def: &MaterialDef) {
    use MipmapTextureKind as K;
    let ext = &def.extensions;
    if let (Some(e), Some(p)) = (ext.specular.as_ref(), pbr.specular.as_mut()) {
        p.tex = resolve_opt(r, &e.tex, false, K::MetallicRoughness);
        p.color_tex = resolve_opt(r, &e.color_tex, true, K::Albedo);
    }
    if let (Some(e), Some(p)) = (ext.transmission.as_ref(), pbr.transmission.as_mut()) {
        p.tex = resolve_opt(r, &e.tex, false, K::MetallicRoughness);
    }
    if let (Some(e), Some(p)) = (
        ext.diffuse_transmission.as_ref(),
        pbr.diffuse_transmission.as_mut(),
    ) {
        p.tex = resolve_opt(r, &e.tex, false, K::MetallicRoughness);
        p.color_tex = resolve_opt(r, &e.color_tex, true, K::Albedo);
    }
    if let (Some(e), Some(p)) = (ext.volume.as_ref(), pbr.volume.as_mut()) {
        p.thickness_tex = resolve_opt(r, &e.thickness_tex, false, K::MetallicRoughness);
    }
    if let (Some(e), Some(p)) = (ext.clearcoat.as_ref(), pbr.clearcoat.as_mut()) {
        p.tex = resolve_opt(r, &e.tex, false, K::MetallicRoughness);
        p.roughness_tex = resolve_opt(r, &e.roughness_tex, false, K::MetallicRoughness);
        p.normal_tex = resolve_opt(r, &e.normal_tex, false, K::Normal);
    }
    if let (Some(e), Some(p)) = (ext.sheen.as_ref(), pbr.sheen.as_mut()) {
        p.color_tex = resolve_opt(r, &e.color_tex, true, K::Albedo);
        p.roughness_tex = resolve_opt(r, &e.roughness_tex, false, K::MetallicRoughness);
    }
    if let (Some(e), Some(p)) = (ext.anisotropy.as_ref(), pbr.anisotropy.as_mut()) {
        p.tex = resolve_opt(r, &e.tex, false, K::Normal);
    }
    if let (Some(e), Some(p)) = (ext.iridescence.as_ref(), pbr.iridescence.as_mut()) {
        p.tex = resolve_opt(r, &e.tex, false, K::MetallicRoughness);
        p.thickness_tex = resolve_opt(r, &e.thickness_tex, false, K::MetallicRoughness);
    }
}

/// Resolve a texture ref → a renderer `MaterialTexture`, uploading the procedural
/// image once (cached by asset id). Raster/file textures are deferred (need the
/// import pipeline) — those refs resolve to `None` (slot stays empty).
fn resolve_texture(
    r: &mut AwsmRenderer,
    tref: &TextureRef,
    srgb: bool,
    kind: MipmapTextureKind,
) -> Option<MaterialTexture> {
    let asset_id = tref.asset;
    let sampler_key = sampler_for(r, tref.sampler)?;
    // The sampler must be in the texture pool's sampler set *before* the material
    // is packed — `Materials::insert` immediately writes the material's uniform
    // buffer, and a sampler that isn't pooled makes `sampler_index` return None,
    // which encodes the slot as "no texture" (and is never re-packed after a
    // later finalize). The procedural branch below pools it implicitly via
    // `add_image`; the cache-hit / reused-key path (e.g. glTF textures baked by
    // populate) does NOT, so pool it explicitly here. `finalize_gpu_textures`
    // (which callers run after) then rebuilds the bind group for the new sampler.
    r.textures.ensure_sampler_in_pool(sampler_key);
    // Honor the binding's UV set + KHR_texture_transform (both non-recompiling).
    let uv_index = Some(tref.uv_index);
    // A transform key is needed if the binding has a transform OR a UV flow (the
    // flow drives the transform's offset each frame — B3). Flow-only bindings get
    // an identity transform to scroll.
    let transform_key = if tref.transform.is_some() || tref.flow.is_some() {
        let (offset, rotation, scale) = match tref.transform {
            Some(t) => (t.offset, t.rotation, t.scale),
            None => ([0.0, 0.0], 0.0, [1.0, 1.0]),
        };
        let key = r
            .textures
            .insert_texture_transform(&awsm_renderer::textures::TextureTransform {
                offset,
                origin: [0.0, 0.0],
                rotation,
                scale,
            });
        if let Some(velocity) = tref.flow {
            r.textures.set_texture_flow(key, offset, velocity);
        }
        Some(key)
    } else {
        None
    };
    let mk = |key: TextureKey| MaterialTexture {
        key,
        sampler_key: Some(sampler_key),
        uv_index,
        transform_key,
    };
    if let Some(key) = TEXTURE_KEYS.with(|c| c.borrow().get(&asset_id).copied()) {
        return Some(mk(key));
    }
    // Look up the texture asset; only procedural textures are materializable today.
    let proc = {
        let ctrl = crate::controller::controller();
        let assets = ctrl.scene.assets.lock().unwrap();
        match assets.entries.get(&asset_id).map(|e| &e.source) {
            Some(AssetSource::Texture(TextureDef::Procedural(p))) => Some(p.clone()),
            _ => None,
        }
    }?;
    let (rgba, w, h) = procedural_rgba(&proc);
    let color = TextureColorInfo {
        mipmap_kind: kind,
        srgb_to_linear: srgb,
        premultiplied_alpha: None,
    };
    let key = r
        .textures
        .add_image_rgba_raw(&rgba, w, h, sampler_key, color)
        .ok()?;
    TEXTURE_KEYS.with(|c| c.borrow_mut().insert(asset_id, key));
    Some(mk(key))
}

/// Pool (or fetch) the sampler for a texture binding from its [`TextureSampler`]
/// settings — wrap modes + filtering imported from the glTF sampler. `None`
/// defaults to glTF's repeat + linear.
fn sampler_for(
    r: &mut AwsmRenderer,
    sampler: Option<awsm_editor_protocol::TextureSampler>,
) -> Option<SamplerKey> {
    use awsm_editor_protocol::{TextureFilter, TextureWrap};
    let s = sampler.unwrap_or_default();
    let addr = |w: TextureWrap| match w {
        TextureWrap::Repeat => AddressMode::Repeat,
        TextureWrap::ClampToEdge => AddressMode::ClampToEdge,
        TextureWrap::MirroredRepeat => AddressMode::MirrorRepeat,
    };
    let filt = |f: TextureFilter| match f {
        TextureFilter::Linear => FilterMode::Linear,
        TextureFilter::Nearest => FilterMode::Nearest,
    };
    let mip = |f: TextureFilter| match f {
        TextureFilter::Linear => MipmapFilterMode::Linear,
        TextureFilter::Nearest => MipmapFilterMode::Nearest,
    };
    let key = SamplerCacheKey {
        address_mode_u: Some(addr(s.wrap_u)),
        address_mode_v: Some(addr(s.wrap_v)),
        address_mode_w: Some(AddressMode::Repeat),
        mag_filter: Some(filt(s.mag_filter)),
        min_filter: Some(filt(s.min_filter)),
        mipmap_filter: Some(mip(s.mipmap_filter)),
        ..Default::default()
    };
    r.textures.get_sampler_key(&r.gpu, key).ok()
}

/// Generate RGBA8 bytes for a procedural texture def (delegates to meshgen).
pub(crate) fn procedural_rgba(p: &ProceduralTextureDef) -> (Vec<u8>, u32, u32) {
    use awsm_meshgen::procedural_texture::{checker_rgba, gradient_rgba, noise_rgba};
    match *p {
        ProceduralTextureDef::Checker {
            width,
            height,
            cells_x,
            cells_y,
            color_a,
            color_b,
        } => (
            checker_rgba(width, height, cells_x, cells_y, color_a, color_b),
            width,
            height,
        ),
        ProceduralTextureDef::Gradient {
            width,
            height,
            color_a,
            color_b,
            horizontal,
        } => (
            gradient_rgba(width, height, color_a, color_b, horizontal),
            width,
            height,
        ),
        ProceduralTextureDef::Noise {
            width,
            height,
            seed,
            scale,
        } => (noise_rgba(width, height, seed, scale), width, height),
    }
}
