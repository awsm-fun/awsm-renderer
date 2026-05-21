{% include "shared_wgsl/vertex/geometry_mesh_meta.wgsl" %}
{% include "shared_wgsl/camera.wgsl" %}
{% include "shared_wgsl/vertex/transform.wgsl" %}
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
    // Plan §16.7/§16.8: non-instanced meshes pull `geometry_mesh_meta`
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

    {% if !instancing_transforms %}
    // Plan §16.7/§16.8: load per-mesh meta from the storage array
    // indexed by `instance_index`. The CPU sets
    // `first_instance = mesh_meta_idx` for each draw (legacy
    // `draw_indexed_with_first_instance` or
    // `IndirectDrawArgs.first_instance` via `drawIndirect`), so
    // `instance_index` lands on this mesh's slot for both
    // `features.gpu_culling` paths.
    geometry_mesh_meta = geometry_mesh_metas[instance_index];
    {% endif %}

    let camera = camera_from_raw(camera_raw);

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
    ), camera);

    out.clip_position = applied.clip_position;
    out.world_normal = applied.world_normal;
    out.world_tangent = applied.world_tangent;

    // Pass through
    out.triangle_index = input.triangle_index;
    out.barycentric = input.barycentric;

    // Per-fragment instance_id. The shading compute pass reads this to look
    // up per-instance attributes (color, size, alpha) from a small storage
    // buffer. For non-instanced meshes the writer side stores `u32::MAX` in
    // `geometry_mesh_meta.instance_attr_base`; we propagate that sentinel
    // through so the read site can branch on a single value.
    let base = geometry_mesh_meta.instance_attr_base;
    if (base == 0xFFFFFFFFu) {
        out.instance_id = 0xFFFFFFFFu;
    } else {
        out.instance_id = base + instance_index;
    }

    // Plan §16.7/§16.8: forward the per-mesh material-meta byte
    // offset to the fragment stage so the fragment's
    // visibility_data write resolves to the correct slot. See
    // VertexOutput's docstring for the rationale.
    out.material_mesh_meta_offset = geometry_mesh_meta.material_mesh_meta_offset;

    return out;
}
