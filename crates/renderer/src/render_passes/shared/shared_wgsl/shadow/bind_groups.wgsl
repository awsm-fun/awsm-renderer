// Shadow bind-group declarations. The bind-group slot is supplied by
// the containing template via `shadow_group_index` — opaque uses slot
// 3. Transparent is deferred to Phase 9.
//
// Bindings 0..=7 must stay in lockstep with
// `shared::material::bind_group::shadow_bind_group_layout_entries`.

const MAX_SHADOW_DESCRIPTORS: u32 = 32u;

struct ShadowDescriptor {
    // Light-space view-projection used at sample time.
    view_projection: mat4x4<f32>,
    // (atlas.x, atlas.y, atlas.w, atlas.h) in normalised UV space.
    atlas_rect: vec4<f32>,
    // (depth_bias, normal_bias, hardness, pcss_penumbra_scale)
    bias_params: vec4<f32>,
    // (split_far_view_z, cascade_index, cascade_count_in_light, evsm_flag)
    // `evsm_flag` is 1.0 when this cascade should sample EVSM moments
    // from `evsm_atlas` instead of the PCF depth atlas. Phase 5 lands
    // the flag + sample-site dispatch; the moment-write compute pass
    // and Gaussian blur are deferred — until they land, EVSM cascades
    // transparently fall back to PCF sampling on `shadow_atlas`.
    cascade_info: vec4<f32>,
};

struct ShadowGlobals {
    // (atlas.w, atlas.h, evsm.w, evsm.h)
    atlas_sizes: vec4<f32>,
    // (evsm_exponent, evsm_blur_radius, sscs_step_count, sscs_enabled)
    evsm_sscs: vec4<f32>,
    // (debug_cascade_colors, max_point_shadows, pad, pad)
    flags: vec4<u32>,
};

struct ShadowDescriptorArray {
    items: array<ShadowDescriptor, MAX_SHADOW_DESCRIPTORS>,
};

@group({{ shadow_group_index }}) @binding(0) var shadow_atlas: texture_depth_2d;
@group({{ shadow_group_index }}) @binding(1) var shadow_atlas_sampler: sampler_comparison;
@group({{ shadow_group_index }}) @binding(2) var shadow_cube_array: texture_depth_cube_array;
@group({{ shadow_group_index }}) @binding(3) var shadow_cube_sampler: sampler_comparison;
@group({{ shadow_group_index }}) @binding(4) var evsm_atlas: texture_2d<f32>;
@group({{ shadow_group_index }}) @binding(5) var evsm_atlas_sampler: sampler;
@group({{ shadow_group_index }}) @binding(6) var<uniform> shadow_globals: ShadowGlobals;
@group({{ shadow_group_index }}) @binding(7) var<uniform> shadow_descriptors: ShadowDescriptorArray;

// Sentinel for "no shadow" — packed into `LightPacked.row4.z`.
const SHADOW_INDEX_NONE: u32 = 0xFFFFFFFFu;

// 16 Poisson-distributed samples in `[-1, 1]^2`. Used by both the
// PCSS blocker search and the variable-kernel PCF pass. The same
// table doubled-up keeps the WGSL small; a per-pixel rotation breaks
// up the regular pattern.
const POISSON_DISK_16: array<vec2<f32>, 16> = array<vec2<f32>, 16>(
    vec2<f32>(-0.94201624, -0.39906216),
    vec2<f32>( 0.94558609, -0.76890725),
    vec2<f32>(-0.09418410, -0.92938870),
    vec2<f32>( 0.34495938,  0.29387760),
    vec2<f32>(-0.91588581,  0.45771432),
    vec2<f32>(-0.81544232, -0.87912464),
    vec2<f32>(-0.38277543,  0.27676845),
    vec2<f32>( 0.97484398,  0.75648379),
    vec2<f32>( 0.44323325, -0.97511554),
    vec2<f32>( 0.53742981, -0.47373420),
    vec2<f32>(-0.26496911, -0.41893023),
    vec2<f32>( 0.79197514,  0.19090188),
    vec2<f32>(-0.24188840,  0.99706507),
    vec2<f32>(-0.81409955,  0.91437590),
    vec2<f32>( 0.19984126,  0.78641367),
    vec2<f32>( 0.14383161, -0.14100790),
);

