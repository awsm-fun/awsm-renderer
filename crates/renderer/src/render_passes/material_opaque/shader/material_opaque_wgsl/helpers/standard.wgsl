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

// Per-sample variant. Used by the Stage 3 `edge_resolve.wgsl` so each
// MSAA sample's `world_position` / `surface_to_camera` is reconstructed
// from its own depth value, not sample 0's — critical at silhouette
// pixels where sample 0 may be on a different surface (e.g. skybox)
// from samples 1-3 (e.g. a capsule). Without this, lighting for
// samples 1-3 at silhouettes was computed against sample 0's depth,
// producing subtly-wrong shadows / point-light falloff at every
// silhouette pixel.
//
// `depth_tex` is bound as `texture_multisampled_2d<f32>` when
// `multisampled_geometry` is true (which is the only context this
// shader runs in — edge_resolve only dispatches under MSAA-on); the
// `sample_index` argument is honoured by `textureLoad`. The non-MSAA
// primary path uses `get_standard_coordinates(coords, screen_dims)`
// (which delegates here with sample 0), preserving existing behaviour.
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
