// Material classify compute pass.
//
// Per 8×8 tile, scan the visibility buffer and discover which opaque
// `shader_id`s its pixels belong to. Aggregate via a workgroup-shared
// bitmask (one bit per registered bucket, including dynamic
// materials), then thread 0 atomically appends the tile's coords to
// each bucket bit is set in. The total atomic traffic is ~1 per
// workgroup-bit, regardless of the 64 threads inside.
//
// Skybox pixels (`triangle_index == U32_MAX`) are routed to the dedicated
// SKYBOX bucket (index 0). Its opaque pipeline is the `skybox_primary.wgsl`
// writer, dispatched over that bucket's tiles; the material kernels
// (compute.wgsl) shade real geometry only. See
// material_opaque/.../skybox_primary.wgsl.
//
// The bit constants + the shader_id → bit dispatch chain + the
// per-bucket extract block are all walked from the same
// `bucket_entries` list the templated `ClassifyOutput` struct uses.
//
// §4a/§4c made the per-pixel + per-sample maps data-driven (`bucket_lut`)
// and the fan-out + append index-driven, so the old per-bucket
// `SHADER_ID_<NAME>` / `BUCKET_BIT_<NAME>` constants are no longer
// referenced — the classify shader text now depends only on counts/widths
// (bucket_count, n_words, edge_slot_bits), never on bucket identities.

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
    // GUARD w — never divide raw here. Under reverse-Z the BACKGROUND/sky
    // carries the depth clear value 0.0, which is the FAR plane; with the main
    // camera's INFINITE-far projection (`perspective_infinite_reverse_rh`) that
    // unprojects to w == 0 EXACTLY, so a raw divide yields ±Inf. Feeding Inf
    // into the relative EDGE_DEPTH_THRESHOLD comparisons below poisons them
    // (the ratio goes Inf or NaN, and NaN compares false), so every
    // geometry↔sky silhouette — precisely the edges MSAA exists to smooth —
    // was classified wrongly.
    //
    // Clamping yields a large FINITE depth, which is also the semantically
    // right reading: sky really is at infinity, so a sky/geometry neighbour
    // pair produces a large relative difference and registers as an edge,
    // while sky/sky pairs still difference to ~0 and do not. Mirrors the
    // existing guards in material_prep/compute.wgsl and helpers/standard.wgsl.
    let view_pos = view_pos_h.xyz / max(view_pos_h.w, 1e-8);
    return view_pos.z;
}

// Full-u32 edge-sample sentinels (§5), width-INDEPENDENT: a real bucket
// index (0..65533) can never equal these, so `sid`/`seen`/append logic needs
// no per-width branching. The slot_map PACK truncates them to the
// width-correct packed sentinel (`& 0xFF` → 0xFE/0xFF for 8-bit; `& 0xFFFF`
// → 0xFFFE/0xFFFF for 16-bit), so the 8-bit packed output is byte-identical
// to before.
const SID_SKYBOX: u32 = 0xFFFFFFFEu;
const SID_EMPTY: u32 = 0xFFFFFFFFu;

// (Unified-edge U2b-3) The per-bucket + skybox edge-SAMPLE-LIST machinery
// (`append_edge_sample`) was removed: those lists fed only the legacy
// cs_edge / skybox_edge_resolve pipelines, which are gone. The unified
// `cs_shade` kernel drives edge shading from the per-pixel edge-id texture +
// the packed slot map instead, so classify no longer appends sample-list
// entries (and `data_buffer` no longer allocates the lists). SID_SKYBOX /
// SID_EMPTY below are still used by the slot_map pack.
{% endif %}

