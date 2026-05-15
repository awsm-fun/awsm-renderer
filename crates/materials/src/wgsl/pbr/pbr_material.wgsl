// --------------------------
// PBR header + decode
// --------------------------

struct PbrMaterialHeaderRaw {
    alpha_mode: u32,
    alpha_cutoff: f32,

    base_color_tex_info: TextureInfoRaw,
    base_color_factor_r: f32,
    base_color_factor_g: f32,
    base_color_factor_b: f32,
    base_color_factor_a: f32,

    metallic_roughness_tex_info: TextureInfoRaw,
    metallic_factor: f32,
    roughness_factor: f32,

    normal_tex_info: TextureInfoRaw,
    normal_scale: f32,

    occlusion_tex_info: TextureInfoRaw,
    occlusion_strength: f32,

    emissive_tex_info: TextureInfoRaw,
    emissive_factor_r: f32,
    emissive_factor_g: f32,
    emissive_factor_b: f32,

    debug_bitmask: u32,

    // 12 u32 relative indices (word indices relative to header start)
    vertex_color_info_relative_index: u32,
    emissive_strength_relative_index: u32,
    ior_relative_index: u32,
    specular_relative_index: u32,
    transmission_relative_index: u32,
    diffuse_transmission_relative_index: u32,
    volume_relative_index: u32,
    clearcoat_relative_index: u32,
    sheen_relative_index: u32,
    dispersion_relative_index: u32,
    anisotropy_relative_index: u32,
    iridescence_relative_index: u32,
};

struct PbrMaterial {
    alpha_mode: u32,
    alpha_cutoff: f32,

    base_color_tex_info: TextureInfo,
    base_color_factor: vec4<f32>,

    metallic_roughness_tex_info: TextureInfo,
    metallic_factor: f32,
    roughness_factor: f32,

    normal_tex_info: TextureInfo,
    normal_scale: f32,

    occlusion_tex_info: TextureInfo,
    occlusion_strength: f32,

    emissive_tex_info: TextureInfo,
    emissive_factor: vec3<f32>,

    debug_bitmask: u32,

    // absolute indices in global `materials` (0 == absent)
    vertex_color_info_index: u32,
    emissive_strength_index: u32,
    ior_index: u32,
    specular_index: u32,
    transmission_index: u32,
    diffuse_transmission_index: u32,
    volume_index: u32,
    clearcoat_index: u32,
    sheen_index: u32,
    dispersion_index: u32,
    anisotropy_index: u32,
    iridescence_index: u32,
};


// Core header words (excluding shader id) BEFORE the 12 reserved indices:
//
// alpha_mode (1)
// alpha_cutoff (1)
// base_color_tex (5)
// base_color_factor (4)
// metallic_roughness_tex (5)
// metallic_factor (1)
// roughness_factor (1)
// normal_tex (5)
// normal_scale (1)
// occlusion_tex (5)
// occlusion_strength (1)
// emissive_tex (5)
// emissive_factor (3)
// debug_bitmask (1)
// = 39 words
const PBR_CORE_WORDS: u32 = 39u;

// Then we reserve 12 u32 indices right after the core:
const PBR_FEATURE_INDEX_WORDS: u32 = 12u;

// Total fixed header words (core + indices)
const PBR_HEADER_WORDS: u32 = PBR_CORE_WORDS + PBR_FEATURE_INDEX_WORDS; // 50

