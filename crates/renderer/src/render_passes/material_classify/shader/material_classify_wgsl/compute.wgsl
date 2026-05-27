// Material classify compute pass.
//
// Per 8×8 tile, scan the visibility buffer and discover which opaque
// `shader_id`s its pixels belong to. Aggregate via a workgroup-shared
// bitmask (one bit per registered bucket, including dynamic
// materials), then thread 0 atomically appends the tile's coords to
// each bucket bit is set in. The total atomic traffic is ~1 per
// workgroup-bit, regardless of the 64 threads inside.
//
// Skybox pixels (`triangle_index == U32_MAX`) are routed to the PBR
// bucket — the PBR pipeline retains the skybox-fallback `textureStore`
// in `material_opaque/.../compute.wgsl` so the existing skybox rendering
// path keeps working with zero extra plumbing. Non-PBR pipelines
// (Unlit / Toon / FlipBook / any registered dynamic material)
// early-return on skybox without writing.
//
// The bit constants + the shader_id → bit dispatch chain + the
// per-bucket extract block are all walked from the same
// `bucket_entries` list the templated `ClassifyOutput` struct uses.

{{ shader_id_consts|safe }}

{% include "shared_wgsl/math.wgsl" %}

{% if emit_edge_data && multisampled_geometry %}
// Linearize a raw depth-buffer value into view-space Z. Ported from
// main's helpers/msaa.wgsl::viewSpaceDepth — same math, same name
// adjusted to snake_case. Required so a relative depth threshold
// (EDGE_DEPTH_THRESHOLD = 0.02 = 2%) works consistently across the
// scene; raw NDC depth is non-linear so the same threshold near vs
// far would produce wildly different real-world distances.
fn view_space_depth(camera: Camera, depth: f32, pixel_coords: vec2<f32>, screen_dims: vec2<f32>) -> f32 {
    let ndc_xy = vec2<f32>(
        (pixel_coords.x / screen_dims.x) * 2.0 - 1.0,
        1.0 - (pixel_coords.y / screen_dims.y) * 2.0,
    );
    let clip_pos = vec4<f32>(ndc_xy, depth, 1.0);
    let view_pos_h = camera.inv_proj * clip_pos;
    let view_pos = view_pos_h.xyz / view_pos_h.w;
    return view_pos.z;
}
{% endif %}

// Bits in the workgroup-shared mask. One per registered bucket; the
// PBR bit is at index 0 by convention so the skybox-fallback routing
// (which assigns the PBR bit unconditionally for skybox pixels) maps
// cleanly. `1u << index` for each entry.
{% for entry in bucket_entries %}
const {{ entry.bucket_bit_const() }}: u32 = (1u << {{ loop.index0 }}u);
{% endfor %}

var<workgroup> tile_mask: atomic<u32>;