// Inter-leaved Gradient Noise — Jorge Jimenez's hash, returns a
// per-pixel angle in `[0, 2π]`. Used to rotate the Poisson disk so
// adjacent fragments don't sample identical patterns.
fn pcss_disk_angle(coords: vec2<f32>) -> f32 {
    let magic = vec3<f32>(0.06711056, 0.00583715, 52.9829189);
    let noise = fract(magic.z * fract(dot(coords, magic.xy)));
    return noise * 6.2831853;
}

fn pcss_rotate(v: vec2<f32>, sin_a: f32, cos_a: f32) -> vec2<f32> {
    return vec2<f32>(v.x * cos_a - v.y * sin_a, v.x * sin_a + v.y * cos_a);
}

// Sample a single shadow descriptor (cascade / spot / face). Returns
// `[0, 1]` visibility (1.0 = lit, 0.0 = fully shadowed).
//
// Hardness branches:
//   0.0 = Hard, 1-tap.
//   1.0 = Soft, 3x3 PCF.
//   2.0 = PCSS (phase 6 — currently falls back to Soft).
fn sample_shadow_descriptor(
    descriptor_index: u32,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
) -> f32 {
    if descriptor_index >= MAX_SHADOW_DESCRIPTORS {
        return 1.0;
    }
    let desc = shadow_descriptors.items[descriptor_index];
    // EVSM cascades currently fall through to PCF — the moment write
    // pass / blur compute aren't online yet. The `cascade_info.w` flag
    // is preserved for the phase-5 follow-up that swaps the sample
    // call site to `sample_shadow_evsm`.

    let biased_pos = world_pos + world_normal * desc.bias_params.y;
    let clip = desc.view_projection * vec4<f32>(biased_pos, 1.0);
    if clip.w <= 0.0 {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let uv_local = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);
    let atlas_uv = desc.atlas_rect.xy + uv_local * desc.atlas_rect.zw;
    let ref_depth = ndc.z - desc.bias_params.x;
    let hardness = desc.bias_params.z;

    if hardness < 0.5 {
        return textureSampleCompareLevel(
            shadow_atlas,
            shadow_atlas_sampler,
            atlas_uv,
            ref_depth,
        );
    }
    let inv_atlas = vec2<f32>(
        1.0 / shadow_globals.atlas_sizes.x,
        1.0 / shadow_globals.atlas_sizes.y,
    );
    if hardness < 1.5 {
        // 3x3 PCF. Offsets in atlas-texel units.
        var sum = 0.0;
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let offset = vec2<f32>(f32(dx), f32(dy)) * inv_atlas;
                sum += textureSampleCompareLevel(
                    shadow_atlas,
                    shadow_atlas_sampler,
                    atlas_uv + offset,
                    ref_depth,
                );
            }
        }
        return sum / 9.0;
    }
    // PCSS — blocker-search + variable-kernel PCF. `pcss_scale`
    // (bias_params.w) tunes both the search radius and the apparent
    // light-source size.
    let pcss_scale = max(desc.bias_params.w, 0.01);
    let atlas_uv_to_texels = vec2<f32>(
        shadow_globals.atlas_sizes.x,
        shadow_globals.atlas_sizes.y,
    );
    let atlas_pixel = atlas_uv * atlas_uv_to_texels;
    let angle = pcss_disk_angle(atlas_pixel);
    let sin_a = sin(angle);
    let cos_a = cos(angle);
    let search_radius_texels = 3.0 * pcss_scale;

    var blocker_sum = 0.0;
    var blocker_count = 0u;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * search_radius_texels;
        let sample_uv = atlas_uv + off * inv_atlas;
        let coord = vec2<i32>(sample_uv * atlas_uv_to_texels);
        // `textureLoad` reads the raw depth value (no comparison)
        // for the blocker search. Out-of-bounds reads clamp to the
        // texture's edge value — for our usage that's a depth of 1.0
        // (cleared) which classifies as "not a blocker" → safe.
        let dim = vec2<i32>(atlas_uv_to_texels);
        let c = clamp(coord, vec2<i32>(0, 0), dim - vec2<i32>(1, 1));
        let d = textureLoad(shadow_atlas, c, 0);
        if d < ref_depth - 0.0005 {
            blocker_sum = blocker_sum + d;
            blocker_count = blocker_count + 1u;
        }
    }
    if blocker_count == 0u {
        return 1.0; // fully lit fast path
    }
    let avg_blocker = blocker_sum / f32(blocker_count);
    let light_size_texels = 5.0 * pcss_scale;
    let penumbra_texels = max(
        (ref_depth - avg_blocker) * light_size_texels / max(avg_blocker, 1e-4),
        1.0,
    );
    var pcf_sum = 0.0;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * penumbra_texels;
        pcf_sum = pcf_sum + textureSampleCompareLevel(
            shadow_atlas,
            shadow_atlas_sampler,
            atlas_uv + off * inv_atlas,
            ref_depth,
        );
    }
    return pcf_sum / 16.0;
}

