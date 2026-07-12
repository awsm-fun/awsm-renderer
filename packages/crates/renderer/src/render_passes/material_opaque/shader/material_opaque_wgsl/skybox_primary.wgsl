// skybox_primary.wgsl — dedicated skybox writer for the canonical skybox bucket.
//
// The SKYBOX bucket (index 0; MaterialShaderId::SKYBOX) is a dedicated bucket,
// NOT a material: classify routes every fully-uncovered ("sky") pixel to it.
// This pipeline is dispatched over that bucket's tile list + indirect args +
// bind groups and does ONLY the skybox write — no material shading. (Real
// materials route to their own feature-variant buckets.) `owns_skybox` selects
// this kernel over the material `compute.wgsl`, keeping the material kernel pure.
//
// Shares the kernel preamble with compute.wgsl; `inc = skybox_only` gates out all
// the heavy PBR shading includes, so this compiles to a tiny shader.
{% include "material_opaque_wgsl/opaque_kernel_includes.wgsl" %}

// Entry point named `cs_opaque` to match the opaque-pipeline convention the
// launcher requests for every opaque bucket (launch.rs `.with_entry_point(
// "cs_opaque")`). The 1024 module-unification (commit 1a3f35cb) switched all
// opaque modules to `cs_opaque` but left this writer's entry as `main`, so the
// skybox bucket's pipeline failed to create ("entry point cs_opaque doesn't
// exist") the moment a scene with an environment compiled it — latent because
// the cube benchmark sets no skybox. This is the skybox writer (no cs_edge).
// Invariant: non-MSAA only — under MSAA the skybox bucket dispatches the
// `cs_shade` arm below, so `cs_opaque` is not emitted in the multisampled module.
{% if !multisampled_geometry %}
@compute @workgroup_size(8, 8)
fn cs_opaque(
    @builtin(workgroup_id) wg_id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>
) {
    // Same bucket-tile lookup as the material kernel — this pipeline is
    // dispatched over the canonical skybox bucket's tile list. The skybox bucket
    // is reserved at index 0 (see `first_party_bucket_entries`), so read
    // `offsets[0]` from the data-driven ClassifyBuckets layout.
    let bucket_offset = classify_buckets.offsets[0u];
    let tile = classify_buckets.tiles[bucket_offset + wg_id.x];
    let coords = vec2<i32>(i32(tile.x * 8u + lid.x), i32(tile.y * 8u + lid.y));
    let screen_dims = textureDimensions(opaque_tex);
    let screen_dims_i32 = vec2<i32>(i32(screen_dims.x), i32(screen_dims.y));
    let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));

    if (coords.x >= screen_dims_i32.x || coords.y >= screen_dims_i32.y) {
        return;
    }

    // Write the skybox iff sample 0 is skybox (`triangle_index == U32_MAX`). This
    // single check matches the old owns_skybox logic exactly: a fully-uncovered
    // tile and an MSAA silhouette edge (sample 0 skybox, some sample hit) both
    // have sample-0 == U32_MAX, and `!any_sample_hit` implies it too. The
    // per-sample MSAA blend at edges is owned by skybox_edge_resolve / final_blend.
    let visibility_data_info = textureLoad(visibility_data_tex, coords, 0);
    let triangle_index = join32(visibility_data_info.x, visibility_data_info.y);
    if (triangle_index == U32_MAX) {
        let camera = camera_from_raw(camera_raw);
        let color = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);
        textureStore(opaque_tex, coords, color);
    }
}
{% endif %}

