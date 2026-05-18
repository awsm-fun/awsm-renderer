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
    // (split_far_view_z, cascade_index, cascade_count_in_light, 0)
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
    // 3x3 PCF. Offsets are scaled by the atlas-rect's UV extent so a
    // 1024² cascade in a 4096 atlas takes 1-cascade-texel steps, not
    // 1-atlas-texel steps.
    let inv_atlas = vec2<f32>(
        1.0 / shadow_globals.atlas_sizes.x,
        1.0 / shadow_globals.atlas_sizes.y,
    );
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
