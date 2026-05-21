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
    {% if multisampled_geometry %}
    // HZB stores max depth per region; the occlusion test culls
    // when `instance.depth_min > hzb_depth`. If we underestimate
    // max depth, more instances pass the `>` test → over-culling
    // → false occlusion. Sample 0 alone can pick the *near* sample
    // of an edge pixel and lose the far samples behind it, so for
    // MSAA we explicitly max-reduce across all sample indices.
    var d = textureLoad(depth_tex, coords, 0);
    d = max(d, textureLoad(depth_tex, coords, 1));
    d = max(d, textureLoad(depth_tex, coords, 2));
    d = max(d, textureLoad(depth_tex, coords, 3));
    {% else %}
    let d = textureLoad(depth_tex, coords, 0);
    {% endif %}
    textureStore(hzb_mip0, coords, vec4<f32>(d, 0.0, 0.0, 0.0));
}
