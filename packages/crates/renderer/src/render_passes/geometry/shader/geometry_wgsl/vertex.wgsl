{% include "shared_wgsl/vertex/geometry_mesh_meta.wgsl" %}
{% include "shared_wgsl/camera.wgsl" %}
{% include "shared_wgsl/frame_globals.wgsl" %}
{% include "shared_wgsl/vertex/transform.wgsl" %}
{% if has_custom_vertex %}
{{ dynamic_vertex_struct_decl|safe }}
{{ dynamic_vertex_loader_decl|safe }}
{% include "shared_wgsl/vertex/custom_vertex.wgsl" %}
{% endif %}
{% include "shared_wgsl/vertex/morph.wgsl" %}
{% include "shared_wgsl/vertex/skin.wgsl" %}
{% include "shared_wgsl/vertex/apply_vertex.wgsl" %}


//***** MAIN *****
struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) position: vec3<f32>,      // Model-space position
    @location(1) triangle_index: u32,      // Triangle index for this vertex
    @location(2) barycentric: vec2<f32>,   // Barycentric coordinates (x, y) - z = 1.0 - x - y
    @location(3) normal: vec3<f32>,        // Model-space normal
    @location(4) tangent: vec4<f32>,       // Model-space tangent (w = handedness)
    @location(5) original_vertex_index: u32, // Original vertex index (for indexed skin/morph access)
    {% if instancing_transforms %}
    // instance transform matrix
    @location(6) instance_transform_row_0: vec4<f32>,
    @location(7) instance_transform_row_1: vec4<f32>,
    @location(8) instance_transform_row_2: vec4<f32>,
    @location(9) instance_transform_row_3: vec4<f32>,
    {% endif %}
    {% if has_custom_vertex %} @location(10) uv0: vec2<f32>, {% endif %}
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) @interpolate(flat) triangle_index: u32,
    @location(1) barycentric: vec2<f32>,  // Full barycentric coordinates
    @location(2) world_normal: vec3<f32>,     // Transformed world-space normal
    @location(3) world_tangent: vec4<f32>,    // Transformed world-space tangent (w = handedness)
    // Stage-1 leaves this at U32_MAX always; Stage-2 wires
    // `geometry_mesh_meta.instance_attr_base + @builtin(instance_index)`.
    @location(4) @interpolate(flat) instance_id: u32,
    // Non-instanced meshes pull `geometry_mesh_meta`
    // from a storage-array binding into a `var<private>` at vertex
    // entry. `var<private>` is per-shader-stage, so the fragment
    // shader's copy is uninitialised — passing the material-meta
    // byte offset as a flat varying gives the fragment access to
    // the right slot's value without re-loading it (which the
    // fragment can't easily do; it doesn't have `instance_index`).
    // For the instanced path the value comes from the uniform
    // binding directly — but since the field is identical across
    // stages there either way, threading it as a varying is the
    // cheaper / more uniform fix.
    @location(5) @interpolate(flat) material_mesh_meta_offset: u32,
}

@vertex
fn vert_main(
    input: VertexInput,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    var out: VertexOutput;

    {% if meta_storage_array %}
    // Load per-mesh meta from the storage array indexed by
    // `instance_index`. The compaction shader (or CPU
    // draw_indexed_with_first_instance) sets `first_instance =
    // mesh_meta_idx` so `instance_index` lands on this mesh's slot.
    // Only reachable when the device exposes the
    // `indirect-first-instance` WebGPU feature; the portable
    // fallback (uniform-with-dynamic-offset) leaves
    // `geometry_mesh_meta` populated by the bind-group dynamic
    // offset and skips this load.
    geometry_mesh_meta = geometry_mesh_metas[instance_index];
    {% endif %}

    let camera = camera_from_raw(camera_raw);
    let frame_globals = frame_globals_from_raw(frame_globals_raw);

    // Per-fragment instance_id. The shading compute pass reads this to look
    // up per-instance attributes (color, size, alpha) from a small storage
    // buffer. For non-instanced meshes the writer side stores `u32::MAX` in
    // `geometry_mesh_meta.instance_attr_base`; we propagate that sentinel
    // through so the read site can branch on a single value. Computed before
    // `apply_vertex` so the custom-vertex hook can receive it.
    let base = geometry_mesh_meta.instance_attr_base;
    var instance_id: u32;
    if (base == 0xFFFFFFFFu) {
        instance_id = 0xFFFFFFFFu;
    } else {
        instance_id = base + instance_index;
    }

    {% if has_custom_vertex %}
    // Build the per-vertex UV array (ALL of the mesh's UV sets) for the
    // custom-vertex hook — parity with the fragment side's multi-UV access.
    // Mirror the masked fragment's `material_mesh_metas` indexing EXACTLY
    // (`material_mesh_meta_offset / META_SIZE_IN_BYTES`; byte→float /4u on the
    // stride + data offset; `uv_sets_index` is already a float offset). Reuses
    // the shared `_mask_uv_per_vertex` reader over `visibility_data` — no new
    // vertex buffers / uploads. The shadow pass builds this IDENTICALLY so the
    // displaced silhouette matches.
    let _mm = material_mesh_metas[geometry_mesh_meta.material_mesh_meta_offset / META_SIZE_IN_BYTES];
    let _cv_stride = _mm.vertex_attribute_stride / 4u;
    let _cv_data_offset = _mm.vertex_attribute_data_offset / 4u;
    let _cv_uv_count = min(_mm.uv_set_count, 4u);
    var _cv_uv: array<vec2<f32>, 4>;
    for (var _i = 0u; _i < 4u; _i = _i + 1u) {
        _cv_uv[_i] = select(
            vec2<f32>(0.0, 0.0),
            _mask_uv_per_vertex(_cv_data_offset, _i, input.original_vertex_index, _cv_stride, _mm.uv_sets_index),
            _i < _cv_uv_count,
        );
    }
    {% endif %}

    let applied = apply_vertex(ApplyVertexInput(
        input.original_vertex_index,
        input.position,
        input.normal,
        input.tangent,
        {% if instancing_transforms %}
            input.instance_transform_row_0,
            input.instance_transform_row_1,
            input.instance_transform_row_2,
            input.instance_transform_row_3,
        {% endif %}
    ), camera {% if has_custom_vertex %}, _cv_uv, _cv_uv_count, instance_id, frame_globals {% endif %});

    out.clip_position = applied.clip_position;
    out.world_normal = applied.world_normal;
    out.world_tangent = applied.world_tangent;

    // Pass through
    out.triangle_index = input.triangle_index;
    out.barycentric = input.barycentric;

    out.instance_id = instance_id;

    // Forward the per-mesh material-meta byte
    // offset to the fragment stage so the fragment's
    // visibility_data write resolves to the correct slot. See
    // VertexOutput's docstring for the rationale.
    out.material_mesh_meta_offset = geometry_mesh_meta.material_mesh_meta_offset;

    return out;
}
