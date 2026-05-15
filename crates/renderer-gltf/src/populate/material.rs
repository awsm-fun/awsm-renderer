use awsm_renderer_core::{
    sampler::{AddressMode, FilterMode, MipmapFilterMode},
    texture::{mipmap::MipmapTextureKind, texture_pool::TextureColorInfo},
};
use ordered_float::OrderedFloat;

use awsm_renderer::{
    materials::{
        pbr::{
            PbrMaterial, PbrMaterialAnisotropy, PbrMaterialClearCoat,
            PbrMaterialDiffuseTransmission, PbrMaterialDispersion, PbrMaterialEmissiveStrength,
            PbrMaterialIor, PbrMaterialIridescence, PbrMaterialSheen, PbrMaterialSpecular,
            PbrMaterialTransmission, PbrMaterialVertexColorInfo, PbrMaterialVolume,
        },
        unlit::UnlitMaterial,
        Material, MaterialAlphaMode, MaterialTexture,
    },
    meshes::buffer_info::{MeshBufferCustomVertexAttributeInfo, MeshBufferVertexAttributeInfo},
    textures::{SamplerCacheKey, SamplerKey, TextureKey, TextureTransform, TextureTransformKey},
    AwsmRenderer,
};

use crate::{
    buffers::MeshBufferInfoWithOffset,
    error::{AwsmGltfError, Result},
    populate::GltfTextureKey,
};

use super::GltfPopulateContext;

pub(super) async fn pbr_material_mapper(
    renderer: &mut AwsmRenderer,
    ctx: &GltfPopulateContext,
    primitive_buffer_info: &MeshBufferInfoWithOffset,
    gltf_material: gltf::Material<'_>,
) -> Result<Material> {
    let mut pbr_material = pbr_material_mapper_core(renderer, ctx, &gltf_material).await?;

    if gltf_material.unlit() {
        let mut unlit_material =
            UnlitMaterial::new(*pbr_material.alpha_mode(), gltf_material.double_sided());
        unlit_material.base_color_tex = pbr_material.base_color_tex;
        unlit_material.base_color_factor = pbr_material.base_color_factor;
        unlit_material.emissive_tex = pbr_material.emissive_tex;
        unlit_material.emissive_factor = pbr_material.emissive_factor;
        return Ok(Material::Unlit(unlit_material));
    }

    // Not quite an extension, but not really core either
    pbr_material.vertex_color_info = primitive_buffer_info
        .triangles
        .vertex_attributes
        .iter()
        .find_map(|attr| {
            if let &MeshBufferVertexAttributeInfo::Custom(
                MeshBufferCustomVertexAttributeInfo::Colors { index, .. },
            ) = attr
            {
                // for right now just always use the first one we find
                Some(PbrMaterialVertexColorInfo { set_index: index })
            } else {
                None
            }
        });

    let LocalPbrMaterialExtensions {
        emissive_strength,
        ior,
        specular,
        transmission,
        diffuse_transmission,
        volume,
        clearcoat,
        sheen,
        dispersion,
        anisotropy,
        iridescence,
    } = LocalPbrMaterialExtensions::new(renderer, ctx, &gltf_material).await?;

    pbr_material.emissive_strength = emissive_strength;
    pbr_material.ior = ior;
    pbr_material.specular = specular;
    pbr_material.transmission = transmission;
    pbr_material.diffuse_transmission = diffuse_transmission;
    pbr_material.volume = volume;
    pbr_material.clearcoat = clearcoat;
    pbr_material.sheen = sheen;
    pbr_material.dispersion = dispersion;
    pbr_material.anisotropy = anisotropy;
    pbr_material.iridescence = iridescence;

    Ok(Material::Pbr(Box::new(pbr_material)))
}

