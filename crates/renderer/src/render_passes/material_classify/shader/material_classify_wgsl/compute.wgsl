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
        // Scan 4 samples. Each entry holds (shader_id, sample_index).
        //
        // ROOT-CAUSE FIX (May 27, post-`a903e4c`): these were `vec4<u32>`
        // with dynamic-index writes (`sample_shader_ids[s] = …`). On the
        // current Tint→SPIR-V/Metal compile path that pattern silently
        // *no-ops* — the writes never land, so `sample_shader_ids` and
        // `sample_mat_offs` stayed at their initial all-equal values for
        // every pixel. `seen_count` was thus always 1 and
        // `any_mesh_differs` always false → **classify emitted zero
        // edges**, the entire Stage-3 dispatch chain ran with
        // `workgroup_count_x = 0`, every silhouette pixel rendered with
        // primary-opaque sample-0 shading, and the user saw blatant
        // stair-step aliasing on the MorphStressTest shelf bottom and
        // capsule edges. `array<u32, 4>` honours dynamic-index writes
        // (it's how the `seen[]` array below has always worked) — that
        // simple change makes the whole edge-emission path actually
        // populate.
        var sample_shader_ids: array<u32, 4> = array<u32, 4>(0xFFFFu, 0xFFFFu, 0xFFFFu, 0xFFFFu);
        var sample_mat_offs: array<u32, 4> = array<u32, 4>(0u, 0u, 0u, 0u);
        var distinct_count: u32 = 0u;
        // 4 slots, each u8 packed into a u32. SHADER_ID_NONE = 0xFF.
        var slot_map: u32 = 0xFFFFFFFFu;

        for (var s = 0u; s < 4u; s++) {
            // Load this sample's shader_id via the multisampled
            // visibility-data texture. For sample 0 the loaded value
            // is the same as the primary sample above; the per-sample
            // textureLoad with explicit sample index needs the texture
            // to be bound as multisampled.
            var sample_vis: vec4<u32>;
            switch (s) {
                case 0u: { sample_vis = textureLoad(visibility_data_tex, coords, 0); }
                case 1u: { sample_vis = textureLoad(visibility_data_tex, coords, 1); }
                case 2u: { sample_vis = textureLoad(visibility_data_tex, coords, 2); }
                case 3u, default: { sample_vis = textureLoad(visibility_data_tex, coords, 3); }
            }
            let sample_tri = join32(sample_vis.x, sample_vis.y);
            // mat_meta_off is the byte offset into `material_mesh_metas`
            // for this sample's mesh. Same offset = same mesh. For
            // uncovered (skybox) samples, sample_vis.z/.w hold U32_MAX
            // sentinels; the joined value collapses to U32_MAX-like
            // and stays distinct from any real mesh offset.
            sample_mat_offs[s] = join32(sample_vis.z, sample_vis.w);
            var sample_sid: u32;
            if (sample_tri == U32_MAX) {
                // Skybox bucket: arbitrary marker we'll route to the
                // skybox-edge slot. Use 0xFE so it can't collide with
                // a real shader_id (kept under 8 bits to fit in the
                // packed slot_map byte).
                sample_sid = 0xFEu;
            } else {
                let sample_meta_off = join32(sample_vis.z, sample_vis.w);
                let sample_mesh_meta = material_mesh_metas[sample_meta_off / 256u];
                if (sample_mesh_meta.is_hud == 1u) {
                    // HUD — same as skybox-effective for edge purposes.
                    sample_sid = 0xFEu;
                } else {
                    let sample_raw_sid = materials_data[sample_mesh_meta.material_offset / 4u];
                    // Clip to 8 bits — first-party ids are 1..5;
                    // dynamic ids are >= 10_000 so they DO collide on
                    // truncation. For the slot_map slot id we instead
                    // store the bucket index (0..bucket_count-1)
                    // which always fits.
                    var bucket_index: u32 = 0xFFu;
                    {% for entry in bucket_entries %}
                    if (sample_raw_sid == {{ entry.shader_id_const() }}) {
                        bucket_index = {{ loop.index0 }}u;
                    }
                    {% endfor %}
                    sample_sid = bucket_index;
                }
            }
            sample_shader_ids[s] = sample_sid;
        }

        // Find distinct shader_ids and pack into slot_map.
        var seen_count: u32 = 0u;
        var seen: array<u32, 4> = array<u32, 4>(0xFFu, 0xFFu, 0xFFu, 0xFFu);
        for (var s = 0u; s < 4u; s++) {
            let sid = sample_shader_ids[s];
            var already_seen = false;
            for (var i = 0u; i < seen_count; i++) {
                if (seen[i] == sid) {
                    already_seen = true;
                    break;
                }
            }
            if (!already_seen && seen_count < 4u) {
                seen[seen_count] = sid;
                seen_count += 1u;
            }
        }

        // Edge pixel: 2+ distinct shader_ids (counts skybox as one)
        // OR samples cover different triangles even within the SAME
        // shader_id. The second condition catches the common case of
        // mesh-vs-mesh boundaries where both meshes share the same
        // material flavour (e.g. PBR-vs-PBR adjacent meshes). Pre-fix,
        // only multi-shader_id pixels routed to edge_resolve, so
        // single-shader_id scenes (the typical PBR case) had silently-
        // broken MSAA at every inter-mesh boundary.
        let any_mesh_differs = (sample_mat_offs[0] != sample_mat_offs[1])
            || (sample_mat_offs[0] != sample_mat_offs[2])
            || (sample_mat_offs[0] != sample_mat_offs[3]);
        if (seen_count >= 2u || any_mesh_differs) {
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
                slot_map = (seen[0] & 0xFFu)
                    | ((seen[1] & 0xFFu) << 8u)
                    | ((seen[2] & 0xFFu) << 16u)
                    | ((seen[3] & 0xFFu) << 24u);
                atomicStore(&edge_data[edge_layout.edge_slot_map_base + edge_id], slot_map);

                // For each per-shader sample mask: append (edge_id,
                // sample_mask) to that bucket's sample list. Skybox
                // samples route to the skybox sample list (separate
                // reserved region).
                var skybox_mask: u32 = 0u;
                {% for entry in bucket_entries %}
                var mask_{{ loop.index0 }}: u32 = 0u;
                {% endfor %}
                for (var s = 0u; s < 4u; s++) {
                    let sid = sample_shader_ids[s];
                    if (sid == 0xFEu) {
                        skybox_mask |= 1u << s;
                    }
                    {% for entry in bucket_entries %}
                    else if (sid == {{ loop.index0 }}u) {
                        mask_{{ loop.index0 }} |= 1u << s;
                    }
                    {% endfor %}
                }
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
