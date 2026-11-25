// Raw camera uniform structure (matches GPU buffer layout with padding)
//
// `frame_count` used to live in this struct (as `frame_count_and_padding.x`)
// but no shader actually read it on the GPU side. It's now exposed via
// the dedicated `frame_globals` uniform (`shared_wgsl/frame_globals.wgsl`)
// alongside `time` / `delta_time` / `resolution`. The 16-byte slot was
// removed; Camera is correspondingly 16 bytes slimmer.
struct CameraRaw {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    inv_proj: mat4x4<f32>,
    inv_view: mat4x4<f32>,
    position: vec4<f32>,  // .xyz = position, .w = unused
    // IMPORTANT: frustum_rays are for SCREEN-SPACE RECONSTRUCTION, NOT frustum culling!
    // 4 normalized view-space ray directions at near plane corners [bottom-left, bottom-right, top-left, top-right]
    // Used for unprojecting screen pixels to world space with better precision than per-pixel unprojection
    frustum_rays: array<vec4<f32>, 4>,
    viewport: vec4<f32>, // in pixels, x,y,width,height
    dof_params: vec4<f32>, // x=focus_distance, y=aperture (f-stop), zw=TAA reactive factors (0 without TAA)
    // M3 SSR temporal reprojection: PRIOR frame's unjittered view-projection.
    // END-APPENDED so every existing field offset is preserved (see camera.rs
    // BYTE_SIZE). Ignored by shaders that don't opt into temporal reprojection.
    prev_view_projection: mat4x4<f32>,
    // TAA (see camera/buffer.rs): view-projection without the jitter.
    view_projection_unjittered: mat4x4<f32>,
    // xy = jitter this frame, zw = previous; render pixels.
    jitter: vec4<f32>,
    prev_position: vec4<f32>, // .xyz = last frame's camera position
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
    // Screen-space reconstruction rays (see CameraRaw for details)
    frustum_rays: array<vec4<f32>, 4>,
    viewport_pos: vec2<f32>, // x,y
    viewport_size: vec2<f32>, // width,height
    focus_distance: f32, // DoF focus distance in world units
    aperture: f32, // DoF aperture f-stop (lower = more blur)
    // M3 SSR temporal reprojection: PRIOR frame's unjittered view-projection.
    prev_view_proj: mat4x4<f32>,
    view_proj_unjittered: mat4x4<f32>,
    jitter: vec2<f32>,
    prev_jitter: vec2<f32>,
    prev_position: vec3<f32>,
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
    camera.viewport_pos = vec2<f32>(raw.viewport.x, raw.viewport.y);
    camera.viewport_size = vec2<f32>(raw.viewport.z, raw.viewport.w);
    camera.focus_distance = raw.dof_params.x;
    camera.aperture = raw.dof_params.y;
    camera.prev_view_proj = raw.prev_view_projection;
    camera.view_proj_unjittered = raw.view_projection_unjittered;
    camera.jitter = raw.jitter.xy;
    camera.prev_jitter = raw.jitter.zw;
    camera.prev_position = raw.prev_position.xyz;
    return camera;
}
