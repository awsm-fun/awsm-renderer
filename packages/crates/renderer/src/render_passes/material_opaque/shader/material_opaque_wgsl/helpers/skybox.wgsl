fn sample_skybox(
    coords: vec2<i32>,
    screen_dims: vec2<f32>,
    camera: Camera,
    skybox_tex: texture_cube<f32>,
    skybox_sampler: sampler
) -> vec4<f32> {
    let uv = (vec2<f32>(coords) + vec2<f32>(0.5, 0.5)) / screen_dims;

    // Detect camera type: perspective has proj[2][3] != 0, orthographic has proj[2][3] == 0
    let is_perspective = camera.proj[2][3] != 0.0;

    var view_ray: vec3<f32>;

    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);

    if (is_perspective) {
        // PERSPECTIVE: unproject the pixel to a view-space DIRECTION.
        //
        // Deliberately NO perspective divide. Under the renderer's reverse-Z
        // convention the main camera uses the INFINITE-far projection
        // (`perspective_infinite_reverse_rh`), so NDC z=0 is the far plane —
        // which sits at infinity. `inv_proj * vec4(ndc, 0, 1)` therefore comes
        // back with w == 0 EXACTLY, and `xyz / w` yields ±Inf → normalize() →
        // NaN → a NaN cube-map fetch. A NaN direction samples one
        // implementation-defined texel, so every pixel at every camera angle
        // got the same colour: the skybox rendered as a flat solid block of
        // the environment's average tone. `compute_view_frustum_rays` in
        // camera.rs documents this same w=0 hazard and avoids the far plane.
        //
        // The divide was never needed here: we want a direction, not a
        // position, and inv_proj's 4th row carries no x/y terms for any
        // projection this renderer builds (perspective, orthographic, and the
        // TAA-jittered variants — jitter is a translation in x/y, which leaves
        // that row untouched). So w is a per-image constant and the undivided
        // xyz is already proportional to the correct per-pixel direction;
        // normalize() below removes the scale. This also stays correct under
        // forward-Z, where z=0 is the near plane and w is a positive constant.
        let clip_pos = vec4<f32>(ndc.x, ndc.y, 0.0, 1.0);
        let view_pos_h = camera.inv_proj * clip_pos;
        view_ray = view_pos_h.xyz;
    } else {
        // ORTHOGRAPHIC: Use fixed angular scale for zoom-independent skybox
        // Simple ray based on NDC with constant field of view
        view_ray = vec3<f32>(ndc.x, ndc.y, -1.0);
    }

    // Transform from view space to world space using inverse view matrix (rotation only for skybox)
    let inv_view_rotation = mat3x3<f32>(
        camera.inv_view[0].xyz,
        camera.inv_view[1].xyz,
        camera.inv_view[2].xyz
    );
    let ray_dir = normalize(inv_view_rotation * view_ray);

    // Sample the cubemap using the ray direction
    let color = textureSampleLevel(skybox_tex, skybox_sampler, ray_dir, 0.0);

    // Return raw HDR values - tone mapping happens in the display pass
    return color;
}
