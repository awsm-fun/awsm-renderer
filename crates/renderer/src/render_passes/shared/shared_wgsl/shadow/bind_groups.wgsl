// Shadow bind-group declarations. The bind-group slot is supplied by
// the containing template via `shadow_group_index` — opaque uses slot
// 3. Transparent is deferred to Phase 9.
//
// Bindings 0..=6 must stay in lockstep with
// `shared::material::bind_group::shadow_bind_group_layout_entries`.
//
// Phase 0: declarations only — sampling helpers come online in
// Phase 2 (PCF), Phase 5 (EVSM), Phase 6 (PCSS), and Phase 8 (cube).
//
// `shadow_descriptors` (a storage buffer) is intentionally absent in
// Phase 0 because adding it would exceed the opaque compute stage's
// `maxStorageBuffersPerShaderStage = 10` adapter limit. Phase 2 will
// free a slot (likely by folding `instance_attrs`) and re-introduce
// the descriptor binding.

struct ShadowGlobals {
    // (atlas.w, atlas.h, evsm.w, evsm.h)
    atlas_sizes: vec4<f32>,
    // (evsm_exponent, evsm_blur_radius, sscs_step_count, sscs_enabled)
    evsm_sscs: vec4<f32>,
    // (debug_cascade_colors, max_point_shadows, pad, pad)
    flags: vec4<u32>,
};

@group({{ shadow_group_index }}) @binding(0) var shadow_atlas: texture_depth_2d;
@group({{ shadow_group_index }}) @binding(1) var shadow_atlas_sampler: sampler_comparison;
@group({{ shadow_group_index }}) @binding(2) var shadow_cube_array: texture_depth_cube_array;
@group({{ shadow_group_index }}) @binding(3) var shadow_cube_sampler: sampler_comparison;
@group({{ shadow_group_index }}) @binding(4) var evsm_atlas: texture_2d<f32>;
@group({{ shadow_group_index }}) @binding(5) var evsm_atlas_sampler: sampler;
@group({{ shadow_group_index }}) @binding(6) var<uniform> shadow_globals: ShadowGlobals;
