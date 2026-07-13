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
            world_position,
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
        {% if needs_shadow_sampling %}
            // View-space z (positive forward) for cascade selection.
            let view_z_for_shadow = -(camera_raw.view * vec4<f32>(world_position, 1.0)).z;
        {% endif %}
        for(var i = 0u; i < lights_info.n_lights; i = i + 1u) {
            let light = get_light(i);
            let light_brdf = light_sample(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if needs_shadow_sampling %}
                // Modulate by shadow visibility (1.0 = lit, 0.0 = fully
                // shadowed). `shadow_index == SHADOW_INDEX_NONE` short-
                // circuits to 1.0; the cascade selector walks
                // descriptors descriptor_base..base+count.
                // NOTE: this flat (non-froxel) `apply_lighting` is the
                // transparent-pass surface; opaque calls the froxel variants
                // below. When `shadow_from_buffer` (opaque prep-read) it isn't
                // called, and the inline sampler is dropped (the size win), so
                // the shadow branch collapses to the unshadowed accumulation.
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
                        // light.direction is normalized at the CPU pack boundary (lights.rs).
                        let sscs_dir = -light.direction;
                        visibility = visibility * apply_sscs(world_position, sscs_dir, shadow_globals.sscs_params.z);
                    }
                }
                color += direct * visibility;
            {% else %}
                color += direct;
            {% endif %}
        }
        {% if needs_shadow_sampling %}
            // Cascade-debug overlay — the first DIRECTIONAL light's
            // descriptor base (cascades are directional-only; `get_light(0u)`
            // could be a punctual light since directionals live behind the
            // prefix indirection). Gated on `needs_shadow_sampling` because
            // `view_z_for_shadow` only exists under it here (this flat
            // variant is the transparent-pass surface, which always
            // inline-samples when it lights).
            if get_n_directional() > 0u {
                color = debug_cascade_tint(
                    color,
                    get_light(get_directional_light_index(0u)).shadow_index,
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

{% if prep_present %}
// Plan B (stage 4/5a/5b): read a prep pass's packed shadow-visibility buffer
// instead of sampling shadow maps inline. `slot` is the j-th shadowed light in
// the canonical froxel walk (directional prefix then per-froxel punctual) — the
// SAME order `cs_prep` / `cs_prep_edge` wrote, so slot j here matches layer j/4 /
// channel j%4 there. Slots >= K (clamped per-pixel caster cap) overflow to lit
// (1.0), matching prep's clamp-and-skip.
//
// The SOURCE is runtime-selected by the per-thread PrepReadContext mode:
//   PRIMARY (cs_opaque) → full-screen prep_shadow_visibility at `pixel_xy`.
//   EDGE    (cs_edge)   → compact prep_edge_shadow at `g_prep_ctx.edge_shadow_xy`
//                         (the 2D texel for this edge_pixel × sample, set per
//                         sample in cs_edge — Stage 5b-shadow).
fn prep_shadow_read(pixel_xy: vec2<f32>, slot: u32) -> f32 {
    if (slot >= {{ max_shadow_casters }}u) {
        return 1.0;
    }
{% if multisampled_geometry %}
    if (g_prep_ctx.mode == PREP_MODE_EDGE) {
        let texel = textureLoad(prep_edge_shadow, g_prep_ctx.edge_shadow_xy, i32(slot / 4u), 0);
        return texel[slot % 4u];
    }
{% endif %}
    let c = vec2<i32>(i32(pixel_xy.x), i32(pixel_xy.y));
    let texel = textureLoad(prep_shadow_visibility, c, i32(slot / 4u), 0);
    return texel[slot % 4u];
}
{% endif %}

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
            world_position,
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
        {% if needs_shadow_sampling %}
            // Only the inline-sampling path consumes the cascade-selection
            // view-z; the buffer path reads precomputed visibility.
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

        {% if prep_present %}
        // Plan B (stage 4/5a): j-th shadowed light in the canonical froxel walk
        // for the PRIMARY (prep-buffer) read path. Advances for EVERY shadowed
        // light (independent of receive_shadows / range-reject) so the slot
        // stays aligned with what `cs_prep` wrote.
        var shadow_slot: u32 = 0u;
        {% endif %}

        // Directional walk — bounded to the directional-light prefix
        // (see get_n_directional / get_directional_light_index).
        let n_directional = get_n_directional();
        for(var d = 0u; d < n_directional; d = d + 1u) {
            let light = get_light(get_directional_light_index(d));
            let light_brdf = light_sample(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if prep_present %}
                // Stage 5a: runtime-select the shadow source by the per-thread
                // PrepReadContext mode. PRIMARY (cs_opaque) reads the prep buffer;
                // any other mode (cs_edge=RECOMPUTE — EDGE in 5b) inline-samples.
                var visibility: f32 = 1.0;
                if (g_prep_ctx.mode == PREP_MODE_PRIMARY || g_prep_ctx.mode == PREP_MODE_EDGE) {
                    if light.shadow_index != SHADOW_INDEX_NONE {
                        // Advance the slot for every shadowed directional (prep did
                        // too); apply receive_shadows at read time.
                        visibility = select(1.0, prep_shadow_read(pixel_xy, shadow_slot), receive_shadows != 0u);
                        shadow_slot = shadow_slot + 1u;
                    }
                }
                {% if needs_shadow_sampling %}
                else {
                    if receive_shadows != 0u {
                        visibility = sample_shadow_directional(
                            light.shadow_index,
                            world_position,
                            shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                            view_z_for_shadow,
                        );
                        if light.shadow_index != SHADOW_INDEX_NONE {
                            // light.direction is normalized at the CPU pack boundary (lights.rs).
                            let sscs_dir = -light.direction;
                            visibility = visibility * apply_sscs(world_position, sscs_dir, shadow_globals.sscs_params.z);
                        }
                    }
                }
                {% endif %}
                color += direct * visibility;
            {% else if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                        view_z_for_shadow,
                    );
                    if light.shadow_index != SHADOW_INDEX_NONE {
                        // light.direction is normalized at the CPU pack boundary (lights.rs).
                        let sscs_dir = -light.direction;
                        visibility = visibility * apply_sscs(world_position, sscs_dir, shadow_globals.sscs_params.z);
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
                // (Prep `continue`s on kind==1 BEFORE the shadow check too, so
                // prep + read skip identical lights and the slot stays aligned.)
                if light.kind == 1u {
                    continue;
                }
                {% if prep_present %}
                    // Stage 5a runtime select. PRIMARY: resolve + advance the slot
                    // for a shadowed light BEFORE the range-reject continue — prep
                    // advanced its slot for every shadowed light regardless of
                    // range, so the read must too. Other mode (RECOMPUTE):
                    // inline-sample AFTER the range-reject (no slot).
                    var visibility: f32 = 1.0;
                    if (g_prep_ctx.mode == PREP_MODE_PRIMARY || g_prep_ctx.mode == PREP_MODE_EDGE) {
                        if light.shadow_index != SHADOW_INDEX_NONE {
                            visibility = select(1.0, prep_shadow_read(pixel_xy, shadow_slot), receive_shadows != 0u);
                            shadow_slot = shadow_slot + 1u;
                        }
                        // Range reject: skip this light's CONTRIBUTION (slot already advanced).
                        let to_light = light.position - world_position;
                        if light.range > 0.0 && dot(to_light, to_light) > light.range * light.range {
                            continue;
                        }
                        let light_brdf = light_sample(light, material_color.normal, world_position);
                        let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
                        color += direct * visibility;
                    } else {
                        // RECOMPUTE inline path (cs_edge). Range reject first, then
                        // inline-sample — matches the no-prep punctual ordering.
                        let to_light = light.position - world_position;
                        if light.range > 0.0 && dot(to_light, to_light) > light.range * light.range {
                            continue;
                        }
                        let light_brdf = light_sample(light, material_color.normal, world_position);
                        let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
                        {% if needs_shadow_sampling %}
                        if receive_shadows != 0u {
                            visibility = sample_shadow_directional(
                                light.shadow_index,
                                world_position,
                                shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                                view_z_for_shadow,
                            );
                            if light.shadow_index != SHADOW_INDEX_NONE {
                                visibility = visibility * apply_sscs(world_position, normalize(to_light), shadow_globals.sscs_params.w);
                            }
                        }
                        {% endif %}
                        color += direct * visibility;
                    }
                {% else %}
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
                            if light.shadow_index != SHADOW_INDEX_NONE {
                                visibility = visibility * apply_sscs(world_position, normalize(to_light), shadow_globals.sscs_params.w);
                            }
                        }
                        color += direct * visibility;
                    {% else %}
                        color += direct;
                    {% endif %}
                {% endif %}
            }
        }

        // Cascade-debug overlay — the first DIRECTIONAL light's descriptor
        // base (cascades are directional-only; `get_light(0u)` could be a
        // punctual light since directionals live behind the prefix
        // indirection). UNGATED on `needs_shadow_sampling`: the opaque pass
        // compiles with it `false` (prep reads the shadow buffer), which used
        // to compile this overlay out of every opaque module — the
        // `debug_cascade_colors` toggle changed zero pixels. The tint only
        // reads the always-bound shadow uniforms (emitted ungated in
        // `shadow/bind_groups.wgsl`) and short-circuits on `flags.x == 0u`.
        if n_directional > 0u {
            color = debug_cascade_tint(
                color,
                get_light(get_directional_light_index(0u)).shadow_index,
                world_position,
                view_z,
            );
        }
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
            world_position,
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
        {% if needs_shadow_sampling %}
            // Only the inline-sampling path consumes the cascade-selection
            // view-z; the buffer path reads precomputed visibility.
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

        {% if prep_present %}
        // Stage 4/5a: PRIMARY-path slot (see apply_lighting_per_froxel).
        var shadow_slot: u32 = 0u;
        {% endif %}

        let n_directional = get_n_directional();
        for(var d = 0u; d < n_directional; d = d + 1u) {
            let light = get_light(get_directional_light_index(d));
            let light_brdf = light_sample(light, material_color.normal, world_position);
            let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
            {% if prep_present %}
                // Stage 5a runtime select by PrepReadContext mode.
                var visibility: f32 = 1.0;
                if (g_prep_ctx.mode == PREP_MODE_PRIMARY || g_prep_ctx.mode == PREP_MODE_EDGE) {
                    if light.shadow_index != SHADOW_INDEX_NONE {
                        visibility = select(1.0, prep_shadow_read(pixel_xy, shadow_slot), receive_shadows != 0u);
                        shadow_slot = shadow_slot + 1u;
                    }
                }
                {% if needs_shadow_sampling %}
                else {
                    if receive_shadows != 0u {
                        visibility = sample_shadow_directional(
                            light.shadow_index,
                            world_position,
                            shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                            view_z_for_shadow,
                        );
                        if light.shadow_index != SHADOW_INDEX_NONE {
                            // light.direction is normalized at the CPU pack boundary (lights.rs).
                            let sscs_dir = -light.direction;
                            visibility = visibility * apply_sscs(world_position, sscs_dir, shadow_globals.sscs_params.z);
                        }
                    }
                }
                {% endif %}
                color += direct * visibility;
            {% else if shadows_enabled %}
                var visibility: f32 = 1.0;
                if receive_shadows != 0u {
                    visibility = sample_shadow_directional(
                        light.shadow_index,
                        world_position,
                        shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                        view_z_for_shadow,
                    );
                    if light.shadow_index != SHADOW_INDEX_NONE {
                        // light.direction is normalized at the CPU pack boundary (lights.rs).
                        let sscs_dir = -light.direction;
                        visibility = visibility * apply_sscs(world_position, sscs_dir, shadow_globals.sscs_params.z);
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
                {% if prep_present %}
                    var visibility: f32 = 1.0;
                    if (g_prep_ctx.mode == PREP_MODE_PRIMARY || g_prep_ctx.mode == PREP_MODE_EDGE) {
                        if light.shadow_index != SHADOW_INDEX_NONE {
                            visibility = select(1.0, prep_shadow_read(pixel_xy, shadow_slot), receive_shadows != 0u);
                            shadow_slot = shadow_slot + 1u;
                        }
                        let to_light = light.position - world_position;
                        if light.range > 0.0 && dot(to_light, to_light) > light.range * light.range {
                            continue;
                        }
                        let light_brdf = light_sample(light, material_color.normal, world_position);
                        let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
                        color += direct * visibility;
                    } else {
                        let to_light = light.position - world_position;
                        if light.range > 0.0 && dot(to_light, to_light) > light.range * light.range {
                            continue;
                        }
                        let light_brdf = light_sample(light, material_color.normal, world_position);
                        let direct = brdf_direct(material_color, light_brdf, surface_to_camera);
                        {% if needs_shadow_sampling %}
                        if receive_shadows != 0u {
                            visibility = sample_shadow_directional(
                                light.shadow_index,
                                world_position,
                                shadow_normal_toward_light(material_color.normal, light_brdf.light_dir),
                                view_z_for_shadow,
                            );
                            if light.shadow_index != SHADOW_INDEX_NONE {
                                visibility = visibility * apply_sscs(world_position, normalize(to_light), shadow_globals.sscs_params.w);
                            }
                        }
                        {% endif %}
                        color += direct * visibility;
                    }
                {% else %}
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
                            if light.shadow_index != SHADOW_INDEX_NONE {
                                visibility = visibility * apply_sscs(world_position, normalize(to_light), shadow_globals.sscs_params.w);
                            }
                        }
                        color += direct * visibility;
                    {% else %}
                        color += direct;
                    {% endif %}
                {% endif %}
            }
        }

        // Cascade-debug overlay — the first DIRECTIONAL light's descriptor
        // base (cascades are directional-only; `get_light(0u)` could be a
        // punctual light since directionals live behind the prefix
        // indirection). UNGATED on `needs_shadow_sampling`: the opaque pass
        // compiles with it `false` (prep reads the shadow buffer), which used
        // to compile this overlay out of every opaque module — the
        // `debug_cascade_colors` toggle changed zero pixels. The tint only
        // reads the always-bound shadow uniforms (emitted ungated in
        // `shadow/bind_groups.wgsl`) and short-circuits on `flags.x == 0u`.
        if n_directional > 0u {
            color = debug_cascade_tint(
                color,
                get_light(get_directional_light_index(0u)).shadow_index,
                world_position,
                view_z,
            );
        }
    {% endif %}

    return color;
}
{% endif %}