async fn pbr_material_mapper_core(
    renderer: &mut AwsmRenderer,
    ctx: &GltfPopulateContext,
    gltf_material: &gltf::Material<'_>,
) -> Result<PbrMaterial> {
    // Check if this is a real material or a default (no material defined in glTF)
    let has_material = gltf_material.index().is_some();

    let (alpha_mode, premultiplied_alpha) = match ctx.data.hints.hud {
        true => (MaterialAlphaMode::Blend, Some(false)),
        false => match gltf_material.alpha_mode() {
            gltf::material::AlphaMode::Opaque => (MaterialAlphaMode::Opaque, None),
            gltf::material::AlphaMode::Mask => (
                MaterialAlphaMode::Mask {
                    cutoff: gltf_material.alpha_cutoff().unwrap_or(0.5),
                },
                Some(false),
            ),
            gltf::material::AlphaMode::Blend => (MaterialAlphaMode::Blend, Some(false)),
        },
    };

    let mut pbr_material = PbrMaterial::new(alpha_mode, gltf_material.double_sided());

    // If no material is defined, use practical defaults for visibility.
    // Note: glTF spec says metallic=1.0, roughness=1.0, but that makes objects
    // invisible without IBL (diffuse *= 1-metallic = 0). Most viewers use metallic=0.
    if !has_material {
        pbr_material.metallic_factor = 0.0;
        return Ok(pbr_material);
    }

    let gltf_pbr = gltf_material.pbr_metallic_roughness();

    if let Some(tex) = gltf_pbr.base_color_texture().map(GltfTextureInfo::from) {
        let GLtfMaterialCacheKey {
            uv_index,
            texture_key,
            sampler_key,
            texture_transform_key,
        } = tex
            .create_material_cache_key(
                renderer,
                ctx,
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Albedo,
                    srgb_to_linear: true,
                    premultiplied_alpha,
                },
            )
            .await?;

        pbr_material.base_color_tex = Some(MaterialTexture {
            key: texture_key,
            sampler_key: Some(sampler_key),
            uv_index: Some(uv_index as u32),
            transform_key: texture_transform_key,
        });
    }
    pbr_material.base_color_factor = gltf_pbr.base_color_factor();

    if let Some(tex) = gltf_pbr
        .metallic_roughness_texture()
        .map(GltfTextureInfo::from)
    {
        let GLtfMaterialCacheKey {
            uv_index,
            texture_key,
            sampler_key,
            texture_transform_key,
        } = tex
            .create_material_cache_key(
                renderer,
                ctx,
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::MetallicRoughness,
                    srgb_to_linear: false,
                    premultiplied_alpha,
                },
            )
            .await?;
        pbr_material.metallic_roughness_tex = Some(MaterialTexture {
            key: texture_key,
            sampler_key: Some(sampler_key),
            uv_index: Some(uv_index as u32),
            transform_key: texture_transform_key,
        });
    }
    pbr_material.metallic_factor = gltf_pbr.metallic_factor();
    pbr_material.roughness_factor = gltf_pbr.roughness_factor();

    if let Some(tex) = gltf_material.normal_texture().map(GltfTextureInfo::from) {
        let GLtfMaterialCacheKey {
            uv_index,
            texture_key,
            sampler_key,
            texture_transform_key,
        } = tex
            .create_material_cache_key(
                renderer,
                ctx,
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Normal,
                    srgb_to_linear: false,
                    premultiplied_alpha,
                },
            )
            .await?;

        pbr_material.normal_tex = Some(MaterialTexture {
            key: texture_key,
            sampler_key: Some(sampler_key),
            uv_index: Some(uv_index as u32),
            transform_key: texture_transform_key,
        });
    }
    if let Some(normal_tex) = gltf_material.normal_texture() {
        pbr_material.normal_scale = normal_tex.scale();
    }

    if let Some(tex) = gltf_material.occlusion_texture().map(GltfTextureInfo::from) {
        let GLtfMaterialCacheKey {
            uv_index,
            texture_key,
            sampler_key,
            texture_transform_key,
        } = tex
            .create_material_cache_key(
                renderer,
                ctx,
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Occlusion,
                    srgb_to_linear: false,
                    premultiplied_alpha,
                },
            )
            .await?;

        pbr_material.occlusion_tex = Some(MaterialTexture {
            key: texture_key,
            sampler_key: Some(sampler_key),
            uv_index: Some(uv_index as u32),
            transform_key: texture_transform_key,
        });
    }
    if let Some(occlusion_tex) = gltf_material.occlusion_texture() {
        pbr_material.occlusion_strength = occlusion_tex.strength();
    }

    if let Some(tex) = gltf_material.emissive_texture().map(GltfTextureInfo::from) {
        let GLtfMaterialCacheKey {
            uv_index,
            texture_key,
            sampler_key,
            texture_transform_key,
        } = tex
            .create_material_cache_key(
                renderer,
                ctx,
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Emissive,
                    srgb_to_linear: true,
                    premultiplied_alpha,
                },
            )
            .await?;

        pbr_material.emissive_tex = Some(MaterialTexture {
            key: texture_key,
            sampler_key: Some(sampler_key),
            uv_index: Some(uv_index as u32),
            transform_key: texture_transform_key,
        });
    }
    pbr_material.emissive_factor = gltf_material.emissive_factor();

    Ok(pbr_material)
}

