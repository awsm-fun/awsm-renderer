// TAA motion vector of one fragment, in UV units: uv_now - uv_prev of the same
// surface point, both UNJITTERED (03 §2.1, G1/G2).
//
// The divide happens HERE, per fragment, on the interpolated clip positions.
// Dividing in the vertex stage and interpolating NDC is wrong under
// perspective (wgsl_validation pins this spelling to the fragment stage).
//
// uv(c) = (c.x/c.w * 0.5 + 0.5, -c.y/c.w * 0.5 + 0.5), so
// uv_now - uv_prev = (ndc_now - ndc_prev) * (0.5, -0.5).
//
// A point that was behind last frame's camera (clip_prev.w <= 0) has no
// previous screen position: report a vector that lands off-screen, which
// makes the resolve reset the pixel instead of fetching garbage history.
fn taa_motion_vector(clip_now: vec4<f32>, clip_prev: vec4<f32>) -> vec2<f32> {
    var v = vec2<f32>(2.0, 2.0);
    if (clip_prev.w > 1.0e-6 && clip_now.w > 1.0e-6) {
        let ndc_now = clip_now.xy / clip_now.w;
        let ndc_prev = clip_prev.xy / clip_prev.w;
        v = (ndc_now - ndc_prev) * vec2<f32>(0.5, -0.5);
    }
    return v;
}
