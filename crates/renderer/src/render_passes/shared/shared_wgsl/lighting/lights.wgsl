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

{% if use_mesh_light_slices %}
// Per-mesh-sliced light walk. Replaces the flat
// `for i in 0..n_lights` punctual walk with an inner loop driven by
// `mesh_light_slices[meta_index]`, so each pixel only shades the
// lights whose AABB overlaps its mesh. Directional lights stay on the
// flat walk (they affect every mesh; no slice would be tighter).
fn apply_lighting_per_mesh(
    material_color: PbrMaterialColor,
    surface_to_camera: vec3<f32>,
    world_position: vec3<f32>,
    lights_info: LightsInfo,
    receive_shadows: u32,
    // Per-mesh slice: the caller already has
    // these on hand from its `material_mesh_metas[meta_index]` fetch,
    // so we take them as parameters instead of refetching.
    slice_offset: u32,
    slice_count: u32,
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
            let view_z_for_shadow = -(camera_raw.view * vec4<f32>(world_position, 1.0)).z;
        {% endif %}

        // Directional walk — directional lights have no bounded AABB
        // so they never live in the per-mesh slice. We still scan the
        // flat list, but skip punctuals (kind != 1u) since the slice
        // owns them.
        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light = get_light(i);
            if light.kind != 1u {
                continue;
            }
            let light_brdf = light_to_brdf(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        material_color.normal,
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

        // Per-mesh punctual walk. The slice metadata
        // (`light_slice_offset` + `light_slice_count`) lives inside the
        // per-mesh `MaterialMeshMeta`, so the caller hands it to us
        // directly — no second storage-buffer fetch.
        // Defensive: the OVERSIZED sentinel (`0xFFFFFFFFu`) is meant to
        // route a mesh through the per-froxel path in the consumer.
        // If it ever reaches the per-mesh slice count (routing bug,
        // stale meta upload, etc.), an unclamped loop spins ~4 billion
        // iterations and device-losts the GPU (black canvas / tab
        // crash). Clamp the sentinel to 0 so the per-mesh walk is a
        // no-op instead.
        let safe_slice_count = select(slice_count, 0u, slice_count == 0xFFFFFFFFu);
        for(var i = 0u; i < safe_slice_count; i = i + 1u) {
            let light_index = lights_storage[slice_offset + i];
            let light = get_light(light_index);
            // Defensive — the CPU side already filters out directional
            // lights from the slice (their AABB is unbounded), but
            // guard against bucket-rebuild bugs that would route one in.
            if light.kind == 1u {
                continue;
            }
            let light_brdf = light_to_brdf(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        material_color.normal,
                        view_z_for_shadow,
                    );
                }
                color += direct * visibility;
            {% else %}
                color += direct;
            {% endif %}
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

// Transmission variant of `apply_lighting_per_mesh` — same slice walk,
// but IBL contribution uses the transmission-aware BRDF.
fn apply_lighting_per_mesh_with_transmission(
    material_color: PbrMaterialColor,
    surface_to_camera: vec3<f32>,
    world_position: vec3<f32>,
    lights_info: LightsInfo,
    transmission_background: vec3<f32>,
    receive_shadows: u32,
    slice_offset: u32,
    slice_count: u32,
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
            let view_z_for_shadow = -(camera_raw.view * vec4<f32>(world_position, 1.0)).z;
        {% endif %}

        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light = get_light(i);
            if light.kind != 1u {
                continue;
            }
            let light_brdf = light_to_brdf(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        material_color.normal,
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

        // Defensive: the OVERSIZED sentinel (`0xFFFFFFFFu`) is meant to
        // route a mesh through the per-froxel path in the consumer.
        // If it ever reaches the per-mesh slice count (routing bug,
        // stale meta upload, etc.), an unclamped loop spins ~4 billion
        // iterations and device-losts the GPU (black canvas / tab
        // crash). Clamp the sentinel to 0 so the per-mesh walk is a
        // no-op instead.
        let safe_slice_count = select(slice_count, 0u, slice_count == 0xFFFFFFFFu);
        for(var i = 0u; i < safe_slice_count; i = i + 1u) {
            let light_index = lights_storage[slice_offset + i];
            let light = get_light(light_index);
            if light.kind == 1u {
                continue;
            }
            let light_brdf = light_to_brdf(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        material_color.normal,
                        view_z_for_shadow,
                    );
                }
                color += direct * visibility;
            {% else %}
                color += direct;
            {% endif %}
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

const FROXEL_TILE_PIXEL_SIZE: u32 = 16u;
const FROXEL_SLICE_COUNT: u32 = {{ froxel_slice_count }}u;
// `max_per_froxel_capacity` is a runtime field on `cull_params` so the
// auto-grow path can bump the budget without recompiling.

// Maps a fragment's screen-space pixel coordinates + view-space depth
// (positive forward) into a froxel base index in `lights_storage`. The
// returned index already accounts for the head-region offset
// (`cull_params.mesh_indices_capacity_u32`) so callers can read
// `lights_storage[base]` for the count and `lights_storage[base + 1u + i]`
// for the i-th light index.
fn froxel_base_for_pixel(pixel_xy: vec2<f32>, view_z: f32) -> u32 {
    let tile_x = u32(pixel_xy.x) / FROXEL_TILE_PIXEL_SIZE;
    let tile_y = u32(pixel_xy.y) / FROXEL_TILE_PIXEL_SIZE;
    let tile_x_clamped = min(tile_x, max(cull_params.tiles_x, 1u) - 1u);
    let tile_y_clamped = min(tile_y, max(cull_params.tiles_y, 1u) - 1u);
    // Exponential z-slice mapping inverse:
    //   s = log(z / z_near) / log(z_far / z_near)
    let z = max(view_z, cull_params.z_near);
    let s = log(z / cull_params.z_near) / max(cull_params.log_far_over_near, 1e-6);
    let z_slice = clamp(u32(s * f32(FROXEL_SLICE_COUNT)), 0u, FROXEL_SLICE_COUNT - 1u);
    let tiles_per_layer = cull_params.tiles_x * cull_params.tiles_y;
    let froxel_idx = z_slice * tiles_per_layer + tile_y_clamped * cull_params.tiles_x + tile_x_clamped;
    let stride = cull_params.max_per_froxel_capacity + 1u;
    return cull_params.mesh_indices_capacity_u32 + froxel_idx * stride;
}

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

        // Directional walk — flat over n_lights, skipping non-directionals.
        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light = get_light(i);
            if light.kind != 1u {
                continue;
            }
            let light_brdf = light_to_brdf(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        material_color.normal,
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
            let raw_count = lights_storage[base];
            let count = min(raw_count, cull_params.max_per_froxel_capacity);
            for(var i = 0u; i < count; i = i + 1u) {
                let li = lights_storage[base + 1u + i];
                let light = get_light(li);
                // Defensive — directional shouldn't appear in the slice.
                if light.kind == 1u {
                    continue;
                }
                let light_brdf = light_to_brdf(light, material_color.normal, world_position);
                let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
                {% if shadows_enabled %}
                    var visibility: f32 = 1.0;
                    if receive_shadows != 0u {
                        visibility = sample_shadow_directional(
                            light.shadow_index,
                            world_position,
                            material_color.normal,
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

        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light = get_light(i);
            if light.kind != 1u {
                continue;
            }
            let light_brdf = light_to_brdf(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        material_color.normal,
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
            let raw_count = lights_storage[base];
            let count = min(raw_count, cull_params.max_per_froxel_capacity);
            for(var i = 0u; i < count; i = i + 1u) {
                let li = lights_storage[base + 1u + i];
                let light = get_light(li);
                if light.kind == 1u {
                    continue;
                }
                let light_brdf = light_to_brdf(light, material_color.normal, world_position);
                let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
                {% if shadows_enabled %}
                    var visibility: f32 = 1.0;
                    if receive_shadows != 0u {
                        visibility = sample_shadow_directional(
                            light.shadow_index,
                            world_position,
                            material_color.normal,
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