fn pbr_get_material(byte_offset: u32) -> PbrMaterial {
    // word 0 at byte_offset is shader_id; header starts right after it
    let base_index = (byte_offset / 4u) + 1u;

    // Load core header fields with explicit layout.
    // (Same indexing you already had for core.)
    let alpha_mode   = material_load_u32(base_index + 0u);
    let alpha_cutoff = material_load_f32(base_index + 1u);

    let base_color_tex = material_load_texture_info_raw(base_index + 2u);
    let bc_r = material_load_f32(base_index + 7u);
    let bc_g = material_load_f32(base_index + 8u);
    let bc_b = material_load_f32(base_index + 9u);
    let bc_a = material_load_f32(base_index + 10u);

    let mr_tex = material_load_texture_info_raw(base_index + 11u);
    let metallic  = material_load_f32(base_index + 16u);
    let roughness = material_load_f32(base_index + 17u);

    let normal_tex  = material_load_texture_info_raw(base_index + 18u);
    let normal_scale = material_load_f32(base_index + 23u);

    let occ_tex = material_load_texture_info_raw(base_index + 24u);
    let occ_strength = material_load_f32(base_index + 29u);

    let emissive_tex = material_load_texture_info_raw(base_index + 30u);
    let em_r = material_load_f32(base_index + 35u);
    let em_g = material_load_f32(base_index + 36u);
    let em_b = material_load_f32(base_index + 37u);

    let debug_bitmask = material_load_u32(base_index + 38u);

    // 12 relative indices live immediately after the 38 core words:
    let fi = base_index + PBR_CORE_WORDS;

    let header = PbrMaterialHeaderRaw(
        alpha_mode,
        alpha_cutoff,

        base_color_tex,
        bc_r, bc_g, bc_b, bc_a,

        mr_tex,
        metallic,
        roughness,

        normal_tex,
        normal_scale,

        occ_tex,
        occ_strength,

        emissive_tex,
        em_r, em_g, em_b,

        debug_bitmask,

        material_load_u32(fi + 0u),  // vertex_color_info
        material_load_u32(fi + 1u),  // emissive_strength
        material_load_u32(fi + 2u),  // ior
        material_load_u32(fi + 3u),  // specular
        material_load_u32(fi + 4u),  // transmission
        material_load_u32(fi + 5u),  // diffuse_transmission
        material_load_u32(fi + 6u),  // volume
        material_load_u32(fi + 7u),  // clearcoat
        material_load_u32(fi + 8u),  // sheen
        material_load_u32(fi + 9u),  // dispersion
        material_load_u32(fi + 10u), // anisotropy
        material_load_u32(fi + 11u)  // iridescence
    );

    return PbrMaterial(
        header.alpha_mode,
        header.alpha_cutoff,

        convert_texture_info(header.base_color_tex_info),
        vec4<f32>(header.base_color_factor_r, header.base_color_factor_g, header.base_color_factor_b, header.base_color_factor_a),

        convert_texture_info(header.metallic_roughness_tex_info),
        header.metallic_factor,
        header.roughness_factor,

        convert_texture_info(header.normal_tex_info),
        header.normal_scale,

        convert_texture_info(header.occlusion_tex_info),
        header.occlusion_strength,

        convert_texture_info(header.emissive_tex_info),
        vec3<f32>(header.emissive_factor_r, header.emissive_factor_g, header.emissive_factor_b),

        debug_bitmask,

        abs_index(base_index, header.vertex_color_info_relative_index),
        abs_index(base_index, header.emissive_strength_relative_index),
        abs_index(base_index, header.ior_relative_index),
        abs_index(base_index, header.specular_relative_index),
        abs_index(base_index, header.transmission_relative_index),
        abs_index(base_index, header.diffuse_transmission_relative_index),
        abs_index(base_index, header.volume_relative_index),
        abs_index(base_index, header.clearcoat_relative_index),
        abs_index(base_index, header.sheen_relative_index),
        abs_index(base_index, header.dispersion_relative_index),
        abs_index(base_index, header.anisotropy_relative_index),
        abs_index(base_index, header.iridescence_relative_index)
    );
}

// --------------------------
// PBR optional feature loaders (decoded, non-Raw)
//
// --------------------------
//
fn pbr_debug_base_color(debug: u32) -> bool {
    return (debug & (1u << 0u)) != 0u;
}
fn pbr_debug_metallic_roughness(debug: u32) -> bool {
    return (debug & (1u << 1u)) != 0u;
}
fn pbr_debug_normals(debug: u32) -> bool {
    return (debug & (1u << 2u)) != 0u;
}
fn pbr_debug_occlusion(debug: u32) -> bool {
    return (debug & (1u << 3u)) != 0u;
}
fn pbr_debug_emissive(debug: u32) -> bool {
    return (debug & (1u << 4u)) != 0u;
}
fn pbr_debug_specular(debug: u32) -> bool {
    return (debug & (1u << 5u)) != 0u;
}