#[derive(Default)]
struct LocalPbrMaterialExtensions {
    pub emissive_strength: Option<PbrMaterialEmissiveStrength>,
    pub ior: Option<PbrMaterialIor>,
    pub specular: Option<PbrMaterialSpecular>,
    pub transmission: Option<PbrMaterialTransmission>,
    pub diffuse_transmission: Option<PbrMaterialDiffuseTransmission>,
    pub volume: Option<PbrMaterialVolume>,
    pub clearcoat: Option<PbrMaterialClearCoat>,
    pub sheen: Option<PbrMaterialSheen>,
    pub dispersion: Option<PbrMaterialDispersion>,
    pub anisotropy: Option<PbrMaterialAnisotropy>,
    pub iridescence: Option<PbrMaterialIridescence>,
}

impl LocalPbrMaterialExtensions {
    async fn new(
        renderer: &mut AwsmRenderer,
        ctx: &GltfPopulateContext,
        gltf_material: &gltf::Material<'_>,
    ) -> Result<Self> {
        let mut extensions = Self::default();

        if let Some(strength) = gltf_material.emissive_strength() {
            extensions.emissive_strength = Some(PbrMaterialEmissiveStrength { strength });
        }

        if let Some(ior) = gltf_material.ior() {
            extensions.ior = Some(PbrMaterialIor { ior });
        }

        if let Some(specular) = gltf_material.specular() {
            let tex = if let Some(tex_info) = specular.specular_texture().map(GltfTextureInfo::from)
            {
                let GLtfMaterialCacheKey {
                    uv_index,
                    texture_key,
                    sampler_key,
                    texture_transform_key,
                } = tex_info
                    .create_material_cache_key(
                        renderer,
                        ctx,
                        TextureColorInfo {
                            mipmap_kind: MipmapTextureKind::Specular,
                            srgb_to_linear: false,
                            premultiplied_alpha: None,
                        },
                    )
                    .await?;

                Some(MaterialTexture {
                    key: texture_key,
                    sampler_key: Some(sampler_key),
                    uv_index: Some(uv_index as u32),
                    transform_key: texture_transform_key,
                })
            } else {
                None
            };

            let color_tex = if let Some(tex_info) =
                specular.specular_color_texture().map(GltfTextureInfo::from)
            {
                let GLtfMaterialCacheKey {
                    uv_index,
                    texture_key,
                    sampler_key,
                    texture_transform_key,
                } = tex_info
                    .create_material_cache_key(
                        renderer,
                        ctx,
                        TextureColorInfo {
                            mipmap_kind: MipmapTextureKind::Specular,
                            srgb_to_linear: true,
                            premultiplied_alpha: None,
                        },
                    )
                    .await?;

                Some(MaterialTexture {
                    key: texture_key,
                    sampler_key: Some(sampler_key),
                    uv_index: Some(uv_index as u32),
                    transform_key: texture_transform_key,
                })
            } else {
                None
            };
            extensions.specular = Some(PbrMaterialSpecular {
                tex,
                factor: specular.specular_factor(),
                color_tex,
                color_factor: specular.specular_color_factor(),
            });
        }

        if let Some(transmission) = gltf_material.transmission() {
            let tex = if let Some(tex_info) = transmission
                .transmission_texture()
                .map(GltfTextureInfo::from)
            {
                let GLtfMaterialCacheKey {
                    uv_index,
                    texture_key,
                    sampler_key,
                    texture_transform_key,
                } = tex_info
                    .create_material_cache_key(
                        renderer,
                        ctx,
                        TextureColorInfo {
                            mipmap_kind: MipmapTextureKind::Transmission,
                            srgb_to_linear: false,
                            premultiplied_alpha: None,
                        },
                    )
                    .await?;

                Some(MaterialTexture {
                    key: texture_key,
                    sampler_key: Some(sampler_key),
                    uv_index: Some(uv_index as u32),
                    transform_key: texture_transform_key,
                })
            } else {
                None
            };

            extensions.transmission = Some(PbrMaterialTransmission {
                tex,
                factor: transmission.transmission_factor(),
            });
        }

        if let Some(volume) = gltf_material.volume() {
            let thickness_tex =
                if let Some(tex_info) = volume.thickness_texture().map(GltfTextureInfo::from) {
                    let GLtfMaterialCacheKey {
                        uv_index,
                        texture_key,
                        sampler_key,
                        texture_transform_key,
                    } = tex_info
                        .create_material_cache_key(
                            renderer,
                            ctx,
                            TextureColorInfo {
                                mipmap_kind: MipmapTextureKind::VolumeThickness,
                                srgb_to_linear: false,
                                premultiplied_alpha: None,
                            },
                        )
                        .await?;

                    Some(MaterialTexture {
                        key: texture_key,
                        sampler_key: Some(sampler_key),
                        uv_index: Some(uv_index as u32),
                        transform_key: texture_transform_key,
                    })
                } else {
                    None
                };
            extensions.volume = Some(PbrMaterialVolume {
                thickness_factor: volume.thickness_factor(),
                attenuation_distance: volume.attenuation_distance(),
                attenuation_color: volume.attenuation_color(),
                thickness_tex,
            });
        }

        #[cfg(feature = "clearcoat")]
        if let Some(clearcoat) = gltf_material.clearcoat() {
            let tex =
                if let Some(tex_info) = clearcoat.clearcoat_texture().map(GltfTextureInfo::from) {
                    let GLtfMaterialCacheKey {
                        uv_index,
                        texture_key,
                        sampler_key,
                        texture_transform_key,
                    } = tex_info
                        .create_material_cache_key(
                            renderer,
                            ctx,
                            TextureColorInfo {
                                mipmap_kind: MipmapTextureKind::Albedo,
                                srgb_to_linear: false,
                                premultiplied_alpha: None,
                            },
                        )
                        .await?;

                    Some(MaterialTexture {
                        key: texture_key,
                        sampler_key: Some(sampler_key),
                        uv_index: Some(uv_index as u32),
                        transform_key: texture_transform_key,
                    })
                } else {
                    None
                };

            let roughness_tex = if let Some(tex_info) = clearcoat
                .clearcoat_roughness_texture()
                .map(GltfTextureInfo::from)
            {
                let GLtfMaterialCacheKey {
                    uv_index,
                    texture_key,
                    sampler_key,
                    texture_transform_key,
                } = tex_info
                    .create_material_cache_key(
                        renderer,
                        ctx,
                        TextureColorInfo {
                            mipmap_kind: MipmapTextureKind::MetallicRoughness,
                            srgb_to_linear: false,
                            premultiplied_alpha: None,
                        },
                    )
                    .await?;

                Some(MaterialTexture {
                    key: texture_key,
                    sampler_key: Some(sampler_key),
                    uv_index: Some(uv_index as u32),
                    transform_key: texture_transform_key,
                })
            } else {
                None
            };

            let normal_tex = if let Some(tex_info) = clearcoat
                .clearcoat_normal_texture()
                .map(GltfTextureInfo::from)
            {
                let GLtfMaterialCacheKey {
                    uv_index,
                    texture_key,
                    sampler_key,
                    texture_transform_key,
                } = tex_info
                    .create_material_cache_key(
                        renderer,
                        ctx,
                        TextureColorInfo {
                            mipmap_kind: MipmapTextureKind::Normal,
                            srgb_to_linear: false,
                            premultiplied_alpha: None,
                        },
                    )
                    .await?;

                Some(MaterialTexture {
                    key: texture_key,
                    sampler_key: Some(sampler_key),
                    uv_index: Some(uv_index as u32),
                    transform_key: texture_transform_key,
                })
            } else {
                None
            };

            extensions.clearcoat = Some(PbrMaterialClearCoat {
                tex,
                factor: clearcoat.clearcoat_factor(),
                roughness_tex,
                roughness_factor: clearcoat.clearcoat_roughness_factor(),
                normal_tex,
                normal_scale: clearcoat
                    .clearcoat_normal_texture()
                    .map(|n| n.scale())
                    .unwrap_or(1.0),
            });
        }

        #[cfg(feature = "sheen")]
        if let Some(sheen) = gltf_material.sheen() {
            let color_tex =
                if let Some(tex_info) = sheen.sheen_color_texture().map(GltfTextureInfo::from) {
                    let GLtfMaterialCacheKey {
                        uv_index,
                        texture_key,
                        sampler_key,
                        texture_transform_key,
                    } = tex_info
                        .create_material_cache_key(
                            renderer,
                            ctx,
                            TextureColorInfo {
                                mipmap_kind: MipmapTextureKind::Specular,
                                srgb_to_linear: true,
                                premultiplied_alpha: None,
                            },
                        )
                        .await?;

                    Some(MaterialTexture {
                        key: texture_key,
                        sampler_key: Some(sampler_key),
                        uv_index: Some(uv_index as u32),
                        transform_key: texture_transform_key,
                    })
                } else {
                    None
                };

            let roughness_tex = if let Some(tex_info) =
                sheen.sheen_roughness_texture().map(GltfTextureInfo::from)
            {
                let GLtfMaterialCacheKey {
                    uv_index,
                    texture_key,
                    sampler_key,
                    texture_transform_key,
                } = tex_info
                    .create_material_cache_key(
                        renderer,
                        ctx,
                        TextureColorInfo {
                            mipmap_kind: MipmapTextureKind::MetallicRoughness,
                            srgb_to_linear: false,
                            premultiplied_alpha: None,
                        },
                    )
                    .await?;

                Some(MaterialTexture {
                    key: texture_key,
                    sampler_key: Some(sampler_key),
                    uv_index: Some(uv_index as u32),
                    transform_key: texture_transform_key,
                })
            } else {
                None
            };

            extensions.sheen = Some(PbrMaterialSheen {
                color_factor: sheen.sheen_color_factor(),
                roughness_factor: sheen.sheen_roughness_factor(),
                roughness_tex,
                color_tex,
            });
        }

        // TODO:
        // pub diffuse_transmission: Option<PbrMaterialDiffuseTransmission>,
        // pub dispersion: Option<PbrMaterialDispersion>,
        // pub anisotropy: Option<PbrMaterialAnisotropy>,
        // pub iridescence: Option<PbrMaterialIridescence>,

        Ok(extensions)
    }
}

