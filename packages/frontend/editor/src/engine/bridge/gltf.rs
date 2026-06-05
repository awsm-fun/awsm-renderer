//! Real glTF/glb model import + **deconstruction**. Fetches the document and
//! `populate_gltf`s it into the renderer (which builds the full transform tree,
//! meshes, and skinning), then snapshots that into an
//! [`AssetTemplate`](super::asset_template::AssetTemplate). The caller
//! (`EditorController::finish_model_import`) mirrors the template as editor
//! `Group`/`Model` nodes so the import appears as an editable hierarchy in the
//! Outliner; each `Model` node duplicates the template meshes under its own
//! transform (see `node_sync::materialize_model`). The template's own meshes are
//! hidden so they don't double-render.

use awsm_renderer::textures::TextureKey;
use awsm_renderer_gltf::data::GltfData;
use awsm_renderer_gltf::loader::{get_type_from_filename, GltfFileType};
use awsm_renderer_gltf::populate::GltfPopulateContext;
use awsm_renderer_gltf::{loader::GltfLoader, AwsmRendererGltfExt};
use awsm_scene_schema::MaterialDef;

use super::asset_template::{self, AssetTemplate};
use crate::engine::context::renderer_handle;

/// The result of importing one glTF/glb: a display name, the node template to
/// deconstruct into the editor scene tree, and the materials + texture names the
/// file brought in (surfaced in the Content Browser + wired onto the meshes).
pub struct GltfImport {
    pub display_name: String,
    pub template: AssetTemplate,
    /// One editable material per glTF material (in glTF material-index order),
    /// with its factors + the renderer textures `populate_gltf` already baked.
    pub materials: Vec<ExtractedMaterial>,
}

