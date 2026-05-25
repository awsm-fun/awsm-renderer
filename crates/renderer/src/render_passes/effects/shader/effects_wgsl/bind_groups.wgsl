@group(0) @binding(0) var composite_tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> camera_raw: CameraRaw;
{% if multisampled_geometry %}
    @group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;
{% else %}
    @group(0) @binding(2) var depth_tex: texture_depth_2d;
{% endif %}

{% if !ping_pong %}
    @group(0) @binding(3) var bloom_tex: texture_2d<f32>;
    @group(0) @binding(4) var effects_tex: texture_storage_2d<rgba16float, write>;
{% else %}
    @group(0) @binding(3) var effects_tex: texture_2d<f32>;
    @group(0) @binding(4) var bloom_tex: texture_storage_2d<rgba16float, write>;
{% endif%}

// Renderer-wide per-frame uniform — see `shared_wgsl/frame_globals.wgsl`.
@group(0) @binding(5) var<uniform> frame_globals_raw: FrameGlobalsRaw;