#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GltfTextureInfo {
    pub index: usize,
    pub tex_coord_index: usize,
    pub texture_transform: Option<GltfTextureTransform>,
}

#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GltfTextureTransform {
    // The offset of the UV coordinate origin as a factor of the texture dimensions.
    pub offset: [OrderedFloat<f32>; 2],

    /// Rotate the UVs by this many radians counter-clockwise around the origin.
    /// This is equivalent to a similar rotation of the image clockwise.
    pub rotation: OrderedFloat<f32>,

    /// The scale factor applied to the components of the UV coordinates.
    pub scale: [OrderedFloat<f32>; 2],
}

impl<'a> From<gltf::texture::Info<'a>> for GltfTextureInfo {
    fn from(info: gltf::texture::Info<'a>) -> Self {
        Self {
            index: info.texture().index(),
            tex_coord_index: match info.texture_transform().and_then(|x| x.tex_coord()) {
                Some(tex_coord_index) => tex_coord_index,
                None => info.tex_coord(),
            } as usize,
            texture_transform: info.texture_transform().map(GltfTextureTransform::from),
        }
    }
}

impl<'a> From<gltf::material::NormalTexture<'a>> for GltfTextureInfo {
    fn from(info: gltf::material::NormalTexture<'a>) -> Self {
        // Extract KHR_texture_transform from extensions if present
        let texture_transform = info.extensions().and_then(|ext| {
            ext.get("KHR_texture_transform").map(|transform_json| {
                // Parse the extension manually
                let offset = transform_json
                    .get("offset")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        [
                            arr.first().and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                            arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                        ]
                    })
                    .unwrap_or([0.0, 0.0]);

                let rotation = transform_json
                    .get("rotation")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;

                let scale = transform_json
                    .get("scale")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        [
                            arr.first().and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
                            arr.get(1).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
                        ]
                    })
                    .unwrap_or([1.0, 1.0]);

                GltfTextureTransform {
                    offset: [
                        ordered_float::OrderedFloat(offset[0]),
                        ordered_float::OrderedFloat(offset[1]),
                    ],
                    rotation: ordered_float::OrderedFloat(rotation),
                    scale: [
                        ordered_float::OrderedFloat(scale[0]),
                        ordered_float::OrderedFloat(scale[1]),
                    ],
                }
            })
        });

        let tex_coord_override = info
            .extensions()
            .and_then(|ext| ext.get("KHR_texture_transform"))
            .and_then(|t| t.get("texCoord"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        Self {
            index: info.texture().index(),
            tex_coord_index: tex_coord_override.unwrap_or(info.tex_coord() as usize),
            texture_transform,
        }
    }
}

