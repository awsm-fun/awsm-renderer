// skybox_primary.wgsl — dedicated skybox writer for the canonical skybox bucket.
//
// The canonical "PBR" bucket (bucket 0) is, in practice, the skybox-only bucket:
// classify routes skybox/uncovered pixels to it, and real PBR materials route to
// their own feature-variant buckets. So this pipeline reuses that bucket's tile
// list + indirect args + bind groups (no classify change), but does ONLY the
// skybox write — no material shading. It replaces the former `owns_skybox` path
// that was tangled into compute.wgsl, leaving the material kernel pure.
//
// Shares the kernel preamble with compute.wgsl; `inc = skybox_only` gates out all
// the heavy PBR shading includes, so this compiles to a tiny shader.
// See docs/plans/SKINNY-MATERIALS.md.
{% include "material_opaque_wgsl/opaque_kernel_includes.wgsl" %}

@compute @workgroup_size(8, 8)
fn main(
    @builtin(workgroup_id) wg_id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>
) {
    // Same bucket-tile lookup as the material kernel — this pipeline is
    // dispatched over the canonical skybox bucket's tile list.
    let bucket_offset =
    {%- for entry in bucket_entries -%}
        {%- if shader_id == entry.shader_id -%}
        classify_buckets.{{ entry.offset_field() }}
        {%- endif -%}
    {%- endfor -%}
    ;
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
