// Material classify pass bind groups.
//
// One bind group:
//   0: visibility_data_tex — sampled to recover per-pixel material id.
//   1: material_mesh_metas — storage[RO] mesh-meta table for the
//      `material_meta_offset → material_offset` step.
//   2: materials_data     — storage[RO] material payload; the first
//      `u32` of each entry is the `shader_id`.
//   3: classify_output    — storage[RW] (atomic) buckets + indirect
//      args. Layout matches the
//      `ClassifyBuffers` Rust-side header verbatim and is generated
//      per-registration by walking `bucket_entries` (first-party +
//      every registered dynamic material).

/*************** START material_mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END material_mesh_meta.wgsl ******************/

// `join32` and `U32_MAX` come from `shared_wgsl/math.wgsl`, included
// once by `compute.wgsl` (concatenated after this file).

{% if multisampled_geometry %}
@group(0) @binding(0) var visibility_data_tex: texture_multisampled_2d<u32>;
{% else %}
@group(0) @binding(0) var visibility_data_tex: texture_2d<u32>;
{% endif %}

@group(0) @binding(1) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;

@group(0) @binding(2) var<storage, read> materials_data: array<u32>;

struct ClassifyIndirectArgs {
    workgroup_count_x: atomic<u32>,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

// Storage-buffer layout — must stay in lockstep with the byte writer
// in `material_classify::buffers::write_header`. The N indirect args
// slots are at offsets `i * 16`; `dispatchWorkgroupsIndirect` reads
// each as `(x, y, z)` from the bound buffer at that offset.
//
// Data-driven layout (§4b): `args` is `array<ClassifyIndirectArgs,
// bucket_count>` and `offsets` is `array<u32, bucket_count>`, indexed
// by bucket index. This is **byte-identical** to the old back-to-back
// per-bucket named fields — `ClassifyIndirectArgs` is 16 B / align 4
// (WGSL array stride 16, matching the host `INDIRECT_ARGS_STRIDE`), and
// the u32 offsets array packs at stride 4 — so the host
// `header_bytes(bucket_count)` / `write_header` writer is unchanged. We
// only stop *naming* each bucket's slot, so thread-0's fan-out can index
// `args[bucket_index]` from the set-bit scan instead of an unrolled
// per-bucket `if`. The trailing `bucket_capacity` + `pad_words` of
// alignment padding push the runtime `tiles` array (vec2<u32>, 8 B
// stride) to a 16-byte boundary, exactly as before.
struct ClassifyOutput {
    args: array<ClassifyIndirectArgs, {{ bucket_count }}u>,
    offsets: array<u32, {{ bucket_count }}u>,
    bucket_capacity: u32,
{% for pad in pad_words_iter %}
    _pad_align_{{ pad }}: u32,
{% endfor %}
    tiles: array<vec2<u32>>,
};

@group(0) @binding(3) var<storage, read_write> classify_output: ClassifyOutput;

{% if emit_edge_data %}
// ─────────────────────────────────────────────────────────────────
// Priority-3 MSAA edge-resolve emission buffers. Split across TWO
// GpuBuffers so the dispatch-indirect args don't collide with the
// storage-writable data inside one compute pass's sync scope (WebGPU
// rejects Indirect|Storage(RW) on the same buffer in one pass).
//
// `edge_buffers` (binding 4) — args_buffer:
//   - edge_count: atomic<u32>              — bytes [0, 4)
//   - edge_overflow_count: atomic<u32>     — bytes [4, 8)
//   - 8 bytes alignment pad
//   - final_blend_args: ClassifyIndirectArgs — bytes [16, 32)
//   - skybox_edge_args: ClassifyIndirectArgs — bytes [32, 48)
//   - per_shader_id_args: array<ClassifyIndirectArgs, bucket_count>
//
// `edge_data` (binding 6) — data_buffer (offsets in u32 strides
// supplied via the EdgeBufferLayout uniform; all relative to the
// start of `edge_data` — no header to skip):
//   - edge_to_xy:    array<u32, MAX_EDGE_BUDGET>        (packed x:16, y:16)
//   - edge_slot_map: array<u32, MAX_EDGE_BUDGET>        (4 shader_ids × 8 bits)
//   - accumulator:   array<vec4<f32>, MAX_EDGE_BUDGET × 4>
//   - per-bucket sample entries: array<u32>             (packed id:24, mask:8)
//   - skybox sample entries:     array<u32>
//
// The classify shader writes the counters (args_buffer) + edge_to_xy
// + edge_slot_map + per-bucket sample lists (data_buffer). The
// per-shader edge_resolve pipelines read the sample lists and write
// the accumulator slots (data_buffer); final_blend reads the
// accumulator + edge_to_xy (data_buffer) and writes opaque_tex.
struct EdgeIndirectArgs {
    workgroup_count_x: atomic<u32>,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

// args_buffer-shaped struct: atomic counters + per-args slots only.
// `per_shader_edge_args` is `array<EdgeIndirectArgs, bucket_count>`,
// indexed by bucket index (§4c). Byte-identical to the old back-to-back
// `<name>_edge` named fields (EdgeIndirectArgs is 16 B / stride 16,
// starting at byte 48 after the 3 fixed fields), so the indirect-dispatch
// offsets the host computes (`48 + bucket_index*16`) are unchanged.
struct EdgeArgsBuffer {
    edge_count: atomic<u32>,
    edge_overflow_count: atomic<u32>,
    _pad_counters: vec2<u32>,
    final_blend_args: EdgeIndirectArgs,
    skybox_edge_args: EdgeIndirectArgs,
    per_shader_edge_args: array<EdgeIndirectArgs, {{ bucket_count }}u>,
};

@group(0) @binding(4) var<storage, read_write> edge_buffers: EdgeArgsBuffer;

// Host-uploaded constants for offsetting into `edge_data`. All values
// in u32-stride units (storage-buffer arrays index by element size, so
// 4-byte u32s naturally line up). The first few entries of `edge_data`
// are atomic counter mirrors (so the post-classify resolve shaders can
// read them through their `edge_data` binding alone — args_buffer is
// not bound there, to stay under the 10-storage-buffer cap).
struct EdgeBufferLayout {
    max_edge_budget: u32,
    edge_count_index: u32,
    per_shader_count_base: u32,
    skybox_count_index: u32,
    edge_to_xy_base: u32,
    edge_slot_map_base: u32,
    accumulator_base: u32,
    // Base (u32-stride) of bucket 0's sample list (§4c). Bucket `i`'s list
    // is at `per_shader_sample_list_base + i * sample_entries_per_bucket`
    // (contiguous, uniformly sized) — replaces the old per-bucket
    // `<name>_sample_list_base` field array, so this uniform is now a fixed
    // 10-u32 (48 B padded) struct regardless of bucket count.
    per_shader_sample_list_base: u32,
    skybox_sample_list_base: u32,
    sample_entries_per_bucket: u32,
};

@group(0) @binding(5) var<uniform> edge_layout: EdgeBufferLayout;

// data_buffer: counter-mirror header (atomics) + edge_to_xy +
// edge_slot_map + accumulator + sample lists. Declared as a flat
// `array<atomic<u32>>` here so classify can atomicAdd into the header
// counters; the edge_resolve / skybox / final_blend shaders bind the
// same buffer as a plain `array<u32>` for read-only access (same
// memory, different view — WebGPU's allowed since the two layouts
// live on different pipelines).
//
// The non-atomic regions (edge_to_xy, accumulator, sample lists) are
// written via `atomicStore` to satisfy the storage type — equivalent
// to a plain store for our purposes.
@group(0) @binding(6) var<storage, read_write> edge_data: array<atomic<u32>>;

// Multisampled depth texture — used to detect mesh-vs-mesh in-pixel
// silhouettes via per-sample depth variance. Mirrors main's
// `edge_mask_depth_msaa` check: if ≥2 covered samples in this pixel
// have meaningfully-different depths, the pixel straddles two
// surfaces (even if mat_meta gets broadcast across samples by Tint).
@group(0) @binding(7) var depth_tex: texture_depth_multisampled_2d;

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

// Camera uniform — needed by viewSpaceDepth() to linearize raw
// depth-buffer values before comparing them with main's relative
// threshold (EDGE_DEPTH_THRESHOLD = 0.02). Without view-space the
// non-linear depth distribution causes the threshold to fire
// indiscriminately near the far plane and not at all near the near
// plane.
@group(0) @binding(8) var<uniform> camera_raw: CameraRaw;

// Normal-tangent texture (per-sample, MSAA). Mirrors main's
// `edge_mask_neighbors` normal-discontinuity check: at platform-tile
// boundaries where adjacent facets share the same depth + coverage +
// mat_meta but different surface orientation, the center vs neighbor
// normals diverge by > ~18° (dot < 0.95) and main detects this as an
// edge. Without it, branch's classify misses the platform's
// top-front-edge tile-grout silhouettes and renders them with
// sample-0-only shading → ~50 pixels of visible aliasing along the
// diagonal slope of the platform top in the canonical MorphStressTest
// view.
@group(0) @binding(9) var normal_tangent_tex: texture_multisampled_2d<f32>;
{% endif %}

// bucket_lut (§4a): `shader_id → bucket_index`, O(1) per-pixel/per-sample
// map replacing the old `shader_id == SHADER_ID_*` if/else chain. Bound
// LAST so the binding index is 4 in the singlesampled variant and 10 in
// the MSAA variant (after the 6 edge bindings) — kept in lockstep with
// `bind_group.rs` (which appends it last in both layout + recreate).
// `bucket_lut[raw_sid]` returns the bucket index, or 0xFFFFFFFF (no live
// bucket — the data-driven form of the old "no arm matched" fall-through).
{% if emit_edge_data %}
@group(0) @binding(10) var<storage, read> bucket_lut: array<u32>;
{% else %}
@group(0) @binding(4) var<storage, read> bucket_lut: array<u32>;
{% endif %}
