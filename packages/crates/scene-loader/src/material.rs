//! Pure `MaterialDef` → renderer `Material` conversion — the single source of
//! truth shared by the editor bridge (live render) and `populate_awsm_scene`
//! (the runtime-bundle / player load). Keeping one copy is what makes the
//! round-trip meaningful: if the editor and the player lowered a `MaterialDef`
//! differently, comparing their renders would flag spurious diffs.
//!
//! Texture-less by design: it maps factors / alpha / double-sided / vertex-
//! colors / KHR-extension variant bits, which is exactly what a thumbnail or a
//! built-in-only load wants. Texture *binding* (uploading + slotting images)
//! stays with each caller, since the texture source differs — the editor
//! resolves procedural/asset textures against its session pool, the player
//! resolves `assets/<id>.png` bytes from the bundle.

use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::toon::ToonMaterial;
use awsm_renderer::materials::unlit::UnlitMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode};
use awsm_scene::{MaterialDef, MaterialShading, PbrExtensions};

/// Wrap an authored [`MaterialDef`] into the renderer's `Material` enum,
/// dispatching on the shading model so a built-in material's *variant* (its
/// shader-generation choice) actually renders. Texture-less; the caller binds
/// texture slots afterward.
pub fn material_to_renderer(def: &MaterialDef) -> Material {
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
        MaterialShading::FlipBook {
            cols,
            rows,
            frame_count,
            fps,
            time_offset,
            mode,
            flip_y,
        } => {
            use awsm_renderer::materials::flipbook::{FlipBookMaterial, FlipBookMode};
            let mut m = FlipBookMaterial::new(alpha_mode, def.double_sided);
            m.tint = def.base_color;
            m.cols = cols;
            m.rows = rows;
            m.frame_count = frame_count;
            m.fps = fps;
            m.time_offset = time_offset;
            m.mode = match mode {
                awsm_scene::FlipBookPlayMode::Loop => FlipBookMode::Loop,
                awsm_scene::FlipBookPlayMode::PingPong => FlipBookMode::PingPong,
                awsm_scene::FlipBookPlayMode::Clamp => FlipBookMode::Clamp,
                awsm_scene::FlipBookPlayMode::Once => FlipBookMode::Once,
            };
            m.flip_y = flip_y;
            // The atlas rides the BASE-COLOR texture slot; binding happens at
            // the caller (editor session pool / player bundle bytes), exactly
            // like PBR's texture slots.
            Material::FlipBook(Box::new(m))
        }
    }
}

/// Build a texture-less [`PbrMaterial`] from a [`MaterialDef`]. `vertex_color_set`
/// is the geometry `COLOR_n` set to sample when vertex colours are enabled (the
/// editor passes the index it detected from the mesh; the player passes the set
/// the glb declares).
pub fn material_to_pbr(
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

/// Resolve the authored alpha mode to the renderer's, applying the legacy
/// "`Opaque` but `base_color.a < 1` ⇒ blend" heuristic the editor has always
/// used for inline procedural materials.
pub fn alpha_mode_of(def: &MaterialDef) -> MaterialAlphaMode {
    match def.alpha_mode {
        awsm_scene::MaterialAlphaMode::Opaque => {
            if def.base_color[3] < 0.999 {
                MaterialAlphaMode::Blend
            } else {
                MaterialAlphaMode::Opaque
            }
        }
        awsm_scene::MaterialAlphaMode::Mask { cutoff } => MaterialAlphaMode::Mask { cutoff },
        awsm_scene::MaterialAlphaMode::Blend => MaterialAlphaMode::Blend,
    }
}

/// Translate each enabled authored KHR extension onto the renderer's `PbrMaterial`
/// `Option<…Extension>` fields. Presence = the variant bit (a distinct compiled
/// shader); the scalar/color factors are the uniform values. Texture slots within
/// each extension stay `None` here (the caller binds them).
fn apply_extensions(pbr: &mut PbrMaterial, ext: &PbrExtensions) {
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
