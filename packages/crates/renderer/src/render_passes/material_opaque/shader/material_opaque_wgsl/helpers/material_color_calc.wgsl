
{# Skinny materials: the PBR PbrMaterialColor builder. Gated so non-PBR
   pipelines (which call compute_unlit_material_color below instead) don't
   compile ~700 lines of PBR texture/extension sampling. The unlit builder
   further down is ungated. #}
{% if inc.material_color_calc %}
{% if mipmap.is_gradient() %}
struct PbrMaterialGradients {
    base_color: UvDerivs,
    metallic_roughness: UvDerivs,
    normal: UvDerivs,
    occlusion: UvDerivs,
    emissive: UvDerivs,
    specular: UvDerivs,
    specular_color: UvDerivs,
    transmission: UvDerivs,
    volume_thickness: UvDerivs,
    // KHR_materials_clearcoat
    clearcoat: UvDerivs,
    clearcoat_roughness: UvDerivs,
    clearcoat_normal: UvDerivs,
    // KHR_materials_sheen
    sheen_color: UvDerivs,
    sheen_roughness: UvDerivs,
    // KHR_materials_diffuse_transmission
    diffuse_transmission: UvDerivs,
    diffuse_transmission_color: UvDerivs,
    // KHR_materials_anisotropy
    anisotropy: UvDerivs,
    // KHR_materials_iridescence
    iridescence: UvDerivs,
    iridescence_thickness: UvDerivs,
    {% if pbr_features.secondary_maps %}
    // Secondary / detail maps (engine extension)
    secondary_base_color: UvDerivs,
    secondary_normal: UvDerivs,
    secondary_metallic_roughness: UvDerivs,
    secondary_occlusion: UvDerivs,
    secondary_emissive: UvDerivs,
    {% endif %}
}
{% endif %}

// Main PBR material color function - samples all textures and computes final material properties
// Returns PbrMaterialColor with perturbed normal (use material_color.normal for lighting!)
fn pbr_get_material_color{{ mipmap.suffix() }}(
    triangle_indices: vec3<u32>,
    attribute_data_offset: u32,
    triangle_index: u32,
    material: PbrMaterial,
    barycentric: vec3<f32>,
    vertex_attribute_stride: u32,
    uv_sets_index: u32,
    color_sets_index: u32,
    {% if mipmap.is_gradient() %}gradients: PbrMaterialGradients,{% endif %}
    geometry_tbn: TBN,
) -> PbrMaterialColor {
    // Load extension data on-demand from indices. Each is gated so a
    // feature-set without the extension never computes it — the dead
    // local then DCE-cascades up through its sampler + this load.
    {% if pbr_features.emissive_strength %}
    let emissive_strength = pbr_material_load_emissive_strength(material.emissive_strength_index);
    {% else %}
    let emissive_strength = 1.0;
    {% endif %}
    let ior = pbr_material_load_ior(material.ior_index);
    let specular = pbr_material_load_specular(material.specular_index);
    let transmission = pbr_material_load_transmission(material.transmission_index);
    let volume = pbr_material_load_volume(material.volume_index);
    let clearcoat = pbr_material_load_clearcoat(material.clearcoat_index);
    let sheen = pbr_material_load_sheen(material.sheen_index);
    let dispersion = pbr_material_load_dispersion(material.dispersion_index);
    let diffuse_trans = pbr_material_load_diffuse_transmission(material.diffuse_transmission_index);
    let anisotropy = pbr_material_load_anisotropy(material.anisotropy_index);
    let iridescence = pbr_material_load_iridescence(material.iridescence_index);
    {% if pbr_features.secondary_maps %}
    let secondary = pbr_material_load_secondary_maps(material.secondary_maps_index);
    {% endif %}

    var base = _pbr_material_base_color{{ mipmap.suffix() }}(
        material,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            material.base_color_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.base_color,{% endif %}
    );

    // Multiply base color by vertex color if present (index 0 means absent).
    // Compile-time gated so feature-sets without vertex colors emit none
    // of this.
    {% if pbr_features.vertex_color %}
    if (material.vertex_color_info_index != 0u) {
        let vertex_color_info = pbr_material_load_vertex_color_info(material.vertex_color_info_index);
        base *= vertex_color(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            vertex_color_info,
            vertex_attribute_stride,
            color_sets_index,
        );
    }
    {% endif %}

    {% if pbr_features.secondary_maps %}
    // Secondary base color: x2 multiply overlay (mid-grey neutral). Skip
    // branch is legal here: explicit-gradient sampling has no
    // uniform-control-flow requirement, and the flag is uniform per material.
    if (secondary.base_color_tex_info.exists) {
        let s_uv = texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            secondary.base_color_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        );
        let s_tex = {{ mipmap.sample_fn() }}(secondary.base_color_tex_info, s_uv{% if mipmap.is_gradient() %}, gradients.secondary_base_color{% endif %});
        let detail = mix(vec3<f32>(1.0), s_tex.rgb * 2.0, secondary.base_color_strength);
        base = vec4<f32>(base.rgb * detail, base.a);
    }
    {% endif %}

    var metallic_roughness = _pbr_material_metallic_roughness_color{{ mipmap.suffix() }}(
        material,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            material.metallic_roughness_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.metallic_roughness,{% endif %}
    );

    {% if pbr_features.secondary_maps %}
    // Secondary metallic-roughness: roughness (G) overlay-blends
    // (mid-grey neutral), metallic (B) multiplies (white neutral).
    if (secondary.metallic_roughness_tex_info.exists) {
        let s_uv = texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            secondary.metallic_roughness_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        );
        let s_tex = {{ mipmap.sample_fn() }}(secondary.metallic_roughness_tex_info, s_uv{% if mipmap.is_gradient() %}, gradients.secondary_metallic_roughness{% endif %});
        let detail_rough = mix(0.5, s_tex.g, secondary.metallic_roughness_strength);
        let detail_metal = mix(1.0, s_tex.b, secondary.metallic_roughness_strength);
        metallic_roughness = vec2<f32>(
            metallic_roughness.x * detail_metal,
            _pbr_overlay_scalar(metallic_roughness.y, detail_rough),
        );
    }
    {% endif %}

    let normal = _pbr_normal_color{{ mipmap.suffix() }}(
        material,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            material.normal_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.normal,{% endif %}
        geometry_tbn,
        {% if pbr_features.secondary_maps %}
        secondary,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            secondary.normal_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.secondary_normal,{% endif %}
        {% endif %}
    );

    var occlusion = _pbr_occlusion_color{{ mipmap.suffix() }}(
        material,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            material.occlusion_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.occlusion,{% endif %}
    );

    {% if pbr_features.secondary_maps %}
    // Secondary occlusion: cavity multiply (white neutral).
    if (secondary.occlusion_tex_info.exists) {
        let s_uv = texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            secondary.occlusion_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        );
        let s_tex = {{ mipmap.sample_fn() }}(secondary.occlusion_tex_info, s_uv{% if mipmap.is_gradient() %}, gradients.secondary_occlusion{% endif %});
        occlusion *= mix(1.0, s_tex.r, secondary.occlusion_strength);
    }
    {% endif %}

    var emissive = _pbr_material_emissive_color{{ mipmap.suffix() }}(
        material,
        emissive_strength,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            material.emissive_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.emissive,{% endif %}
    );

    {% if pbr_features.secondary_maps %}
    // Secondary emissive: additive (black neutral).
    if (secondary.emissive_tex_info.exists) {
        let s_uv = texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            secondary.emissive_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        );
        let s_tex = {{ mipmap.sample_fn() }}(secondary.emissive_tex_info, s_uv{% if mipmap.is_gradient() %}, gradients.secondary_emissive{% endif %});
        emissive += mix(vec3<f32>(0.0), s_tex.rgb, secondary.emissive_strength);
    }
    {% endif %}

    let specular_factor = _pbr_specular{{ mipmap.suffix() }}(
        specular,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            specular.tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.specular,{% endif %}
    );

    let specular_color_factor = _pbr_specular_color{{ mipmap.suffix() }}(
        specular,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            specular.color_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.specular_color,{% endif %}
    );

    let transmission_factor = _pbr_transmission{{ mipmap.suffix() }}(
        transmission,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            transmission.tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.transmission,{% endif %}
    );

    let volume_thickness = _pbr_volume_thickness{{ mipmap.suffix() }}(
        volume,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            volume.thickness_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.volume_thickness,{% endif %}
    );

    // Clearcoat sampling
    let clearcoat_factor = _pbr_clearcoat{{ mipmap.suffix() }}(
        clearcoat,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            clearcoat.tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.clearcoat,{% endif %}
    );

    let clearcoat_roughness_factor = _pbr_clearcoat_roughness{{ mipmap.suffix() }}(
        clearcoat,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            clearcoat.roughness_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.clearcoat_roughness,{% endif %}
    );

    let clearcoat_normal_value = _pbr_clearcoat_normal{{ mipmap.suffix() }}(
        clearcoat,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            clearcoat.normal_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.clearcoat_normal,{% endif %}
        geometry_tbn,
        (material.normal_packing >> 2u) & 3u,
    );

    // Sheen sampling
    let sheen_color_factor = _pbr_sheen_color{{ mipmap.suffix() }}(
        sheen,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            sheen.color_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.sheen_color,{% endif %}
    );

    let sheen_roughness_factor = _pbr_sheen_roughness{{ mipmap.suffix() }}(
        sheen,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            sheen.roughness_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.sheen_roughness,{% endif %}
    );

    // Diffuse transmission
    let diffuse_trans_factor = _pbr_diffuse_transmission{{ mipmap.suffix() }}(
        diffuse_trans,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            diffuse_trans.tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.diffuse_transmission,{% endif %}
    );

    let diffuse_trans_color = _pbr_diffuse_transmission_color{{ mipmap.suffix() }}(
        diffuse_trans,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            diffuse_trans.color_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.diffuse_transmission_color,{% endif %}
    );

    // Anisotropy
    let aniso_basis = _pbr_anisotropy_basis{{ mipmap.suffix() }}(
        anisotropy,
        geometry_tbn,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            anisotropy.tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.anisotropy,{% endif %}
    );

    // Iridescence
    let iridescence_factor = _pbr_iridescence_factor{{ mipmap.suffix() }}(
        iridescence,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            iridescence.tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.iridescence,{% endif %}
    );

    let iridescence_thickness = _pbr_iridescence_thickness{{ mipmap.suffix() }}(
        iridescence,
        texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            iridescence.thickness_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        ),
        {% if mipmap.is_gradient() %}gradients.iridescence_thickness,{% endif %}
    );

    // Per-feature gating: an off feature emits a compile-time
    // CONSTANT here instead of its computed local. The local (and its
    // sampler + extension load above) then become dead code the compiler
    // eliminates — the win is the dropped register pressure, not just
    // skipped ALU. `ior`/`specular` feed the base F0 unconditionally, so
    // their off-defaults are the glTF-absent values (ior 1.5, specular
    // 1.0, specular_color 1). The additive lobes get 0; their brdf
    // contribution is either explicitly gated (sheen/clearcoat) or
    // const-folds away via the existing `if (color.x > 0)` runtime guard.
    return PbrMaterialColor(
        base,
        metallic_roughness,
        normal,
        occlusion,
        emissive,
        // KHR_materials_specular (feeds base F0)
        {% if pbr_features.specular %}specular_factor{% else %}1.0{% endif %},
        {% if pbr_features.specular %}specular_color_factor{% else %}vec3<f32>(1.0){% endif %},
        // KHR_materials_ior (feeds base F0)
        {% if pbr_features.ior %}ior{% else %}1.5{% endif %},
        // KHR_materials_transmission
        {% if pbr_features.transmission %}transmission_factor{% else %}0.0{% endif %},
        // KHR_materials_volume
        {% if pbr_features.volume %}volume_thickness{% else %}0.0{% endif %},
        {% if pbr_features.volume %}volume.attenuation_distance{% else %}0.0{% endif %},
        {% if pbr_features.volume %}volume.attenuation_color{% else %}vec3<f32>(1.0){% endif %},
        // Clearcoat
        {% if pbr_features.clearcoat %}clearcoat_factor{% else %}0.0{% endif %},
        {% if pbr_features.clearcoat %}clearcoat_roughness_factor{% else %}0.0{% endif %},
        {% if pbr_features.clearcoat %}clearcoat_normal_value{% else %}geometry_tbn.N{% endif %},
        // Sheen
        {% if pbr_features.sheen %}sheen_color_factor{% else %}vec3<f32>(0.0){% endif %},
        {% if pbr_features.sheen %}sheen_roughness_factor{% else %}0.0{% endif %},
        // Dispersion
        {% if pbr_features.dispersion %}dispersion{% else %}0.0{% endif %},
        // Diffuse transmission
        {% if pbr_features.diffuse_transmission %}diffuse_trans_factor{% else %}0.0{% endif %},
        {% if pbr_features.diffuse_transmission %}diffuse_trans_color{% else %}vec3<f32>(0.0){% endif %},
        // Anisotropy
        {% if pbr_features.anisotropy %}aniso_basis.t{% else %}geometry_tbn.T{% endif %},
        {% if pbr_features.anisotropy %}aniso_basis.b{% else %}geometry_tbn.B{% endif %},
        {% if pbr_features.anisotropy %}aniso_basis.strength{% else %}0.0{% endif %},
        // Iridescence
        {% if pbr_features.iridescence %}iridescence_factor{% else %}0.0{% endif %},
        {% if pbr_features.iridescence %}iridescence.ior{% else %}1.3{% endif %},
        {% if pbr_features.iridescence %}iridescence_thickness{% else %}0.0{% endif %},
        // SSR receive mask (no feature gate — one scalar, always loaded)
        material.ssr_mask,
    );
}

