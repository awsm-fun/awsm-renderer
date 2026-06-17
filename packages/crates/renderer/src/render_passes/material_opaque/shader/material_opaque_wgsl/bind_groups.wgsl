// `instance_attrs` (binding 20) uses `InstanceAttr`; declare the struct here
// so the binding's type is in scope at parse time.
{% include "shared_wgsl/instance_attrs.wgsl" %}

{% if multisampled_geometry %}
    @group(0) @binding(0) var visibility_data_tex: texture_multisampled_2d<u32>;
    // Barycentric tex packs: RG = bary.xy as u16 fixed-point, BA = instance_id (split u32).
    @group(0) @binding(1) var barycentric_tex: texture_multisampled_2d<u32>;
    @group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;
    @group(0) @binding(3) var normal_tangent_tex: texture_multisampled_2d<f32>;
    @group(0) @binding(4) var barycentric_derivatives_tex: texture_multisampled_2d<f32>;
{% else %}
    @group(0) @binding(0) var visibility_data_tex: texture_2d<u32>;
    @group(0) @binding(1) var barycentric_tex: texture_2d<u32>;
    @group(0) @binding(2) var depth_tex: texture_depth_2d;
    @group(0) @binding(3) var normal_tangent_tex: texture_2d<f32>;
    @group(0) @binding(4) var barycentric_derivatives_tex: texture_2d<f32>;
{% endif %}
// `visibility_data` is a view over the merged geometry pool — per-mesh
// sections (visibility, attribute_indices, attribute_data)
// are addressed at the sub-offsets carried by MaterialMeshMeta. The
// declared element type stays `f32` because position/normal reads stay
// natural; u32 reads (attribute indices) come through `bitcast<u32>(…)`.
@group(0) @binding(5) var<storage, read> visibility_data: array<f32>;
@group(0) @binding(6) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;
@group(0) @binding(7) var<storage, read> materials: array<u32>;
// Packed transform (Option E): each entry is model (mat4x4) + normal
// matrix (mat3x3 with vec3-column padding). The shader reads both
// from the same array; `Transforms::BYTE_SIZE` = 112 = stride.
struct TransformPacked {
    model_world: mat4x4<f32>,
    normal_world: mat3x3<f32>,
};
@group(0) @binding(8) var<storage, read> transforms: array<TransformPacked>;
@group(0) @binding(9) var<storage, read> texture_transforms: array<TextureTransform>;
@group(0) @binding(10) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(11) var skybox_tex: texture_cube<f32>;
@group(0) @binding(12) var skybox_sampler: sampler;
@group(0) @binding(13) var ibl_filtered_env_tex: texture_cube<f32>;
@group(0) @binding(14) var ibl_filtered_env_sampler: sampler;
@group(0) @binding(15) var ibl_irradiance_tex: texture_cube<f32>;
@group(0) @binding(16) var ibl_irradiance_sampler: sampler;
@group(0) @binding(17) var brdf_lut_tex: texture_2d<f32>;
@group(0) @binding(18) var brdf_lut_sampler: sampler;
@group(0) @binding(19) var opaque_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(20) var<storage, read> instance_attrs: array<InstanceAttr>;

// Material classify output (read-only here — the read-write atomic
// view is bound on the classify pass). Layout matches
// `ClassifyOutput` in `material_classify_wgsl/bind_groups.wgsl`; the
// indirect-args header is consumed by `dispatchWorkgroupsIndirect`
// host-side. The shader only reads `*_offset` + `tiles[…]` to map
// `workgroup_id.x` back to a tile's `(x, y)` coords.
// Read-only view of the classify-pass output. Layout MUST match the
// classify-pass writer's `ClassifyOutput` struct byte-for-byte —
// both are templated from the same `bucket_entries`.
// Data-driven layout (matches the classify writer's `ClassifyOutput`, §4b):
// `args`/`offsets` arrays indexed by bucket index, byte-identical to the old 2N
// named per-bucket fields but O(1) struct text (the O(N²) fix — a 1024-bucket
// scene no longer embeds 2048 field decls in every shader). Reads only
// `offsets[bucket_index]`; `args` is host-consumed (indirect dispatch), layout-only.
struct ClassifyBuckets {
    args: array<vec4<u32>, {{ bucket_entries.len() }}u>,
    offsets: array<u32, {{ bucket_entries.len() }}u>,
    bucket_capacity: u32,
{% for pad in pad_words_iter %}
    _pad_align_{{ pad }}: u32,
{% endfor %}
    tiles: array<vec2<u32>>,
};
@group(0) @binding(21) var<storage, read> classify_buckets: ClassifyBuckets;