// Per-light cascade selection. `descriptor_base` points to the first
// cascade descriptor of a directional light; `cascade_info.z` gives
// the cascade count. We walk descriptors descriptor_base..base+count
// and pick the first whose `cascade_info.x` (split_far in world-space
// depth) exceeds `view_z`. Returns 1.0 (no shadow) if `view_z` is
// beyond the last cascade.
fn sample_shadow_directional(
    descriptor_base: u32,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
    view_z: f32,
) -> f32 {
    if descriptor_base == SHADOW_INDEX_NONE {
        return 1.0;
    }
    if descriptor_base >= MAX_SHADOW_DESCRIPTORS {
        return 1.0;
    }
    let cascade_count = u32(shadow_descriptors.items[descriptor_base].cascade_info.z);
    var picked: u32 = SHADOW_INDEX_NONE;
    for (var i = 0u; i < cascade_count; i = i + 1u) {
        let idx = descriptor_base + i;
        if idx >= MAX_SHADOW_DESCRIPTORS {
            break;
        }
        let split_far = shadow_descriptors.items[idx].cascade_info.x;
        if view_z <= split_far {
            picked = idx;
            break;
        }
    }
    if picked == SHADOW_INDEX_NONE {
        return 1.0;
    }
    return sample_shadow_descriptor(picked, world_pos, world_normal);
}

// Debug-overlay tint for cascade visualisation. Driven by
// `shadow_globals.flags.x` (`debug_cascade_colors`). Returns the
// cascade-tinted color if enabled, otherwise the input unchanged.
fn debug_cascade_tint(
    base_color: vec3<f32>,
    descriptor_base: u32,
    view_z: f32,
) -> vec3<f32> {
    if shadow_globals.flags.x == 0u {
        return base_color;
    }
    if descriptor_base == SHADOW_INDEX_NONE || descriptor_base >= MAX_SHADOW_DESCRIPTORS {
        return base_color;
    }
    let cascade_count = u32(shadow_descriptors.items[descriptor_base].cascade_info.z);
    var picked_idx: u32 = cascade_count;
    for (var i = 0u; i < cascade_count; i = i + 1u) {
        let idx = descriptor_base + i;
        if idx >= MAX_SHADOW_DESCRIPTORS {
            break;
        }
        if view_z <= shadow_descriptors.items[idx].cascade_info.x {
            picked_idx = i;
            break;
        }
    }
    let palette = array<vec3<f32>, 4>(
        vec3<f32>(1.0, 0.4, 0.4),
        vec3<f32>(0.4, 1.0, 0.4),
        vec3<f32>(0.4, 0.5, 1.0),
        vec3<f32>(1.0, 1.0, 0.4),
    );
    if picked_idx >= 4u {
        return base_color;
    }
    return mix(base_color, palette[picked_idx], 0.35);
}
