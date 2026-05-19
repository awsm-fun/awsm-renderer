struct LightsInfoPacked {
    data: vec4<u32>,
}

struct LightsInfo {
    n_lights: u32,
    ibl: IblInfo
}

struct IblInfo {
    prefiltered_env_mip_count: u32,
    irradiance_mip_count: u32,
}

struct LightPacked {
  // pos.xyz + range
  pos_range: vec4<f32>,
  // dir.xyz + inner_cone
  dir_inner: vec4<f32>,
  // color.rgb + intensity
  color_intensity: vec4<f32>,
  // kind (as uint) + outer_cone + shadow_index (bit-cast u32) + 1 pad
  kind_outer_pad: vec4<f32>,
};

struct Light {
    kind: u32,
    color: vec3<f32>,
    intensity: f32,
    position: vec3<f32>,
    range: f32,
    direction: vec3<f32>,
    inner_cone: f32,
    outer_cone: f32,
    // Index into `shadow_descriptors`. `0xFFFFFFFF` = no shadow.
    shadow_index: u32,
};

fn get_lights_info() -> LightsInfo {
    // expects `lights_info` is global LightsInfoPacked
    return LightsInfo(
        lights_info.data.x,
        IblInfo(
            lights_info.data.y,
            lights_info.data.z
        )
    );
}

fn get_light(i: u32) -> Light {
    // expects `lights` is global array<LightPacked>
    let p = lights[i];
    return Light(
        u32(p.kind_outer_pad.x),
        p.color_intensity.xyz,
        p.color_intensity.w,
        p.pos_range.xyz,
        p.pos_range.w,
        p.dir_inner.xyz,
        p.dir_inner.w,
        p.kind_outer_pad.y,
        bitcast<u32>(p.kind_outer_pad.z),
    );
}

struct LightBrdf {
    normal: vec3<f32>,
    n_dot_l: f32,
    light_dir: vec3<f32>,
    radiance: vec3<f32>,
};

// KHR_lights_punctual unit-handling note:
//   * Directional intensity is `lux` (lm/m²).
//   * Point/spot intensity is `candela` (lm/sr).
// We treat both as already-radiometric: `radiance = color * intensity * attenuation`.
// That matches what the glTF Sample Renderer does and is what most assets
// are authored for, but it is NOT a proper photometric → radiometric
// conversion. A tonemapped, exposure-aware renderer can hide the
// difference; a strictly physical pipeline would multiply each kind by
// the appropriate `683 lm/W` luminous-efficacy scale and divide by the
// shaded surface's projected area.
fn light_to_brdf(light:Light, normal: vec3<f32>, world_position: vec3<f32>) -> LightBrdf {
    var light_dir: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
    var radiance: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
    var n_dot_l: f32 = 0.0;

    switch (light.kind) {
        case 0u: {
            // no light, skip
        }
        case 1u: { // Directional
            light_dir = normalize(-light.direction); // light -> surface
            radiance = light.color * light.intensity;
            n_dot_l = max(dot(normal, light_dir), 0.0);
        }
        case 2u: { // Point
            let surface_to_light = light.position - world_position;
            let dist = length(surface_to_light);
            light_dir = surface_to_light / dist; // light -> surface
            let attenuation = inverse_square(light.range, dist);
            radiance = light.color * light.intensity * attenuation;
            n_dot_l = max(dot(normal, light_dir), 0.0);
        }
        case 3u: { // Spot
            let surface_to_light = light.position - world_position;
            let dist = length(surface_to_light);
            light_dir = surface_to_light / dist; // light -> surface
            let cos_l = dot(light_dir, -normalize(light.direction));
            let spot = spot_falloff(light.inner_cone, light.outer_cone, cos_l);
            let attenuation = inverse_square(light.range, dist) * spot;
            radiance = light.color * light.intensity * attenuation;
            n_dot_l = max(dot(normal, light_dir), 0.0);
        }
        default: { // unexpected
        }
    }

    return LightBrdf(
        normal,
        n_dot_l,
        light_dir,
        radiance,
    );
}

// spot light mask (smooth edge)
fn spot_falloff(inner_cos: f32, outer_cos: f32, cos_l: f32) -> f32 {
    let smoothed = saturate((cos_l - outer_cos) / (inner_cos - outer_cos));
    return smoothed * smoothed;
}

