// Final-blend compositor for the MSAA edge-resolve flow.
//
// Indirect-dispatched over edge pixels (one thread per edge_pixel_id,
// workgroup_size = 64). Reads up to 4 accumulator slots
// (`accumulator[edge_pixel_id × 4 .. +4]`) — each slot holds
// `vec4<f32>(color_sum, sample_count)` written by either a per-shader-id
// edge_resolve pass or the skybox_edge_resolve pass. Sums color sums,
// totals sample counts, divides, and stores the result into
// `opaque_tex[edge_to_xy[edge_pixel_id]]`.

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let edge_pixel_id = gid.x;
    let total_edges = edge_args.edge_count;
    if (edge_pixel_id >= total_edges) {
        return;
    }
    if (edge_pixel_id >= edge_layout.max_edge_budget) {
        return;
    }

    let packed_xy = edge_data[edge_layout.edge_to_xy_base + edge_pixel_id];
    let coords = vec2<i32>(
        i32(packed_xy & 0xFFFFu),
        i32((packed_xy >> 16u) & 0xFFFFu),
    );

    var color_sum = vec3<f32>(0.0);
    var total_count: f32 = 0.0;

    for (var slot = 0u; slot < 4u; slot++) {
        let accum_word_index = edge_layout.accumulator_base + (edge_pixel_id * 4u + slot) * 4u;
        let r = bitcast<f32>(edge_data[accum_word_index + 0u]);
        let g = bitcast<f32>(edge_data[accum_word_index + 1u]);
        let b = bitcast<f32>(edge_data[accum_word_index + 2u]);
        let count = bitcast<f32>(edge_data[accum_word_index + 3u]);
        if (count > 0.0) {
            color_sum += vec3<f32>(r, g, b);
            total_count += count;
        }
    }

    if (total_count <= 0.0) {
        return;
    }

    let final_color = color_sum / total_count;
    // Alpha resolution: simplification — opaque outputs assume alpha
    // tracks count for visibility (1.0 if any contribution). For
    // alpha-blended edges, a parallel alpha-accumulator buffer would
    // be needed (out of scope for Stage 3.5; opaque alpha is unused by
    // the display pass anyway).
    let final_alpha: f32 = 1.0;

    textureStore(opaque_tex, coords, vec4<f32>(final_color, final_alpha));
}
