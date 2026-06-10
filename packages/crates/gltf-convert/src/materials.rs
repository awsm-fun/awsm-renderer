//! Material extraction — reads a source glTF's materials into NEUTRAL specs,
//! decoupled from both editor-protocol and scene types (each maps at its own
//! wiring step). Pure glTF reading; no GPU, no image upload (texture *image*
//! bytes are pure data shipped separately; refs here point at glTF image
//! indices).
//!
//! Status: base PBR (factors + standard texture slots + alpha + double-sided +
//! unlit). FOLLOW-ON (its own increment): the KHR extensions
//! (transmission/ior/volume/iridescence/specular/clearcoat/sheen/…), sampler +
//! `KHR_texture_transform` on the refs. Mirrors the editor's
//! `extract_material_specs`, which stays until the wiring step adopts this.

/// glTF `alphaMode` (+ mask cutoff).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlphaMode {
    Opaque,
    Mask { cutoff: f32 },
    Blend,
}

/// A texture reference by glTF **image** (source) index + UV set. (Sampler +
/// `KHR_texture_transform` are a follow-on.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TexRef {
    /// Index of the glTF image (the `source` of the referenced texture).
    pub image: usize,
    /// Which `TEXCOORD_n` set this slot samples.
    pub uv_index: u32,
}

/// A material lifted from the source glTF, in neutral form.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialSpec {
    pub label: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub normal_scale: f32,
    pub occlusion_strength: f32,
    pub double_sided: bool,
    /// `KHR_materials_unlit`.
    pub unlit: bool,
    pub alpha_mode: AlphaMode,
    pub base_color_tex: Option<TexRef>,
    pub metallic_roughness_tex: Option<TexRef>,
    pub normal_tex: Option<TexRef>,
    pub occlusion_tex: Option<TexRef>,
    pub emissive_tex: Option<TexRef>,
    pub extensions: MaterialExtensions,
}

fn info_ref(info: gltf::texture::Info) -> TexRef {
    TexRef {
        image: info.texture().source().index(),
        uv_index: info.tex_coord(),
    }
}