// Apply all enabled lighting to a material and return the final color
fn apply_lighting(
    material_color: PbrMaterialColor,
    surface_to_camera: vec3<f32>,
    world_position: vec3<f32>,
    lights_info: LightsInfo,
    // Mirrors `Mesh::receive_shadows` (1 = enabled, 0 = mesh opts
    // out). Drives an inner gate around `sample_shadow_directional`
    // so a non-receiver mesh stays fully lit even when shadow
    // descriptors are otherwise live for this light.
    receive_shadows: u32,
) -> vec3<f32> {
    var color = vec3<f32>(0.0);

    {% if has_lighting_ibl() %}
        color = brdf_ibl(
            material_color,
            material_color.normal,
            surface_to_camera,
            ibl_filtered_env_tex,
            ibl_filtered_env_sampler,
            ibl_irradiance_tex,
            ibl_irradiance_sampler,
            brdf_lut_tex,
            brdf_lut_sampler,
            lights_info.ibl
        );
    {% endif %}

    {% if has_lighting_punctual() %}
        {% if shadows_enabled %}
            // View-space z (positive forward) for cascade selection.
            let view_z_for_shadow = -(camera_raw.view * vec4<f32>(world_position, 1.0)).z;
        {% endif %}
        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light = get_light(i);
            let light_brdf = light_to_brdf(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                // Modulate by shadow visibility (1.0 = lit, 0.0 = fully
                // shadowed). `shadow_index == SHADOW_INDEX_NONE` short-
                // circuits to 1.0; the cascade selector walks
                // descriptors descriptor_base..base+count.
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        material_color.normal,
                        view_z_for_shadow,
                    );
                    // Contact-shadow refinement: directional lights only,
                    // since the SSCS ray-march needs a meaningful
                    // surface-to-light direction. Point/spot already
                    // sample their own short-range shadow maps so SSCS
                    // would double-cost them for no win.
                    if light.kind == 1u && light.shadow_index != SHADOW_INDEX_NONE {
                        let sscs_dir = normalize(-light.direction);
                        visibility = visibility * apply_sscs(world_position, sscs_dir);
                    }
                }
                color += direct * visibility;
            {% else %}
                color += direct;
            {% endif %}
        }
        {% if shadows_enabled %}
            // Cascade-debug overlay (uses the dominant directional
            // light's descriptor base, fetched via light 0's
            // `shadow_index` — sufficient until phase 4 surfaces a
            // proper sun-light index).
            if lights_info.n_lights > 0u {
                color = debug_cascade_tint(
                    color,
                    get_light(0u).shadow_index,
                    world_position,
                    view_z_for_shadow,
                );
            }
        {% endif %}
    {% endif %}

    return color;
}

// Apply lighting with explicit transmission background (for screen-space transmission)
fn apply_lighting_with_transmission(
    material_color: PbrMaterialColor,
    surface_to_camera: vec3<f32>,
    world_position: vec3<f32>,
    lights_info: LightsInfo,
    transmission_background: vec3<f32>,
    // See `apply_lighting`.
    receive_shadows: u32,
) -> vec3<f32> {
    var color = vec3<f32>(0.0);

    {% if has_lighting_ibl() %}
        color = brdf_ibl_with_transmission(
            material_color,
            material_color.normal,
            surface_to_camera,
            ibl_filtered_env_tex,
            ibl_filtered_env_sampler,
            ibl_irradiance_tex,
            ibl_irradiance_sampler,
            brdf_lut_tex,
            brdf_lut_sampler,
            lights_info.ibl,
            transmission_background
        );
    {% endif %}

    {% if has_lighting_punctual() %}
        {% if shadows_enabled %}
            // View-space z (positive forward) for cascade selection.
            let view_z_for_shadow = -(camera_raw.view * vec4<f32>(world_position, 1.0)).z;
        {% endif %}
        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light = get_light(i);
            let light_brdf = light_to_brdf(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                // Modulate by shadow visibility (1.0 = lit, 0.0 = fully
                // shadowed). `shadow_index == SHADOW_INDEX_NONE` short-
                // circuits to 1.0; the cascade selector walks
                // descriptors descriptor_base..base+count.
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        material_color.normal,
                        view_z_for_shadow,
                    );
                    // Contact-shadow refinement: directional lights only,
                    // since the SSCS ray-march needs a meaningful
                    // surface-to-light direction. Point/spot already
                    // sample their own short-range shadow maps so SSCS
                    // would double-cost them for no win.
                    if light.kind == 1u && light.shadow_index != SHADOW_INDEX_NONE {
                        let sscs_dir = normalize(-light.direction);
                        visibility = visibility * apply_sscs(world_position, sscs_dir);
                    }
                }
                color += direct * visibility;
            {% else %}
                color += direct;
            {% endif %}
        }
        {% if shadows_enabled %}
            // Cascade-debug overlay (uses the dominant directional
            // light's descriptor base, fetched via light 0's
            // `shadow_index` — sufficient until phase 4 surfaces a
            // proper sun-light index).
            if lights_info.n_lights > 0u {
                color = debug_cascade_tint(
                    color,
                    get_light(0u).shadow_index,
                    world_position,
                    view_z_for_shadow,
                );
            }
        {% endif %}
    {% endif %}

    return color;
}
