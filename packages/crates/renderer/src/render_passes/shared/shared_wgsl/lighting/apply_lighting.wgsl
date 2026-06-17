// apply_lighting.wgsl — PBR lighting orchestration (apply_lighting*) + froxel
// light walking. Calls into brdf.wgsl; depends on light_access.wgsl. PBR only.
// Split out of the former lights.wgsl.

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

    {% if debug.views %}
    // Global unlit/flat view mode — emit the base color, skip all lighting.
    if (cull_params.debug_view_mode == 1u) {
        return material_color.base.rgb;
    }
    {% endif %}

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
            let light_brdf = light_sample(light, material_color.normal, world_position);
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
                        shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
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

{% if use_froxel_lights %}
// ─────────────────────────────────────────────────────────────────
// Per-froxel walks.
//
// Used when the shading shader binds the GPU light-culling pass's
// output (`cull_params` + the per-froxel tail of `lights_storage`).
// Mirrors `apply_lighting` / `_with_transmission` but the punctual
// loop reads the froxel slice instead of walking the full `n_lights`
// range.
//
// Per-froxel slice layout (see
// `render_passes/light_culling/shader/light_culling_wgsl/bind_groups.wgsl`):
//   stride = cull_params.max_per_froxel_capacity + 1
//   slot 0:           count (clamped at read time)
//   slots 1..1+count: light indices
//
// The directional walk stays flat (no spatial culling — directional
// lights affect every pixel by definition).
// ─────────────────────────────────────────────────────────────────

// Froxel addressing + light-walk enumeration order (the single source of truth
// shared with the Plan B prep pass — keep them aligned for deferred shadows).
{% include "shared_wgsl/lighting/froxel_walk.wgsl" %}

fn apply_lighting_per_froxel(
    material_color: PbrMaterialColor,
    surface_to_camera: vec3<f32>,
    world_position: vec3<f32>,
    lights_info: LightsInfo,
    receive_shadows: u32,
    // Fragment's screen-space pixel coords (`@builtin(position).xy`).
    pixel_xy: vec2<f32>,
) -> vec3<f32> {
    var color = vec3<f32>(0.0);

    {% if debug.views %}
    // Global unlit/flat view mode — emit the base color, skip all lighting.
    if (cull_params.debug_view_mode == 1u) {
        return material_color.base.rgb;
    }
    {% endif %}

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
        let view_z = -(camera_raw.view * vec4<f32>(world_position, 1.0)).z;
        {% if shadows_enabled %}
            let view_z_for_shadow = view_z;
        {% endif %}

        {% if debug.views %}
        // Debug: visualize this pixel's froxel light count (what the cull
        // binned for this froxel) instead of shading. See `light_count_heatmap`.
        if (cull_params.debug_light_heatmap != 0u) {
            let dbg_base = froxel_base_for_pixel(pixel_xy, view_z);
            let dbg_count = froxel_light_count(dbg_base);
            return light_count_heatmap(dbg_count);
        }
        {% endif %}

        // Directional walk — bounded to the directional-light prefix
        // (see get_n_directional / get_directional_light_index).
        let n_directional = get_n_directional();
        for(var d = 0u; d < n_directional; d = d + 1u) {
            let light = get_light(get_directional_light_index(d));
            let light_brdf = light_sample(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                        view_z_for_shadow,
                    );
                    if light.shadow_index != SHADOW_INDEX_NONE {
                        let sscs_dir = normalize(-light.direction);
                        visibility = visibility * apply_sscs(world_position, sscs_dir);
                    }
                }
                color += direct * visibility;
            {% else %}
                color += direct;
            {% endif %}
        }

        // Per-froxel punctual walk.
        if lights_info.n_lights > 0u {
            let base = froxel_base_for_pixel(pixel_xy, view_z);
            let count = froxel_light_count(base);
            for(var i = 0u; i < count; i = i + 1u) {
                let li = lights_storage[base + 1u + i];
                let light = get_light(li);
                // Defensive — directional shouldn't appear in the slice.
                if light.kind == 1u {
                    continue;
                }
                // Range reject: the froxel bins every light whose bounding
                // sphere touches the froxel volume — large distant froxels
                // over-include lights that can't reach this pixel. Skip them
                // for the cost of one dot product, before light_sample.
                let to_light = light.position - world_position;
                if light.range > 0.0 && dot(to_light, to_light) > light.range * light.range {
                    continue;
                }
                let light_brdf = light_sample(light, material_color.normal, world_position);
                let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
                {% if shadows_enabled %}
                    var visibility: f32 = 1.0;
                    if receive_shadows != 0u {
                        visibility = sample_shadow_directional(
                            light.shadow_index,
                            world_position,
                            shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                            view_z_for_shadow,
                        );
                    }
                    color += direct * visibility;
                {% else %}
                    color += direct;
                {% endif %}
            }
        }

        {% if shadows_enabled %}
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

