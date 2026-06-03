@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;

// Raw camera uniform structure (matches GPU buffer layout with padding).
//
// Mirrors `shared_wgsl/camera.wgsl`'s `CameraRaw` minus the trailing
// fields this shader doesn't need. `frame_count_and_padding` was
// removed when the monotonic frame counter migrated to the
// `frame_globals` uniform (see `crates/renderer/src/frame_globals`).
// Total layout is now 496 bytes; the `_padding_end` array sizes
// the locally-declared struct out to that figure.
struct CameraRaw {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    inv_proj: mat4x4<f32>,
    inv_view: mat4x4<f32>,
    position: vec4<f32>,  // .xyz = position, .w = unused
    frustum_rays: array<vec4<f32>, 4>,
    _padding_end: array<vec4<f32>, 2>,  // viewport + dof_params (this shader doesn't use them)
};

// Friendly camera structure (no padding, easier to work with)
struct Camera {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    inv_proj: mat4x4<f32>,
    inv_view: mat4x4<f32>,
    position: vec3<f32>,
    frustum_rays: array<vec4<f32>, 4>,
};

// Convert from raw uniform to friendly structure
fn camera_from_raw(raw: CameraRaw) -> Camera {
    var camera: Camera;
    camera.view = raw.view;
    camera.proj = raw.proj;
    camera.view_proj = raw.view_proj;
    camera.inv_view_proj = raw.inv_view_proj;
    camera.inv_proj = raw.inv_proj;
    camera.inv_view = raw.inv_view;
    camera.position = raw.position.xyz;
    camera.frustum_rays = raw.frustum_rays;
    return camera;
}


@vertex
fn vert_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;

    // Generate oversized triangle vertices using bit manipulation
    // Goal: vertex 0→(-1,-1), vertex 1→(3,-1), vertex 2→(-1,3)

    // X coordinate generation:
    // vertex_index: 0 → 0<<1 = 0 → 0&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 1 → 1<<1 = 2 → 2&2 = 2 → 2*2-1 = 3  ✓
    // vertex_index: 2 → 2<<1 = 4 → 4&2 = 0 → 0*2-1 = -1 ✓
    let x = f32((vertex_index << 1u) & 2u) * 2.0 - 1.0;

    // Y coordinate generation:
    // vertex_index: 0 → 0&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 1 → 1&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 2 → 2&2 = 2 → 2*2-1 = 3  ✓
    let y = f32(vertex_index & 2u) * 2.0 - 1.0;

    out.clip_position = vec4<f32>(x, y, 0.0, 1.0);
    out.ndc = vec2<f32>(x, y);

    return out;
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

