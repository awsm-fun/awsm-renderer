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