impl<'a> From<gltf::material::OcclusionTexture<'a>> for GltfTextureInfo {
    fn from(info: gltf::material::OcclusionTexture<'a>) -> Self {
        // Extract KHR_texture_transform from extensions if present
        let texture_transform = info.extensions().and_then(|ext| {
            ext.get("KHR_texture_transform").map(|transform_json| {
                // Parse the extension manually
                let offset = transform_json
                    .get("offset")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        [
                            arr.first().and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                            arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                        ]
                    })
                    .unwrap_or([0.0, 0.0]);

                let rotation = transform_json
                    .get("rotation")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;

                let scale = transform_json
                    .get("scale")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        [
                            arr.first().and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
                            arr.get(1).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
                        ]
                    })
                    .unwrap_or([1.0, 1.0]);

                GltfTextureTransform {
                    offset: [
                        ordered_float::OrderedFloat(offset[0]),
                        ordered_float::OrderedFloat(offset[1]),
                    ],
                    rotation: ordered_float::OrderedFloat(rotation),
                    scale: [
                        ordered_float::OrderedFloat(scale[0]),
                        ordered_float::OrderedFloat(scale[1]),
                    ],
                }
            })
        });

        let tex_coord_override = info
            .extensions()
            .and_then(|ext| ext.get("KHR_texture_transform"))
            .and_then(|t| t.get("texCoord"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        Self {
            index: info.texture().index(),
            tex_coord_index: tex_coord_override.unwrap_or(info.tex_coord() as usize),
            texture_transform,
        }
    }
}