{% if multisampled_geometry %}
// ════════════════════════════════════════════════════════════════════
// UNIFIED MODULE — skybox `cs_shade` arm (U1, unified-edge-shading.md).
//
// The skybox bucket's `cs_shade` = skybox_primary's interior sky shading
// (sample-0 sky → opaque_tex) + skybox_edge_resolve's sky-edge-sample
// handling (sky samples → the SKYBOX accumulator slot). A model silhouette
// is a SKY edge, so sky-edge byte-parity requires the skybox bucket to also
// go through `cs_shade`. Dispatched over the skybox bucket's tile list (the
// U0 ANY-sample list) like the writer above. The OLD skybox_primary /
// skybox_edge_resolve pipelines stay for the toggle-OFF path.
//
// Replicates the interior write (sample-0 sky) + the per-sample accumulate
// (sky samples) EXACTLY, reusing the unchanged accumulator + edge_slot_map
// (the skybox sentinel slot) + final_blend resolve.
// ════════════════════════════════════════════════════════════════════
@compute @workgroup_size(8, 8)
fn cs_shade(
    @builtin(workgroup_id) wg_id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>
) {
    // SKYBOX bucket is reserved at index 0 (see first_party_bucket_entries).
    let bucket_offset = classify_buckets.offsets[0u];
    let tile = classify_buckets.tiles[bucket_offset + wg_id.x];
    let coords = vec2<i32>(i32(tile.x * 8u + lid.x), i32(tile.y * 8u + lid.y));
    let screen_dims = textureDimensions(opaque_tex);
    let screen_dims_i32 = vec2<i32>(i32(screen_dims.x), i32(screen_dims.y));
    let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));

    if (coords.x >= screen_dims_i32.x || coords.y >= screen_dims_i32.y) {
        return;
    }

    let camera = camera_from_raw(camera_raw);
    let edge_id = textureLoad(edge_id_tex, coords).x;

    if (edge_id == U32_MAX) {
        // ── INTERIOR ARM (skybox_primary writer body, verbatim) ──────
        // Write the skybox iff sample 0 is skybox.
        let visibility_data_info = textureLoad(visibility_data_tex, coords, 0);
        let triangle_index = join32(visibility_data_info.x, visibility_data_info.y);
        if (triangle_index == U32_MAX) {
            let color = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);
            textureStore(opaque_tex, coords, color);
        }
        return;
    }

    // ── EDGE ARM (skybox_edge_resolve body, verbatim) ────────────────
    // Find the SKYBOX slot in the slot_map (sentinel 0xFE / 0xFFFE), same
    // scan skybox_edge_resolve uses.
    let edge_pixel_id = edge_id;
    {% if edge_slot_bits == 16 %}
    let slot_w0 = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id * 2u];
    let slot_w1 = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id * 2u + 1u];
    {% else %}
    let slot_map = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id];
    {% endif %}
    var slot_index: u32 = 4u;
    for (var i = 0u; i < 4u; i++) {
        {% if edge_slot_bits == 16 %}
        let word = select(slot_w0, slot_w1, i >= 2u);
        let field = (word >> ((i % 2u) * 16u)) & 0xFFFFu;
        if (field == 0xFFFEu) {
        {% else %}
        let field = (slot_map >> (i * 8u)) & 0xFFu;
        if (field == 0xFEu) {
        {% endif %}
            slot_index = i;
            break;
        }
    }
    if (slot_index >= 4u) {
        return;
    }

    // skybox_edge_resolve uses the camera viewport size as the screen dims
    // for ray reconstruction (NOT textureDimensions(skybox_tex)).
    let sky_screen_dims_f32 = camera.viewport_size;

    // Sky samples at this pixel = samples NOT covered by geometry
    // (tri_id == U32_MAX) — the classify SID_SKYBOX assignment. This
    // reconstructs the sky sample_mask skybox_edge_resolve read from the
    // compact skybox sample list.
    var color_sum = vec3<f32>(0.0);
    var alpha_sum: f32 = 0.0;
    var sample_count: u32 = 0u;
    var weight_sum: f32 = 0.0;
    for (var s = 0u; s < 4u; s++) {
        var vis_s: vec4<u32>;
        switch(s) {
            case 0u: { vis_s = textureLoad(visibility_data_tex, coords, 0); }
            case 1u: { vis_s = textureLoad(visibility_data_tex, coords, 1); }
            case 2u: { vis_s = textureLoad(visibility_data_tex, coords, 2); }
            case 3u, default: { vis_s = textureLoad(visibility_data_tex, coords, 3); }
        }
        let tri_s = join32(vis_s.x, vis_s.y);
        var is_sky = tri_s == U32_MAX;
        if (!is_sky) {
            // HUD samples are routed to SID_SKYBOX in classify too.
            let mesh_meta_s = material_mesh_metas[join32(vis_s.z, vis_s.w) / 256u];
            if (mesh_meta_s.is_hud == 1u) { is_sky = true; }
        }
        if (is_sky) {
            let sky_col = sample_skybox(coords, sky_screen_dims_f32, camera, skybox_tex, skybox_sampler);
            // Karis weighting — MUST match the material edge arm (both
            // writers feed the same accumulator; mixing weighted and
            // unweighted slots would bias the final_blend division).
            let karis_w = 1.0 / (1.0 + max(sky_col.r, max(sky_col.g, sky_col.b)));
            color_sum += sky_col.rgb * karis_w;
            alpha_sum += sky_col.a;
            sample_count += 1u;
            weight_sum += karis_w;
        }
    }

    if (sample_count == 0u) {
        return;
    }

    let accum_word_index = edge_layout.accumulator_base + (edge_pixel_id * 4u + slot_index) * 4u;
    edge_data[accum_word_index + 0u] = bitcast<u32>(color_sum.x);
    edge_data[accum_word_index + 1u] = bitcast<u32>(color_sum.y);
    edge_data[accum_word_index + 2u] = bitcast<u32>(color_sum.z);
    // Karis WEIGHT sum, not the raw sample count (matches the material arm).
    edge_data[accum_word_index + 3u] = bitcast<u32>(weight_sum);
}
{% endif %}
