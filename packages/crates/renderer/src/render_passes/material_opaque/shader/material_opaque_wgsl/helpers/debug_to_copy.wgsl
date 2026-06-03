// TODO - make these into functions and slog them in where it's helpful

/*

// SEE DEBUG LIGHTING SLOG_1
{% match debug.lighting %}
    {% when ShaderTemplateMaterialOpaqueDebugLighting::None | ShaderTemplateMaterialOpaqueDebugLighting::IblOnly %}
        sample_color = brdf_ibl(
            material_color_{{s}},
            material_color_{{s}}.normal,
            standard_coordinates.surface_to_camera,
            ibl_filtered_env_tex,
            ibl_filtered_env_sampler,
            ibl_irradiance_tex,
            ibl_irradiance_sampler,
            brdf_lut_tex,
            brdf_lut_sampler,
            lights_info.ibl
        );
    {% when _ %}
{% endmatch %}

{% match debug.lighting %}
    {% when ShaderTemplateMaterialOpaqueDebugLighting::None | ShaderTemplateMaterialOpaqueDebugLighting::PunctualOnly %}
        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light_brdf = light_to_brdf(get_light(i), material_color_{{s}}.normal, standard_coordinates.world_position);
            sample_color += brdf_direct(material_color_{{s}}, light_brdf, standard_coordinates.surface_to_camera);
        }
    {% when _ %}
{% endmatch %}

// SEE DEBUG MSAA SLOG_1
{% if multisampled_geometry && debug.msaa_detect_edges %}
    // Debug visualization: show detected edges in magenta
    if (depth_edge_mask(coords, pixel_center, screen_dims_f32, world_normal, triangle_index)) {
        textureStore(opaque_tex, coords, vec4<f32>(1.0, 0.0, 1.0, 1.0));
        return;
    }
{% endif %}

// erm, somewhere
{% if debug.mips %}
    // Visualize mip level selection using base color texture (if present)
    {% match mipmap %}
        {% when MipmapMode::Gradient %}
            if (pbr_material.has_base_color_texture) {
                // Get the actual gradients being used
                let ddx_uv = gradients.base_color.ddx;
                let ddy_uv = gradients.base_color.ddy;
                let tex_info = pbr_material.base_color_tex_info;

                // Calculate mip level that hardware will select
                let mip_level = debug_calculate_mip_level(
                    ddx_uv,
                    ddy_uv,
                    tex_info.size
                );

                // Show gradient magnitude
                let grad_mag = max(length(ddx_uv), length(ddy_uv));

                // Visualize raw LOD as a smooth color gradient
                // This shows the exact fractional LOD value that hardware uses
                //
                // Color mapping (smooth gradient):
                //   LOD 0.0 → Blue (sharpest)
                //   LOD 1.0 → Cyan
                //   LOD 2.0 → Green
                //   LOD 3.0 → Yellow
                //   LOD 4.0 → Orange
                //   LOD 5.0+ → Red (blurriest)

                let lod_clamped = clamp(mip_level, 0.0, 5.0);
                let lod_normalized = lod_clamped / 5.0;  // Map [0, 5] to [0, 1]

                // Create smooth gradient: Blue → Cyan → Green → Yellow → Red
                // Using a heatmap-style color ramp
                if (lod_clamped < 1.0) {
                    // 0.0-1.0: Blue → Cyan
                    let t = lod_clamped;
                    color = vec3<f32>(0.0, t, 1.0);
                } else if (lod_clamped < 2.0) {
                    // 1.0-2.0: Cyan → Green
                    let t = lod_clamped - 1.0;
                    color = vec3<f32>(0.0, 1.0, 1.0 - t);
                } else if (lod_clamped < 3.0) {
                    // 2.0-3.0: Green → Yellow
                    let t = lod_clamped - 2.0;
                    color = vec3<f32>(t, 1.0, 0.0);
                } else if (lod_clamped < 4.0) {
                    // 3.0-4.0: Yellow → Orange
                    let t = lod_clamped - 3.0;
                    color = vec3<f32>(1.0, 1.0 - 0.5 * t, 0.0);
                } else {
                    // 4.0-5.0: Orange → Red
                    let t = lod_clamped - 4.0;
                    color = vec3<f32>(1.0, 0.5 - 0.5 * t, 0.0);
                }


                // COLOR MODE 2: Show orthographic correction effect
                // Visualize how surface tilt affects anisotropic filtering
                let packed_nt = textureLoad(normal_tangent_tex, coords, 0);
                let tbn_debug = unpack_normal_tangent(packed_nt);
                let world_normal_debug = tbn_debug.N;

                // Extract view direction
                let view_forward = -normalize(vec3<f32>(camera.view[0][2], camera.view[1][2], camera.view[2][2]));
                let n_dot_v = abs(dot(world_normal_debug, view_forward));

                // UV derivative magnitudes (with orthographic correction applied)
                let ddx_mag = length(ddx_uv);
                let ddy_mag = length(ddy_uv);
                let uv_ratio = max(ddx_mag, ddy_mag) / max(min(ddx_mag, ddy_mag), 0.0001);

                // Red channel: Surface tilt (0 = perpendicular/edge-on, 1 = face-on)
                // Green channel: UV anisotropy ratio (should be HIGH now on tilted surfaces!)
                // Blue channel: Inverse tilt (high = needs correction)
                color = vec3<f32>(
                    n_dot_v,                        // Face-on = bright red
                    min(uv_ratio / 4.0, 1.0),      // Anisotropy = green
                    1.0 - n_dot_v                   // Edge-on = bright blue
                );

                // COLOR MODE 3: Show both gradients and mip level side-by-side
                // Uncomment to use this mode instead
                /*
                // Left half of screen: gradient magnitude as brightness
                // Right half: mip level as color
                if (f32(coords.x) < screen_dims_f32.x * 0.5) {
                    // Gradient magnitude mode
                    let brightness = grad_mag * 100.0;
                    color = vec3<f32>(brightness);
                } else {
                    // Mip level color mode
                    if (mip_level < 0.5) {
                        color = vec3<f32>(0.0, 0.0, 1.0);
                    } else if (mip_level < 1.5) {
                        color = vec3<f32>(0.0, 1.0, 0.0);
                    } else if (mip_level < 2.5) {
                        color = vec3<f32>(0.5, 1.0, 0.0);
                    } else if (mip_level < 3.5) {
                        color = vec3<f32>(1.0, 1.0, 0.0);
                    } else if (mip_level < 4.5) {
                        color = vec3<f32>(1.0, 0.5, 0.0);
                    } else {
                        color = vec3<f32>(1.0, 0.0, 0.0);
                    }
                }
                */
            } else {
                color = vec3<f32>(0.5, 0.5, 0.5); // Gray = no texture
            }
        {% when _ %}
            color = vec3<f32>(0.5, 0.5, 0.5); // Gray = no mipmap mode
    {% endmatch %}
{% endif %}

