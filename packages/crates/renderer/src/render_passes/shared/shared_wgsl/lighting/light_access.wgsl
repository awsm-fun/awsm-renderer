// light_access.wgsl — light-data access shared by every lit shading model
// (PBR + toon). Split out of the former lights.wgsl so non-PBR materials can
// walk the light list WITHOUT pulling in apply_lighting + the PBR BRDF.
//
// DELIBERATELY NOT skinny-gated, and included in every opaque pipeline. The
// packed structs (LightsInfoPacked/LightPacked) are part of the bind-group ABI
// (bind_groups.wgsl declares the bindings with these types), so — exactly like
// the bindings themselves — they must always be present. The accessor functions
// below are ~80 lines of trivial unpack/switch code: their compile cost is
// negligible next to the gated brdf.wgsl (889 lines of GGX/Fresnel/IBL math),
// and gating them would only entangle the per-pixel shade entry points (which
// take `LightsInfo` for every material) for no real win. So this whole file is
// cheap, always-present shared infrastructure.

// `data`: x = n_lights, y = prefiltered-env mip count, z = irradiance mip
// count, w = n_directional (count of directional lights this frame, ≤ 8).
// `directional`: packed-array indices of the (≤ 8) directional lights.
// The shading paths use these to walk *only* the directionals in
// O(n_directional) instead of scanning all `n_lights` per pixel — the
// latter is catastrophic when a scene has hundreds/thousands of punctuals
// (each pixel would skip over every punctual just to find the sun).
struct LightsInfoPacked {
    data: vec4<u32>,
    directional: array<vec4<u32>, 2>,
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
            light_dir = normalize(-light.direction); // surface -> light
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
