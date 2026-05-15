struct UnlitMaterialHeaderRaw {
    alpha_mode: u32,
    alpha_cutoff: f32,

    base_color_tex_info: TextureInfoRaw,
    base_color_factor_r: f32,
    base_color_factor_g: f32,
    base_color_factor_b: f32,
    base_color_factor_a: f32,

    emissive_tex_info: TextureInfoRaw,
    emissive_factor_r: f32,
    emissive_factor_g: f32,
    emissive_factor_b: f32,
}

struct UnlitMaterial {
    alpha_mode: u32,
    alpha_cutoff: f32,

    base_color_tex_info: TextureInfo,
    base_color_factor: vec4<f32>,

    emissive_tex_info: TextureInfo,
    emissive_factor: vec3<f32>,
}

fn unlit_get_material(byte_offset: u32) -> UnlitMaterial {
    let base_index = (byte_offset / 4u) + 1u; // skip shader id word

    // Layout:
    // 0 alpha_mode
    // 1 alpha_cutoff
    // 2..6 base_color_tex (5)
    // 7..10 base_color_factor (4)
    // 11..15 emissive_tex (5)
    // 16..18 emissive_factor (3)

    let alpha_mode = material_load_u32(base_index + 0u);
    let alpha_cutoff = material_load_f32(base_index + 1u);

    let base_color_tex = material_load_texture_info_raw(base_index + 2u);
    let bc_r = material_load_f32(base_index + 7u);
    let bc_g = material_load_f32(base_index + 8u);
    let bc_b = material_load_f32(base_index + 9u);
    let bc_a = material_load_f32(base_index + 10u);

    let emissive_tex = material_load_texture_info_raw(base_index + 11u);
    let em_r = material_load_f32(base_index + 16u);
    let em_g = material_load_f32(base_index + 17u);
    let em_b = material_load_f32(base_index + 18u);

    return UnlitMaterial(
        alpha_mode,
        alpha_cutoff,
        convert_texture_info(base_color_tex),
        vec4<f32>(bc_r, bc_g, bc_b, bc_a),
        convert_texture_info(emissive_tex),
        vec3<f32>(em_r, em_g, em_b)
    );
}

// Result of unlit material color computation
struct UnlitMaterialColor {
    base: vec4<f32>,     // base color with alpha
    emissive: vec3<f32>, // emissive color
}

// Compute final unlit color from UnlitMaterialColor
// Per glTF KHR_materials_unlit: output = base_color.rgb + emissive
fn compute_unlit_output(color: UnlitMaterialColor) -> vec3<f32> {
    return color.base.rgb + color.emissive;
}