// Renderer-wide per-frame uniform — see `shared_wgsl/frame_globals.wgsl`
// for layout. Rides alongside the camera uniform; one upload per frame.
@group(0) @binding(22) var<uniform> frame_globals_raw: FrameGlobalsRaw;

// Renderer-wide variable-length per-material data pool. Backs
// `BufferSlot` declarations on registered dynamic materials. See
// `shared_wgsl/extras.wgsl` for the load helpers and
// `crates/renderer/src/dynamic_materials/extras_pool.rs` for the
// host-side allocator.
@group(0) @binding(23) var<storage, read> extras_pool: array<u32>;

{% if prep_present %}
// Plan B (stage 5a): the shared prep pass materialized interpolated UV
// sets + vertex color into these array textures (layer = set index).
// `cs_opaque` (PRIMARY) reads them via `textureLoad` instead of recomputing
// from the geometry pool — now under MSAA too (the prep textures are full-res
// sample-0). Sampled `texture_2d_array<f32>` (the rg32float / rgba32float
// storage views read back as f32).
@group(0) @binding(24) var prep_uv: texture_2d_array<f32>;
@group(0) @binding(25) var prep_vcolor: texture_2d_array<f32>;
// Plan B (stage 4/5a): the prep pass's per-pixel packed shadow-visibility
// buffer (Rgba8unorm array — 4 slots/texel: slot j -> layer j/4, channel j%4).
// `cs_opaque` (PRIMARY) reads it via `prep_shadow_read` instead of sampling
// shadow maps inline. Declared whenever the prep bind group is present
// (binding 26, ANY AA); only READ on the PRIMARY mode path in apply_lighting.
@group(0) @binding(26) var prep_shadow_visibility: texture_2d_array<f32>;
{% if multisampled_geometry %}
// Plan B (stage 5b-shadow): the compact per-edge-sample shadow buffer
// `cs_prep_edge` fills. `cs_edge` (EDGE mode) reads it via `prep_shadow_read`
// instead of inline-sampling shadow maps — which is what lets the inline
// `sample_shadow_*` block (~50 KB) drop from the MSAA opaque module. Declared on
// the shared group(0) so BOTH entry points (cs_opaque PRIMARY + cs_edge EDGE)
// see it; only cs_edge actually reads it. A TEXTURE (not a storage buffer) so it
// doesn't count against cs_edge's 10-storage-buffer cap. Binding 27, gated
// prep_present + MSAA.
@group(0) @binding(27) var prep_edge_shadow: texture_2d_array<f32>;
{% endif %}
{% endif %}

@group(1) @binding(0) var<uniform> lights_info: LightsInfoPacked;
// `lights` is a uniform array.
// Uniform memory is constant-cached for the lockstep per-pixel walk;
// the hard cap (64 KB / 64 B) is `MAX_PUNCTUAL_LIGHTS` = 1024 lights.
// `MAX_PUNCTUAL_LIGHTS` is the Rust-side constant; the WGSL array
// length must match it exactly for binding-size validation.
@group(1) @binding(1) var<uniform> lights: array<LightPacked, 1024>;
// `lights_storage`: the GPU cull pass's per-froxel light-slice u32
// array, consumed via the per-pixel `apply_lighting_per_froxel*`
// helpers. The head region `[0..cull_params.mesh_indices_capacity_u32)`
// is reserved but unwritten since the per-mesh lighting path was
// removed; the froxel tail starts after it (the offset keeps the
// froxel base in lockstep with what the cull pass writes).
@group(1) @binding(2) var<storage, read> lights_storage: array<u32>;
// `cull_params`: per-frame uniform written by the cull pass. The
// per-pixel froxel index calc reads `tiles_x/y`, `viewport_w/h`,
// `z_near/z_far`, `log_far_over_near`, and `mesh_indices_capacity_u32`
// (the head→tail boundary in `lights_storage`).
//
// The struct decl is duplicated from the cull pass's
// `light_culling_wgsl/bind_groups.wgsl`; both must stay byte-aligned.
struct CullParams {
    tiles_x: u32,
    tiles_y: u32,
    viewport_w: u32,
    viewport_h: u32,
    mesh_indices_capacity_u32: u32,
    max_per_froxel_capacity: u32,
    // Cull-pass-internal (Stage-A tile candidate budget); declared here
    // only to keep this duplicated struct byte-aligned — consumers don't
    // read it.
    tile_light_capacity: u32,
    z_near: f32,
    z_far: f32,
    log_far_over_near: f32,
    debug_light_heatmap: u32,           // 0 = normal; 1 = applied-light-count heatmap
    debug_view_mode: u32,               // 0 = normal lit; 1 = unlit/flat (base color only)
    debug_wireframe: u32,               // 0 = off; 1 = triangle-edge overlay
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
};
@group(1) @binding(3) var<uniform> cull_params: CullParams;