fn pbr_material_load_vertex_color_info(index: u32) -> VertexColorInfo {
    if (index == 0u) {
        return VertexColorInfo(0u);
    }
    return VertexColorInfo(material_load_u32(index));
}

// emissive_strength: [strength]
fn pbr_material_load_emissive_strength(index: u32) -> f32 {
    if (index == 0u) { return 1.0; }
    return material_load_f32(index);
}

// ior: [ior]
fn pbr_material_load_ior(index: u32) -> f32 {
    if (index == 0u) { return 1.5; }
    return material_load_f32(index);
}

// dispersion: [dispersion]
fn pbr_material_load_dispersion(index: u32) -> f32 {
    if (index == 0u) { return 0.0; }
    return material_load_f32(index);
}

// specular packed by Rust as:
//   tex(5) + factor + color_tex(5) + color_factor(3)
struct PbrSpecular {
    tex_info: TextureInfo,
    factor: f32,
    color_tex_info: TextureInfo,
    color_factor: vec3<f32>,
}

fn pbr_material_load_specular(index: u32) -> PbrSpecular {
    if (index == 0u) {
        return PbrSpecular(texture_info_none(), 1.0, texture_info_none(), vec3<f32>(1.0, 1.0, 1.0));
    }

    let tex = material_load_texture_info(index + 0u);
    let factor = material_load_f32(index + 5u);
    let ctex = material_load_texture_info(index + 6u);

    let r = material_load_f32(index + 11u);
    let g = material_load_f32(index + 12u);
    let b = material_load_f32(index + 13u);

    return PbrSpecular(tex, factor, ctex, vec3<f32>(r, g, b));
}

// transmission packed by Rust as:
//   tex(5) + factor
struct PbrTransmission {
    tex_info: TextureInfo,
    factor: f32,
}

fn pbr_material_load_transmission(index: u32) -> PbrTransmission {
    if (index == 0u) {
        return PbrTransmission(texture_info_none(), 0.0);
    }

    let tex = material_load_texture_info(index + 0u);
    let factor = material_load_f32(index + 5u);
    return PbrTransmission(tex, factor);
}

// diffuse transmission packed by Rust as:
//   tex(5) + factor + color_tex(5) + color_factor(3)
struct PbrDiffuseTransmission {
    tex_info: TextureInfo,
    factor: f32,
    color_tex_info: TextureInfo,
    color_factor: vec3<f32>,
}

fn pbr_material_load_diffuse_transmission(index: u32) -> PbrDiffuseTransmission {
    if (index == 0u) {
        return PbrDiffuseTransmission(texture_info_none(), 0.0, texture_info_none(), vec3<f32>(1.0, 1.0, 1.0));
    }

    let tex = material_load_texture_info(index + 0u);
    let factor = material_load_f32(index + 5u);
    let ctex = material_load_texture_info(index + 6u);

    let r = material_load_f32(index + 11u);
    let g = material_load_f32(index + 12u);
    let b = material_load_f32(index + 13u);

    return PbrDiffuseTransmission(tex, factor, ctex, vec3<f32>(r, g, b));
}

// volume packed by Rust as:
//   thickness_tex(5) + thickness_factor + attenuation_distance + attenuation_color(3)
struct PbrVolume {
    thickness_tex_info: TextureInfo,
    thickness_factor: f32,
    attenuation_distance: f32,
    attenuation_color: vec3<f32>,
}

fn pbr_material_load_volume(index: u32) -> PbrVolume {
    if (index == 0u) {
        return PbrVolume(texture_info_none(), 0.0, 0.0, vec3<f32>(1.0, 1.0, 1.0));
    }

    let ttex = material_load_texture_info(index + 0u);
    let tfac = material_load_f32(index + 5u);
    let dist = material_load_f32(index + 6u);

    let r = material_load_f32(index + 7u);
    let g = material_load_f32(index + 8u);
    let b = material_load_f32(index + 9u);

    return PbrVolume(ttex, tfac, dist, vec3<f32>(r, g, b));
}