impl<'a> From<gltf::texture::TextureTransform<'a>> for GltfTextureTransform {
    fn from(transform: gltf::texture::TextureTransform<'a>) -> Self {
        Self {
            offset: [
                OrderedFloat(transform.offset()[0]),
                OrderedFloat(transform.offset()[1]),
            ],
            rotation: OrderedFloat(transform.rotation()),
            scale: [
                OrderedFloat(transform.scale()[0]),
                OrderedFloat(transform.scale()[1]),
            ],
        }
    }
}

/// Cache key for glTF material textures and samplers.
pub struct GLtfMaterialCacheKey {
    pub uv_index: usize,
    pub texture_key: TextureKey,
    pub sampler_key: SamplerKey,
    pub texture_transform_key: Option<TextureTransformKey>,
}
impl GltfTextureInfo {
    pub async fn create_material_cache_key(
        &self,
        renderer: &mut AwsmRenderer,
        ctx: &GltfPopulateContext,
        color: TextureColorInfo,
    ) -> Result<GLtfMaterialCacheKey> {
        let lookup_key = GltfTextureKey {
            index: self.index,
            color,
        };

        let sampler_key = self.create_sampler_key(renderer, ctx)?;

        let texture_key = {
            let textures = ctx.textures.lock().unwrap();
            textures.get(&lookup_key).cloned()
        };

        let texture_key = match texture_key {
            Some(texture_key) => texture_key,
            None => {
                let gltf_texture = ctx
                    .data
                    .doc
                    .textures()
                    .nth(self.index)
                    .ok_or(AwsmGltfError::MissingTextureDocIndex(self.index))?;
                let texture_index = gltf_texture.source().index();
                let image_data = ctx
                    .data
                    .images
                    .get(texture_index)
                    .ok_or(AwsmGltfError::MissingTextureIndex(texture_index))?;

                let texture_key = renderer.textures.add_image(
                    image_data.clone(),
                    image_data.format(),
                    sampler_key,
                    color,
                )?;

                ctx.textures.lock().unwrap().insert(lookup_key, texture_key);

                texture_key
            }
        };

        let texture_transform_key = match self.texture_transform {
            None => None,
            Some(texture_transform) => Some(renderer.textures.insert_texture_transform(
                &TextureTransform {
                    offset: [*texture_transform.offset[0], *texture_transform.offset[1]],
                    origin: [0.0, 0.0],
                    rotation: *texture_transform.rotation,
                    scale: [*texture_transform.scale[0], *texture_transform.scale[1]],
                },
            )),
        };

        Ok(GLtfMaterialCacheKey {
            uv_index: self.tex_coord_index,
            texture_key,
            sampler_key,
            texture_transform_key,
        })
    }