{% if debug.n_dot_v %}
    let n = safe_normalize(material_color.normal);
    let v = safe_normalize(standard_coordinates.surface_to_camera);
    let n_dot_v_val = saturate(dot(n, v));
    // Show n_dot_v as grayscale, but also show it in green channel for visibility
    // R = n_dot_v, G = n_dot_v * 2 for emphasis, B = 0
    color = vec3<f32>(n_dot_v_val, n_dot_v_val * 2.0, 0.0);
{% endif %}

{% if debug.normals %}
    // Visualize normals as RGB (map from [-1,1] to [0,1])
    let n = safe_normalize(material_color.normal);
    color = n * 0.5 + 0.5;
{% endif %}

{% if debug.solid_color %}
    // Just output bright magenta to verify debug system works
    color = vec3<f32>(1.0, 0.0, 1.0);
{% endif %}

{% if debug.view_direction %}
    // Visualize view direction (surface_to_camera) as RGB
    let v = safe_normalize(standard_coordinates.surface_to_camera);
    color = v * 0.5 + 0.5;
{% endif %}

{% if debug.irradiance_sample %}
    // Sample the irradiance map directly using the normal
    let n = safe_normalize(material_color.normal);
    let irradiance = textureSampleLevel(ibl_irradiance_tex, ibl_irradiance_sampler, n, 0.0).rgb;
    color = irradiance;
{% endif %}
*/


