// ToonMaterial — banded diffuse + stepped Blinn-Phong specular + rim.
//
// The struct layout in the storage buffer mirrors UnlitMaterial (the simplest
// existing material) with three extra trailing fields:
//
//   shader_id        (u32, skipped by callers via material_load_shader_id)
//   alpha_mode       (u32)
//   alpha_cutoff     (f32)
//   base_color_tex   (TextureInfoRaw, 5 words — unused at the WGSL level for v1)
//   base_color       (vec4<f32>, 4 words)
//   emissive_tex     (TextureInfoRaw, 5 words — unused at the WGSL level for v1)
//   emissive         (vec3<f32>, 3 words)
//   diffuse_bands    (u32, 1 word)
//   specular_steps   (u32, 1 word)
//   shininess        (f32, 1 word)
//   rim_strength     (f32, 1 word)
//   rim_power        (f32, 1 word)
//
// Total: shader_id (skipped) + 22 words = 23 words.

struct ToonMaterial {
    alpha_mode: u32,
    alpha_cutoff: f32,
    base_color_tex_info: TextureInfo,
    base_color_factor: vec4<f32>,
    emissive_tex_info: TextureInfo,
    emissive_factor: vec3<f32>,
    diffuse_bands: u32,
    specular_steps: u32,
    shininess: f32,
    rim_strength: f32,
    rim_power: f32,
}

fn toon_get_material(byte_offset: u32) -> ToonMaterial {
    let base_index = (byte_offset / 4u) + 1u; // skip shader id word

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

    let diffuse_bands = material_load_u32(base_index + 19u);
    let specular_steps = material_load_u32(base_index + 20u);
    let shininess = material_load_f32(base_index + 21u);
    let rim_strength = material_load_f32(base_index + 22u);
    let rim_power = material_load_f32(base_index + 23u);

    return ToonMaterial(
        alpha_mode,
        alpha_cutoff,
        convert_texture_info(base_color_tex),
        vec4<f32>(bc_r, bc_g, bc_b, bc_a),
        convert_texture_info(emissive_tex),
        vec3<f32>(em_r, em_g, em_b),
        diffuse_bands,
        specular_steps,
        shininess,
        rim_strength,
        rim_power,
    );
}

// Quantize a value in [0, 1] to `bands` discrete steps. With bands=3 you get
// roughly { 0.33, 0.67, 1.0 } when input >= corresponding threshold.
fn toon_quantize(value: f32, bands: u32) -> f32 {
    let n = max(f32(bands), 1.0);
    return floor(value * n + 0.001) / n;
}

// Compute the lit toon color at a fragment. Mirrors apply_lighting's signature
// (returns vec3 final color) so call sites can drop it in beside the PBR /
// unlit shading branches.
fn compute_toon_lit_color(
    material: ToonMaterial,
    world_normal: vec3<f32>,
    surface_to_camera: vec3<f32>,
    world_position: vec3<f32>,
    lights_info: LightsInfo,
) -> vec3<f32> {
    let base = material.base_color_factor.rgb;
    let view_dir = normalize(surface_to_camera);

    var diffuse_acc = vec3<f32>(0.0);
    var specular_acc = vec3<f32>(0.0);

    for (var i = 0u; i < lights_info.n_lights; i = i + 1u) {
        let light = get_light(i);
        let lb = light_sample(light, world_normal, world_position);
        if (lb.radiance.x + lb.radiance.y + lb.radiance.z <= 0.0) {
            continue;
        }
        let banded = toon_quantize(clamp(lb.n_dot_l, 0.0, 1.0), material.diffuse_bands);
        diffuse_acc = diffuse_acc + lb.radiance * banded;

        // Stepped Blinn-Phong specular.
        let half_dir = normalize(lb.light_dir + view_dir);
        let n_dot_h = max(dot(world_normal, half_dir), 0.0);
        let spec_raw = pow(n_dot_h, max(material.shininess, 1.0));
        let stepped = toon_quantize(clamp(spec_raw, 0.0, 1.0), max(material.specular_steps, 1u));
        // Mask specular so it only appears where diffuse is meaningfully lit —
        // matches the "highlight rides the lit half" feel of classic cel.
        let mask = step(0.001, banded);
        specular_acc = specular_acc + lb.radiance * stepped * mask;
    }

    // Ambient term — a small floor so unlit faces aren't pure black.
    let ambient = vec3<f32>(0.18, 0.18, 0.20);

    // Rim term: brighten silhouette edges to read as cel-shaded.
    let rim_raw = 1.0 - max(dot(world_normal, view_dir), 0.0);
    let rim = pow(clamp(rim_raw, 0.0, 1.0), max(material.rim_power, 0.5));
    let rim_band = toon_quantize(rim, 2u);
    let rim_contrib = rim_band * material.rim_strength;

    var color = base * (ambient + diffuse_acc) + specular_acc + base * rim_contrib;
    color = color + material.emissive_factor;
    return color;
}
