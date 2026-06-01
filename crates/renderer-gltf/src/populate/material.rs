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
    // `AWSM_material_none` (per-material extension, Decision 7): a
    // geometry-only primitive that opts out of PBR shading entirely. Route
    // it to the shared flat/Unlit bucket (Decision 8) — visible + cheap,
    // and crucially it builds NO PbrMaterial, so a load made only of
    // material-none primitives fires ZERO PBR shader compiles (criterion
    // 6). Checked BEFORE `pbr_material_mapper_core` so none of the PBR
    // texture / extension work runs.
    if gltf_material
        .extension_value("AWSM_material_none")
        .is_some()
    {
        let unlit = UnlitMaterial::new(MaterialAlphaMode::Opaque, gltf_material.double_sided());
        return Ok(Material::Unlit(unlit));
    }

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

    // Raw-JSON material node, used below to read KHR_texture_transform for the
    // normal/occlusion textures (see `normal_occlusion_texture_info`).
    let mat_json = gltf_material
        .index()
        .and_then(|i| ctx.data.doc.as_json().materials.get(i));

    if let Some(tex) = gltf_material.normal_texture().map(|n| {
        normal_occlusion_texture_info(
            n.texture().index(),
            n.tex_coord(),
            mat_json
                .and_then(|m| m.normal_texture.as_ref())
                .and_then(|nt| nt.extensions.as_ref())
                .and_then(|e| e.others.get("KHR_texture_transform")),
        )
    }) {
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

    if let Some(tex) = gltf_material.occlusion_texture().map(|o| {
        normal_occlusion_texture_info(
            o.texture().index(),
            o.tex_coord(),
            mat_json
                .and_then(|m| m.occlusion_texture.as_ref())
                .and_then(|ot| ot.extensions.as_ref())
                .and_then(|e| e.others.get("KHR_texture_transform")),
        )
    }) {
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

        // KHR_materials_clearcoat / KHR_materials_sheen are parsed from the
        // raw extensions JSON (like the others below) rather than the `gltf`
        // crate's typed accessors, which are unreleased on crates.io. Going
        // through `load_json_texture` also routes the clearcoat normal map
        // through the same texture path as every other extension.
        if let Some(value) = gltf_material.extension_value("KHR_materials_clearcoat") {
            let tex = load_json_texture(
                renderer,
                ctx,
                value.get("clearcoatTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Albedo,
                    srgb_to_linear: false,
                    premultiplied_alpha: None,
                },
            )
            .await?;
            let roughness_tex = load_json_texture(
                renderer,
                ctx,
                value.get("clearcoatRoughnessTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::MetallicRoughness,
                    srgb_to_linear: false,
                    premultiplied_alpha: None,
                },
            )
            .await?;
            let normal_tex = load_json_texture(
                renderer,
                ctx,
                value.get("clearcoatNormalTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Normal,
                    srgb_to_linear: false,
                    premultiplied_alpha: None,
                },
            )
            .await?;
            // `scale` lives on the normal textureInfo object (default 1.0).
            let normal_scale = value
                .get("clearcoatNormalTexture")
                .map(|t| read_f32(t, "scale", 1.0))
                .unwrap_or(1.0);

            extensions.clearcoat = Some(PbrMaterialClearCoat {
                tex,
                factor: read_f32(value, "clearcoatFactor", 0.0),
                roughness_tex,
                roughness_factor: read_f32(value, "clearcoatRoughnessFactor", 0.0),
                normal_tex,
                normal_scale,
            });
        }

        if let Some(value) = gltf_material.extension_value("KHR_materials_sheen") {
            let color_tex = load_json_texture(
                renderer,
                ctx,
                value.get("sheenColorTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Specular,
                    srgb_to_linear: true,
                    premultiplied_alpha: None,
                },
            )
            .await?;
            let roughness_tex = load_json_texture(
                renderer,
                ctx,
                value.get("sheenRoughnessTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::MetallicRoughness,
                    srgb_to_linear: false,
                    premultiplied_alpha: None,
                },
            )
            .await?;

            extensions.sheen = Some(PbrMaterialSheen {
                color_factor: read_color3(value, "sheenColorFactor", [0.0, 0.0, 0.0]),
                roughness_factor: read_f32(value, "sheenRoughnessFactor", 0.0),
                roughness_tex,
                color_tex,
            });
        }

        // The remaining extensions aren't yet exposed by the `gltf` crate, so
        // we read them straight off the extensions JSON map.
        if let Some(value) = gltf_material.extension_value("KHR_materials_dispersion") {
            let dispersion = read_f32(value, "dispersion", 0.0);
            extensions.dispersion = Some(PbrMaterialDispersion { dispersion });
        }

        if let Some(value) = gltf_material.extension_value("KHR_materials_diffuse_transmission") {
            let factor = read_f32(value, "diffuseTransmissionFactor", 0.0);
            let color_factor =
                read_color3(value, "diffuseTransmissionColorFactor", [1.0, 1.0, 1.0]);

            let tex = load_json_texture(
                renderer,
                ctx,
                value.get("diffuseTransmissionTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Albedo,
                    srgb_to_linear: false,
                    premultiplied_alpha: None,
                },
            )
            .await?;
            let color_tex = load_json_texture(
                renderer,
                ctx,
                value.get("diffuseTransmissionColorTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Albedo,
                    srgb_to_linear: true,
                    premultiplied_alpha: None,
                },
            )
            .await?;

            extensions.diffuse_transmission = Some(PbrMaterialDiffuseTransmission {
                tex,
                factor,
                color_tex,
                color_factor,
            });
        }

        if let Some(value) = gltf_material.extension_value("KHR_materials_anisotropy") {
            let strength = read_f32(value, "anisotropyStrength", 0.0);
            let rotation = read_f32(value, "anisotropyRotation", 0.0);
            let tex = load_json_texture(
                renderer,
                ctx,
                value.get("anisotropyTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Normal,
                    srgb_to_linear: false,
                    premultiplied_alpha: None,
                },
            )
            .await?;
            extensions.anisotropy = Some(PbrMaterialAnisotropy {
                tex,
                strength,
                rotation,
            });
        }

        if let Some(value) = gltf_material.extension_value("KHR_materials_iridescence") {
            let factor = read_f32(value, "iridescenceFactor", 0.0);
            let ior = read_f32(value, "iridescenceIor", 1.3);
            let thickness_min = read_f32(value, "iridescenceThicknessMinimum", 100.0);
            let thickness_max = read_f32(value, "iridescenceThicknessMaximum", 400.0);
            let tex = load_json_texture(
                renderer,
                ctx,
                value.get("iridescenceTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Albedo,
                    srgb_to_linear: false,
                    premultiplied_alpha: None,
                },
            )
            .await?;
            let thickness_tex = load_json_texture(
                renderer,
                ctx,
                value.get("iridescenceThicknessTexture"),
                TextureColorInfo {
                    mipmap_kind: MipmapTextureKind::Albedo,
                    srgb_to_linear: false,
                    premultiplied_alpha: None,
                },
            )
            .await?;
            extensions.iridescence = Some(PbrMaterialIridescence {
                tex,
                factor,
                ior,
                thickness_tex,
                thickness_min,
                thickness_max,
            });
        }

        Ok(extensions)
    }
}