{% for i in 0..texture_pool_arrays_len %}
    @group(2) @binding({{ i }}u) var pool_tex_{{ i }}: texture_2d_array<f32>;
{% endfor %}
{% for i in 0..texture_pool_samplers_len %}
    @group(2) @binding({{ texture_pool_arrays_len + i }}u) var pool_sampler_{{ i }}: sampler;
{% endfor %}

// === Shadow bind group (group 3) ===
{% include "shared_wgsl/shadow/bind_groups.wgsl" %}

// ─────────────────────────────────────────────────────────────────
// Group(3) extension for the UNIFIED `cs_edge` entry point (§ Part B —
// the "1024 fix"). Under MSAA this module exposes BOTH `cs_opaque`
// (the material kernel) and `cs_edge` (the per-shader MSAA edge
// resolve) so a material compiles ONE shader module instead of two.
//
// `cs_edge` needs the edge-resolve data buffer (read-write storage) +
// the edge-layout uniform, appended to the shadow group at bindings
// 10/11 exactly as the standalone `edge_resolve_bind_groups.wgsl` did
// (so its binding ABI is byte-identical). `cs_opaque` never references
// these, so the opaque pipeline layout (shadows only) stays valid —
// WebGPU validates the bind-group layout per entry point.
//
// Gated on `multisampled_geometry`: there are no edges without MSAA,
// so the singlesampled module carries no `cs_edge` and no edge
// bindings.
{% if multisampled_geometry %}
// data_buffer: small counter-mirror header + edge_to_xy + edge_slot_map
// + accumulator + sample lists, indexed via the EdgeBufferLayout
// uniform's u32-stride offsets.
@group({{ shadow_group_index }}) @binding(10) var<storage, read_write> edge_data: array<u32>;

struct EdgeBufferLayout {
    max_edge_budget: u32,
    // u32-stride indices into `edge_data` of the atomic-counter
    // mirrors classify writes during edge emission.
    edge_count_index: u32,
    per_shader_count_base: u32,
    skybox_count_index: u32,
    edge_to_xy_base: u32,
    edge_slot_map_base: u32,
    accumulator_base: u32,
    // Base of bucket 0's sample list; bucket `i` at
    // `per_shader_sample_list_base + i * sample_entries_per_bucket` (§4c).
    per_shader_sample_list_base: u32,
    skybox_sample_list_base: u32,
    sample_entries_per_bucket: u32,
};

@group({{ shadow_group_index }}) @binding(11) var<uniform> edge_layout: EdgeBufferLayout;

{% if unified_edge %}
// Unified-edge (U1): the per-pixel edge-id texture classify writes (the
// compact `edge_pixel_id` at edge pixels, `U32_MAX` sentinel at non-edge
// in-bounds pixels). ONLY the `cs_shade` entry point reads it — it branches
// interior-vs-edge on the sentinel and uses the compact id as the
// accumulator base. `cs_opaque`/`cs_edge` never reference it, so their
// pipeline layouts (which omit binding 12) stay valid; WebGPU validates
// bind-group layouts per entry point. Read-only storage texture (binding 12,
// appended LAST so it never perturbs the edge_data/edge_layout indices).
@group({{ shadow_group_index }}) @binding(12) var edge_id_tex: texture_storage_2d<r32uint, read>;
{% endif %}
{% endif %}