    fn create_sampler_key(
        &self,
        renderer: &mut AwsmRenderer,
        ctx: &GltfPopulateContext,
    ) -> Result<SamplerKey> {
        let gltf_texture = ctx
            .data
            .doc
            .textures()
            .nth(self.index)
            .ok_or(AwsmGltfError::MissingTextureDocIndex(self.index))?;
        let gltf_sampler = gltf_texture.sampler();

        let mut sampler_cache_key = SamplerCacheKey {
            // This looks better with our mipmap generation...
            // if it's overridden by the glTF sampler, fine.
            // but otherwise, let's just do what looks best.
            min_filter: Some(FilterMode::Linear),
            mag_filter: Some(FilterMode::Linear),
            mipmap_filter: Some(MipmapFilterMode::Linear),
            // Enable anisotropic filtering for thin lines at oblique angles
            // Without this, textures become severely aliased when viewed at angles
            max_anisotropy: Some(16),
            ..Default::default()
        };
        // glTF allows omitting the wrap mode; the spec states the default is repeat. Record that
        // here so downstream shader logic can faithfully emulate it if the sampler isn't cached yet.
        sampler_cache_key.address_mode_u = Some(AddressMode::Repeat);
        sampler_cache_key.address_mode_v = Some(AddressMode::Repeat);
        sampler_cache_key.address_mode_w = Some(AddressMode::Repeat);

        if let Some(mag_filter) = gltf_sampler.mag_filter() {
            match mag_filter {
                gltf::texture::MagFilter::Linear => {
                    sampler_cache_key.mag_filter = Some(FilterMode::Linear)
                }
                gltf::texture::MagFilter::Nearest => {
                    sampler_cache_key.mag_filter = Some(FilterMode::Nearest)
                }
            }
        }

        if let Some(min_filter) = gltf_sampler.min_filter() {
            match min_filter {
                gltf::texture::MinFilter::Linear => {
                    sampler_cache_key.min_filter = Some(FilterMode::Linear)
                }
                gltf::texture::MinFilter::Nearest => {
                    sampler_cache_key.min_filter = Some(FilterMode::Nearest)
                }
                gltf::texture::MinFilter::NearestMipmapNearest => {
                    sampler_cache_key.min_filter = Some(FilterMode::Nearest);
                    sampler_cache_key.mipmap_filter = Some(MipmapFilterMode::Nearest);
                }
                gltf::texture::MinFilter::LinearMipmapNearest => {
                    sampler_cache_key.min_filter = Some(FilterMode::Linear);
                    sampler_cache_key.mipmap_filter = Some(MipmapFilterMode::Nearest);
                }
                gltf::texture::MinFilter::NearestMipmapLinear => {
                    sampler_cache_key.min_filter = Some(FilterMode::Nearest);
                    sampler_cache_key.mipmap_filter = Some(MipmapFilterMode::Linear);
                }
                gltf::texture::MinFilter::LinearMipmapLinear => {
                    sampler_cache_key.min_filter = Some(FilterMode::Linear);
                    sampler_cache_key.mipmap_filter = Some(MipmapFilterMode::Linear);
                }
            }
        }

        match gltf_sampler.wrap_s() {
            gltf::texture::WrappingMode::ClampToEdge => {
                sampler_cache_key.address_mode_u = Some(AddressMode::ClampToEdge)
            }
            gltf::texture::WrappingMode::MirroredRepeat => {
                sampler_cache_key.address_mode_u = Some(AddressMode::MirrorRepeat)
            }
            gltf::texture::WrappingMode::Repeat => {
                sampler_cache_key.address_mode_u = Some(AddressMode::Repeat)
            }
        }

        match gltf_sampler.wrap_t() {
            gltf::texture::WrappingMode::ClampToEdge => {
                sampler_cache_key.address_mode_v = Some(AddressMode::ClampToEdge)
            }
            gltf::texture::WrappingMode::MirroredRepeat => {
                sampler_cache_key.address_mode_v = Some(AddressMode::MirrorRepeat)
            }
            gltf::texture::WrappingMode::Repeat => {
                sampler_cache_key.address_mode_v = Some(AddressMode::Repeat)
            }
        }

        if !sampler_cache_key.allowed_ansiotropy() {
            //tracing::warn!("Disabling max ansiotropy!");
            sampler_cache_key.max_anisotropy = None;
        }

        Ok(renderer
            .textures
            .get_sampler_key(&renderer.gpu, sampler_cache_key)?)
    }
}