// clearcoat packed by Rust as:
//   tex(5) + factor + roughness_tex(5) + roughness_factor + normal_tex(5) + normal_scale
struct PbrClearcoat {
    tex_info: TextureInfo,
    factor: f32,
    roughness_tex_info: TextureInfo,
    roughness_factor: f32,
    normal_tex_info: TextureInfo,
    normal_scale: f32,
}

fn pbr_material_load_clearcoat(index: u32) -> PbrClearcoat {
    if (index == 0u) {
        return PbrClearcoat(
            texture_info_none(), 0.0,
            texture_info_none(), 0.0,
            texture_info_none(), 1.0
        );
    }

    let tex = material_load_texture_info(index + 0u);
    let factor = material_load_f32(index + 5u);

    let rtex = material_load_texture_info(index + 6u);
    let rfac = material_load_f32(index + 11u);

    let ntex = material_load_texture_info(index + 12u);
    let nsca = material_load_f32(index + 17u);

    return PbrClearcoat(tex, factor, rtex, rfac, ntex, nsca);
}

// sheen packed by Rust as:
//   roughness_tex(5) + roughness_factor + color_tex(5) + color_factor(3)
struct PbrSheen {
    roughness_tex_info: TextureInfo,
    roughness_factor: f32,
    color_tex_info: TextureInfo,
    color_factor: vec3<f32>,
}

fn pbr_material_load_sheen(index: u32) -> PbrSheen {
    if (index == 0u) {
        return PbrSheen(texture_info_none(), 0.0, texture_info_none(), vec3<f32>(0.0, 0.0, 0.0));
    }

    let rtex = material_load_texture_info(index + 0u);
    let rfac = material_load_f32(index + 5u);
    let ctex = material_load_texture_info(index + 6u);

    let r = material_load_f32(index + 11u);
    let g = material_load_f32(index + 12u);
    let b = material_load_f32(index + 13u);

    return PbrSheen(rtex, rfac, ctex, vec3<f32>(r, g, b));
}

// anisotropy packed by Rust as:
//   tex(5) + strength + rotation
struct PbrAnisotropy {
    tex_info: TextureInfo,
    strength: f32,
    rotation: f32,
}

fn pbr_material_load_anisotropy(index: u32) -> PbrAnisotropy {
    if (index == 0u) {
        return PbrAnisotropy(texture_info_none(), 0.0, 0.0);
    }

    let tex = material_load_texture_info(index + 0u);
    let strength = material_load_f32(index + 5u);
    let rotation = material_load_f32(index + 6u);

    return PbrAnisotropy(tex, strength, rotation);
}

// iridescence: you have the WGSL struct, but note:
// your Rust currently does NOT write iridescence payload yet (and never sets feature_indices.iridescence).
// This loader is here for when you add it.
struct PbrIridescence {
    tex_info: TextureInfo,
    factor: f32,
    ior: f32,
    thickness_tex_info: TextureInfo,
    thickness_min: f32,
    thickness_max: f32,
}

fn pbr_material_load_iridescence(index: u32) -> PbrIridescence {
    if (index == 0u) {
        return PbrIridescence(
            texture_info_none(),
            0.0,
            1.3,
            texture_info_none(),
            100.0,
            400.0
        );
    }

    // Expected layout (when you implement Rust packing):
    // tex(5) + factor + ior + thickness_tex(5) + thickness_min + thickness_max
    let tex = material_load_texture_info(index + 0u);
    let factor = material_load_f32(index + 5u);
    let ior = material_load_f32(index + 6u);
    let ttex = material_load_texture_info(index + 7u);
    let tmin = material_load_f32(index + 12u);
    let tmax = material_load_f32(index + 13u);

    return PbrIridescence(tex, factor, ior, ttex, tmin, tmax);
}
