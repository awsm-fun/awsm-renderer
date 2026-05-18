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

// Sample a directional/spot shadow descriptor at `world_pos`, offset
// along `world_normal` by the descriptor's `normal_bias` to suppress
// acne. Returns `[0, 1]` visibility (1.0 = lit, 0.0 = fully shadowed).
//
// Phase 2: 1-tap `textureSampleCompare`. Phase 3 adds PCF + the
// hardness branch; phase 6 adds PCSS.
fn sample_shadow_directional(
    descriptor_index: u32,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
) -> f32 {
    if descriptor_index == SHADOW_INDEX_NONE {
        return 1.0;
    }
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
    // Reject samples outside the cascade's clip volume — they fall
    // back to "lit" so geometry past the cascade range doesn't pick
    // up a false dark blob from the edge of the atlas.
    if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    // NDC.xy [-1, 1] → UV [0, 1] with Y flipped (WebGPU's framebuffer
    // origin is top-left while NDC origin is bottom-left).
    let uv_local = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);
    let atlas_uv = desc.atlas_rect.xy + uv_local * desc.atlas_rect.zw;
    let ref_depth = ndc.z - desc.bias_params.x;
    let hardness = desc.bias_params.z;
    // 0.0 = Hard, 1.0 = Soft (3x3 PCF), 2.0 = PCSS (phase 6).
    if hardness < 0.5 {
        return textureSampleCompareLevel(
            shadow_atlas,
            shadow_atlas_sampler,
            atlas_uv,
            ref_depth,
        );
    }
    // 3x3 PCF. `shadow_globals.atlas_sizes.x` is the atlas width in
    // texels; the kernel offset is one texel per step in normalised UV
    // space.
    let inv_atlas = 1.0 / shadow_globals.atlas_sizes.x;
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