// Workgroup-shared bucket mask. One bit per registered bucket; the SKYBOX
// bucket is at index 0 (id 0 sorts first) → word 0, bit 0. Each bucket lives
// in word `index / 32` at bit `index % 32`; `tile_mask` is
// `array<atomic<u32>, n_words>` (n_words = ceil(live_count / 32)) so the
// bucket budget grows past 32 as the live count crosses each word boundary.
var<workgroup> tile_mask: array<atomic<u32>, {{ n_words }}u>;

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
        {% for w in words_iter %}
        atomicStore(&tile_mask[{{ w }}u], 0u);
        {% endfor %}
    }
    workgroupBarrier();

    let screen_dims = textureDimensions(visibility_data_tex);
    let coords = vec2<i32>(wg.xy * 8u + lid.xy);
    let in_bounds = coords.x < i32(screen_dims.x) && coords.y < i32(screen_dims.y);

    // Per-pixel map → O(1) LUT load (§4a). `bucket_lut[raw_sid]` replaces
    // the old O(buckets) `shader_id == SHADER_ID_*` if/else chain: one
    // dependent load, warp-coherent (neighbouring pixels share a material →
    // same slot). `bucket_index == 0xFFFFFFFF` (NOT_FOUND) reproduces the
    // old "no arm matched" fall-through — the pixel contributes no bucket
    // bit. The bucket index's word is `bucket_index / 32`, its bit
    // `bucket_index % 32`; at n_words == 1 the word is always 0.
    var bucket_index: u32 = 0xFFFFFFFFu;
    if in_bounds {
        let vis = textureLoad(visibility_data_tex, coords, 0);
        let tri = join32(vis.x, vis.y);
        if tri == U32_MAX {
            // Fully-uncovered ("sky") pixel → the dedicated SKYBOX bucket
            // (index 0, since id 0 sorts first), whose opaque pipeline is the
            // `skybox_primary` writer. Routed directly — sky pixels carry no
            // material payload to look up. See skybox_primary.wgsl.
            bucket_index = 0u;
        } else {
            let meta_offset = join32(vis.z, vis.w);
            let mesh_meta = material_mesh_metas[meta_offset / 256u];
            if mesh_meta.is_hud == 0u {
                // shader_id is stored as the first u32 of each
                // material payload; `material_offset` is in bytes.
                let shader_id = materials_data[mesh_meta.material_offset / 4u];
                bucket_index = bucket_lut[shader_id];
            }
            // HUD pixels are redrawn by the transparency pass — skip
            // them in classify so the opaque pipelines don't process
            // tiles that contain only HUD geometry.
        }
    }

    if bucket_index != 0xFFFFFFFFu {
        atomicOr(&tile_mask[bucket_index / 32u], 1u << (bucket_index % 32u));
    }
    {% if multisampled_geometry %}
    // Unified-edge ANY-sample tile_mask (U0). The sample-0 `bucket_index`
    // OR above runs UNCHANGED (so the `unified_edge=false` WGSL is byte
    // -identical); this block ADDS samples 1..3 so a bucket's tile list
    // covers tiles where it appears at ANY sample (atomicOr is idempotent,
    // so re-OR'ing sample 0 here is unnecessary — only 1..3 are added).
    // This makes an edge-only material's tiles reachable by its own
    // (future) unified dispatch. Inert in U0: the extra tiles only add a
    // few check-and-skip lanes to the existing per-pixel shader_id guard in
    // cs_opaque → same output. Per-sample bucket id derived the same way the
    // sample-0 path does (sky → bucket 0; HUD → skip; else bucket_lut).
    // Unrolled (no dynamic indexing into per-sample locals).
    if in_bounds {
        let uv1 = textureLoad(visibility_data_tex, coords, 1);
        let uv2 = textureLoad(visibility_data_tex, coords, 2);
        let uv3 = textureLoad(visibility_data_tex, coords, 3);
        let utri1 = join32(uv1.x, uv1.y);
        let utri2 = join32(uv2.x, uv2.y);
        let utri3 = join32(uv3.x, uv3.y);
        var ubucket1: u32 = 0xFFFFFFFFu;
        if utri1 == U32_MAX {
            ubucket1 = 0u;
        } else {
            let mm = material_mesh_metas[join32(uv1.z, uv1.w) / 256u];
            if mm.is_hud == 0u { ubucket1 = bucket_lut[materials_data[mm.material_offset / 4u]]; }
        }
        var ubucket2: u32 = 0xFFFFFFFFu;
        if utri2 == U32_MAX {
            ubucket2 = 0u;
        } else {
            let mm = material_mesh_metas[join32(uv2.z, uv2.w) / 256u];
            if mm.is_hud == 0u { ubucket2 = bucket_lut[materials_data[mm.material_offset / 4u]]; }
        }
        var ubucket3: u32 = 0xFFFFFFFFu;
        if utri3 == U32_MAX {
            ubucket3 = 0u;
        } else {
            let mm = material_mesh_metas[join32(uv3.z, uv3.w) / 256u];
            if mm.is_hud == 0u { ubucket3 = bucket_lut[materials_data[mm.material_offset / 4u]]; }
        }
        if ubucket1 != 0xFFFFFFFFu {
            atomicOr(&tile_mask[ubucket1 / 32u], 1u << (ubucket1 % 32u));
        }
        if ubucket2 != 0xFFFFFFFFu {
            atomicOr(&tile_mask[ubucket2 / 32u], 1u << (ubucket2 % 32u));
        }
        if ubucket3 != 0xFFFFFFFFu {
            atomicOr(&tile_mask[ubucket3 / 32u], 1u << (ubucket3 % 32u));
        }
    }
    {% endif %}
    workgroupBarrier();

    if lii == 0u {
        let tile = vec2<u32>(wg.xy);

        // Data-driven fan-out (§4b): iterate only the bucket bits
        // actually set in this tile's mask — `O(active buckets in tile)`,
        // never `O(total buckets)`. A typical tile touches 1–3 materials,
        // so this is ~1–3 iterations whether the registry holds 16 or
        // 1024 buckets. `firstTrailingBit` walks the set bits of each
        // mask word; `bucket_index = w*32 + bit` indexes the `args` /
        // `offsets` arrays directly. The atomicAdd returns the previous
        // count, which doubles as the next free slot into the bucket's
        // `tiles` sub-range at `offsets[bucket_index] + idx`.
        for (var w = 0u; w < {{ n_words }}u; w = w + 1u) {
            var bits = atomicLoad(&tile_mask[w]);
            while (bits != 0u) {
                let b = firstTrailingBit(bits);
                bits = bits & (bits - 1u);
                let bucket_index = w * 32u + b;
                let idx = atomicAdd(&classify_output.args[bucket_index].workgroup_count_x, 1u);
                let base = classify_output.offsets[bucket_index];
                let slot = base + idx;
                if slot < base + classify_output.bucket_capacity {
                    classify_output.tiles[slot] = tile;
                }
            }
        }
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
        // Unified-edge (U0): initialize this pixel's edge-id to the
        // U32_MAX sentinel ("not an edge pixel"). Edge pixels overwrite
        // it with their compact edge_pixel_id below. Every in-bounds pixel
        // is written exactly once here, so non-edge pixels reliably read
        // the sentinel without a separate per-frame clear. Inert in U0
        // (edge_id_tex is unread).
        textureStore(edge_id_tex, coords, vec4<u32>(0xFFFFFFFFu, 0u, 0u, 0u));
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
        // Per-sample map → O(1) LUT load (§4a), one per sample. `sid_s`
        // is SID_SKYBOX (skybox/uncovered/HUD), a real bucket index
        // [0, bucket_count), or SID_EMPTY (unmapped NOT_FOUND fall-through —
        // the data-driven form of the old "no arm matched"). Width-independent
        // full-u32 sentinels (§5) so the same code serves 8- and 16-bit slot
        // widths; they truncate at the slot_map pack. Each `sid_s` stays a
        // static local — no dynamic indexing into per-sample locals (Naga/Tint
        // scar, :163); only the `bucket_lut` storage load is dynamically
        // indexed, which is safe.
        var sid_0: u32 = SID_EMPTY;
        if (tri_0 == U32_MAX) {
            sid_0 = SID_SKYBOX;
        } else {
            let mm = material_mesh_metas[mat_off_0 / 256u];
            if (mm.is_hud == 1u) { sid_0 = SID_SKYBOX; }
            else {
                let bi = bucket_lut[materials_data[mm.material_offset / 4u]];
                if (bi != 0xFFFFFFFFu) { sid_0 = bi; }
            }
        }
        var sid_1: u32 = SID_EMPTY;
        if (tri_1 == U32_MAX) {
            sid_1 = SID_SKYBOX;
        } else {
            let mm = material_mesh_metas[mat_off_1 / 256u];
            if (mm.is_hud == 1u) { sid_1 = SID_SKYBOX; }
            else {
                let bi = bucket_lut[materials_data[mm.material_offset / 4u]];
                if (bi != 0xFFFFFFFFu) { sid_1 = bi; }
            }
        }
        var sid_2: u32 = SID_EMPTY;
        if (tri_2 == U32_MAX) {
            sid_2 = SID_SKYBOX;
        } else {
            let mm = material_mesh_metas[mat_off_2 / 256u];
            if (mm.is_hud == 1u) { sid_2 = SID_SKYBOX; }
            else {
                let bi = bucket_lut[materials_data[mm.material_offset / 4u]];
                if (bi != 0xFFFFFFFFu) { sid_2 = bi; }
            }
        }
        var sid_3: u32 = SID_EMPTY;
        if (tri_3 == U32_MAX) {
            sid_3 = SID_SKYBOX;
        } else {
            let mm = material_mesh_metas[mat_off_3 / 256u];
            if (mm.is_hud == 1u) { sid_3 = SID_SKYBOX; }
            else {
                let bi = bucket_lut[materials_data[mm.material_offset / 4u]];
                if (bi != 0xFFFFFFFFu) { sid_3 = bi; }
            }
        }

        // Build the distinct-shader-id list (`seen[0..seen_count)`) by
        // explicit static comparisons. Static `seen_*` vars avoid the
        // dynamic-write-into-array problem. Unused slots = SID_EMPTY.
        var seen_0: u32 = sid_0;
        var seen_1: u32 = SID_EMPTY;
        var seen_2: u32 = SID_EMPTY;
        var seen_3: u32 = SID_EMPTY;
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

                // Unified-edge (U0): mirror the compact edge_pixel_id into
                // the per-pixel edge-id texture. Overwrites the U32_MAX
                // sentinel written above for this pixel. WRITTEN-only in U0
                // (the future unified kernel reads it to branch
                // interior-vs-edge + index the per-sample accumulator). The
                // existing edge-sample-list machinery (append_edge_sample
                // below) stays INTACT alongside it.
                textureStore(edge_id_tex, coords, vec4<u32>(edge_id, 0u, 0u, 0u));

                // Pack the slot_map (§5): the 4 per-sample bucket ids, each
                // truncated to the slot width. 8-bit (≤254 buckets): one u32
                // (4×8) — byte-identical to before, sentinels 0xFE/0xFF.
                // 16-bit (>254): two u32 (4×16), sentinels 0xFFFE/0xFFFF.
                {% if edge_slot_bits == 16 %}
                let slot_base = edge_layout.edge_slot_map_base + edge_id * 2u;
                atomicStore(&edge_data[slot_base],
                    (seen_0 & 0xFFFFu) | ((seen_1 & 0xFFFFu) << 16u));
                atomicStore(&edge_data[slot_base + 1u],
                    (seen_2 & 0xFFFFu) | ((seen_3 & 0xFFFFu) << 16u));
                {% else %}
                let slot_map = (seen_0 & 0xFFu)
                    | ((seen_1 & 0xFFu) << 8u)
                    | ((seen_2 & 0xFFu) << 16u)
                    | ((seen_3 & 0xFFu) << 24u);
                atomicStore(&edge_data[edge_layout.edge_slot_map_base + edge_id], slot_map);
                {% endif %}

                // Clear this edge pixel's 4 accumulator slots (4 slots x
                // 8 u32 words — color+weight plus the SSR-descriptor half;
                // see ACCUMULATOR_SLOT_BYTES in edge_buffers.rs) so a bucket
                // whose per-shader edge_resolve pipeline isn't resident this
                // frame leaves count==0 — which final_blend skips — instead
                // of reading a stale value left at the same slot index by a
                // previous frame's edge pixel. This is what makes the
                // resolve per-bucket-independent. Bounded by the live edge
                // count: only freshly-allocated edge pixels are cleared.
                // STRIDE BUG (fixed): when the slots widened 16->32 bytes
                // for the per-sample SSR descriptor, this clear kept the old
                // 16-word math — half the span at half the stride — so edge
                // pixels in the upper half of the id range were NEVER
                // cleared and could resolve a stale prior-frame value.
                let accum_clear_base = edge_layout.accumulator_base + edge_id * {% if wide_edge_slots %}32u{% else %}16u{% endif %};
                for (var ci: u32 = 0u; ci < {% if wide_edge_slots %}32u{% else %}16u{% endif %}; ci = ci + 1u) {
                    atomicStore(&edge_data[accum_clear_base + ci], 0u);
                }

                // (Unified-edge U2b-3) Sample-list append removed — see the
                // note where `append_edge_sample` used to be defined. cs_shade
                // reads the packed slot map (above) + the per-pixel edge-id
                // texture to drive per-sample edge shading, so no per-bucket
                // lists are built.
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