fn read_f32(value: &gltf::json::Value, key: &str, default: f32) -> f32 {
    value
        .get(key)
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(default)
}

fn read_color3(value: &gltf::json::Value, key: &str, default: [f32; 3]) -> [f32; 3] {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            [
                arr.first()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(default[0] as f64) as f32,
                arr.get(1)
                    .and_then(|v| v.as_f64())
                    .unwrap_or(default[1] as f64) as f32,
                arr.get(2)
                    .and_then(|v| v.as_f64())
                    .unwrap_or(default[2] as f64) as f32,
            ]
        })
        .unwrap_or(default)
}

fn parse_json_texture_info(value: &gltf::json::Value) -> Option<GltfTextureInfo> {
    let index = value.get("index").and_then(|v| v.as_u64())? as usize;
    let base_tex_coord = value.get("texCoord").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let transform_json = value
        .get("extensions")
        .and_then(|ext| ext.get("KHR_texture_transform"));

    let texture_transform = transform_json.map(|t| {
        let offset = t
            .get("offset")
            .and_then(|v| v.as_array())
            .map(|arr| {
                [
                    arr.first().and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                ]
            })
            .unwrap_or([0.0, 0.0]);
        let rotation = t.get("rotation").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let scale = t
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
            offset: [OrderedFloat(offset[0]), OrderedFloat(offset[1])],
            rotation: OrderedFloat(rotation),
            scale: [OrderedFloat(scale[0]), OrderedFloat(scale[1])],
        }
    });

    let tex_coord_index = transform_json
        .and_then(|t| t.get("texCoord"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(base_tex_coord);

    Some(GltfTextureInfo {
        index,
        tex_coord_index,
        texture_transform,
    })
}

async fn load_json_texture(
    renderer: &mut AwsmRenderer,
    ctx: &GltfPopulateContext,
    value: Option<&gltf::json::Value>,
    color: TextureColorInfo,
) -> Result<Option<MaterialTexture>> {
    let Some(value) = value else { return Ok(None) };
    let Some(info) = parse_json_texture_info(value) else {
        return Ok(None);
    };
    let GLtfMaterialCacheKey {
        uv_index,
        texture_key,
        sampler_key,
        texture_transform_key,
    } = info.create_material_cache_key(renderer, ctx, color).await?;
    Ok(Some(MaterialTexture {
        key: texture_key,
        sampler_key: Some(sampler_key),
        uv_index: Some(uv_index as u32),
        transform_key: texture_transform_key,
    }))
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

/// Builds a `GltfTextureInfo` for a normal/occlusion texture. `index` and
/// `base_tex_coord` come from the released high-level accessors; the
/// `transform_json` (KHR_texture_transform) is read from the raw glTF JSON
/// (`Document::as_json()`) because `NormalTexture`/`OcclusionTexture::
/// texture_transform()` are *unreleased* gltf-crate accessors (git-only).
/// Reading the JSON field directly keeps us on crates.io `gltf` 1.4.1.
fn normal_occlusion_texture_info(
    index: usize,
    base_tex_coord: u32,
    transform_json: Option<&gltf::json::Value>,
) -> GltfTextureInfo {
    let read2 = |t: &gltf::json::Value, key: &str, default: f32| {
        let a = t.get(key).and_then(|v| v.as_array());
        [
            OrderedFloat(
                a.and_then(|x| x.first())
                    .and_then(|v| v.as_f64())
                    .unwrap_or(default as f64) as f32,
            ),
            OrderedFloat(
                a.and_then(|x| x.get(1))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(default as f64) as f32,
            ),
        ]
    };
    let texture_transform = transform_json.map(|t| GltfTextureTransform {
        offset: read2(t, "offset", 0.0),
        rotation: OrderedFloat(read_f32(t, "rotation", 0.0)),
        scale: read2(t, "scale", 1.0),
    });
    let tex_coord_index = transform_json
        .and_then(|t| t.get("texCoord"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(base_tex_coord as usize);
    GltfTextureInfo {
        index,
        tex_coord_index,
        texture_transform,
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
