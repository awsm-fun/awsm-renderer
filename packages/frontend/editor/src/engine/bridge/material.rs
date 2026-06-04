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
        } => {
            let mut m = ToonMaterial::new(alpha_mode, def.double_sided);
            m.base_color_factor = def.base_color;
            m.emissive_factor = def.emissive;
            m.diffuse_bands = diffuse_bands.max(1);
            m.rim_strength = rim_strength;
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
    pbr
}
