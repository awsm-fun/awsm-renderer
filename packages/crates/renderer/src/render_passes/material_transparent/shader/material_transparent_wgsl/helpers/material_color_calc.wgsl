// Fragment shader versions of PBR material color sampling
// These functions work with interpolated vertex data (no barycentrics/attribute buffers needed)
// Hardware automatically handles mip level selection via screen-space derivatives

fn orthonormal_tangent_from_vertex(normal: vec3<f32>, tangent_xyz: vec3<f32>) -> vec3<f32> {
    var t = tangent_xyz - normal * dot(tangent_xyz, normal);
    let len_sq = dot(t, t);
    if (len_sq > 1e-8) {
        return t * inverseSqrt(len_sq);
    }

    let fallback_axis = select(
        vec3<f32>(0.0, 0.0, 1.0),
        vec3<f32>(0.0, 1.0, 0.0),
        abs(normal.z) > 0.999
    );
    return normalize(cross(fallback_axis, normal));
}

{# Skinny materials: PBR material-color builder, gated so a thin non-PBR
   transparent pipeline (materials_wgsl carries only its own fragment) doesn't
   reference the PbrMaterial type. Only the base==Pbr fragment branch calls it. #}
{% if inc.material_color_calc %}
// Main function: Sample all PBR material textures and return combined material properties
// Returns PbrMaterialColor with perturbed normal (use material_color.normal for lighting!)
fn pbr_get_material_color(
    material: PbrMaterial,
    world_normal: vec3<f32>,
    world_tangent: vec4<f32>,
    fragment_input: FragmentInput
) -> PbrMaterialColor {
    // Load extension data on-demand from indices
    let emissive_strength = pbr_material_load_emissive_strength(material.emissive_strength_index);
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

    var base = pbr_material_base_color(material, fragment_input);

    // Multiply base color by vertex color if variant has color_sets
    {%- if color_sets.is_some() %}
        let vertex_color_info = pbr_material_load_vertex_color_info(material.vertex_color_info_index);
        base *= vertex_color(vertex_color_info, fragment_input);
    {% endif %}

    if material.alpha_mode == ALPHA_MODE_MASK {
        // Discard fragment if alpha below cutoff
        if base.a < material.alpha_cutoff {
            discard;
        } else {
            base.a = 1.0;
        }
    }

    let metallic_roughness = pbr_material_metallic_roughness(material, fragment_input);
    let normal = pbr_normal(material, world_normal, world_tangent, fragment_input);
    let occlusion = pbr_occlusion(material, fragment_input);
    let emissive = pbr_emissive(material, emissive_strength, fragment_input);
    let specular_factor = pbr_specular(specular, fragment_input);
    let specular_color_factor = pbr_specular_color(specular, fragment_input);
    let transmission_factor = pbr_transmission(transmission, fragment_input);
    let volume_thickness = pbr_volume_thickness(volume, fragment_input);

    // Clearcoat
    let clearcoat_factor = pbr_clearcoat(clearcoat, fragment_input);
    let clearcoat_roughness_factor = pbr_clearcoat_roughness(clearcoat, fragment_input);
    let clearcoat_normal_value = pbr_clearcoat_normal(clearcoat, world_normal, world_tangent, fragment_input);

    // Sheen
    let sheen_color_factor = pbr_sheen_color(sheen, fragment_input);
    let sheen_roughness_factor = pbr_sheen_roughness(sheen, fragment_input);

    // Diffuse transmission
    let diffuse_trans_factor = pbr_diffuse_transmission(diffuse_trans, fragment_input);
    let diffuse_trans_color = pbr_diffuse_transmission_color(diffuse_trans, fragment_input);

    // Anisotropy: rotate the world-space tangent into the per-fragment
    // anisotropy direction using the texture-encoded rotation (and the
    // constant rotation factor when no texture is supplied).
    let aniso_basis = pbr_anisotropy_basis(anisotropy, normal, world_tangent, fragment_input);

    // Iridescence
    let iridescence_factor = pbr_iridescence_factor(iridescence, fragment_input);
    let iridescence_thickness = pbr_iridescence_thickness(iridescence, fragment_input);

    return PbrMaterialColor(
        base,
        metallic_roughness,
        normal,
        occlusion,
        emissive,
        specular_factor,
        specular_color_factor,
        ior,
        transmission_factor,
        volume_thickness,
        volume.attenuation_distance,
        volume.attenuation_color,
        // Clearcoat
        clearcoat_factor,
        clearcoat_roughness_factor,
        clearcoat_normal_value,
        // Sheen
        sheen_color_factor,
        sheen_roughness_factor,
        // Dispersion
        dispersion,
        // Diffuse transmission
        diffuse_trans_factor,
        diffuse_trans_color,
        // Anisotropy
        aniso_basis.t,
        aniso_basis.b,
        aniso_basis.strength,
        // Iridescence
        iridescence_factor,
        iridescence.ior,
        iridescence_thickness,
    );
}

// Sample base color texture and apply material factor
fn pbr_material_base_color(
    material: PbrMaterial,
    fragment_input: FragmentInput
) -> vec4<f32> {
    var color = material.base_color_factor;
    // Branchless: an unbound slot packs the shared 1x1 NEUTRAL (white) —
    // identity multiply, glTF's defined no-texture result.
    let uv = texture_uv(material.base_color_tex_info, fragment_input);
    color *= texture_pool_sample(material.base_color_tex_info, uv);
    return color;
}

// Sample metallic-roughness texture and apply material factors
// glTF uses B channel for metallic, G channel for roughness
fn pbr_material_metallic_roughness(
    material: PbrMaterial,
    fragment_input: FragmentInput
) -> vec2<f32> {
    var color = vec2<f32>(material.metallic_factor, material.roughness_factor);
    // Branchless: unbound slot = the 1x1 NEUTRAL (white).
    let uv = texture_uv(material.metallic_roughness_tex_info, fragment_input);
    let tex = texture_pool_sample(material.metallic_roughness_tex_info, uv);
    color *= vec2<f32>(tex.b, tex.g);
    return color;
}

// Apply normal mapping using interpolated tangent space basis from vertex shader
// Much simpler than compute version - relies on vertex shader providing correct tangents
fn pbr_normal(
    material: PbrMaterial,
    world_normal: vec3<f32>,
    world_tangent: vec4<f32>,  // w = handedness (+1 or -1)
    fragment_input: FragmentInput
) -> vec3<f32> {
    // Branchless: unbound slot = the 1x1 NEUTRAL flat normal (0.5, 0.5, 1)
    // → tangent (0,0,1) → tbn * (0,0,1) == N exactly.
    // Sample normal map and unpack from [0,1] to [-1,1] range
    let uv = texture_uv(material.normal_tex_info, fragment_input);
    let tex = texture_pool_sample(material.normal_tex_info, uv);
    let tangent_normal = vec3<f32>(
        (tex.r * 2.0 - 1.0) * material.normal_scale,
        (tex.g * 2.0 - 1.0) * material.normal_scale,
        tex.b * 2.0 - 1.0,
    );

    // Build TBN matrix from interpolated vertex data
    let N = normalize(world_normal);
    let T = orthonormal_tangent_from_vertex(N, world_tangent.xyz);
    let B = cross(N, T) * world_tangent.w;
    let tbn = mat3x3<f32>(T, B, N);

    // Transform tangent-space normal to world space
    return normalize(tbn * tangent_normal);
}

// Sample occlusion texture and apply strength factor
fn pbr_occlusion(
    material: PbrMaterial,
    fragment_input: FragmentInput
) -> f32 {
    // Branchless: unbound slot = the 1x1 NEUTRAL (white) → mix(1,1,s) == 1.
    let uv = texture_uv(material.occlusion_tex_info, fragment_input);
    let tex = texture_pool_sample(material.occlusion_tex_info, uv);
    let occlusion = mix(1.0, tex.r, material.occlusion_strength);
    return occlusion;
}

// Sample emissive texture and apply factors
fn pbr_emissive(
    material: PbrMaterial,
    emissive_strength: f32,
    fragment_input: FragmentInput
) -> vec3<f32> {
    var color = material.emissive_factor;
    // Branchless: unbound slot = the 1x1 NEUTRAL (white) — identity multiply.
    let uv = texture_uv(material.emissive_tex_info, fragment_input);
    color *= texture_pool_sample(material.emissive_tex_info, uv).rgb;
    color *= emissive_strength;
    return color;
}

// Sample specular texture (alpha channel) and apply factor
fn pbr_specular(
    specular: PbrSpecular,
    fragment_input: FragmentInput
) -> f32 {
    var factor = specular.factor;
    if specular.tex_info.exists {
        let uv = texture_uv(specular.tex_info, fragment_input);
        factor *= texture_pool_sample(specular.tex_info, uv).a;
    }
    return factor;
}

// Sample specular color texture (RGB) and apply factor
fn pbr_specular_color(
    specular: PbrSpecular,
    fragment_input: FragmentInput
) -> vec3<f32> {
    var color = specular.color_factor;
    if specular.color_tex_info.exists {
        let uv = texture_uv(specular.color_tex_info, fragment_input);
        color *= texture_pool_sample(specular.color_tex_info, uv).rgb;
    }
    return color;
}

// Sample transmission texture (R channel) and apply factor
fn pbr_transmission(
    transmission: PbrTransmission,
    fragment_input: FragmentInput
) -> f32 {
    // Early exit: if no texture and factor is 0, skip entirely
    if (!transmission.tex_info.exists && transmission.factor == 0.0) {
        return 0.0;
    }
    var factor = transmission.factor;
    if transmission.tex_info.exists {
        let uv = texture_uv(transmission.tex_info, fragment_input);
        factor *= texture_pool_sample(transmission.tex_info, uv).r;
    }
    return factor;
}

// Sample volume thickness texture (G channel) and apply factor
fn pbr_volume_thickness(
    volume: PbrVolume,
    fragment_input: FragmentInput
) -> f32 {
    // Early exit: no volume if thickness is 0 and no texture
    if (!volume.thickness_tex_info.exists && volume.thickness_factor == 0.0) {
        return 0.0;
    }
    var thickness = volume.thickness_factor;
    if volume.thickness_tex_info.exists {
        let uv = texture_uv(volume.thickness_tex_info, fragment_input);
        // Volume thickness is stored in the G channel per glTF spec
        thickness *= texture_pool_sample(volume.thickness_tex_info, uv).g;
    }
    return thickness;
}

// ============================================================================
// Clearcoat (KHR_materials_clearcoat)
// ============================================================================

// Sample clearcoat intensity texture (R channel) and apply factor
fn pbr_clearcoat(
    clearcoat: PbrClearcoat,
    fragment_input: FragmentInput
) -> f32 {
    // Early exit: no clearcoat if factor is 0 and no texture
    if (!clearcoat.tex_info.exists && clearcoat.factor == 0.0) {
        return 0.0;
    }
    var factor = clearcoat.factor;
    if clearcoat.tex_info.exists {
        let uv = texture_uv(clearcoat.tex_info, fragment_input);
        factor *= texture_pool_sample(clearcoat.tex_info, uv).r;
    }
    return factor;
}

// Sample clearcoat roughness texture (G channel) and apply factor
fn pbr_clearcoat_roughness(
    clearcoat: PbrClearcoat,
    fragment_input: FragmentInput
) -> f32 {
    var roughness = clearcoat.roughness_factor;
    if clearcoat.roughness_tex_info.exists {
        let uv = texture_uv(clearcoat.roughness_tex_info, fragment_input);
        roughness *= texture_pool_sample(clearcoat.roughness_tex_info, uv).g;
    }
    return roughness;
}

// Sample clearcoat normal texture and apply normal mapping
fn pbr_clearcoat_normal(
    clearcoat: PbrClearcoat,
    world_normal: vec3<f32>,
    world_tangent: vec4<f32>,
    fragment_input: FragmentInput
) -> vec3<f32> {
    // If no clearcoat normal texture, use geometry normal
    if !clearcoat.normal_tex_info.exists {
        return normalize(world_normal);
    }

    // Sample clearcoat normal map and unpack from [0,1] to [-1,1] range
    let uv = texture_uv(clearcoat.normal_tex_info, fragment_input);
    let tex = texture_pool_sample(clearcoat.normal_tex_info, uv);
    let tangent_normal = vec3<f32>(
        (tex.r * 2.0 - 1.0) * clearcoat.normal_scale,
        (tex.g * 2.0 - 1.0) * clearcoat.normal_scale,
        tex.b * 2.0 - 1.0,
    );

    // Build TBN matrix from interpolated vertex data
    let N = normalize(world_normal);
    let T = orthonormal_tangent_from_vertex(N, world_tangent.xyz);
    let B = cross(N, T) * world_tangent.w;
    let tbn = mat3x3<f32>(T, B, N);

    // Transform tangent-space normal to world space
    return normalize(tbn * tangent_normal);
}

// ============================================================================
// Sheen (KHR_materials_sheen)
// ============================================================================

// Sample sheen color texture (RGB) and apply factor
fn pbr_sheen_color(
    sheen: PbrSheen,
    fragment_input: FragmentInput
) -> vec3<f32> {
    var color = sheen.color_factor;
    if sheen.color_tex_info.exists {
        let uv = texture_uv(sheen.color_tex_info, fragment_input);
        color *= texture_pool_sample(sheen.color_tex_info, uv).rgb;
    }
    return color;
}

// Sample sheen roughness texture (A channel) and apply factor
fn pbr_sheen_roughness(
    sheen: PbrSheen,
    fragment_input: FragmentInput
) -> f32 {
    var roughness = sheen.roughness_factor;
    if sheen.roughness_tex_info.exists {
        let uv = texture_uv(sheen.roughness_tex_info, fragment_input);
        roughness *= texture_pool_sample(sheen.roughness_tex_info, uv).a;
    }
    return roughness;
}

// ============================================================================
// Diffuse Transmission (KHR_materials_diffuse_transmission)
// ============================================================================

fn pbr_diffuse_transmission(
    dt: PbrDiffuseTransmission,
    fragment_input: FragmentInput
) -> f32 {
    if (!dt.tex_info.exists && dt.factor == 0.0) {
        return 0.0;
    }
    var factor = dt.factor;
    if dt.tex_info.exists {
        let uv = texture_uv(dt.tex_info, fragment_input);
        factor *= texture_pool_sample(dt.tex_info, uv).a;
    }
    return factor;
}

fn pbr_diffuse_transmission_color(
    dt: PbrDiffuseTransmission,
    fragment_input: FragmentInput
) -> vec3<f32> {
    var color = dt.color_factor;
    if dt.color_tex_info.exists {
        let uv = texture_uv(dt.color_tex_info, fragment_input);
        color *= texture_pool_sample(dt.color_tex_info, uv).rgb;
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

fn pbr_anisotropy_basis(
    aniso: PbrAnisotropy,
    world_normal: vec3<f32>,
    world_tangent: vec4<f32>,
    fragment_input: FragmentInput
) -> AnisotropyBasis {
    // Build the same TBN we use for normal mapping so the rotation is in
    // the surface tangent plane.
    let N = normalize(world_normal);
    let T0 = orthonormal_tangent_from_vertex(N, world_tangent.xyz);
    let B0 = cross(N, T0) * world_tangent.w;

    // Defaults: no rotation, zero strength.
    var anisotropy_dir = vec2<f32>(1.0, 0.0);
    var strength = aniso.strength;

    if aniso.tex_info.exists {
        // RG store a unit vector in [0,1] for the local rotation; B holds
        // the per-fragment strength multiplier.
        let uv = texture_uv(aniso.tex_info, fragment_input);
        let sample = texture_pool_sample(aniso.tex_info, uv);
        anisotropy_dir = sample.rg * 2.0 - vec2<f32>(1.0);
        strength *= sample.b;
    }

    let cos_r = cos(aniso.rotation);
    let sin_r = sin(aniso.rotation);
    // Rotate the texture-direction by the material's constant rotation.
    let dir = vec2<f32>(
        cos_r * anisotropy_dir.x - sin_r * anisotropy_dir.y,
        sin_r * anisotropy_dir.x + cos_r * anisotropy_dir.y,
    );

    let t_aniso = T0 * dir.x + B0 * dir.y;
    let b_aniso = cross(N, t_aniso);

    return AnisotropyBasis(t_aniso, b_aniso, strength);
}

// ============================================================================
// Iridescence (KHR_materials_iridescence)
// ============================================================================

fn pbr_iridescence_factor(
    iri: PbrIridescence,
    fragment_input: FragmentInput
) -> f32 {
    var factor = iri.factor;
    if iri.tex_info.exists {
        let uv = texture_uv(iri.tex_info, fragment_input);
        factor *= texture_pool_sample(iri.tex_info, uv).r;
    }
    return factor;
}

fn pbr_iridescence_thickness(
    iri: PbrIridescence,
    fragment_input: FragmentInput
) -> f32 {
    if iri.thickness_tex_info.exists {
        let uv = texture_uv(iri.thickness_tex_info, fragment_input);
        let g = texture_pool_sample(iri.thickness_tex_info, uv).g;
        return mix(iri.thickness_min, iri.thickness_max, g);
    }
    return iri.thickness_max;
}
{% endif %}{# end inc.material_color_calc (transparent PBR builder) #}

// ============================================================================
// Unlit Material Color Computation
// ============================================================================

{# Skinny materials: gated by base==Unlit (references UnlitMaterial, only the
   base==Unlit fragment branch calls it). #}
{% if base == ShadingBase::Unlit %}
// Compute unlit material color for fragment shader
fn unlit_get_material_color(
    material: UnlitMaterial,
    fragment_input: FragmentInput
) -> UnlitMaterialColor {
    // Compute base color
    var base = material.base_color_factor;
    if material.base_color_tex_info.exists {
        let uv = texture_uv(material.base_color_tex_info, fragment_input);
        base *= texture_pool_sample(material.base_color_tex_info, uv);
    }

    // Handle alpha modes
    if material.alpha_mode == ALPHA_MODE_MASK {
        if base.a < material.alpha_cutoff {
            discard;
        } else {
            base.a = 1.0;
        }
    }

    // Compute emissive
    var emissive = material.emissive_factor;
    if material.emissive_tex_info.exists {
        let uv = texture_uv(material.emissive_tex_info, fragment_input);
        emissive *= texture_pool_sample(material.emissive_tex_info, uv).rgb;
    }

    return UnlitMaterialColor(base, emissive);
}
{% endif %}{# end base==Unlit (transparent unlit builder) #}
