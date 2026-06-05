//! `MaterialDef` → renderer `Material` conversion. Dispatches on the authored
//! shading model (PBR / Unlit / Toon) so a built-in material's *variant* (its
//! shader-generation choice) actually renders. Texture binding resolution is the
//! follow-on; factors/alpha/double-sided/vertex-colors are wired here.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::toon::ToonMaterial;
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey, MaterialTexture};
use awsm_renderer::textures::{SamplerCacheKey, SamplerKey, TextureKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::sampler::{AddressMode, FilterMode, MipmapFilterMode};
use awsm_renderer_core::texture::mipmap::MipmapTextureKind;
use awsm_renderer_core::texture::texture_pool::TextureColorInfo;
use awsm_scene_schema::{
    AssetSource, MaterialDef, MaterialShading, ProceduralTextureDef, TextureDef, TextureRef,
};

use crate::engine::scene::AssetId;

thread_local! {
    /// Maps a texture-asset id → its uploaded renderer `TextureKey`, so a texture
    /// is generated + uploaded once and reused across every mesh/material that
    /// binds it (and across re-materializations). Session-scoped — a full page
    /// reload rebuilds it (acceptable; project reset within a session is rare).
    static TEXTURE_KEYS: RefCell<HashMap<AssetId, TextureKey>> = RefCell::new(HashMap::new());
}

/// Resolve a [`TextureRef`] to a renderer [`TextureKey`] (uploading a procedural
/// texture once / reusing a pre-registered key), pooling its sampler so the
/// binding is valid. Used for per-mesh texture overrides on dynamic materials.
pub(crate) fn resolve_texture_key(r: &mut AwsmRenderer, tref: &TextureRef) -> Option<TextureKey> {
    resolve_texture(r, tref, true, MipmapTextureKind::Albedo).map(|t| t.key)
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
    let sampler_key = material_sampler(r)?;
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
    let transform_key = tref.transform.map(|t| {
        r.textures
            .insert_texture_transform(&awsm_renderer::textures::TextureTransform {
                offset: t.offset,
                origin: [0.0, 0.0],
                rotation: t.rotation,
                scale: t.scale,
            })
    });
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

/// A shared linear-filtered, repeat-wrapped sampler for material textures.
fn material_sampler(r: &mut AwsmRenderer) -> Option<SamplerKey> {
    let key = SamplerCacheKey {
        address_mode_u: Some(AddressMode::Repeat),
        address_mode_v: Some(AddressMode::Repeat),
        address_mode_w: Some(AddressMode::Repeat),
        mag_filter: Some(FilterMode::Linear),
        min_filter: Some(FilterMode::Linear),
        mipmap_filter: Some(MipmapFilterMode::Linear),
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

/// Resolve the authored alpha mode, preserving the legacy "base_color.a < 1 ⇒
/// Blend" heuristic for `Opaque` inline materials.
fn alpha_mode_of(def: &MaterialDef) -> MaterialAlphaMode {
    match def.alpha_mode {
        awsm_scene_schema::MaterialAlphaMode::Opaque => {
            if def.base_color[3] < 0.999 {
                MaterialAlphaMode::Blend
            } else {
                MaterialAlphaMode::Opaque
            }
        }
        awsm_scene_schema::MaterialAlphaMode::Mask { cutoff } => MaterialAlphaMode::Mask { cutoff },
        awsm_scene_schema::MaterialAlphaMode::Blend => MaterialAlphaMode::Blend,
    }
}

/// Dispatch on the shading model so Unlit / Toon built-ins render as their real
/// variant (previously every `MaterialDef` collapsed to PBR). Texture-less (the
/// texture binding lives in `insert_material`) — which is exactly what the
/// thumbnail renderer wants (its TextureKeys would differ from the main pool).
/// `pub(crate)` for the thumbnail renderer.
pub(crate) fn material_to_renderer(def: &MaterialDef) -> Material {
    let alpha_mode = alpha_mode_of(def);
    match def.shading {
        MaterialShading::Unlit => {
            let mut m = UnlitMaterial::new(alpha_mode, def.double_sided);
            m.base_color_factor = def.base_color;
            m.emissive_factor = def.emissive;
            Material::Unlit(m)
        }
        MaterialShading::Toon {
            diffuse_bands,
            rim_strength,
            specular_steps,
            shininess,
            rim_power,
        } => {
            let mut m = ToonMaterial::new(alpha_mode, def.double_sided);
            m.base_color_factor = def.base_color;
            m.emissive_factor = def.emissive;
            m.diffuse_bands = diffuse_bands.max(1);
            m.rim_strength = rim_strength;
            m.specular_steps = specular_steps.max(1);
            m.shininess = shininess;
            m.rim_power = rim_power;
            Material::Toon(Box::new(m))
        }
        MaterialShading::Pbr => Material::Pbr(Box::new(material_to_pbr(def, alpha_mode, None))),
    }
}

fn material_to_pbr(
    def: &MaterialDef,
    alpha_mode: MaterialAlphaMode,
    vertex_color_set: Option<u32>,
) -> PbrMaterial {
    let mut pbr = PbrMaterial::new(alpha_mode, def.double_sided);
    pbr.base_color_factor = def.base_color;
    pbr.metallic_factor = def.metallic;
    pbr.roughness_factor = def.roughness;
    pbr.emissive_factor = def.emissive;
    pbr.normal_scale = def.normal_scale;
    pbr.occlusion_strength = def.occlusion_strength;
    if def.vertex_colors_enabled {
        pbr.vertex_color_info = Some(awsm_renderer::materials::pbr::PbrMaterialVertexColorInfo {
            set_index: vertex_color_set.unwrap_or(0),
        });
    }
    apply_extensions(&mut pbr, &def.extensions);
    pbr
}

/// Translate each enabled authored KHR extension onto the renderer's `PbrMaterial`
/// `Option<…Extension>` fields. Presence = the variant bit (a distinct compiled
/// shader); the scalar/color factors are the uniform values. Texture slots within
/// each extension stay `None` until the texture-asset picker lands.
fn apply_extensions(pbr: &mut PbrMaterial, ext: &awsm_scene_schema::PbrExtensions) {
    use awsm_renderer::materials::pbr as r;
    if let Some(e) = ext.emissive_strength {
        pbr.emissive_strength = Some(r::PbrMaterialEmissiveStrength {
            strength: e.strength,
        });
    }
    if let Some(e) = ext.ior {
        pbr.ior = Some(r::PbrMaterialIor { ior: e.ior });
    }
    if let Some(e) = ext.specular {
        pbr.specular = Some(r::PbrMaterialSpecular {
            tex: None,
            factor: e.factor,
            color_tex: None,
            color_factor: e.color_factor,
        });
    }
    if let Some(e) = ext.transmission {
        pbr.transmission = Some(r::PbrMaterialTransmission {
            tex: None,
            factor: e.factor,
        });
    }
    if let Some(e) = ext.diffuse_transmission {
        pbr.diffuse_transmission = Some(r::PbrMaterialDiffuseTransmission {
            tex: None,
            factor: e.factor,
            color_tex: None,
            color_factor: e.color_factor,
        });
    }
    if let Some(e) = ext.volume {
        pbr.volume = Some(r::PbrMaterialVolume {
            thickness_tex: None,
            thickness_factor: e.thickness_factor,
            attenuation_distance: e.attenuation_distance,
            attenuation_color: e.attenuation_color,
        });
    }
    if let Some(e) = ext.clearcoat {
        pbr.clearcoat = Some(r::PbrMaterialClearCoat {
            tex: None,
            factor: e.factor,
            roughness_tex: None,
            roughness_factor: e.roughness_factor,
            normal_tex: None,
            normal_scale: e.normal_scale,
        });
    }
    if let Some(e) = ext.sheen {
        pbr.sheen = Some(r::PbrMaterialSheen {
            roughness_tex: None,
            roughness_factor: e.roughness_factor,
            color_tex: None,
            color_factor: e.color_factor,
        });
    }
    if let Some(e) = ext.dispersion {
        pbr.dispersion = Some(r::PbrMaterialDispersion {
            dispersion: e.dispersion,
        });
    }
    if let Some(e) = ext.anisotropy {
        pbr.anisotropy = Some(r::PbrMaterialAnisotropy {
            tex: None,
            strength: e.strength,
            rotation: e.rotation,
        });
    }
    if let Some(e) = ext.iridescence {
        pbr.iridescence = Some(r::PbrMaterialIridescence {
            tex: None,
            factor: e.factor,
            ior: e.ior,
            thickness_tex: None,
            thickness_min: e.thickness_min,
            thickness_max: e.thickness_max,
        });
    }
}
