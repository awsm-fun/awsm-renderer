// Skybox-sample MSAA edge-resolve shader.
//
// Indirect-dispatched over the skybox-sample edge list. One thread per
// (edge_pixel_id, sample_mask). For each set sample bit, samples the
// skybox at the (coords, sample_index) and accumulates. Writes to the
// `skybox` slot in the accumulator — slot_index found by scanning
// edge_slot_map for the sentinel byte value 0xFE (assigned to skybox
// in the classify pass's slot_map emission).

/*************** START color_space.wgsl ******************/
{% include "shared_wgsl/color_space.wgsl" %}
/*************** END color_space.wgsl ******************/

// camera.wgsl is already included by skybox_edge_bind_groups.wgsl
// (concatenated before this file). Re-including here would redefine
// the CameraRaw struct.

/*************** START math.wgsl ******************/
{% include "shared_wgsl/math.wgsl" %}
/*************** END math.wgsl ******************/

/*************** START skybox.wgsl ******************/
{% include "material_opaque_wgsl/helpers/skybox.wgsl" %}
/*************** END skybox.wgsl ******************/

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let thread_index = gid.x;
    // Skybox entry count is mirrored into edge_data's header.
    let entry_count = edge_data[edge_layout.skybox_count_index];
    if (thread_index >= entry_count) {
        return;
    }
    if (thread_index >= edge_layout.sample_entries_per_bucket) {
        return;
    }
    // The skybox sample list lives at the host-supplied
    // `skybox_sample_list_base` offset (see EdgeBufferLayout). It's a
    // separate reserved region — the classify pass appends here via
    // skybox_edge_args.workgroup_count_x as the index allocator.
    let packed_entry = edge_data[edge_layout.skybox_sample_list_base + thread_index];
    if (packed_entry == 0u) {
        return;
    }
    let edge_pixel_id = packed_entry & 0x00FFFFFFu;
    let sample_mask = (packed_entry >> 24u) & 0xFFu;
    if (sample_mask == 0u) {
        return;
    }

    let packed_xy = edge_data[edge_layout.edge_to_xy_base + edge_pixel_id];
    let coords = vec2<i32>(
        i32(packed_xy & 0xFFFFu),
        i32((packed_xy >> 16u) & 0xFFFFu),
    );

    let slot_map = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id];
    var slot_index: u32 = 4u;
    for (var i = 0u; i < 4u; i++) {
        let byte_val = (slot_map >> (i * 8u)) & 0xFFu;
        if (byte_val == 0xFEu) {
            slot_index = i;
            break;
        }
    }
    if (slot_index >= 4u) {
        return;
    }

    let camera = camera_from_raw(camera_raw);
    // BUG FIX: previously used `textureDimensions(skybox_tex)` which
    // returns CUBEMAP FACE dimensions (e.g. 512×512), NOT the render
    // target screen size. `sample_skybox` reconstructs the ray
    // direction from `coords / screen_dims_f32`, so wrong dimensions
    // produced wildly wrong ray directions, sampling the wrong part
    // of the skybox at every silhouette pixel. Use the camera's
    // viewport size — that's the render-target screen size by
    // construction (Camera.viewport_size from CameraRaw.viewport.zw).
    let screen_dims_f32 = camera.viewport_size;

    var color_sum = vec3<f32>(0.0);
    var alpha_sum: f32 = 0.0;
    var sample_count: u32 = 0u;
    for (var s = 0u; s < 4u; s++) {
        if ((sample_mask & (1u << s)) != 0u) {
            // Skybox sampling doesn't actually depend on sample index
            // (it's purely a function of the pixel center direction),
            // but we add one entry per set bit so the per-slot count
            // matches what final_blend expects.
            let sky_col = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);
            color_sum += sky_col.rgb;
            alpha_sum += sky_col.a;
            sample_count += 1u;
        }
    }

    if (sample_count == 0u) {
        return;
    }

    let accum_word_index = edge_layout.accumulator_base + (edge_pixel_id * 4u + slot_index) * 4u;
    edge_data[accum_word_index + 0u] = bitcast<u32>(color_sum.x);
    edge_data[accum_word_index + 1u] = bitcast<u32>(color_sum.y);
    edge_data[accum_word_index + 2u] = bitcast<u32>(color_sum.z);
    edge_data[accum_word_index + 3u] = bitcast<u32>(f32(sample_count));
}
