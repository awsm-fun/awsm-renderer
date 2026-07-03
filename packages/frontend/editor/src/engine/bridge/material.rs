//! `MaterialDef` → renderer `Material` conversion. Dispatches on the authored
//! shading model (PBR / Unlit / Toon) so a built-in material's *variant* (its
//! shader-generation choice) actually renders. Texture binding resolution is the
//! follow-on; factors/alpha/double-sided/vertex-colors are wired here.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey, MaterialTexture};
use awsm_renderer::textures::{SamplerCacheKey, SamplerKey, TextureKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::sampler::{AddressMode, FilterMode, MipmapFilterMode};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_core::texture::texture_pool::TextureColorInfo;
use awsm_renderer_editor_protocol::{
    AssetSource, MaterialDef, MaterialShading, ProceduralTextureDef, TextureColorKind, TextureDef,
    TextureRef,
};

use crate::engine::scene::AssetId;

// The pure `MaterialDef` → renderer `Material` conversion is shared with the
// runtime-bundle loader (`populate_awsm_scene`) so the editor's live render and
// the player lower a material identically — the precondition for a meaningful
// round-trip. Re-exported at the old paths so `material::material_to_renderer`
// (and the thumbnail renderer's `bmat::material_to_renderer`) keep resolving.
pub(crate) use awsm_renderer_scene_loader::material::{
    alpha_mode_of, material_to_pbr, material_to_renderer,
};

thread_local! {
    /// `(texture asset, srgb_to_linear, mipmap kind)` → uploaded renderer
    /// `TextureKey`. Keyed by the upload SEMANTICS as well as the asset —
    /// binding decides color space + mipmap kind per SLOT (an albedo is
    /// sRGB-decoded, a normal map is not), so one asset bound to slots with
    /// different semantics gets one upload per semantic, exactly like the
    /// player loader. (Keying by asset alone let the FIRST upload's semantics
    /// win silently: a URL-imported texture is uploaded as sRGB albedo, so
    /// binding it to a normal slot sampled sRGB-decoded normals — shading the
    /// editor viewport differently from the player until a save→reload.)
    /// Session-scoped — a full page reload rebuilds it.
    static TEXTURE_KEYS: RefCell<HashMap<(AssetId, bool, MipmapTextureKind), TextureKey>> =
        RefCell::new(HashMap::new());
    /// Legacy per-asset entries with UNKNOWN upload semantics — glTF import
    /// pre-registrations (`populate_gltf` uploaded them with its own per-slot
    /// semantics and only hands back the key). Consulted as a fallback when no
    /// exact-semantics entry exists.
    static TEXTURE_KEYS_ANY: RefCell<HashMap<AssetId, TextureKey>> = RefCell::new(HashMap::new());
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

/// A/the renderer [`TextureKey`] a texture asset resolves to, if it's been
/// materialized/registered this session (any semantics). Used by the
/// image-query seam to read a raster/file texture back from the GPU.
pub(crate) fn texture_key_for(asset_id: AssetId) -> Option<TextureKey> {
    if let Some(key) = TEXTURE_KEYS_ANY.with(|c| c.borrow().get(&asset_id).copied()) {
        return Some(key);
    }
    TEXTURE_KEYS.with(|c| {
        c.borrow()
            .iter()
            .find(|((id, _, _), _)| *id == asset_id)
            .map(|(_, key)| *key)
    })
}

/// Pre-register a texture-asset id against a renderer [`TextureKey`] that's
/// already uploaded with UNKNOWN (caller-chosen) semantics — e.g. one
/// `populate_gltf` baked for an imported model. `resolve_texture` uses it as a
/// fallback when it has no exact-semantics upload. Callers that KNOW the
/// upload's semantics should use [`register_texture_key_semantics`] instead.
pub(crate) fn register_texture_key(asset_id: AssetId, key: TextureKey) {
    TEXTURE_KEYS_ANY.with(|c| c.borrow_mut().insert(asset_id, key));
}

/// Register an uploaded texture under its exact upload semantics, so slot
/// bindings that MATCH reuse it and slot bindings that DIFFER re-materialize
/// with their own semantics instead of silently inheriting these.
pub(crate) fn register_texture_key_semantics(
    asset_id: AssetId,
    color: &TextureColorInfo,
    key: TextureKey,
) {
    TEXTURE_KEYS.with(|c| {
        c.borrow_mut()
            .insert((asset_id, color.srgb_to_linear, color.mipmap_kind), key)
    });
}

/// Best-effort image mime from a URL's extension (query/fragment stripped). Only
/// tags the decode + the persisted `assets/<hash>.<ext>` side file; the browser's
/// decoder sniffs the actual content regardless, so it is only a hint. Defaults
/// to PNG.
fn mime_from_url(url: &str) -> awsm_renderer_glb_export::ImageMime {
    use awsm_renderer_glb_export::ImageMime;
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) {
        Some(e) if e == "jpg" || e == "jpeg" => ImageMime::Jpeg,
        _ => ImageMime::Png,
    }
}

/// Decode ENCODED image bytes (PNG/JPEG) to RGBA8 via the browser, for restoring
/// persisted textures on load. `mime` is the source mime (`image/png` etc.).
pub(crate) async fn decode_rgba_from_bytes(
    bytes: &[u8],
    mime: &str,
) -> Result<(Vec<u8>, u32, u32), String> {
    let bitmap = awsm_renderer_core::image::bitmap::load_u8(bytes, mime, None)
        .await
        .map_err(|e| format!("decode image: {e}"))?;
    bitmap_to_rgba(bitmap)
}

/// Fetch an encoded image from `url` and decode it to RGBA8 + dims. Used by
/// `DisplaceFromTexture` (§16) to read an agent-hosted heightmap by URL — the same
/// fetch + decode path imported textures use (no inline base64).
pub(crate) async fn decode_rgba_from_url(url: &str) -> Result<(Vec<u8>, u32, u32), String> {
    let mime = mime_from_url(url);
    let bytes = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?
        .binary()
        .await
        .map_err(|e| format!("fetch {url} body: {e}"))?;
    decode_rgba_from_bytes(&bytes, mime.as_str()).await
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
pub(crate) async fn import_texture_url(
    id: AssetId,
    url: &str,
) -> Result<(Vec<u8>, awsm_renderer_glb_export::ImageMime), String> {
    // Fetch the ENCODED bytes (network wait, no renderer lock) so we can BOTH decode
    // them for the GPU AND return them for persistence — Save writes the encoded
    // bytes to `assets/<hash>.<ext>`. Decoding from the fetched bytes is the exact
    // path a project reload uses (`restore_raster_textures`), so import == reload.
    let mime = mime_from_url(url);
    let bytes = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?
        .binary()
        .await
        .map_err(|e| format!("fetch {url} body: {e}"))?;
    let (rgba, w, h) = decode_rgba_from_bytes(&bytes, mime.as_str()).await?;
    let handle = crate::engine::context::renderer_handle();
    let mut r = handle.lock().await;
    let sampler_key = sampler_for(&mut r, None).ok_or("sampler")?;
    // Uploaded as sRGB albedo — the common case for a URL import, and only a
    // DEFAULT: the upload is registered under these exact semantics, so binding
    // the asset to a data slot (normal / metallic-roughness / …) re-materializes
    // it with that slot's color space + mipmap kind instead of sampling this
    // sRGB-decoded copy.
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
    register_texture_key_semantics(id, &color, key);
    Ok((bytes, mime))
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
/// The renderer upload descriptor for a texture's semantic role — the SINGLE place
/// that maps a [`TextureColorKind`] to its `TextureColorInfo` (color space + mipmap
/// kind), so the import and reload paths can never disagree. `premultiplied_alpha`
/// stays `None` (use the image's own setting), matching `renderer-gltf::populate`.
pub(crate) fn color_info_for_kind(kind: TextureColorKind) -> TextureColorInfo {
    let mipmap_kind = match kind {
        TextureColorKind::Albedo => MipmapTextureKind::Albedo,
        TextureColorKind::Normal => MipmapTextureKind::Normal,
        TextureColorKind::MetallicRoughness => MipmapTextureKind::MetallicRoughness,
        TextureColorKind::Occlusion => MipmapTextureKind::Occlusion,
        TextureColorKind::Emissive => MipmapTextureKind::Emissive,
        TextureColorKind::Specular => MipmapTextureKind::Specular,
        TextureColorKind::SpecularColor => MipmapTextureKind::SpecularColor,
        TextureColorKind::Transmission => MipmapTextureKind::Transmission,
        TextureColorKind::VolumeThickness => MipmapTextureKind::VolumeThickness,
    };
    TextureColorInfo {
        mipmap_kind,
        srgb_to_linear: kind.is_srgb(),
        premultiplied_alpha: None,
    }
}

/// Color space + mipmaps: each item carries its [`TextureColorKind`] (4th tuple
/// field) — the texture's semantic role persisted on the asset. [`color_info_for_kind`]
/// maps it to the full `TextureColorInfo` (sRGB-decode for color kinds, verbatim for
/// data kinds; role-specific mipmap kind). This makes RELOAD upload textures with the
/// exact same meaning as fresh IMPORT — a normal map reloaded as sRGB albedo has
/// corrupted normals + wrong mipmaps (the save→reload shading drift).
pub(crate) async fn restore_raster_textures(
    items: Vec<(AssetId, Vec<u8>, String, TextureColorKind)>,
) {
    if items.is_empty() {
        return;
    }
    // Decode all (async, network/codec) BEFORE taking the renderer lock. Carry the
    // per-texture semantic ROLE through so the upload picks the right color space +
    // mipmap kind (data maps — normal/metal-rough/occlusion — must NOT sRGB-decode
    // and get role-specific mipmaps).
    let mut decoded: Vec<(AssetId, Vec<u8>, u32, u32, TextureColorKind)> =
        Vec::with_capacity(items.len());
    for (id, bytes, mime, kind) in &items {
        match decode_rgba_from_bytes(bytes, mime).await {
            Ok((rgba, w, h)) => decoded.push((*id, rgba, w, h, *kind)),
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
    for (id, rgba, w, h, kind) in &decoded {
        // The role's full TextureColorInfo (color space + mipmap kind) — identical
        // to what the fresh-import path uses, so RELOAD == IMPORT. Registered
        // under those exact semantics: a slot binding with a DIFFERENT role
        // (persisted kind stale/inferred wrong) re-materializes with the slot's
        // semantics rather than sampling this upload.
        let color = color_info_for_kind(*kind);
        match r
            .textures
            .add_image_rgba_raw(rgba, *w, *h, sampler_key, color)
        {
            Ok(key) => register_texture_key_semantics(*id, &color, key),
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
        MaterialShading::Unlit => {
            // Unlit samples base-color + emissive at runtime (gated by the
            // TextureInfo `exists` flag — see compute_unlit_material_color); the
            // renderer `UnlitMaterial` carries both slots. Resolve + bind them here
            // so a per-node texture (set_node_texture) actually renders instead of
            // storing the binding and showing flat (F2). Mirrors the PBR path.
            let mut material = material_to_renderer(def);
            if let Material::Unlit(m) = &mut material {
                if let Some(t) = &def.base_color_texture {
                    m.base_color_tex =
                        resolve_texture(renderer, t, true, MipmapTextureKind::Albedo);
                }
                if let Some(t) = &def.emissive_texture {
                    m.emissive_tex =
                        resolve_texture(renderer, t, true, MipmapTextureKind::Emissive);
                }
            }
            material
        }
        // Toon doesn't carry texture slots in the editor yet.
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
    // Persist the slot's semantic role onto the texture ASSET: `color_kind` is
    // what Save writes and what a reload's initial upload + the
    // `screenshot_texture` readback use, so it must track how the texture is
    // actually USED — not the import-time default (URL imports start `None`,
    // which restores via name inference).
    record_asset_color_kind(asset_id, kind);
    // Exact-semantics upload for this (asset, color space, mipmap kind)?
    if let Some(key) = TEXTURE_KEYS.with(|c| c.borrow().get(&(asset_id, srgb, kind)).copied()) {
        return Some(mk(key));
    }
    // Legacy unknown-semantics upload (glTF populate pre-registration).
    if let Some(key) = TEXTURE_KEYS_ANY.with(|c| c.borrow().get(&asset_id).copied()) {
        return Some(mk(key));
    }
    let color = TextureColorInfo {
        mipmap_kind: kind,
        srgb_to_linear: srgb,
        premultiplied_alpha: None,
    };
    // Raster asset with captured bytes → decode + upload with THIS binding's
    // semantics. (The `image` crate decodes synchronously; this runs once per
    // (asset, semantics), then cache-hits above.)
    if let Some((bytes, _mime)) = super::texture_cache::get(asset_id) {
        let rgba = match image::load_from_memory(&bytes) {
            Ok(img) => img.to_rgba8(),
            Err(e) => {
                tracing::warn!("texture {asset_id}: decode for slot bind failed ({e})");
                return None;
            }
        };
        let (w, h) = rgba.dimensions();
        let key = r
            .textures
            .add_image_rgba_raw(rgba.as_raw(), w, h, sampler_key, color)
            .ok()?;
        register_texture_key_semantics(asset_id, &color, key);
        return Some(mk(key));
    }
    // Procedural asset → generate + upload with this binding's semantics.
    let proc = {
        let ctrl = crate::controller::controller();
        let assets = ctrl.scene.assets.lock().unwrap();
        match assets.entries.get(&asset_id).map(|e| &e.source) {
            Some(AssetSource::Texture(TextureDef::Procedural(p))) => Some(p.clone()),
            _ => None,
        }
    }?;
    let (rgba, w, h) = procedural_rgba(&proc);
    let key = r
        .textures
        .add_image_rgba_raw(&rgba, w, h, sampler_key, color)
        .ok()?;
    register_texture_key_semantics(asset_id, &color, key);
    Some(mk(key))
}

/// The persistable [`TextureColorKind`] for an upload's mipmap kind — the
/// inverse of [`color_info_for_kind`]'s kind mapping (color space is implied:
/// `TextureColorKind::is_srgb` mirrors what the slot passes).
fn color_kind_for_mipmap(kind: MipmapTextureKind) -> TextureColorKind {
    use MipmapTextureKind as M;
    use TextureColorKind as K;
    match kind {
        M::Albedo => K::Albedo,
        M::Normal => K::Normal,
        M::MetallicRoughness => K::MetallicRoughness,
        M::Occlusion => K::Occlusion,
        M::Emissive => K::Emissive,
        M::Specular => K::Specular,
        M::SpecularColor => K::SpecularColor,
        M::Transmission => K::Transmission,
        M::VolumeThickness => K::VolumeThickness,
    }
}

/// Record the semantic role a slot binding resolved a RASTER texture asset
/// with, so the persisted `color_kind` tracks actual use. No-op when the
/// stored value already matches (re-materializes are frequent); a texture
/// genuinely bound to slots with different roles keeps the LAST resolved one —
/// advisory only, since rendering semantics are per-binding now.
fn record_asset_color_kind(asset_id: AssetId, kind: MipmapTextureKind) {
    let want = color_kind_for_mipmap(kind);
    let ctrl = crate::controller::controller();
    let mut assets = ctrl.scene.assets.lock().unwrap();
    if let Some(entry) = assets.entries.get_mut(&asset_id) {
        if let AssetSource::Texture(TextureDef::Raster { color_kind, .. }) = &mut entry.source {
            if *color_kind != Some(want) {
                *color_kind = Some(want);
            }
        }
    }
}

/// Pool (or fetch) the sampler for a texture binding from its [`TextureSampler`]
/// settings — wrap modes + filtering imported from the glTF sampler. `None`
/// defaults to glTF's repeat + linear.
fn sampler_for(
    r: &mut AwsmRenderer,
    sampler: Option<awsm_renderer_editor_protocol::TextureSampler>,
) -> Option<SamplerKey> {
    use awsm_renderer_editor_protocol::{TextureFilter, TextureWrap};
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
    use awsm_renderer_meshgen::procedural_texture::{checker_rgba, gradient_rgba, noise_rgba};
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
