// Depth reads + linearization, shared by the effects-pass consumers of the
// depth attachment (DoF and atmosphere). Included when EITHER is on; both
// helpers used to live in `dof.wgsl`, which meant haze-without-DoF couldn't
// see them.

// Linearize depth from NDC depth buffer value
fn linearize_depth(depth: f32, camera: Camera) -> f32 {
    // Check for reverse-Z infinite far (proj[2][2] ≈ 0)
    if (abs(camera.proj[2][2]) < 0.0001) {
        // Reverse-Z with infinite far: proj[3][2] = near; depth = near / z.
        let near = camera.proj[3][2];
        return near / max(depth, 0.0001);
    } else {
        // Standard RH 0..1 projection (glam `perspective_rh`):
        //   proj[2][2] = far/(near-far),  proj[3][2] = near*far/(near-far)
        // so near = proj[3][2]/proj[2][2] and far = proj[3][2]/(proj[2][2]+1).
        // Using proj[3][2] directly as `near` (the old code) yields a NEGATIVE
        // near → negative linear depth → CoC clamped to 0 → DoF silently never
        // blurred a single pixel.
        let near = camera.proj[3][2] / camera.proj[2][2];
        let far = camera.proj[3][2] / (camera.proj[2][2] + 1.0);
        return (near * far) / (far - depth * (far - near));
    }
}

// Load depth, handling both multisampled and single-sampled textures
fn load_depth(coords: vec2<i32>) -> f32 {
    {% if multisampled_geometry %}
        {% if reverse_z %}
        // Reverse-Z (003): nearest = LARGEST depth; start from the far
        // extreme (0.0) and max-reduce.
        var min_depth = 0.0;
        for (var s = 0u; s < 4u; s = s + 1u) {
            let d = textureLoad(depth_tex, coords, i32(s));
            min_depth = max(min_depth, d);
        }
        {% else %}
        var min_depth = 1.0;
        for (var s = 0u; s < 4u; s = s + 1u) {
            let d = textureLoad(depth_tex, coords, i32(s));
            min_depth = min(min_depth, d);
        }
        {% endif %}
        return min_depth;
    {% else %}
        return textureLoad(depth_tex, coords, 0);
    {% endif %}
}

// True at the far extreme of the depth range — nothing was drawn, so the
// pixel is skybox / cleared background. Reverse-Z (003) puts far at 0.
fn is_sky_depth(depth: f32) -> bool {
    {% if reverse_z %}
        return depth <= 0.0;
    {% else %}
        return depth >= 1.0;
    {% endif %}
}
