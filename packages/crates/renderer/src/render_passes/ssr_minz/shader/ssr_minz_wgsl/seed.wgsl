// SSR min-Z seed — copies the final-resolution depth buffer into the
// min-Z pyramid mip 0 as r32float. One dispatch over the full screen / 8.
//
// Reads the SAME depth binding the SSR trace reads, so the pyramid's
// depth values are byte-identical to what the linear-DDA path probes
// (mirrors the trace's multisampled-awareness). Under MSAA we take the
// per-texel MIN across sample indices: the pyramid stores the NEAREST
// surface, and a reflection ray must never skip a real occluder, so
// under-including (picking the near sample) is the conservative choice.

{% if multisampled_geometry %}
@group(0) @binding(0) var depth_tex: texture_depth_multisampled_2d;
{% else %}
@group(0) @binding(0) var depth_tex: texture_depth_2d;
{% endif %}
@group(0) @binding(1) var minz_mip0: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coords = vec2<i32>(gid.xy);
    let dims = textureDimensions(minz_mip0);
    if coords.x >= i32(dims.x) || coords.y >= i32(dims.y) {
        return;
    }
    {% if multisampled_geometry %}
    // Min-reduce across samples (flip of the occlusion HZB's max): the
    // nearest sample is the closest potential reflection occluder, and
    // the ray march must not skip it.
    var d = textureLoad(depth_tex, coords, 0);
    {% if reverse_z %}
    // Reverse-Z (003): NEAREST = largest depth — max-reduce.
    d = max(d, textureLoad(depth_tex, coords, 1));
    d = max(d, textureLoad(depth_tex, coords, 2));
    d = max(d, textureLoad(depth_tex, coords, 3));
    {% else %}
    d = min(d, textureLoad(depth_tex, coords, 1));
    d = min(d, textureLoad(depth_tex, coords, 2));
    d = min(d, textureLoad(depth_tex, coords, 3));
    {% endif %}
    {% else %}
    let d = textureLoad(depth_tex, coords, 0);
    {% endif %}
    textureStore(minz_mip0, coords, vec4<f32>(d, 0.0, 0.0, 0.0));
}
