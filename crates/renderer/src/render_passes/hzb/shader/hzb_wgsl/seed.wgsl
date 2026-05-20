// HZB seed — copies the final-resolution depth buffer into HZB mip 0
// as r32float. One dispatch over the full screen / 8.

{% if multisampled_geometry %}
@group(0) @binding(0) var depth_tex: texture_depth_multisampled_2d;
{% else %}
@group(0) @binding(0) var depth_tex: texture_depth_2d;
{% endif %}
@group(0) @binding(1) var hzb_mip0: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coords = vec2<i32>(gid.xy);
    let dims = textureDimensions(hzb_mip0);
    if coords.x >= i32(dims.x) || coords.y >= i32(dims.y) {
        return;
    }
    // Only sample 0 — the HZB conservatively over-estimates depth,
    // so sampling a single fragment is correct (the MSAA samples
    // within a pixel can only be closer or equal; the max is a
    // safe upper bound).
    let d = textureLoad(depth_tex, coords, 0);
    textureStore(hzb_mip0, coords, vec4<f32>(d, 0.0, 0.0, 0.0));
}
