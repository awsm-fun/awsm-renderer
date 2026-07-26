@group(0) @binding(0) var composite_tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> camera_raw: CameraRaw;
{% if multisampled_geometry %}
    @group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;
{% else %}
    @group(0) @binding(2) var depth_tex: texture_depth_2d;
{% endif %}

@group(0) @binding(3) var bloom_tex: texture_2d<f32>;
@group(0) @binding(4) var effects_tex: texture_storage_2d<rgba16float, write>;

// Renderer-wide per-frame uniform — see `shared_wgsl/frame_globals.wgsl`.
@group(0) @binding(5) var<uniform> frame_globals_raw: FrameGlobalsRaw;
// SMAA blend weights (up/down/left/right) from the SMAA pre-pass; a 1x1
// zero dummy when SMAA is off (the smaa-off shader variant never reads it,
// but the layout keeps a stable shape across the toggle).
@group(0) @binding(6) var smaa_weights_tex: texture_2d<f32>;

{% if atmosphere %}
// Live atmospheric-haze knobs. The LAYOUT always carries this entry (so the
// haze toggle doesn't move the bind-group shape) but the haze-off shader
// variant doesn't declare it — a WGSL module may bind a subset of its layout.
struct AtmosphereParamsRaw {
    color: vec3<f32>,
    density: f32,
    base_height: f32,
    height_falloff: f32,
    // Froxel grid depth mapping — volumetric composite only. Copied from the
    // light-culling params so the pixel→slice map here is the same one the
    // volume was written with.
    slice_count: f32,
    froxel_z_near: f32,
    froxel_log_far_over_near: f32,
};
@group(0) @binding(7) var<uniform> atmosphere_params: AtmosphereParamsRaw;
{% endif %}

{% if atmosphere_volumetric %}
// The integrated froxel volume built by the volumetrics pass:
//   rgb = in-scatter accumulated from the eye to this slice
//   a   = transmittance to that slice
// A 1×1×1 dummy sits here on every other path, so the LAYOUT never moves.
// Must equal `FROXEL_TILE_PIXEL_SIZE` in volumetrics/texture.rs and
// `shared_wgsl/lighting/froxel_walk.wgsl` — three declarations of one number
// because they live in three shader modules with no shared include.
const FROXEL_TILE_PIXEL_SIZE: f32 = 16.0;
@group(0) @binding(8) var volumetric_tex: texture_3d<f32>;
{% endif %}
