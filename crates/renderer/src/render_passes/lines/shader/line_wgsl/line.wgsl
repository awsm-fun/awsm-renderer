@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;

struct CameraRaw {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    inv_proj: mat4x4<f32>,
    inv_view: mat4x4<f32>,
    position: vec4<f32>,
    frame_count_and_padding: vec4<u32>,
    frustum_rays: array<vec4<f32>, 4>,
    _padding_end: array<vec4<f32>, 2>,
};

// One segment per instance. Each segment stretches between `a` and `b`
// and is expanded into a screen-space rectangle of width `width_px` in
// the vertex shader.
struct LineSegment {
    a: vec4<f32>,        // .xyz = world pos A, .w = unused
    color_a: vec4<f32>,  // RGBA at A
    b: vec4<f32>,        // .xyz = world pos B, .w = unused
    color_b: vec4<f32>,  // RGBA at B
};

@group(0) @binding(1) var<storage, read> segments: array<LineSegment>;

struct LineUniform {
    width_px: f32,
    viewport_w: f32,
    viewport_h: f32,
    _pad: f32,
};

@group(0) @binding(2) var<uniform> line_uniform: LineUniform;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

// `vertex_index` 0..4 picks the four corners of a triangle-strip quad
// laid across the screen-space segment:
//   0 = A-left, 1 = A-right, 2 = B-left, 3 = B-right
@vertex
fn vert_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    let seg = segments[instance_index];

    let clip_a = camera_raw.view_proj * vec4<f32>(seg.a.xyz, 1.0);
    let clip_b = camera_raw.view_proj * vec4<f32>(seg.b.xyz, 1.0);

    // Project to NDC.
    let ndc_a = clip_a.xy / clip_a.w;
    let ndc_b = clip_b.xy / clip_b.w;

    // Screen-aspect correction: convert NDC delta to pixel delta, then
    // compute the perpendicular direction in pixel space.
    let viewport = vec2<f32>(line_uniform.viewport_w, line_uniform.viewport_h);
    let half_viewport = viewport * 0.5;

    let screen_a = ndc_a * half_viewport;
    let screen_b = ndc_b * half_viewport;
    var dir = screen_b - screen_a;
    let len = max(length(dir), 1e-6);
    dir = dir / len;
    let perp_px = vec2<f32>(-dir.y, dir.x) * (line_uniform.width_px * 0.5);

    // Convert the per-vertex pixel offset back into NDC space at the
    // respective endpoint's clip-w so the line stays a fixed pixel width.
    let perp_ndc_a = perp_px / half_viewport;
    let perp_ndc_b = perp_px / half_viewport;

    let is_b = vertex_index >= 2u;
    let is_right = (vertex_index & 1u) == 1u;

    var clip_pos: vec4<f32>;
    var color: vec4<f32>;
    if (is_b) {
        let sign = select(-1.0, 1.0, is_right);
        let offset_xy = perp_ndc_b * sign * clip_b.w;
        clip_pos = vec4<f32>(clip_b.xy + offset_xy, clip_b.zw);
        color = seg.color_b;
    } else {
        let sign = select(-1.0, 1.0, is_right);
        let offset_xy = perp_ndc_a * sign * clip_a.w;
        clip_pos = vec4<f32>(clip_a.xy + offset_xy, clip_a.zw);
        color = seg.color_a;
    }

    var out: VertexOutput;
    out.clip_position = clip_pos;
    out.color = color;
    return out;
}

@fragment
fn frag_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
