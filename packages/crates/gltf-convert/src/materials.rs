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
}

fn info_ref(info: gltf::texture::Info) -> TexRef {
    TexRef {
        image: info.texture().source().index(),
        uv_index: info.tex_coord(),
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
            }
        })
        .collect()
}