/// A glTF material extracted into an editable [`MaterialDef`] (factors only;
/// the controller fills the texture refs once it has minted texture-asset ids)
/// plus the renderer [`TextureKey`]s the populate pass already uploaded, so they
/// can be **reused** (not re-decoded) when this material renders.
pub struct ExtractedMaterial {
    pub def: MaterialDef,
    pub textures: MaterialTextureKeys,
    /// Resolved KHR-extension texture slots, keyed by `"<ext>.<field>"` (e.g.
    /// `"clearcoat.normal_tex"`). The controller turns each into a `TextureRef`
    /// on the matching `def.extensions` field once it has minted asset ids.
    pub ext_textures: Vec<(&'static str, (TextureKey, TexBinding))>,
}

/// The per-binding sampling metadata for one texture slot: which UV set (glTF
/// `texCoord`) and an optional `KHR_texture_transform`. Travels with the texture
/// key/index so it can be written onto the `TextureRef` at import.
#[derive(Clone, Copy, Default)]
pub struct TexBinding {
    pub uv_index: u32,
    pub transform: Option<awsm_scene_schema::TextureTransform>,
}

/// Baked renderer textures for a material's PBR slots (reused from populate),
/// each with its glTF binding metadata (UV set + transform).
#[derive(Default)]
pub struct MaterialTextureKeys {
    pub base_color: Option<(TextureKey, TexBinding)>,
    pub metallic_roughness: Option<(TextureKey, TexBinding)>,
    pub normal: Option<(TextureKey, TexBinding)>,
    pub occlusion: Option<(TextureKey, TexBinding)>,
    pub emissive: Option<(TextureKey, TexBinding)>,
}

/// glTF texture indices for a material's PBR slots (resolved to keys
/// post-populate), each with its binding metadata.
#[derive(Default)]
struct MaterialTextureIndices {
    base_color: Option<(usize, TexBinding)>,
    metallic_roughness: Option<(usize, TexBinding)>,
    normal: Option<(usize, TexBinding)>,
    occlusion: Option<(usize, TexBinding)>,
    emissive: Option<(usize, TexBinding)>,
}

/// Read a texture slot's UV set + `KHR_texture_transform` from its glTF info.
/// (Works for any of `gltf::texture::Info` / `NormalTexture` / `OcclusionTexture`
/// — they all expose `tex_coord()` + `texture_transform()`.) The transform may
/// override the texCoord per glTF spec.
fn tex_binding(tex_coord: u32, xform: Option<gltf::texture::TextureTransform>) -> TexBinding {
    let uv_index = xform.as_ref().and_then(|x| x.tex_coord()).unwrap_or(tex_coord);
    let transform = xform.map(|x| awsm_scene_schema::TextureTransform {
        offset: x.offset(),
        rotation: x.rotation(),
        scale: x.scale(),
    });
    TexBinding {
        uv_index,
        transform,
    }
}

/// Load + populate a glTF/glb from `url`; display name derived from the URL.
/// File type is inferred from the URL extension (`.glb`/`.gltf`).
pub async fn import(url: &str) -> Result<GltfImport, String> {
    import_typed(url, None, None).await
}

/// Load + populate a glTF/glb from a URL with an explicit file type + display
/// name. Used by the **file picker**: the picked file becomes a `blob:` object
/// URL (which has no extension, so the type can't be inferred), and we want the
/// real filename for the Outliner label rather than the opaque blob id.
pub async fn import_file(name: &str, url: &str) -> Result<GltfImport, String> {
    let file_type = get_type_from_filename(name);
    import_typed(url, file_type, Some(name)).await
}

async fn import_typed(
    url: &str,
    file_type: Option<GltfFileType>,
    name: Option<&str>,
) -> Result<GltfImport, String> {
    let loader = GltfLoader::load(url, file_type)
        .await
        .map_err(|e| format!("load: {e}"))?;
    let data = loader.into_data(None).map_err(|e| format!("decode: {e}"))?;
    // Read material factors + texture indices from the document before it's moved
    // into `populate_gltf`; the indices are resolved to baked texture keys after.
    let mat_specs = extract_material_specs(&data);
    let (template, materials) = {
        // Hold the renderer lock across the async populate + the synchronous
        // template snapshot, so nothing mutates the freshly-built tree first.
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        let ctx = r
            .populate_gltf(data, None)
            .await
            .map_err(|e| format!("populate: {e}"))?;
        let template = asset_template::build_from_context(&r, &ctx);
        // The renderer already rendered these meshes directly; hide them so the
        // editor's user-movable Model-node duplicates are the only visible copy.
        asset_template::hide_template_meshes(&mut r, &template);
        let materials = resolve_materials(&ctx, mat_specs);
        (template, materials)
    };
    Ok(GltfImport {
        display_name: name.map(str::to_owned).unwrap_or_else(|| model_name(url)),
        template,
        materials,
    })
}

/// Read each glTF material's editable factors + its slot texture indices.
type MatSpec = (
    MaterialDef,
    MaterialTextureIndices,
    Vec<(&'static str, (usize, TexBinding))>,
);

fn extract_material_specs(data: &GltfData) -> Vec<MatSpec> {
    data.doc
        .materials()
        .map(|m| {
            let pbr = m.pbr_metallic_roughness();
            let idx = m.index().unwrap_or(0);
            let mut ext_textures = Vec::new();
            let extensions = extract_extensions(&m, &mut ext_textures);
            let def = MaterialDef {
                label: m
                    .name()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Material {idx}")),
                base_color: pbr.base_color_factor(),
                metallic: pbr.metallic_factor(),
                roughness: pbr.roughness_factor(),
                emissive: m.emissive_factor(),
                normal_scale: m.normal_texture().map(|t| t.scale()).unwrap_or(1.0),
                occlusion_strength: m.occlusion_texture().map(|t| t.strength()).unwrap_or(1.0),
                double_sided: m.double_sided(),
                alpha_mode: extract_alpha_mode(&m),
                // KHR_materials_unlit → the editor's flat/unlit shading model.
                shading: if m.unlit() {
                    awsm_scene_schema::MaterialShading::Unlit
                } else {
                    awsm_scene_schema::MaterialShading::Pbr
                },
                extensions,
                ..MaterialDef::default()
            };
            let ix = MaterialTextureIndices {
                base_color: pbr.base_color_texture().map(|t| {
                    (t.texture().index(), tex_binding(t.tex_coord(), t.texture_transform()))
                }),
                metallic_roughness: pbr.metallic_roughness_texture().map(|t| {
                    (t.texture().index(), tex_binding(t.tex_coord(), t.texture_transform()))
                }),
                // NormalTexture / OcclusionTexture don't expose the typed
                // texture_transform() accessor (only the base `Info` does), so
                // they carry their UV set; a transform on a normal/occlusion map
                // is rare and left off.
                normal: m.normal_texture().map(|t| {
                    (t.texture().index(), TexBinding { uv_index: t.tex_coord(), transform: None })
                }),
                occlusion: m.occlusion_texture().map(|t| {
                    (t.texture().index(), TexBinding { uv_index: t.tex_coord(), transform: None })
                }),
                emissive: m.emissive_texture().map(|t| {
                    (t.texture().index(), tex_binding(t.tex_coord(), t.texture_transform()))
                }),
            };
            (def, ix, ext_textures)
        })
        .collect()
}

/// glTF `material.alphaMode` (+ cutoff) → the editor's [`MaterialAlphaMode`].
fn extract_alpha_mode(m: &gltf::Material) -> awsm_scene_schema::MaterialAlphaMode {
    use awsm_scene_schema::MaterialAlphaMode;
    match m.alpha_mode() {
        gltf::material::AlphaMode::Opaque => MaterialAlphaMode::Opaque,
        gltf::material::AlphaMode::Mask => MaterialAlphaMode::Mask {
            cutoff: m.alpha_cutoff().unwrap_or(0.5),
        },
        gltf::material::AlphaMode::Blend => MaterialAlphaMode::Blend,
    }
}

/// Read a scalar field off a raw glTF extension JSON object.
fn ext_f32(v: &gltf::json::Value, key: &str, default: f32) -> f32 {
    v.get(key)
        .and_then(|x| x.as_f64())
        .map(|x| x as f32)
        .unwrap_or(default)
}

/// Read a 3-component colour/vector field off a raw glTF extension JSON object.
fn ext_color3(v: &gltf::json::Value, key: &str, default: [f32; 3]) -> [f32; 3] {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            let c = |i: usize| {
                a.get(i)
                    .and_then(|x| x.as_f64())
                    .unwrap_or(default[i] as f64) as f32
            };
            [c(0), c(1), c(2)]
        })
        .unwrap_or(default)
}

