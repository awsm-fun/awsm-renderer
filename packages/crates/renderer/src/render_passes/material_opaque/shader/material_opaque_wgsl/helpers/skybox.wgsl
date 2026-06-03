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
        // PERSPECTIVE: Unproject NDC point to get diverging rays
        let clip_pos = vec4<f32>(ndc.x, ndc.y, 0.0, 1.0);
        let view_pos_h = camera.inv_proj * clip_pos;
        view_ray = view_pos_h.xyz / view_pos_h.w;
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
