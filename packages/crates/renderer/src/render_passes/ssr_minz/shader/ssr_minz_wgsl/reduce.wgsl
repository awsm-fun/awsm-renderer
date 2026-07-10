// SSR min-Z reduce — MIN-reduce a 2×2 block of mip N-1 into a single
// texel of mip N. One dispatch per mip transition (`N = 1..mip_count - 1`).
//
// Stores the **minimum** depth per tile (the NEAREST surface in each
// downsampled region) — the flip of the occlusion HZB's max-reduce.
// The SSR trace descends this pyramid to skip empty space: while the
// reflection ray is in FRONT of a tile's min-Z, no surface in that tile
// can be hit, so the whole tile can be stepped over.
//
// Non-power-of-2 textures: at odd mip-N-1 dimensions the 2×2 sample
// would read past the edge by one texel on the last column / row.
// `min(coords, max_coord)` clamps the read. For a MAX pyramid that
// spillover conservatively over-includes; for this MIN pyramid it
// slightly UNDER-estimates at odd edges (the clamped edge texel may be
// nearer than the true off-edge texel would have been). That is
// harmless for a march accelerator — an under-estimated min-Z only
// makes the ray descend/refine a touch earlier (conservative: it never
// causes a real hit to be skipped), it never produces a false hit.

@group(0) @binding(0) var src_mip: texture_2d<f32>;
@group(0) @binding(1) var dst_mip: texture_storage_2d<r32float, write>;

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

    let d00 = textureLoad(src_mip, c00, 0).r;
    let d10 = textureLoad(src_mip, c10, 0).r;
    let d01 = textureLoad(src_mip, c01, 0).r;
    let d11 = textureLoad(src_mip, c11, 0).r;

    {% if reverse_z %}
    // Reverse-Z (003): NEAREST = largest depth — max-reduce.
    let m = max(max(d00, d10), max(d01, d11));
    {% else %}
    let m = min(min(d00, d10), min(d01, d11));
    {% endif %}
    textureStore(dst_mip, dst_coords, vec4<f32>(m, 0.0, 0.0, 0.0));
}