// Debug lighting modes - use with debug.lighting template variable
// SEE DEBUG LIGHTING SLOG_1 - insert this where lighting is applied
// Note: Replace <S> with the actual sample index variable when using
/*
match debug.lighting {
    None | IblOnly =>
        sample_color = brdf_ibl(
            material_color_<S>,
            material_color_<S>.normal,
            standard_coordinates.surface_to_camera,
            ibl_filtered_env_tex,
            ibl_filtered_env_sampler,
            ibl_irradiance_tex,
            ibl_irradiance_sampler,
            brdf_lut_tex,
            brdf_lut_sampler,
            lights_info.ibl
        );
    _ => {}
}

match debug.lighting {
    None | PunctualOnly =>
        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light_brdf = light_to_brdf(get_light(i), material_color_<S>.normal, standard_coordinates.world_position);
            sample_color += brdf_direct(material_color_<S>, light_brdf, standard_coordinates.surface_to_camera);
        }
    _ => {}
}

// Debug MSAA edge detection - insert after color computation
// SEE DEBUG MSAA SLOG_1
if multisampled_geometry && debug.msaa_detect_edges {
    // Debug visualization: show detected edges in magenta
    if (depth_edge_mask(coords, pixel_center, screen_dims_f32, world_normal, triangle_index)) {
        textureStore(opaque_tex, coords, vec4<f32>(1.0, 0.0, 1.0, 1.0));
        return;
    }
}

// Debug mipmap visualization - replaces final color output
if debug.mips {
    // Visualize mip level selection using base color texture (if present)
    if mipmap == Gradient {
            if (pbr_material.has_base_color_texture) {
                // Get the actual gradients being used
                let ddx_uv = gradients.base_color.ddx;
                let ddy_uv = gradients.base_color.ddy;
                let tex_info = pbr_material.base_color_tex_info;

                // Calculate mip level that hardware will select
                let mip_level = debug_calculate_mip_level(
                    ddx_uv,
                    ddy_uv,
                    tex_info.size
                );

                // Show gradient magnitude
                let grad_mag = max(length(ddx_uv), length(ddy_uv));

                // Visualize raw LOD as a smooth color gradient
                // This shows the exact fractional LOD value that hardware uses
                //
                // Color mapping (smooth gradient):
                //   LOD 0.0 → Blue (sharpest)
                //   LOD 1.0 → Cyan
                //   LOD 2.0 → Green
                //   LOD 3.0 → Yellow
                //   LOD 4.0 → Orange
                //   LOD 5.0+ → Red (blurriest)

                let lod_clamped = clamp(mip_level, 0.0, 5.0);
                let lod_normalized = lod_clamped / 5.0;  // Map [0, 5] to [0, 1]

                // Create smooth gradient: Blue → Cyan → Green → Yellow → Red
                // Using a heatmap-style color ramp
                if (lod_clamped < 1.0) {
                    // 0.0-1.0: Blue → Cyan
                    let t = lod_clamped;
                    color = vec3<f32>(0.0, t, 1.0);
                } else if (lod_clamped < 2.0) {
                    // 1.0-2.0: Cyan → Green
                    let t = lod_clamped - 1.0;
                    color = vec3<f32>(0.0, 1.0, 1.0 - t);
                } else if (lod_clamped < 3.0) {
                    // 2.0-3.0: Green → Yellow
                    let t = lod_clamped - 2.0;
                    color = vec3<f32>(t, 1.0, 0.0);
                } else if (lod_clamped < 4.0) {
                    // 3.0-4.0: Yellow → Orange
                    let t = lod_clamped - 3.0;
                    color = vec3<f32>(1.0, 1.0 - 0.5 * t, 0.0);
                } else {
                    // 4.0-5.0: Orange → Red
                    let t = lod_clamped - 4.0;
                    color = vec3<f32>(1.0, 0.5 - 0.5 * t, 0.0);
                }


                // COLOR MODE 2: Show orthographic correction effect
                // Visualize how surface tilt affects anisotropic filtering
                let packed_nt = textureLoad(normal_tangent_tex, coords, 0);
                let tbn_debug = unpack_normal_tangent(packed_nt);
                let world_normal_debug = tbn_debug.N;

                // Extract view direction
                let view_forward = -normalize(vec3<f32>(camera.view[0][2], camera.view[1][2], camera.view[2][2]));
                let n_dot_v = abs(dot(world_normal_debug, view_forward));

                // UV derivative magnitudes (with orthographic correction applied)
                let ddx_mag = length(ddx_uv);
                let ddy_mag = length(ddy_uv);
                let uv_ratio = max(ddx_mag, ddy_mag) / max(min(ddx_mag, ddy_mag), 0.0001);

                // Red channel: Surface tilt (0 = perpendicular/edge-on, 1 = face-on)
                // Green channel: UV anisotropy ratio (should be HIGH now on tilted surfaces!)
                // Blue channel: Inverse tilt (high = needs correction)
                color = vec3<f32>(
                    n_dot_v,                        // Face-on = bright red
                    min(uv_ratio / 4.0, 1.0),      // Anisotropy = green
                    1.0 - n_dot_v                   // Edge-on = bright blue
                );

                // COLOR MODE 3: Show both gradients and mip level side-by-side
                // Uncomment to use this mode instead
                // Left half of screen: gradient magnitude as brightness
                // Right half: mip level as color
                if (f32(coords.x) < screen_dims_f32.x * 0.5) {
                    // Gradient magnitude mode
                    let brightness = grad_mag * 100.0;
                    color = vec3<f32>(brightness);
                } else {
                    // Mip level color mode
                    if (mip_level < 0.5) {
                        color = vec3<f32>(0.0, 0.0, 1.0);
                    } else if (mip_level < 1.5) {
                        color = vec3<f32>(0.0, 1.0, 0.0);
                    } else if (mip_level < 2.5) {
                        color = vec3<f32>(0.5, 1.0, 0.0);
                    } else if (mip_level < 3.5) {
                        color = vec3<f32>(1.0, 1.0, 0.0);
                    } else if (mip_level < 4.5) {
                        color = vec3<f32>(1.0, 0.5, 0.0);
                    } else {
                        color = vec3<f32>(1.0, 0.0, 0.0);
                    }
                }
            } else {
                color = vec3<f32>(0.5, 0.5, 0.5); // Gray = no texture
            }
    } else {
        color = vec3<f32>(0.5, 0.5, 0.5); // Gray = no mipmap mode
    }
}


// Debug N dot V visualization
if debug.n_dot_v {
    let n = safe_normalize(material_color.normal);
    let v = safe_normalize(standard_coordinates.surface_to_camera);
    let n_dot_v_val = saturate(dot(n, v));
    // Show n_dot_v as grayscale, but also show it in green channel for visibility
    // R = n_dot_v, G = n_dot_v * 2 for emphasis, B = 0
    color = vec3<f32>(n_dot_v_val, n_dot_v_val * 2.0, 0.0);
}

// Debug solid color test
if debug.solid_color {
    // Just output bright magenta to verify debug system works
    color = vec3<f32>(1.0, 0.0, 1.0);
}

// Debug view direction visualization
if debug.view_direction {
    // Visualize view direction (surface_to_camera) as RGB
    let v = safe_normalize(standard_coordinates.surface_to_camera);
    color = v * 0.5 + 0.5;
}

// Debug irradiance sampling
if debug.irradiance_sample {
    // Sample the irradiance map directly using the normal
    let n = safe_normalize(material_color.normal);
    let irradiance = textureSampleLevel(ibl_irradiance_tex, ibl_irradiance_sampler, n, 0.0).rgb;
    color = irradiance;
}
*/