// Base Color
fn _pbr_material_base_color{{ mipmap.suffix() }}(
    material: PbrMaterial,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> vec4<f32> {
    var color = material.base_color_factor;
    // Branchless: an unbound slot packs the shared 1x1 NEUTRAL (white), so
    // sampling always runs and multiplies by identity — exactly glTF's
    // defined no-texture result. No feature gate, no exists check: texture
    // binds are pure data and never re-key the pipeline.
    color *= {{ mipmap.sample_fn() }}(material.base_color_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %});
    // compute pass only deals with fully opaque
    // mask and blend are handled in the fragment shader
    color.a = 1.0;
    return color;
}

// Metallic-Roughness
fn _pbr_material_metallic_roughness_color{{ mipmap.suffix() }}(
    material: PbrMaterial,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> vec2<f32> {
    var color = vec2<f32>(material.metallic_factor, material.roughness_factor);
    // Branchless: unbound slot = the 1x1 NEUTRAL (white), so B/G multiply by
    // identity — glTF's defined no-texture result.
    let tex = {{ mipmap.sample_fn() }}(material.metallic_roughness_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %});
    // glTF uses B channel for metallic, G channel for roughness
    color *= vec2<f32>(tex.b, tex.g);
    return color;
}

// Unpack a sampled normal-map texel to tangent space, honoring the two-channel
// packing pair (docs/plans/compression.md F3): 0 = full RGB (Z from .b),
// 1 = X/Y in .rg (a BC5 / EAC-RG11 transcode), 2 = the packed RGBA layout
// (X in .rgb, Y in .a — ASTC/RGBA8 fallback). Two-channel modes reconstruct
// z = sqrt(1 - x*x - y*y), exact for unit normals.
fn _pbr_unpack_tangent_normal{{ mipmap.suffix() }}(tex: vec4<f32>, packing: u32, scale: f32) -> vec3<f32> {
    var x = tex.r * 2.0 - 1.0;
    var y = tex.g * 2.0 - 1.0;
    var z = tex.b * 2.0 - 1.0;
    if packing != 0u {
        if packing == 2u {
            y = tex.a * 2.0 - 1.0;
        }
        z = sqrt(max(1.0 - x * x - y * y, 0.0));
    }
    return vec3<f32>(x * scale, y * scale, z);
}