@compute @workgroup_size(8, 8, 1)
fn cs_main(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(local_invocation_index) lii: u32,
) {
    // Zero the workgroup mask once per dispatch. The barrier below
    // makes the zero visible to every thread before the per-pixel
    // OR's land.
    if lii == 0u {
        atomicStore(&tile_mask, 0u);
    }
    workgroupBarrier();

    let screen_dims = textureDimensions(visibility_data_tex);
    let coords = vec2<i32>(wg.xy * 8u + lid.xy);
    let in_bounds = coords.x < i32(screen_dims.x) && coords.y < i32(screen_dims.y);

    var local_bit: u32 = 0u;
    if in_bounds {
        let vis = textureLoad(visibility_data_tex, coords, 0);
        let tri = join32(vis.x, vis.y);
        if tri == U32_MAX {
            // Skybox — handled by the PBR pipeline (it retains the
            // `triangle_index == U32_MAX → sample_skybox` fallback).
            local_bit = BUCKET_BIT_PBR;
        } else {
            let meta_offset = join32(vis.z, vis.w);
            let mesh_meta = material_mesh_metas[meta_offset / 256u];
            if mesh_meta.is_hud == 0u {
                // shader_id is stored as the first u32 of each
                // material payload; `material_offset` is in bytes.
                let shader_id = materials_data[mesh_meta.material_offset / 4u];
                {% for entry in bucket_entries %}
                {% if loop.first %}if{% else %}else if{% endif %} shader_id == {{ entry.shader_id_const() }} {
                    local_bit = {{ entry.bucket_bit_const() }};
                }
                {% endfor %}
            }
            // HUD pixels are redrawn by the transparency pass — skip
            // them in classify so the opaque pipelines don't process
            // tiles that contain only HUD geometry.
        }
    }

    if local_bit != 0u {
        atomicOr(&tile_mask, local_bit);
    }
    workgroupBarrier();

    if lii == 0u {
        let mask = atomicLoad(&tile_mask);
        let tile = vec2<u32>(wg.xy);

        // One extract block per registered bucket. The atomic returns
        // the previous count, which also doubles as the next free
        // index into the tile array at
        // `classify_output.<name>_offset + index`.
        {% for entry in bucket_entries %}
        if (mask & {{ entry.bucket_bit_const() }}) != 0u {
            let idx_{{ loop.index0 }} = atomicAdd(&classify_output.{{ entry.args_field() }}.workgroup_count_x, 1u);
            let slot_{{ loop.index0 }} = classify_output.{{ entry.offset_field() }} + idx_{{ loop.index0 }};
            if slot_{{ loop.index0 }} < classify_output.{{ entry.offset_field() }} + classify_output.bucket_capacity {
                classify_output.tiles[slot_{{ loop.index0 }}] = tile;
            }
        }
        {% endfor %}
    }

    {% if emit_edge_data && multisampled_geometry %}
    // ─────────────────────────────────────────────────────────────
    // MSAA edge emission (Priority 3).
    //
    // For each pixel: scan all 4 samples; collect the distinct
    // shader_ids; if there are at least 2 distinct shader_ids
    // (counting "skybox" as its own shader_id), the pixel is an edge
    // pixel. Allocate a compact edge_pixel_id via the global atomic,
    // write its (x, y) into edge_to_xy, build the slot_map (4 bytes
    // packed: up to 4 distinct shader_ids), and append a per-shader
    // (edge_pixel_id, sample_mask) entry to each contributing bucket's
    // sample list.
    //
    // Saturation: if edge_count saturates at MAX_EDGE_BUDGET, the
    // `edge_buffers.edge_overflow_count` atomic increments (see else
    // branch below) and we fall through without writing. The dropped
    // pixels render with the primary-sample shading written by the
    // material's primary pass — visually a missing-MSAA-resolve, not a
    // black hole. Stage 3.8 MVP (see edge_buffers.rs::note_edge_overflow_observed)
    // surfaces this via a one-shot tracing::warn when overflow is
    // observed CPU-side; the full atomic-add fallback (a hash-bucketed
    // overflow accumulator region) is parked as Block C.2 future work.
    if (in_bounds) {
        // Scan 4 samples — FULLY-STATIC version, no dynamic indexing
        // anywhere. Naga/Tint silently no-op'd dynamic-index writes
        // into both `vec4<u32>` and (turns out per the user's repro)
        // also `array<u32, 4>` in some configurations. Working around
        // entirely by unrolling and using individual `let`s.

        let v0 = textureLoad(visibility_data_tex, coords, 0);
        let v1 = textureLoad(visibility_data_tex, coords, 1);
        let v2 = textureLoad(visibility_data_tex, coords, 2);
        let v3 = textureLoad(visibility_data_tex, coords, 3);

        let tri_0 = join32(v0.x, v0.y);
        let tri_1 = join32(v1.x, v1.y);
        let tri_2 = join32(v2.x, v2.y);
        let tri_3 = join32(v3.x, v3.y);

        let mat_off_0 = join32(v0.z, v0.w);
        let mat_off_1 = join32(v1.z, v1.w);
        let mat_off_2 = join32(v2.z, v2.w);
        let mat_off_3 = join32(v3.z, v3.w);

        // Helper bucket-id derivation, inlined per sample.
        // `sample_sid` semantics: 0xFEu = skybox/uncovered/HUD,
        // [0, bucket_count) = real bucket, 0xFFu = sentinel-unmapped
        // (shouldn't happen in practice; treated like skybox below).
        var sid_0: u32 = 0xFFu;
        if (tri_0 == U32_MAX) {
            sid_0 = 0xFEu;
        } else {
            let mm = material_mesh_metas[mat_off_0 / 256u];
            if (mm.is_hud == 1u) { sid_0 = 0xFEu; }
            else {
                let raw_sid = materials_data[mm.material_offset / 4u];
                {% for entry in bucket_entries %}
                if (raw_sid == {{ entry.shader_id_const() }}) { sid_0 = {{ loop.index0 }}u; }
                {% endfor %}
            }
        }
        var sid_1: u32 = 0xFFu;
        if (tri_1 == U32_MAX) {
            sid_1 = 0xFEu;
        } else {
            let mm = material_mesh_metas[mat_off_1 / 256u];
            if (mm.is_hud == 1u) { sid_1 = 0xFEu; }
            else {
                let raw_sid = materials_data[mm.material_offset / 4u];
                {% for entry in bucket_entries %}
                if (raw_sid == {{ entry.shader_id_const() }}) { sid_1 = {{ loop.index0 }}u; }
                {% endfor %}
            }
        }
        var sid_2: u32 = 0xFFu;
        if (tri_2 == U32_MAX) {
            sid_2 = 0xFEu;
        } else {
            let mm = material_mesh_metas[mat_off_2 / 256u];
            if (mm.is_hud == 1u) { sid_2 = 0xFEu; }
            else {
                let raw_sid = materials_data[mm.material_offset / 4u];
                {% for entry in bucket_entries %}
                if (raw_sid == {{ entry.shader_id_const() }}) { sid_2 = {{ loop.index0 }}u; }
                {% endfor %}
            }
        }
        var sid_3: u32 = 0xFFu;
        if (tri_3 == U32_MAX) {
            sid_3 = 0xFEu;
        } else {
            let mm = material_mesh_metas[mat_off_3 / 256u];
            if (mm.is_hud == 1u) { sid_3 = 0xFEu; }
            else {
                let raw_sid = materials_data[mm.material_offset / 4u];
                {% for entry in bucket_entries %}
                if (raw_sid == {{ entry.shader_id_const() }}) { sid_3 = {{ loop.index0 }}u; }
                {% endfor %}
            }
        }

        // Build the distinct-shader-id list (`seen[0..seen_count)`) by
        // explicit static comparisons. Static `seen_*` vars avoid the
        // dynamic-write-into-array problem.
        var seen_0: u32 = sid_0;
        var seen_1: u32 = 0xFFu;
        var seen_2: u32 = 0xFFu;
        var seen_3: u32 = 0xFFu;
        var seen_count: u32 = 1u;

        if (sid_1 != seen_0) {
            seen_1 = sid_1;
            seen_count = 2u;
        }
        let sid_2_new = (sid_2 != seen_0) && (sid_2 != seen_1);
        if (sid_2_new) {
            if (seen_count == 1u) { seen_1 = sid_2; }
            else if (seen_count == 2u) { seen_2 = sid_2; }
            seen_count = seen_count + 1u;
        }
        let sid_3_new = (sid_3 != seen_0) && (sid_3 != seen_1) && (sid_3 != seen_2);
        if (sid_3_new) {
            if (seen_count == 1u) { seen_1 = sid_3; }
            else if (seen_count == 2u) { seen_2 = sid_3; }
            else if (seen_count == 3u) { seen_3 = sid_3; }
            seen_count = seen_count + 1u;
        }

        var slot_map: u32 = 0xFFFFFFFFu;

        // Edge pixel: 2+ distinct shader_ids (counts skybox as one)
        // OR samples cover different meshes (different mat_off).
        // We deliberately do NOT include "different tri_id within the
        // same mesh" — that fires at intra-mesh triangle seams of
        // tessellated curved surfaces, and the resulting per-sample
        // re-shading produces wireframe-like artifacts (samples on
        // adjacent triangles can have wildly different bary derivs /
        // depth, so the average isn't a smooth blend).
        //
        // SILHOUETTE detection: a pixel is a silhouette edge iff its
        // 4 samples are MIXED between "covered by geometry" and "not".
        // Diagnosis showed that tri_id (v.x / v.y channels) IS distinct
        // per sample at silhouettes (and intra-mesh tri seams), while
        // mat_meta_offset (v.z / v.w) gets broadcast across samples by
        // the fragment shader output path on this Tint compile. Using
        // mat_meta diff alone never caught silhouettes; using tri_id
        // diff caught silhouettes AND intra-mesh tri seams — but the
        // latter produced wireframe artifacts when edge_resolve
        // shaded per-sample. The clean discriminator: was sample N
        // *covered* by any triangle? `tri_id == U32_MAX` means
        // uncovered (clear value). Mixed coverage across the 4
        // samples = silhouette against skybox/uncovered region. All
        // covered with differing tri_ids = intra-mesh seam (NOT a
        // silhouette; skip).
        let cov_0 = tri_0 != U32_MAX;
        let cov_1 = tri_1 != U32_MAX;
        let cov_2 = tri_2 != U32_MAX;
        let cov_3 = tri_3 != U32_MAX;
        // Mesh-vs-skybox silhouette (coverage mismatch) OR mesh-vs-mesh
        // silhouette in the same pixel (different mat_meta_offset
        // across samples). Earlier hypothesis that mat_meta gets
        // broadcast by Tint may have been wrong — empirical retest with
        // the depth-tex / view-space pipeline showed the per-sample data
        // IS distinct after all when MULTIPLE fragments cover the same
        // pixel (e.g. capsule fragment over samples 0,1 + platform
        // fragment over samples 2,3 each write their own mat_meta).
        let any_cov_differs = (cov_0 != cov_1)
            || (cov_0 != cov_2)
            || (cov_0 != cov_3);
        let any_mat_differs = (mat_off_0 != mat_off_1)
            || (mat_off_0 != mat_off_2)
            || (mat_off_0 != mat_off_3);
        let any_mesh_differs = any_cov_differs || any_mat_differs;

        // PORT OF MAIN'S MSAA EDGE DETECTION (edge_mask_depth_msaa +
        // edge_mask_neighbors).
        //
        // Two checks combined:
        // (1) Per-sample VIEW-SPACE depth variance in this pixel —
        //     catches in-pixel mesh-vs-mesh silhouettes where ≥2
        //     samples cover different surfaces. Uses view-space depth
        //     (linearized via camera.inv_proj) so the same relative
        //     threshold works near AND far from the camera. Raw
        //     depth-buffer comparison fired too aggressively at far
        //     depths and not at all near the camera.
        // (2) 4-neighbor view-space depth/coverage check — catches
        //     silhouettes where the silhouette runs *between* pixels
        //     (current pixel fully covered, neighbor fully uncovered
        //     or at very different depth). Matches main's
        //     edge_mask_neighbors which returns true for uncovered
        //     neighbors AND for covered neighbors with depth delta >
        //     EDGE_DEPTH_THRESHOLD.
        //
        // Thresholds taken verbatim from main:
        //   EDGE_DEPTH_THRESHOLD = 0.02  (2% relative view-space depth)
        //   EDGE_MSAA_DEPTH_THRESHOLD = 0.02  (same scale, same threshold)
        let camera = camera_from_raw(camera_raw);
        let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));
        let pixel_center = vec2<f32>(f32(coords.x) + 0.5, f32(coords.y) + 0.5);

        // ── Check 1: in-pixel depth variance (view-space) ──────────
        var sample_count: u32 = 0u;
        var vdmin: f32 = 1.0e9;
        var vdmax: f32 = -1.0e9;
        if (cov_0) {
            let d = textureLoad(depth_tex, coords, 0);
            let vd = view_space_depth(camera, d, pixel_center, screen_dims_f32);
            vdmin = min(vdmin, vd);
            vdmax = max(vdmax, vd);
            sample_count = sample_count + 1u;
        }
        if (cov_1) {
            let d = textureLoad(depth_tex, coords, 1);
            let vd = view_space_depth(camera, d, pixel_center, screen_dims_f32);
            vdmin = min(vdmin, vd);
            vdmax = max(vdmax, vd);
            sample_count = sample_count + 1u;
        }
        if (cov_2) {
            let d = textureLoad(depth_tex, coords, 2);
            let vd = view_space_depth(camera, d, pixel_center, screen_dims_f32);
            vdmin = min(vdmin, vd);
            vdmax = max(vdmax, vd);
            sample_count = sample_count + 1u;
        }
        if (cov_3) {
            let d = textureLoad(depth_tex, coords, 3);
            let vd = view_space_depth(camera, d, pixel_center, screen_dims_f32);
            vdmin = min(vdmin, vd);
            vdmax = max(vdmax, vd);
            sample_count = sample_count + 1u;
        }
        var depth_edge: bool = false;
        if (sample_count >= 2u) {
            let depth_range = abs(vdmax - vdmin);
            let avg_depth = abs((vdmax + vdmin) * 0.5);
            depth_edge = depth_range > (0.02 * avg_depth);
        }

        // ── Check 2: 4-neighbor coverage + normal + depth check ─────
        //
        // Order matches main's `edge_mask_neighbors`:
        //   1. neighbor uncovered (tri_id == U32_MAX) → edge.
        //   2. normal discontinuity (dot < 0.95 ≈ 18°) → edge.
        //   3. depth discontinuity (relative 2% in view-space) → edge.
        //
        // The normal check is critical at tile-facet boundaries on the
        // platform top, where adjacent same-mesh tiles share depth +
        // mat_meta but rotate their surface normal by a small angle.
        // Depth-only neighbour detection misses these and leaves a
        // diagonal stripe of aliasing along the platform's top-front
        // edge.
        let EDGE_NORMAL_THRESHOLD: f32 = 0.95;
        var neighbor_edge: bool = false;
        if (cov_0) {
            let center_d = textureLoad(depth_tex, coords, 0);
            let center_vd = view_space_depth(camera, center_d, pixel_center, screen_dims_f32);
            let depth_threshold = 0.02 * abs(center_vd);
            // Center normal: sample-0 of the multisampled normal_tangent
            // texture, unpacked via the shared TBN helper. Same convention
            // the primary opaque + edge_resolve paths use.
            let center_nt = textureLoad(normal_tangent_tex, coords, 0);
            let center_tbn = unpack_normal_tangent(center_nt);
            let center_normal = center_tbn.N;
            let neighbor_offsets = array<vec2<i32>, 4>(
                vec2<i32>(1, 0),
                vec2<i32>(-1, 0),
                vec2<i32>(0, 1),
                vec2<i32>(0, -1),
            );
            let pixel_offsets = array<vec2<f32>, 4>(
                vec2<f32>(1.0, 0.0),
                vec2<f32>(-1.0, 0.0),
                vec2<f32>(0.0, 1.0),
                vec2<f32>(0.0, -1.0),
            );
            for (var ni = 0; ni < 4; ni++) {
                let n_coords = coords + neighbor_offsets[ni];
                if (n_coords.x < 0 || n_coords.y < 0
                    || n_coords.x >= i32(screen_dims.x)
                    || n_coords.y >= i32(screen_dims.y)) {
                    continue;
                }
                let nv = textureLoad(visibility_data_tex, n_coords, 0);
                let n_tri = join32(nv.x, nv.y);
                if (n_tri == U32_MAX) {
                    neighbor_edge = true;
                    break;
                }
                // Normal discontinuity (sample 0 of neighbour pixel).
                let n_nt = textureLoad(normal_tangent_tex, n_coords, 0);
                let n_tbn = unpack_normal_tangent(n_nt);
                if (dot(center_normal, n_tbn.N) < EDGE_NORMAL_THRESHOLD) {
                    neighbor_edge = true;
                    break;
                }
                // Depth discontinuity (view-space relative).
                let n_depth = textureLoad(depth_tex, n_coords, 0);
                let n_pixel_center = pixel_center + pixel_offsets[ni];
                let n_vd = view_space_depth(camera, n_depth, n_pixel_center, screen_dims_f32);
                if (abs(center_vd - n_vd) > depth_threshold) {
                    neighbor_edge = true;
                    break;
                }
            }
        }

        // Edge gate. Four signals, all needed for parity with main's
        // `msaa_resolve_samples` edge detection:
        //   * `seen_count >= 2u`        — multi-shader-id silhouette
        //   * `any_mesh_differs`        — mesh-vs-mesh (different mat_meta)
        //   * `depth_edge`              — per-sample depth variance
        //                                 (same-mesh in-pixel, e.g. the
        //                                 platform's top/front-face seam)
        //   * `neighbor_edge`           — 4-neighbour depth/coverage check
        //                                 (silhouette runs between pixels)
        //
        // depth_edge + neighbor_edge were previously disabled because
        // they produced "black sides on capsules" — that was an artefact
        // of the texture-pool-arrays-len template mismatch in
        // `texture_pool_sample_grad` (every same-mesh edge fell into the
        // `default → vec4(0)` branch in edge_resolve's compiled shader).
        // With the recompile wired into `finalize_gpu_textures`
        // (textures.rs), per-sample shading at same-mesh edges produces
        // the right value and the depth signals can be re-enabled.
        if (seen_count >= 2u || any_mesh_differs || depth_edge || neighbor_edge) {
            // Allocate compact edge_pixel_id. The atomic counter lives
            // in args_buffer / `edge_buffers` (drives indirect dispatch
            // for final_blend); we also mirror it into edge_data's
            // header so the resolve shaders can read it without binding
            // args_buffer (saves a storage-buffer slot vs the 10-cap).
            let edge_id = atomicAdd(&edge_buffers.edge_count, 1u);
            atomicAdd(&edge_data[edge_layout.edge_count_index], 1u);
            if (edge_id < edge_layout.max_edge_budget) {
                // Write edge_to_xy via atomicStore (edge_data is
                // declared as array<atomic<u32>> so even plain stores
                // go through the atomic interface).
                let packed_xy = (u32(coords.x) & 0xFFFFu) | ((u32(coords.y) & 0xFFFFu) << 16u);
                atomicStore(&edge_data[edge_layout.edge_to_xy_base + edge_id], packed_xy);

                // Pack slot_map: 4 bytes, each byte is a bucket index
                // (or 0xFE for skybox, 0xFF for empty slot).
                slot_map = (seen_0 & 0xFFu)
                    | ((seen_1 & 0xFFu) << 8u)
                    | ((seen_2 & 0xFFu) << 16u)
                    | ((seen_3 & 0xFFu) << 24u);
                atomicStore(&edge_data[edge_layout.edge_slot_map_base + edge_id], slot_map);

                // For each per-shader sample mask: append (edge_id,
                // sample_mask) to that bucket's sample list. Skybox
                // samples route to the skybox sample list (separate
                // reserved region). Unrolled per-sample to avoid any
                // dynamic indexing into per-sample arrays.
                var skybox_mask: u32 = 0u;
                {% for entry in bucket_entries %}
                var mask_{{ loop.index0 }}: u32 = 0u;
                {% endfor %}

                // Sample 0
                if (sid_0 == 0xFEu) { skybox_mask |= 1u; }
                {% for entry in bucket_entries %}
                else if (sid_0 == {{ loop.index0 }}u) { mask_{{ loop.index0 }} |= 1u; }
                {% endfor %}
                // Sample 1
                if (sid_1 == 0xFEu) { skybox_mask |= 2u; }
                {% for entry in bucket_entries %}
                else if (sid_1 == {{ loop.index0 }}u) { mask_{{ loop.index0 }} |= 2u; }
                {% endfor %}
                // Sample 2
                if (sid_2 == 0xFEu) { skybox_mask |= 4u; }
                {% for entry in bucket_entries %}
                else if (sid_2 == {{ loop.index0 }}u) { mask_{{ loop.index0 }} |= 4u; }
                {% endfor %}
                // Sample 3
                if (sid_3 == 0xFEu) { skybox_mask |= 8u; }
                {% for entry in bucket_entries %}
                else if (sid_3 == {{ loop.index0 }}u) { mask_{{ loop.index0 }} |= 8u; }
                {% endfor %}
                // Append per-bucket entries. The atomic index counter
                // is mirrored on both args_buffer (for indirect
                // dispatch) and edge_data's header (for shader reads).
                {% for entry in bucket_entries %}
                if (mask_{{ loop.index0 }} != 0u) {
                    let slot_idx_{{ loop.index0 }} = atomicAdd(&edge_buffers.{{ entry.args_field() }}_edge.workgroup_count_x, 1u);
                    atomicAdd(&edge_data[edge_layout.per_shader_count_base + {{ loop.index0 }}u], 1u);
                    if (slot_idx_{{ loop.index0 }} < edge_layout.sample_entries_per_bucket) {
                        let entry_packed_{{ loop.index0 }} = (edge_id & 0x00FFFFFFu) | ((mask_{{ loop.index0 }} & 0xFFu) << 24u);
                        atomicStore(&edge_data[edge_layout.{{ entry.args_field() }}_sample_list_base + slot_idx_{{ loop.index0 }}], entry_packed_{{ loop.index0 }});
                    }
                }
                {% endfor %}
                // Skybox sample list — counter mirrored into both
                // args_buffer and edge_data header.
                if (skybox_mask != 0u) {
                    let sky_slot_idx = atomicAdd(&edge_buffers.skybox_edge_args.workgroup_count_x, 1u);
                    atomicAdd(&edge_data[edge_layout.skybox_count_index], 1u);
                    if (sky_slot_idx < edge_layout.sample_entries_per_bucket) {
                        let sky_entry_packed = (edge_id & 0x00FFFFFFu) | ((skybox_mask & 0xFFu) << 24u);
                        atomicStore(&edge_data[edge_layout.skybox_sample_list_base + sky_slot_idx], sky_entry_packed);
                    }
                }
                // Final blend args: one workgroup per edge pixel
                // (workgroup_size = 64, so divide by 64).
                if ((edge_id & 63u) == 0u) {
                    atomicAdd(&edge_buffers.final_blend_args.workgroup_count_x, 1u);
                }
            } else {
                atomicAdd(&edge_buffers.edge_overflow_count, 1u);
            }
        }
    }
    {% endif %}
}
