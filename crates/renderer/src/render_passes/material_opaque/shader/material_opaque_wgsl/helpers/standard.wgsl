struct StandardCoordinates {
    pixel_center: vec2<f32>,
    depth_sample: f32,
    ndc: vec3<f32>,
    clip_position: vec4<f32>,
    world_position: vec3<f32>,
    view_position: vec3<f32>,
    surface_to_camera: vec3<f32>,
}

fn get_standard_coordinates(coords: vec2<i32>, screen_dims: vec2<u32>) -> StandardCoordinates {
    return get_standard_coordinates_sample(coords, screen_dims, 0);
}

// Per-sample variant. Reconstructs `world_position` / `surface_to_camera`
// from a specific MSAA sample's depth, not sample 0's. Intended to
// give the Stage 3 `edge_resolve.wgsl::shade_sample` per-sample
// world-position fidelity at silhouette pixels where sample 0 may be
// on a different surface (e.g. skybox) from samples 1-3 (e.g. a
// capsule).
//
// **Status: defined but not currently called by `edge_resolve.wgsl`.**
// The Stage 3 silhouette debug pass (May 27) found that per-sample
// world-position produced visible dark shading deltas at intra-mesh
// triangle seams of tessellated curved surfaces — once averaged
// through `final_blend`, those deltas read as wireframe artifacts at
// every classify-detected edge. `edge_resolve.wgsl::shade_sample`
// reverted to `get_standard_coordinates(coords, screen_dims)`
// (sample-0 depth, matching main's pre-Stage-3 `msaa_resolve_samples`
// behaviour exactly) which produces numerical parity with main on
// MorphStressTest.
//
// This per-sample helper is kept available for two reasons:
//   * It's the right tool if the per-sample shading delta problem is
//     ever isolated and fixed (e.g. via a smoothing filter or by
//     using sample-0 depth only when intra-mesh seams are detected).
//   * It documents the per-sample convention so a future
//     `edge_resolve.wgsl` rewrite has a starting point.
//
// `depth_tex` is bound as `texture_multisampled_2d<f32>` when
// `multisampled_geometry` is true. The non-MSAA primary path uses
// `get_standard_coordinates(coords, screen_dims)` (which delegates
// here with `sample_index = 0`).
fn get_standard_coordinates_sample(
    coords: vec2<i32>,
    screen_dims: vec2<u32>,
    sample_index: u32,
) -> StandardCoordinates {
    // Convert raw camera uniform to friendly structure
    let camera = camera_from_raw(camera_raw);

    let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));
    let depth_sample : f32 = textureLoad(depth_tex, coords, i32(sample_index));

    // Pixel center UV and NDC (flip Y once)
    let uv = (vec2<f32>(coords) + 0.5) / screen_dims_f32;
    let ndc_xy = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);

    // WebGPU: NDC.z in [0,1]; no remap
    let ndc = vec3<f32>(ndc_xy, depth_sample);
    let clip_position = vec4<f32>(ndc, 1.0);

    let view_h        = camera.inv_proj * clip_position;
    let view_position = view_h.xyz / max(view_h.w, 1e-8);

    let world_position = (camera.inv_view * vec4<f32>(view_position, 1.0)).xyz;

    // Compute surface-to-camera direction for lighting calculations
    // This differs fundamentally between projection types:
    // - Orthographic: parallel rays (constant direction across all pixels): proj[3][3]=1.0
    // - Perspective: diverging rays from camera origin: proj[3][3]=0.0
    // we compare to 0.9 to allow for some numerical imprecision
    let is_ortho = camera.proj[3][3] > 0.9;

    var surface_to_camera: vec3<f32>;
    if (is_ortho) {
        // For orthographic projection, transform view-space forward direction (0,0,-1) to world space
        // This simplifies to just the third column (z-axis) of the inverse view matrix
        surface_to_camera = normalize(camera.inv_view[2].xyz);
    } else {
        // For perspective projection, compute direction from surface to camera position
        let to_camera = camera.position - world_position;
        surface_to_camera = select(
            vec3<f32>(0.0, 0.0, 1.0),
            safe_normalize(to_camera),
            dot(to_camera, to_camera) > 0.0
        );
    }

    return StandardCoordinates(
        uv,
        depth_sample,
        ndc,
        clip_position,
        world_position,
        view_position,
        surface_to_camera
    );
}