// Normal mapping - transforms normal texture from tangent to world space using geometry TBN
// The TBN is passed from the geometry pass (already interpolated and transformed)
fn _pbr_normal_color{{ mipmap.suffix() }}(
    material: PbrMaterial,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
    geometry_tbn: TBN,
    {% if pbr_features.secondary_maps %}
    secondary: PbrSecondaryMaps,
    secondary_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}secondary_uv_derivs: UvDerivs,{% endif %}
    {% endif %}
) -> vec3<f32> {
    // Branchless: unbound slot = the 1x1 NEUTRAL flat normal (0.5, 0.5, 1),
    // which unpacks to tangent (0,0,1) — and TBN * (0,0,1) is exactly the
    // geometry normal, glTF's defined no-normal-map result.
    // Sample normal map and unpack from [0,1] to [-1,1] range
    let tex = {{ mipmap.sample_fn() }}(material.normal_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %});
    var tangent_normal = _pbr_unpack_tangent_normal{{ mipmap.suffix() }}(
        tex,
        material.normal_packing & 3u,
        material.normal_scale,
    );

    {% if pbr_features.secondary_maps %}
    // Secondary / detail normal: RNM blend (Barré-Brisebois & Hill) in
    // tangent space, before the TBN transform. Strength scales the detail
    // XY (flattening toward (0,0,1) — the blend-neutral). Detail maps are
    // always full-RGB (no two-channel packing pair for the secondary slot).
    if (secondary.normal_tex_info.exists) {
        let s_tex = {{ mipmap.sample_fn() }}(secondary.normal_tex_info, secondary_uv{% if mipmap.is_gradient() %}, secondary_uv_derivs{% endif %});
        let detail = _pbr_unpack_tangent_normal{{ mipmap.suffix() }}(
            s_tex,
            0u,
            secondary.normal_strength,
        );
        let t = normalize(tangent_normal) + vec3<f32>(0.0, 0.0, 1.0);
        let u = normalize(detail) * vec3<f32>(-1.0, -1.0, 1.0);
        tangent_normal = t * dot(t, u) / t.z - u;
    }
    {% endif %}

    // Transform the tangent-space normal to world space using the TBN matrix from geometry pass
    let tbn_matrix = mat3x3<f32>(geometry_tbn.T, geometry_tbn.B, geometry_tbn.N);
    return normalize(tbn_matrix * tangent_normal);
}