fn apply_lighting_per_froxel_with_transmission(
    material_color: PbrMaterialColor,
    surface_to_camera: vec3<f32>,
    world_position: vec3<f32>,
    lights_info: LightsInfo,
    transmission_background: vec3<f32>,
    receive_shadows: u32,
    pixel_xy: vec2<f32>,
) -> vec3<f32> {
    var color = vec3<f32>(0.0);

    {% if debug.views %}
    // Global unlit/flat view mode — emit the base color, skip all lighting.
    if (cull_params.debug_view_mode == 1u) {
        return material_color.base.rgb;
    }
    {% endif %}

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
        let view_z = -(camera_raw.view * vec4<f32>(world_position, 1.0)).z;
        {% if shadows_enabled %}
            let view_z_for_shadow = view_z;
        {% endif %}

        {% if debug.views %}
        // Debug: visualize this pixel's froxel light count (what the cull
        // binned for this froxel) instead of shading. See `light_count_heatmap`.
        if (cull_params.debug_light_heatmap != 0u) {
            let dbg_base = froxel_base_for_pixel(pixel_xy, view_z);
            let dbg_count = froxel_light_count(dbg_base);
            return light_count_heatmap(dbg_count);
        }
        {% endif %}

        let n_directional = get_n_directional();
        for(var d = 0u; d < n_directional; d = d + 1u) {
            let light = get_light(get_directional_light_index(d));
            let light_brdf = light_sample(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                        view_z_for_shadow,
                    );
                    if light.shadow_index != SHADOW_INDEX_NONE {
                        let sscs_dir = normalize(-light.direction);
                        visibility = visibility * apply_sscs(world_position, sscs_dir);
                    }
                }
                color += direct * visibility;
            {% else %}
                color += direct;
            {% endif %}
        }

        if lights_info.n_lights > 0u {
            let base = froxel_base_for_pixel(pixel_xy, view_z);
            let count = froxel_light_count(base);
            for(var i = 0u; i < count; i = i + 1u) {
                let li = lights_storage[base + 1u + i];
                let light = get_light(li);
                if light.kind == 1u {
                    continue;
                }
                // Range reject: the froxel bins every light whose bounding
                // sphere touches the froxel volume — large distant froxels
                // over-include lights that can't reach this pixel. Skip them
                // for the cost of one dot product, before light_sample.
                let to_light = light.position - world_position;
                if light.range > 0.0 && dot(to_light, to_light) > light.range * light.range {
                    continue;
                }
                let light_brdf = light_sample(light, material_color.normal, world_position);
                let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
                {% if shadows_enabled %}
                    var visibility: f32 = 1.0;
                    if receive_shadows != 0u {
                        visibility = sample_shadow_directional(
                            light.shadow_index,
                            world_position,
                            shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                            view_z_for_shadow,
                        );
                    }
                    color += direct * visibility;
                {% else %}
                    color += direct;
                {% endif %}
            }
        }

        {% if shadows_enabled %}
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
{% endif %}
