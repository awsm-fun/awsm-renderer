//! Slim `MaterialDef` → renderer `Material` conversion. M4-C handles the
//! textureless PBR factors (base color / metallic / roughness / emissive /
//! alpha / double-sided / vertex-colors) — enough for inserted primitives'
//! inline materials. Texture resolution (base-color/normal/etc. maps) lands
//! with the texture cache in M7/M8.

use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode};
use awsm_renderer::AwsmRenderer;
use awsm_scene_schema::MaterialDef;

/// Wrap an authored `MaterialDef` into the renderer's `Material` enum + insert
/// it, returning the `MaterialKey`.
pub fn insert_material(
    renderer: &mut AwsmRenderer,
    def: &MaterialDef,
) -> awsm_renderer::materials::MaterialKey {
    let material = Material::Pbr(Box::new(material_to_pbr(def)));
    renderer.materials.insert(
        material,
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    )
}

fn material_to_pbr(def: &MaterialDef) -> PbrMaterial {
    let alpha_mode = match def.alpha_mode {
        awsm_scene_schema::MaterialAlphaMode::Opaque => {
            // Back-compat: an explicit base-color alpha < 1 implies Blend intent.
            if def.base_color[3] < 0.999 {
                MaterialAlphaMode::Blend
            } else {
                MaterialAlphaMode::Opaque
            }
        }
        awsm_scene_schema::MaterialAlphaMode::Mask { cutoff } => MaterialAlphaMode::Mask { cutoff },
        awsm_scene_schema::MaterialAlphaMode::Blend => MaterialAlphaMode::Blend,
    };
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
