// light_access.wgsl — light-data ACCESSOR FUNCTIONS (get_lights_info / get_light
// / light_sample / …). The STRUCTS they return live in `light_access_types.wgsl`
// (always included — bind-group ABI).
//
// GATED on `inc.light_access` (Phase 4 of docs/plans/material-optimizations.md).
// A material — or a whole scene known to have no lights — can opt out of lighting
// completely: these accessors are dropped, and the per-pixel shade entry points
// gate their `get_lights_info()` calls + `lights_info` params to match. PBR/Toon
// declare LIGHT_ACCESS so they keep them; an unlit/flipbook/no-light custom
// material does not.
//
// (Historical note: this file was previously "deliberately not skinny-gated" on
// the reasoning that the structs are ABI and the accessors are cheap. That
// predated the granular include splits + the explicit lighting opt-out; the
// structs are now split into light_access_types.wgsl so the ABI stays intact
// while the accessor bodies gate out.)

fn get_lights_info() -> LightsInfo {
    // expects `lights_info` is global LightsInfoPacked
    return LightsInfo(
        lights_info.data.x,
        IblInfo(
            lights_info.data.y,
            lights_info.data.z,
            lights_info.probe_center_enabled,
            lights_info.probe_half_pad.xyz
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

// Count of directional lights this frame (≤ 8). Reads the global
// `lights_info` uniform directly so the shading functions (whose
// `lights_info` *parameter* shadows the global) can still reach it.
fn get_n_directional() -> u32 {
    return lights_info.data.w;
}

// Packed-array index of the `d`-th directional light (`d < get_n_directional()`).
// Pair with `get_light` to walk only directionals instead of scanning all
// `n_lights` per pixel.
fn get_directional_light_index(d: u32) -> u32 {
    return lights_info.directional[d >> 2u][d & 3u];
}

// Debug: map an applied-punctual-light count to a jet colormap
// (black = 0, blue = few, green = mid, red = many; 64+ clamps to red).
// Used by the `cull_params.debug_light_heatmap` visualization to inspect
// froxel occupancy / cull behaviour. Tune the 64.0 reference to taste.
fn light_count_heatmap(count: u32) -> vec3<f32> {
    if (count == 0u) {
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    let t = saturate(f32(count) / 64.0);
    let r = saturate(1.5 - abs(4.0 * t - 3.0));
    let g = saturate(1.5 - abs(4.0 * t - 2.0));
    let b = saturate(1.5 - abs(4.0 * t - 1.0));
    return vec3<f32>(r, g, b);
}

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
fn light_sample(light:Light, normal: vec3<f32>, world_position: vec3<f32>) -> LightSample {
    var light_dir: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
    var radiance: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
    var n_dot_l: f32 = 0.0;

    switch (light.kind) {
        case 0u: {
            // no light, skip
        }
        case 1u: { // Directional
            light_dir = -light.direction; // surface -> light (unit at pack boundary)
            radiance = light.color * light.intensity;
            n_dot_l = max(dot(normal, light_dir), 0.0);
        }
        case 2u: { // Point
            let surface_to_light = light.position - world_position;
            let dist = length(surface_to_light);
            light_dir = surface_to_light / max(dist, 1e-6); // surface -> light (guard dist==0)
            let attenuation = inverse_square(light.range, dist);
            radiance = light.color * light.intensity * attenuation;
            n_dot_l = max(dot(normal, light_dir), 0.0);
        }
        case 3u: { // Spot
            let surface_to_light = light.position - world_position;
            let dist = length(surface_to_light);
            light_dir = surface_to_light / max(dist, 1e-6); // surface -> light (guard dist==0)
            let cos_l = dot(light_dir, -light.direction);
            let spot = spot_falloff(light.inner_cone, light.outer_cone, cos_l);
            let attenuation = inverse_square(light.range, dist) * spot;
            radiance = light.color * light.intensity * attenuation;
            n_dot_l = max(dot(normal, light_dir), 0.0);
        }
        default: { // unexpected
        }
    }

    return LightSample(
        normal,
        n_dot_l,
        light_dir,
        radiance,
    );
}

// Orient the surface normal toward the light for shadow-bias purposes.
// The shadow normal-offset bias (`world_pos + normal * normal_bias`) assumes
// the lit side faces the light. For a diffuse-transmissive surface lit from
// BEHIND (n·l < 0, driving the back-transmission lobe), the front normal
// would push the sample into self-shadow and extinguish the transmission.
// Flipping the normal to face the light fixes the bias for that lobe. For
// every other case this is a no-op: front-lit fragments keep the front
// normal, and a back-lit *non*-transmissive fragment has `brdf_direct == 0`,
// so the (changed) visibility multiplies zero — bit-identical result.
fn shadow_normal_toward_light(surface_normal: vec3<f32>, light_dir: vec3<f32>) -> vec3<f32> {
    return surface_normal * select(1.0, -1.0, dot(surface_normal, light_dir) < 0.0);
}

// spot light mask (smooth edge)
fn spot_falloff(inner_cos: f32, outer_cos: f32, cos_l: f32) -> f32 {
    let smoothed = saturate((cos_l - outer_cos) / (inner_cos - outer_cos));
    return smoothed * smoothed;
}
