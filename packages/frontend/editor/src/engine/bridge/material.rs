//! `MaterialDef` → renderer `Material` conversion. Dispatches on the authored
//! shading model (PBR / Unlit / Toon) so a built-in material's *variant* (its
//! shader-generation choice) actually renders. Texture binding resolution is the
//! follow-on; factors/alpha/double-sided/vertex-colors are wired here.

use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::toon::ToonMaterial;
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::AwsmRenderer;
use awsm_scene_schema::{MaterialDef, MaterialShading};

/// Wrap an authored `MaterialDef` into the renderer's `Material` enum + insert
/// it, returning the `MaterialKey`.
pub fn insert_material(renderer: &mut AwsmRenderer, def: &MaterialDef) -> MaterialKey {
    let material = material_to_renderer(def);
    renderer.materials.insert(
        material,
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    )
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
/// variant (previously every `MaterialDef` collapsed to PBR).
fn material_to_renderer(def: &MaterialDef) -> Material {
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
        MaterialShading::Pbr => Material::Pbr(Box::new(material_to_pbr(def, alpha_mode))),
    }
}

fn material_to_pbr(def: &MaterialDef, alpha_mode: MaterialAlphaMode) -> PbrMaterial {
    let mut pbr = PbrMaterial::new(alpha_mode, def.double_sided);
    pbr.base_color_factor = def.base_color;
    pbr.metallic_factor = def.metallic;
    pbr.roughness_factor = def.roughness;
    pbr.emissive_factor = def.emissive;
    if def.vertex_colors_enabled {
        pbr.vertex_color_info =
            Some(awsm_renderer::materials::pbr::PbrMaterialVertexColorInfo { set_index: 0 });
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
            normal_scale: 1.0,
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