{% if pbr_features.secondary_maps %}
// Photoshop-style scalar overlay: a < 0.5 darkens (2ab), a >= 0.5
// brightens (1 - 2(1-a)(1-b)). Neutral b = 0.5 is the identity, so the
// secondary-roughness strength lerp toward 0.5 makes strength 0 a no-op.
fn _pbr_overlay_scalar(a: f32, b: f32) -> f32 {
    if (a < 0.5) {
        return 2.0 * a * b;
    }
    return 1.0 - 2.0 * (1.0 - a) * (1.0 - b);
}
{% endif %}

// Occlusion
fn _pbr_occlusion_color{{ mipmap.suffix() }}(
    material: PbrMaterial,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    // Branchless: unbound slot = the 1x1 NEUTRAL (white), so
    // mix(1, 1, strength) == 1 — glTF's defined no-occlusion-map result.
    let tex = {{ mipmap.sample_fn() }}(material.occlusion_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %});
    let occlusion = mix(1.0, tex.r, material.occlusion_strength);
    return occlusion;
}

// Emissive
fn _pbr_material_emissive_color{{ mipmap.suffix() }}(
    material: PbrMaterial,
    emissive_strength: f32,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> vec3<f32> {
    var color = material.emissive_factor;
    // Branchless: unbound slot = the 1x1 NEUTRAL (white) — identity multiply.
    color *= {{ mipmap.sample_fn() }}(material.emissive_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).rgb;
    color *= emissive_strength;
    return color;
}

// Specular factor
fn _pbr_specular{{ mipmap.suffix() }}(
    specular: PbrSpecular,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    var factor = specular.factor;
    if specular.tex_info.exists {
        factor *= {{ mipmap.sample_fn() }}(specular.tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).a;
    }
    return factor;
}

// Specular color
fn _pbr_specular_color{{ mipmap.suffix() }}(
    specular: PbrSpecular,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> vec3<f32> {
    var color = specular.color_factor;
    if specular.color_tex_info.exists {
        color *= {{ mipmap.sample_fn() }}(specular.color_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).rgb;
    }
    return color;
}

// Transmission
fn _pbr_transmission{{ mipmap.suffix() }}(
    transmission: PbrTransmission,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    // Early exit: if no texture and factor is 0, skip entirely
    if (!transmission.tex_info.exists && transmission.factor == 0.0) {
        return 0.0;
    }
    var factor = transmission.factor;
    if transmission.tex_info.exists {
        factor *= {{ mipmap.sample_fn() }}(transmission.tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).r;
    }
    return factor;
}

// Volume thickness
fn _pbr_volume_thickness{{ mipmap.suffix() }}(
    volume: PbrVolume,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    // Early exit: no volume if thickness is 0 and no texture
    if (!volume.thickness_tex_info.exists && volume.thickness_factor == 0.0) {
        return 0.0;
    }
    var thickness = volume.thickness_factor;
    if volume.thickness_tex_info.exists {
        // Volume thickness is stored in the G channel per glTF spec
        thickness *= {{ mipmap.sample_fn() }}(volume.thickness_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).g;
    }
    return thickness;
}

// ============================================================================
// Clearcoat (KHR_materials_clearcoat)
// ============================================================================

// Clearcoat intensity factor (R channel)
fn _pbr_clearcoat{{ mipmap.suffix() }}(
    clearcoat: PbrClearcoat,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    // Early exit: no clearcoat if factor is 0 and no texture
    if (!clearcoat.tex_info.exists && clearcoat.factor == 0.0) {
        return 0.0;
    }
    var factor = clearcoat.factor;
    if clearcoat.tex_info.exists {
        factor *= {{ mipmap.sample_fn() }}(clearcoat.tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).r;
    }
    return factor;
}

// Clearcoat roughness (G channel)
fn _pbr_clearcoat_roughness{{ mipmap.suffix() }}(
    clearcoat: PbrClearcoat,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    var roughness = clearcoat.roughness_factor;
    if clearcoat.roughness_tex_info.exists {
        roughness *= {{ mipmap.sample_fn() }}(clearcoat.roughness_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).g;
    }
    return roughness;
}

// Clearcoat normal - transforms clearcoat normal texture from tangent to world space using geometry TBN
fn _pbr_clearcoat_normal{{ mipmap.suffix() }}(
    clearcoat: PbrClearcoat,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
    geometry_tbn: TBN,
    packing: u32,
) -> vec3<f32> {
    // If no clearcoat normal texture, use geometry normal
    if !clearcoat.normal_tex_info.exists {
        return geometry_tbn.N;
    }

    // Sample clearcoat normal map and unpack from [0,1] to [-1,1] range
    let tex = {{ mipmap.sample_fn() }}(clearcoat.normal_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %});
    let tangent_normal = _pbr_unpack_tangent_normal{{ mipmap.suffix() }}(
        tex,
        packing,
        clearcoat.normal_scale,
    );

    // Transform the tangent-space normal to world space using the TBN matrix from geometry pass
    let tbn_matrix = mat3x3<f32>(geometry_tbn.T, geometry_tbn.B, geometry_tbn.N);
    return normalize(tbn_matrix * tangent_normal);
}

// ============================================================================
// Sheen (KHR_materials_sheen)
// ============================================================================

// Sheen color (RGB)
fn _pbr_sheen_color{{ mipmap.suffix() }}(
    sheen: PbrSheen,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> vec3<f32> {
    var color = sheen.color_factor;
    if sheen.color_tex_info.exists {
        color *= {{ mipmap.sample_fn() }}(sheen.color_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).rgb;
    }
    return color;
}

// Sheen roughness (A channel)
fn _pbr_sheen_roughness{{ mipmap.suffix() }}(
    sheen: PbrSheen,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    var roughness = sheen.roughness_factor;
    if sheen.roughness_tex_info.exists {
        roughness *= {{ mipmap.sample_fn() }}(sheen.roughness_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).a;
    }
    return roughness;
}

// ============================================================================
// Diffuse Transmission (KHR_materials_diffuse_transmission)
// ============================================================================

fn _pbr_diffuse_transmission{{ mipmap.suffix() }}(
    dt: PbrDiffuseTransmission,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    if (!dt.tex_info.exists && dt.factor == 0.0) {
        return 0.0;
    }
    var factor = dt.factor;
    if dt.tex_info.exists {
        factor *= {{ mipmap.sample_fn() }}(dt.tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).a;
    }
    return factor;
}

fn _pbr_diffuse_transmission_color{{ mipmap.suffix() }}(
    dt: PbrDiffuseTransmission,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> vec3<f32> {
    var color = dt.color_factor;
    if dt.color_tex_info.exists {
        color *= {{ mipmap.sample_fn() }}(dt.color_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).rgb;
    }
    return color;
}

// ============================================================================
// Anisotropy (KHR_materials_anisotropy)
// ============================================================================

struct AnisotropyBasis {
    t: vec3<f32>,
    b: vec3<f32>,
    strength: f32,
};

fn _pbr_anisotropy_basis{{ mipmap.suffix() }}(
    aniso: PbrAnisotropy,
    geometry_tbn: TBN,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> AnisotropyBasis {
    var anisotropy_dir = vec2<f32>(1.0, 0.0);
    var strength = aniso.strength;

    if aniso.tex_info.exists {
        let sample = {{ mipmap.sample_fn() }}(aniso.tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %});
        anisotropy_dir = sample.rg * 2.0 - vec2<f32>(1.0);
        strength *= sample.b;
    }

    let cos_r = cos(aniso.rotation);
    let sin_r = sin(aniso.rotation);
    let dir = vec2<f32>(
        cos_r * anisotropy_dir.x - sin_r * anisotropy_dir.y,
        sin_r * anisotropy_dir.x + cos_r * anisotropy_dir.y,
    );

    let t_aniso = geometry_tbn.T * dir.x + geometry_tbn.B * dir.y;
    let b_aniso = cross(geometry_tbn.N, t_aniso);

    return AnisotropyBasis(t_aniso, b_aniso, strength);
}

// ============================================================================
// Iridescence (KHR_materials_iridescence)
// ============================================================================

fn _pbr_iridescence_factor{{ mipmap.suffix() }}(
    iri: PbrIridescence,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    var factor = iri.factor;
    if iri.tex_info.exists {
        factor *= {{ mipmap.sample_fn() }}(iri.tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).r;
    }
    return factor;
}

fn _pbr_iridescence_thickness{{ mipmap.suffix() }}(
    iri: PbrIridescence,
    attribute_uv: vec2<f32>,
    {% if mipmap.is_gradient() %}uv_derivs: UvDerivs,{% endif %}
) -> f32 {
    if iri.thickness_tex_info.exists {
        let g = {{ mipmap.sample_fn() }}(iri.thickness_tex_info, attribute_uv{% if mipmap.is_gradient() %}, uv_derivs{% endif %}).g;
        return mix(iri.thickness_min, iri.thickness_max, g);
    }
    return iri.thickness_max;
}
{% endif %}{# end inc.material_color_calc (PBR builder) #}

// ============================================================================
// Unlit Material Color Computation
// ============================================================================

{# Skinny materials: gated by base==Unlit. References the UnlitMaterial type
   (defined in the unlit fragment, which materials_wgsl only emits for the Unlit
   base), and is only called from the base==Unlit dispatch branch. #}
{% if base == ShadingBase::Unlit %}
// Compute unlit material color
fn compute_unlit_material_color(
    triangle_indices: vec3<u32>,
    attribute_data_offset: u32,
    material: UnlitMaterial,
    barycentric: vec3<f32>,
    vertex_attribute_stride: u32,
    uv_sets_index: u32,
    {% if mipmap.is_gradient() %}
    bary_derivs: vec4<f32>,
    world_normal: vec3<f32>,
    view_matrix: mat4x4<f32>,
    {% endif %}
) -> UnlitMaterialColor {
    // Compute base color
    var base = material.base_color_factor;
    if material.base_color_tex_info.exists {
        let uv = texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            material.base_color_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        );
        {% if mipmap.is_gradient() %}
        let gradients = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset,
            vertex_attribute_stride,
            uv_sets_index,
            material.base_color_tex_info,
            world_normal,
            view_matrix
        );
        base *= texture_pool_sample_grad(material.base_color_tex_info, uv, gradients);
        {% else %}
        base *= texture_pool_sample_no_mips(material.base_color_tex_info, uv);
        {% endif %}
    }

    // Compute emissive
    var emissive = material.emissive_factor;
    if material.emissive_tex_info.exists {
        let uv = texture_uv(
            attribute_data_offset,
            triangle_indices,
            barycentric,
            material.emissive_tex_info,
            vertex_attribute_stride,
            uv_sets_index,
        );
        {% if mipmap.is_gradient() %}
        let gradients = get_uv_derivatives(
            barycentric,
            bary_derivs,
            triangle_indices,
            attribute_data_offset,
            vertex_attribute_stride,
            uv_sets_index,
            material.emissive_tex_info,
            world_normal,
            view_matrix
        );
        emissive *= texture_pool_sample_grad(material.emissive_tex_info, uv, gradients).rgb;
        {% else %}
        emissive *= texture_pool_sample_no_mips(material.emissive_tex_info, uv).rgb;
        {% endif %}
    }

    // Opaque pass forces alpha to 1.0
    base.a = 1.0;

    return UnlitMaterialColor(base, emissive);
}
{% endif %}{# end base==Unlit (compute_unlit_material_color) #}

// ============================================================================
// Tangent Helpers
// ============================================================================

// Interpolate tangent vectors across a triangle using barycentric coordinates
fn get_vertex_tangent(
    attribute_data_offset: u32,
    triangle_indices: vec3<u32>,
    barycentric: vec3<f32>,
    vertex_attribute_stride: u32,
) -> vec4<f32> {
    let t0 = _get_vertex_tangent(attribute_data_offset, triangle_indices.x, vertex_attribute_stride);
    let t1 = _get_vertex_tangent(attribute_data_offset, triangle_indices.y, vertex_attribute_stride);
    let t2 = _get_vertex_tangent(attribute_data_offset, triangle_indices.z, vertex_attribute_stride);
    return barycentric.x * t0 + barycentric.y * t1 + barycentric.z * t2;
}

// Read tangent from packed attribute buffer
// Attribute layout per vertex: [normal.xyz (3 floats), tangent.xyzw (4 floats), ...]
fn _get_vertex_tangent(
    attribute_data_offset: u32,
    vertex_index: u32,
    vertex_attribute_stride: u32,
) -> vec4<f32> {
    if (vertex_attribute_stride < 7u) {
        // No tangent data available (stride < normal(3) + tangent(4))
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let vertex_start = attribute_data_offset + (vertex_index * vertex_attribute_stride);
    let base = vertex_start + 3u; // tangents follow normals (3 float offset)

    // attribute_data lives in the merged geometry pool aliased
    // here by `visibility_data` (binding 5).
    return vec4<f32>(
        visibility_data[base],
        visibility_data[base + 1u],
        visibility_data[base + 2u],
        visibility_data[base + 3u],  // w component = handedness sign (±1)
    );
}
