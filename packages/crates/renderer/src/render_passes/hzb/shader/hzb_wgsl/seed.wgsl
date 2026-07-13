// HZB seed — copies the final-resolution depth buffer into HZB mip 0
// as r32float. One dispatch over the full screen / 8.

{% if multisampled_geometry %}
@group(0) @binding(0) var depth_tex: texture_depth_multisampled_2d;
{% else %}
@group(0) @binding(0) var depth_tex: texture_depth_2d;
{% endif %}
@group(0) @binding(1) var hzb_mip0: texture_storage_2d<rg32float, write>;

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
    let s0 = textureLoad(depth_tex, coords, 0);
    let s1 = textureLoad(depth_tex, coords, 1);
    let s2 = textureLoad(depth_tex, coords, 2);
    let s3 = textureLoad(depth_tex, coords, 3);
    {% if reverse_z %}
    // Reverse-Z (003): farthest = MIN across samples (occluder bound, .r);
    // closest = MAX across samples (reflector bound, .g).
    let furthest = min(min(s0, s1), min(s2, s3));
    let closest = max(max(s0, s1), max(s2, s3));
    {% else %}
    let furthest = max(max(s0, s1), max(s2, s3));
    let closest = min(min(s0, s1), min(s2, s3));
    {% endif %}
    {% else %}
    let d = textureLoad(depth_tex, coords, 0);
    let furthest = d;
    let closest = d;
    {% endif %}
    textureStore(hzb_mip0, coords, vec4<f32>(furthest, closest, 0.0, 0.0));
}