/// Extract every KHR material extension the editor models into per-mesh
/// uniforms. Read straight off the raw extensions JSON (uniform across all 11,
/// and independent of which typed accessors the `gltf` crate version exposes) —
/// only the *factors* matter here (the editor's `MaterialDef` carries no
/// extension texture slots). An enabled extension becomes a variant bit on the
/// imported material; its parameters become the per-mesh overrides this mesh
/// seeds from.
fn extract_extensions(
    m: &gltf::Material,
    ext_textures: &mut Vec<(&'static str, (usize, TexBinding))>,
) -> awsm_scene_schema::material::PbrExtensions {
    use awsm_scene_schema::material::*;
    let mut e = PbrExtensions::default();
    // Capture an extension texture slot (a glTF `textureInfo` object) by name.
    let mut grab = |slot: &'static str, v: &gltf::json::Value, json_key: &str| {
        if let Some(t) = ext_tex(v, json_key) {
            ext_textures.push((slot, t));
        }
    };
    if let Some(v) = m.extension_value("KHR_materials_emissive_strength") {
        e.emissive_strength = Some(EmissiveStrengthExt {
            strength: ext_f32(v, "emissiveStrength", 1.0),
        });
    }
    if let Some(v) = m.extension_value("KHR_materials_ior") {
        e.ior = Some(IorExt {
            ior: ext_f32(v, "ior", 1.5),
        });
    }
    if let Some(v) = m.extension_value("KHR_materials_specular") {
        e.specular = Some(SpecularExt {
            factor: ext_f32(v, "specularFactor", 1.0),
            color_factor: ext_color3(v, "specularColorFactor", [1.0, 1.0, 1.0]),
            ..Default::default()
        });
        grab("specular.tex", v, "specularTexture");
        grab("specular.color_tex", v, "specularColorTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_transmission") {
        e.transmission = Some(TransmissionExt {
            factor: ext_f32(v, "transmissionFactor", 0.0),
            ..Default::default()
        });
        grab("transmission.tex", v, "transmissionTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_diffuse_transmission") {
        e.diffuse_transmission = Some(DiffuseTransmissionExt {
            factor: ext_f32(v, "diffuseTransmissionFactor", 0.0),
            color_factor: ext_color3(v, "diffuseTransmissionColorFactor", [1.0, 1.0, 1.0]),
            ..Default::default()
        });
        grab("diffuse_transmission.tex", v, "diffuseTransmissionTexture");
        grab("diffuse_transmission.color_tex", v, "diffuseTransmissionColorTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_volume") {
        e.volume = Some(VolumeExt {
            thickness_factor: ext_f32(v, "thicknessFactor", 0.0),
            attenuation_distance: ext_f32(v, "attenuationDistance", 1.0),
            attenuation_color: ext_color3(v, "attenuationColor", [1.0, 1.0, 1.0]),
            ..Default::default()
        });
        grab("volume.thickness_tex", v, "thicknessTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_clearcoat") {
        e.clearcoat = Some(ClearcoatExt {
            factor: ext_f32(v, "clearcoatFactor", 0.0),
            roughness_factor: ext_f32(v, "clearcoatRoughnessFactor", 0.0),
            normal_scale: v
                .get("clearcoatNormalTexture")
                .map(|t| ext_f32(t, "scale", 1.0))
                .unwrap_or(1.0),
            ..Default::default()
        });
        grab("clearcoat.tex", v, "clearcoatTexture");
        grab("clearcoat.roughness_tex", v, "clearcoatRoughnessTexture");
        grab("clearcoat.normal_tex", v, "clearcoatNormalTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_sheen") {
        e.sheen = Some(SheenExt {
            roughness_factor: ext_f32(v, "sheenRoughnessFactor", 0.0),
            color_factor: ext_color3(v, "sheenColorFactor", [0.0, 0.0, 0.0]),
            ..Default::default()
        });
        grab("sheen.color_tex", v, "sheenColorTexture");
        grab("sheen.roughness_tex", v, "sheenRoughnessTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_dispersion") {
        e.dispersion = Some(DispersionExt {
            dispersion: ext_f32(v, "dispersion", 0.0),
        });
    }
    if let Some(v) = m.extension_value("KHR_materials_anisotropy") {
        e.anisotropy = Some(AnisotropyExt {
            strength: ext_f32(v, "anisotropyStrength", 0.0),
            rotation: ext_f32(v, "anisotropyRotation", 0.0),
            ..Default::default()
        });
        grab("anisotropy.tex", v, "anisotropyTexture");
    }
    if let Some(v) = m.extension_value("KHR_materials_iridescence") {
        e.iridescence = Some(IridescenceExt {
            factor: ext_f32(v, "iridescenceFactor", 0.0),
            ior: ext_f32(v, "iridescenceIor", 1.3),
            thickness_min: ext_f32(v, "iridescenceThicknessMinimum", 100.0),
            thickness_max: ext_f32(v, "iridescenceThicknessMaximum", 400.0),
            ..Default::default()
        });
        grab("iridescence.tex", v, "iridescenceTexture");
        grab("iridescence.thickness_tex", v, "iridescenceThicknessTexture");
    }
    e
}

/// Read an extension `textureInfo` JSON object → (glTF texture index, binding).
/// Honors the slot's own `texCoord` + an inline `KHR_texture_transform`.
fn ext_tex(v: &gltf::json::Value, key: &str) -> Option<(usize, TexBinding)> {
    let info = v.get(key)?;
    let index = info.get("index").and_then(|x| x.as_u64())? as usize;
    let tex_coord = info.get("texCoord").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let xform = info
        .get("extensions")
        .and_then(|e| e.get("KHR_texture_transform"));
    let (uv_index, transform) = match xform {
        Some(t) => {
            let uv = t
                .get("texCoord")
                .and_then(|x| x.as_u64())
                .map(|x| x as u32)
                .unwrap_or(tex_coord);
            let transform = awsm_scene_schema::TextureTransform {
                offset: read_vec2(t, "offset", [0.0, 0.0]),
                rotation: ext_f32(t, "rotation", 0.0),
                scale: read_vec2(t, "scale", [1.0, 1.0]),
            };
            (uv, Some(transform))
        }
        None => (tex_coord, None),
    };
    Some((
        index,
        TexBinding {
            uv_index,
            transform,
        },
    ))
}

/// Read a 2-component float field off a raw glTF JSON object.
fn read_vec2(v: &gltf::json::Value, key: &str, default: [f32; 2]) -> [f32; 2] {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            let c = |i: usize| {
                a.get(i)
                    .and_then(|x| x.as_f64())
                    .unwrap_or(default[i] as f64) as f32
            };
            [c(0), c(1)]
        })
        .unwrap_or(default)
}

/// Resolve each material's slot texture indices to the renderer [`TextureKey`]s
/// the populate pass uploaded (matched by glTF texture index — a texture maps to
/// one baked key regardless of the colour-space variant used in the lookup key).
fn resolve_materials(ctx: &GltfPopulateContext, specs: Vec<MatSpec>) -> Vec<ExtractedMaterial> {
    let textures = ctx.textures.lock().unwrap();
    // Resolve a (glTF texture index, binding) → (baked TextureKey, binding).
    let find = |slot: Option<(usize, TexBinding)>| -> Option<(TextureKey, TexBinding)> {
        let (i, binding) = slot?;
        textures
            .iter()
            .find(|(k, _)| k.index == i)
            .map(|(_, v)| (*v, binding))
    };
    specs
        .into_iter()
        .map(|(def, ix, ext_idx)| {
            let ext_textures = ext_idx
                .into_iter()
                .filter_map(|(slot, (i, b))| find(Some((i, b))).map(|kb| (slot, kb)))
                .collect();
            ExtractedMaterial {
                def,
                textures: MaterialTextureKeys {
                    base_color: find(ix.base_color),
                    metallic_roughness: find(ix.metallic_roughness),
                    normal: find(ix.normal),
                    occlusion: find(ix.occlusion),
                    emissive: find(ix.emissive),
                },
                ext_textures,
            }
        })
        .collect()
}

fn model_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("model")
        .to_string()
}
