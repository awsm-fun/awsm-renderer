// Motion vector (UV units, uv_now - uv_prev, both unjittered) of the sky at
// unjittered screen position `uv`. Sky has no geometry and no depth, so the
// vector is the camera's ROTATION only: the view direction through `uv` is
// projected with last frame's view-projection at w = 0 (a direction, so the
// camera's translation drops out).
//
// Orthographic cameras have no per-pixel sky direction (w stays 0 below), so
// their sky resets every frame: aliased, never ghosted.
//
// Shared by the TAA resolve and the velocity debug view, so the two cannot
// disagree. Needs `Camera` (shared_wgsl/camera.wgsl).
fn taa_sky_velocity(uv: vec2<f32>, cam: Camera) -> vec2<f32> {
    // `inv_view_proj` is the JITTERED inverse, so feed it the jittered NDC of
    // this unjittered position: one render pixel is 2 / viewport in NDC, y up.
    let ndc_unjittered = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let ndc = ndc_unjittered + vec2<f32>(2.0 * cam.jitter.x, -2.0 * cam.jitter.y) / cam.viewport_size;
    // Any depth strictly inside the frustum gives a finite point on the ray.
    let world = cam.inv_view_proj * vec4<f32>(ndc, 0.5, 1.0);
    let dir = world.xyz / world.w - cam.position;
    let prev_clip = cam.prev_view_proj * vec4<f32>(dir, 0.0);
    if (prev_clip.w <= 1.0e-6) {
        // Behind last frame's camera: no history. A vector that lands
        // off-screen makes the resolve reset the pixel.
        return vec2<f32>(2.0, 2.0);
    }
    let prev_ndc = prev_clip.xy / prev_clip.w;
    let uv_prev = vec2<f32>(prev_ndc.x * 0.5 + 0.5, 0.5 - prev_ndc.y * 0.5);
    return uv - uv_prev;
}
