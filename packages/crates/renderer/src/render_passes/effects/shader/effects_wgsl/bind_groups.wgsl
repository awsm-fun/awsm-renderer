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
};
@group(0) @binding(7) var<uniform> atmosphere_params: AtmosphereParamsRaw;
{% endif %}
