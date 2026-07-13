// HZB reduce — extreme-reduce a 2×2 block of mip N-1 into a single
// texel of mip N. One dispatch per mip transition (`N = 1..mip_count-1`).
//
// DUAL-CHANNEL: `.r` = the FURTHEST depth per tile (the conservative
// occluder bound occlusion culling + decal classify test against);
// `.g` = the CLOSEST depth per tile (the conservative reflector bound
// the Hi-Z SSR traversal tests against — "can this whole span possibly
// contain a surface in front of the ray?"). Which raw-value extreme is
// "furthest"/"closest" flips with the reverse-Z convention below.
//
// Non-power-of-2 textures: at odd mip-N-1 dimensions the 2×2 sample
// would read past the edge by one texel on the last column / row.
// `min(coords, max_coord)` clamps the read; the resulting "spillover"
// is the same value as the edge texel, which is a conservative
// over-include in the max (never under-estimates the worst-case
// depth, so the occlusion test stays correct).

@group(0) @binding(0) var src_mip: texture_2d<f32>;
@group(0) @binding(1) var dst_mip: texture_storage_2d<rg32float, write>;

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_coords = vec2<i32>(gid.xy);
    let dst_dims = textureDimensions(dst_mip);
    if dst_coords.x >= i32(dst_dims.x) || dst_coords.y >= i32(dst_dims.y) {
        return;
    }

    let src_dims = textureDimensions(src_mip);
    let src_max_x = i32(src_dims.x) - 1;
    let src_max_y = i32(src_dims.y) - 1;
    let base = dst_coords * 2;
    let c00 = vec2<i32>(min(base.x, src_max_x), min(base.y, src_max_y));
    let c10 = vec2<i32>(min(base.x + 1, src_max_x), min(base.y, src_max_y));
    let c01 = vec2<i32>(min(base.x, src_max_x), min(base.y + 1, src_max_y));
    let c11 = vec2<i32>(min(base.x + 1, src_max_x), min(base.y + 1, src_max_y));

    let d00 = textureLoad(src_mip, c00, 0).rg;
    let d10 = textureLoad(src_mip, c10, 0).rg;
    let d01 = textureLoad(src_mip, c01, 0).rg;
    let d11 = textureLoad(src_mip, c11, 0).rg;

    {% if reverse_z %}
    // Reverse-Z (003): farthest (occluder bound) = MIN of .r;
    // closest (reflector bound) = MAX of .g.
    let furthest = min(min(d00.x, d10.x), min(d01.x, d11.x));
    let closest = max(max(d00.y, d10.y), max(d01.y, d11.y));
    {% else %}
    let furthest = max(max(d00.x, d10.x), max(d01.x, d11.x));
    let closest = min(min(d00.y, d10.y), min(d01.y, d11.y));
    {% endif %}
    textureStore(dst_mip, dst_coords, vec4<f32>(furthest, closest, 0.0, 0.0));
}