struct FragmentInput {
    @location(0) ndc: vec2<f32>,
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

// ===== GRID CONFIGURATION =====
// Blender-like color scheme
const GRID_COLOR_BACKGROUND: vec3<f32> = vec3<f32>(0.18, 0.18, 0.18); // Light gray background/tiles
const GRID_COLOR_MINOR: vec3<f32> = vec3<f32>(0.28, 0.28, 0.28);      // Medium gray minor lines
const GRID_COLOR_MAJOR: vec3<f32> = vec3<f32>(0.38, 0.38, 0.38);      // Lighter gray major lines
const GRID_COLOR_X_AXIS: vec3<f32> = vec3<f32>(0.95, 0.3, 0.3);       // Red X axis
const GRID_COLOR_Z_AXIS: vec3<f32> = vec3<f32>(0.3, 0.5, 0.95);       // Blue Z axis

const GRID_ALPHA_BACKGROUND: f32 = 0.7;
const GRID_ALPHA_MINOR: f32 = 0.9;
const GRID_ALPHA_MAJOR: f32 = 1.0;
const GRID_ALPHA_AXIS: f32 = 1.0;
// Push grid fragments slightly farther than coplanar scene geometry to avoid z-fighting.
const GRID_DEPTH_EPSILON: f32 = 1e-4;

@fragment
fn frag_main(in: FragmentInput) -> FragmentOutput {
    let ndc = in.ndc;

    // Convert raw camera uniform to friendly structure
    let camera = camera_from_raw(camera_raw);

    // ===== PERSPECTIVE & ORTHOGRAPHIC =====
    // Unproject NDC to world space
    let clip_near = vec4<f32>(ndc.x, ndc.y, 0.0, 1.0);
    var world_near_h = camera.inv_view_proj * clip_near;
    let world_near = world_near_h.xyz / world_near_h.w;

    // Detect camera type
    let is_ortho = camera.proj[3][3] > 0.9;

    var ray_origin: vec3<f32>;
    var ray_dir: vec3<f32>;
    var world_pos: vec3<f32>;
    var t: f32;

    if (is_ortho) {
        // Orthographic: unproject far plane for direction
        let clip_far = vec4<f32>(ndc.x, ndc.y, 1.0, 1.0);
        var world_far_h = camera.inv_view_proj * clip_far;
        let world_far = world_far_h.xyz / world_far_h.w;

        ray_origin = world_near;
        // DON'T normalize - preserves world-space derivative consistency
        ray_dir = world_far - world_near;

        // Intersect with y=0 plane
        t = (0.0 - ray_origin.y) / ray_dir.y;
        world_pos = ray_origin + ray_dir * t;
    } else {
        // Perspective: use pre-computed frustum rays
        let uv = (ndc + 1.0) * 0.5;

        // Bilinearly interpolate frustum rays
        let ray_bottom = mix(camera.frustum_rays[0].xyz, camera.frustum_rays[1].xyz, uv.x);
        let ray_top = mix(camera.frustum_rays[2].xyz, camera.frustum_rays[3].xyz, uv.x);
        let view_ray = mix(ray_bottom, ray_top, uv.y);

        // Transform view-space ray to world space (rotation only)
        let world_ray = mat3x3<f32>(
            camera.inv_view[0].xyz,
            camera.inv_view[1].xyz,
            camera.inv_view[2].xyz
        ) * view_ray;

        ray_origin = camera.position;
        ray_dir = world_ray; // Already normalized from CPU

        // Intersect with y=0 plane
        t = (0.0 - camera.position.y) / ray_dir.y;
        world_pos = camera.position + ray_dir * t;
    }

    // Calculate derivatives BEFORE any branching
    let coord = world_pos.xz;
    let derivative = fwidth(coord);

    // Check for invalid intersections
    let is_parallel = abs(ray_dir.y) < 0.001;

    // For perspective, reject rays pointing away from the ground plane (t < 0)
    // For orthographic, only reject if parallel (no horizon line - grid fills screen like Blender)
    let is_behind = !is_ortho && t < 0.0;

    if (is_parallel || is_behind) {
        discard;
        // // Discard pixels that don't hit the ground
        // var output: FragmentOutput;
        // output.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        // output.depth = 1.0; // Far plane
        // return output;
    }

    // Minor grid (every 1 unit)
    let grid = abs(fract(coord - 0.5) - 0.5) / derivative;
    let line_minor = min(grid.x, grid.y);
    let minor_alpha = (1.0 - min(line_minor, 1.0)) * GRID_ALPHA_MINOR;

    // Major grid (every 10 units)
    let grid_major = abs(fract(coord / 10.0 - 0.5) - 0.5) / (derivative / 10.0);
    let line_major = min(grid_major.x, grid_major.y);
    let major_alpha = (1.0 - min(line_major, 1.0)) * GRID_ALPHA_MAJOR;

    // Axes
    let x_axis_dist = abs(world_pos.z) / derivative.y;
    let z_axis_dist = abs(world_pos.x) / derivative.x;
    let x_axis_alpha = (1.0 - min(x_axis_dist, 1.0)) * GRID_ALPHA_AXIS;
    let z_axis_alpha = (1.0 - min(z_axis_dist, 1.0)) * GRID_ALPHA_AXIS;

    // Priority-based color selection with background
    var final_color = GRID_COLOR_BACKGROUND;
    var final_alpha = GRID_ALPHA_BACKGROUND;

    // Layer lines on top of background (higher priority = drawn on top)
    if (minor_alpha > 0.01) {
        final_color = GRID_COLOR_MINOR;
        final_alpha = minor_alpha;
    }

    if (major_alpha > 0.01) {
        final_color = GRID_COLOR_MAJOR;
        final_alpha = major_alpha;
    }

    if (z_axis_alpha > 0.01) {
        final_color = GRID_COLOR_Z_AXIS;
        final_alpha = z_axis_alpha;
    }

    if (x_axis_alpha > 0.01) {
        final_color = GRID_COLOR_X_AXIS;
        final_alpha = x_axis_alpha;
    }

    // Calculate depth by transforming world position back through view and projection
    let view_pos_depth = camera.view * vec4<f32>(world_pos, 1.0);
    let clip_pos_depth = camera.proj * view_pos_depth;
    let ndc_depth = clip_pos_depth.z / clip_pos_depth.w;

    // Clamp depth to valid WebGPU range [0, 1]
    let depth = clamp(ndc_depth + GRID_DEPTH_EPSILON, 0.0, 1.0);

    var output_final: FragmentOutput;
    output_final.color = vec4<f32>(final_color, final_alpha);
    output_final.depth = depth;
    return output_final;
}
