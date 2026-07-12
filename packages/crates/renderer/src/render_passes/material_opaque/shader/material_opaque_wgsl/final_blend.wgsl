// Final-blend compositor for the MSAA edge-resolve flow.
//
// Indirect-dispatched over edge pixels (one thread per edge_pixel_id,
// workgroup_size = 64). Reads up to 4 accumulator slots
// (`accumulator[edge_pixel_id × 4 .. +4]`) — each slot holds
// `vec4<f32>(karis_weighted_color_sum, karis_weight_sum)` written by either
// a per-shader-id edge_resolve pass or the skybox_edge_resolve pass. The
// division below therefore computes the KARIS (tonemap-weighted) average:
// every writer weights each HDR sample by 1/(1+maxc), which keeps one hot
// emissive sample from dominating the resolve and collapsing the edge
// gradient after tonemapping (plain linear averaging read as barely-AA'd
// on bright silhouettes at 4x).

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let edge_pixel_id = gid.x;
    // edge_count is mirrored into edge_data's header.
    let total_edges = edge_data[edge_layout.edge_count_index];

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

    // §5: width-gated slot_map read; empty sentinel widens 0xFF→0xFFFF.
    {% if edge_slot_bits == 16 %}
    let slot_w0 = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id * 2u];
    let slot_w1 = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id * 2u + 1u];
    {% else %}
    let slot_map = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id];
    {% endif %}

    var color_sum = vec3<f32>(0.0);
    var total_count: f32 = 0.0;
    {% if write_ssr_descriptor %}
    var desc_rgb_sum = vec3<f32>(0.0);
    var desc_spread_wsum: f32 = 0.0;
    {% endif %}

    for (var slot = 0u; slot < 4u; slot++) {
        // Skip slots that have no shader_id assigned this frame. Their
        // accumulator region holds stale data.
        {% if edge_slot_bits == 16 %}
        let word = select(slot_w0, slot_w1, slot >= 2u);
        let slot_byte = (word >> ((slot % 2u) * 16u)) & 0xFFFFu;
        if (slot_byte == 0xFFFFu) {
            continue;
        }
        {% else %}
        let slot_byte = (slot_map >> (slot * 8u)) & 0xFFu;
        if (slot_byte == 0xFFu) {
            continue;
        }
        {% endif %}
        // 8 words per slot: 0..4 = Karis color sum + weight, 4..8 = SSR
        // descriptor sums (see ACCUMULATOR_SLOT_BYTES in edge_buffers.rs).
        let accum_word_index = edge_layout.accumulator_base + (edge_pixel_id * 4u + slot) * 8u;
        let r = bitcast<f32>(edge_data[accum_word_index + 0u]);
        let g = bitcast<f32>(edge_data[accum_word_index + 1u]);
        let b = bitcast<f32>(edge_data[accum_word_index + 2u]);
        let count = bitcast<f32>(edge_data[accum_word_index + 3u]);
        if (count > 0.0) {
            color_sum += vec3<f32>(r, g, b);
            total_count += count;
        }
        {% if write_ssr_descriptor %}
        desc_rgb_sum += vec3<f32>(
            bitcast<f32>(edge_data[accum_word_index + 4u]),
            bitcast<f32>(edge_data[accum_word_index + 5u]),
            bitcast<f32>(edge_data[accum_word_index + 6u]),
        );
        desc_spread_wsum += bitcast<f32>(edge_data[accum_word_index + 7u]);
        {% endif %}
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

    {% if write_ssr_descriptor %}
    // Per-pixel SSR descriptor resolve (wgsl_validation pins this): raw
    // per-sample average over the 4 MSAA samples — samples owned by no slot
    // (sky) contribute zero, which is exactly their reflectivity. Spread is
    // reflectivity-weighted (each sample's spread entered the sum scaled by
    // its own max-component reflectivity), so a strong mirror's spread
    // dominates a weak dielectric's at mixed edges.
    let desc_rgb = desc_rgb_sum / 4.0;
    let desc_w = max(max(desc_rgb_sum.r, desc_rgb_sum.g), desc_rgb_sum.b);
    let desc_spread = select(0.0, desc_spread_wsum / max(desc_w, 1e-5), desc_w > 1e-5);
    textureStore(reflection_descriptor_tex, coords, vec4<f32>(desc_rgb, desc_spread));
    {% endif %}
}