/// The KHR material extensions, in neutral form. Presence of a field = the
/// extension is enabled (drives the renderer's specialized shader variant);
/// scalars/colors are the factor values. Extension *texture* refs are a
/// follow-on increment (the variant-determining presence + factors are here).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaterialExtensions {
    pub emissive_strength: Option<f32>,
    pub ior: Option<f32>,
    /// (specular_factor, specular_color_factor).
    pub specular: Option<(f32, [f32; 3])>,
    /// transmission factor.
    pub transmission: Option<f32>,
    pub volume: Option<Volume>,
    pub iridescence: Option<Iridescence>,
    /// (factor, color_factor).
    pub diffuse_transmission: Option<(f32, [f32; 3])>,
    pub clearcoat: Option<Clearcoat>,
    pub sheen: Option<Sheen>,
    pub dispersion: Option<f32>,
    /// (strength, rotation).
    pub anisotropy: Option<(f32, f32)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volume {
    pub thickness_factor: f32,
    /// glTF's +inf default ("no absorption") is clamped to `f32::MAX` to stay
    /// serializable without changing the look.
    pub attenuation_distance: f32,
    pub attenuation_color: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Iridescence {
    pub factor: f32,
    pub ior: f32,
    pub thickness_min: f32,
    pub thickness_max: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clearcoat {
    pub factor: f32,
    pub roughness_factor: f32,
    pub normal_scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sheen {
    pub roughness_factor: f32,
    pub color_factor: [f32; 3],
}

fn ext_f32(v: &serde_json::Value, key: &str, default: f32) -> f32 {
    v.get(key).and_then(|x| x.as_f64()).map(|x| x as f32).unwrap_or(default)
}

fn ext_color3(v: &serde_json::Value, key: &str, default: [f32; 3]) -> [f32; 3] {
    v.get(key)
        .and_then(|x| x.as_array())
        .filter(|a| a.len() == 3)
        .map(|a| {
            let f = |i: usize| a[i].as_f64().unwrap_or(0.0) as f32;
            [f(0), f(1), f(2)]
        })
        .unwrap_or(default)
}

/// Extract the KHR extensions on a material into neutral form. Mirrors the
/// editor's `extract_extensions`: typed accessors for the crate-native ones
/// (emissive_strength/ior/specular/transmission/volume), raw JSON for the rest.
pub fn extract_extensions(m: &gltf::Material) -> MaterialExtensions {
    let volume = m.volume().map(|vol| {
        let d = vol.attenuation_distance();
        Volume {
            thickness_factor: vol.thickness_factor(),
            attenuation_distance: if d.is_finite() { d } else { f32::MAX },
            attenuation_color: vol.attenuation_color(),
        }
    });
    let diffuse_transmission = m
        .extension_value("KHR_materials_diffuse_transmission")
        .map(|v| {
            (
                ext_f32(v, "diffuseTransmissionFactor", 0.0),
                ext_color3(v, "diffuseTransmissionColorFactor", [1.0, 1.0, 1.0]),
            )
        });
    let clearcoat = m.extension_value("KHR_materials_clearcoat").map(|v| Clearcoat {
        factor: ext_f32(v, "clearcoatFactor", 0.0),
        roughness_factor: ext_f32(v, "clearcoatRoughnessFactor", 0.0),
        normal_scale: v
            .get("clearcoatNormalTexture")
            .map(|t| ext_f32(t, "scale", 1.0))
            .unwrap_or(1.0),
    });
    let sheen = m.extension_value("KHR_materials_sheen").map(|v| Sheen {
        roughness_factor: ext_f32(v, "sheenRoughnessFactor", 0.0),
        color_factor: ext_color3(v, "sheenColorFactor", [0.0, 0.0, 0.0]),
    });
    let anisotropy = m.extension_value("KHR_materials_anisotropy").map(|v| {
        (
            ext_f32(v, "anisotropyStrength", 0.0),
            ext_f32(v, "anisotropyRotation", 0.0),
        )
    });
    let iridescence = m
        .extension_value("KHR_materials_iridescence")
        .map(|v| Iridescence {
            factor: ext_f32(v, "iridescenceFactor", 0.0),
            ior: ext_f32(v, "iridescenceIor", 1.3),
            thickness_min: ext_f32(v, "iridescenceThicknessMinimum", 100.0),
            thickness_max: ext_f32(v, "iridescenceThicknessMaximum", 400.0),
        });
    MaterialExtensions {
        emissive_strength: m.emissive_strength(),
        ior: m.ior(),
        specular: m
            .specular()
            .map(|s| (s.specular_factor(), s.specular_color_factor())),
        transmission: m.transmission().map(|t| t.transmission_factor()),
        volume,
        iridescence,
        diffuse_transmission,
        clearcoat,
        sheen,
        dispersion: m
            .extension_value("KHR_materials_dispersion")
            .map(|v| ext_f32(v, "dispersion", 0.0)),
        anisotropy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A scene-less glTF with the dish's extension set (typed:
    // transmission/ior/volume; raw-JSON: iridescence).
    const GLASS_GLTF: &str = r#"{
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_materials_transmission","KHR_materials_ior","KHR_materials_volume","KHR_materials_iridescence"],
        "materials": [{
            "name": "glass",
            "pbrMetallicRoughness": {"metallicFactor": 0.0, "roughnessFactor": 0.07},
            "doubleSided": true,
            "extensions": {
                "KHR_materials_transmission": {"transmissionFactor": 0.9},
                "KHR_materials_ior": {"ior": 1.5},
                "KHR_materials_volume": {"thicknessFactor": 0.1, "attenuationColor": [0.9, 0.95, 1.0]},
                "KHR_materials_iridescence": {"iridescenceFactor": 1.0, "iridescenceIor": 1.3, "iridescenceThicknessMinimum": 500.0, "iridescenceThicknessMaximum": 550.0}
            }
        }]
    }"#;

    #[test]
    fn extracts_khr_extensions() {
        let (doc, _, _) = gltf::import_slice(GLASS_GLTF.as_bytes()).expect("parse gltf");
        let specs = extract_materials(&doc);
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.metallic, 0.0);
        assert!(s.double_sided);
        let x = &s.extensions;
        assert_eq!(x.transmission, Some(0.9));
        assert_eq!(x.ior, Some(1.5));
        let vol = x.volume.expect("volume");
        assert_eq!(vol.thickness_factor, 0.1);
        assert_eq!(vol.attenuation_color, [0.9, 0.95, 1.0]);
        let ir = x.iridescence.expect("iridescence");
        assert_eq!(ir.factor, 1.0);
        assert_eq!(ir.thickness_min, 500.0);
        assert_eq!(ir.thickness_max, 550.0);
    }
}

/// Extract every material in the document into neutral [`MaterialSpec`]s, index-
/// aligned with `doc.materials()`.
pub fn extract_materials(doc: &gltf::Document) -> Vec<MaterialSpec> {
    doc.materials()
        .map(|m| {
            let pbr = m.pbr_metallic_roughness();
            let idx = m.index().unwrap_or(0);
            MaterialSpec {
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
                unlit: m.unlit(),
                alpha_mode: match m.alpha_mode() {
                    gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                    gltf::material::AlphaMode::Mask => AlphaMode::Mask {
                        cutoff: m.alpha_cutoff().unwrap_or(0.5),
                    },
                    gltf::material::AlphaMode::Blend => AlphaMode::Blend,
                },
                base_color_tex: pbr.base_color_texture().map(info_ref),
                metallic_roughness_tex: pbr.metallic_roughness_texture().map(info_ref),
                normal_tex: m.normal_texture().map(|t| TexRef {
                    image: t.texture().source().index(),
                    uv_index: t.tex_coord(),
                }),
                occlusion_tex: m.occlusion_texture().map(|t| TexRef {
                    image: t.texture().source().index(),
                    uv_index: t.tex_coord(),
                }),
                emissive_tex: m.emissive_texture().map(info_ref),
                extensions: extract_extensions(&m),
            }
        })
        .collect()
}
