{% include "shared_wgsl/vertex/geometry_mesh_meta.wgsl" %}
{% include "shared_wgsl/vertex/transform.wgsl" %}

// Slim ApplyVertexInput stub so the morph/skin helpers compile unchanged
// (they read `input.position`, `input.vertex_index`, etc).
struct ApplyVertexInput {
    vertex_index: u32,
    position: vec3<f32>,
    normal: vec3<f32>,
    tangent: vec4<f32>,
    {% if instancing_transforms %}
        instance_transform_row_0: vec4<f32>,
        instance_transform_row_1: vec4<f32>,
        instance_transform_row_2: vec4<f32>,
        instance_transform_row_3: vec4<f32>,
    {% endif %}
};

{% include "shared_wgsl/vertex/morph.wgsl" %}
{% include "shared_wgsl/vertex/skin.wgsl" %}

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) triangle_index: u32,
    @location(2) barycentric: vec2<f32>,
    @location(3) normal: vec3<f32>,
    @location(4) tangent: vec4<f32>,
    @location(5) original_vertex_index: u32,
    {% if instancing_transforms %}
    @location(6) instance_transform_row_0: vec4<f32>,
    @location(7) instance_transform_row_1: vec4<f32>,
    @location(8) instance_transform_row_2: vec4<f32>,
    @location(9) instance_transform_row_3: vec4<f32>,
    {% endif %}
};

// Forwards the cutout inputs the masked fragment needs: the triangle index +
// barycentric (to reconstruct UVs from the merged pool) and the per-mesh
// material-meta byte offset. `var<private> geometry_mesh_meta` is per-stage, so
// the fragment can't reload it — thread it as a flat varying, exactly like the
// masked geometry vertex.
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) @interpolate(flat) triangle_index: u32,
    @location(1) barycentric: vec2<f32>,
    @location(2) @interpolate(flat) material_mesh_meta_offset: u32,
};

@vertex
fn vert_main(
    input: VertexInput,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    {% if !instancing_transforms %}
    // Load per-mesh meta from the storage array indexed by `instance_index`
    // (CPU sets `first_instance = mesh_meta_idx` per shadow draw).
    geometry_mesh_meta = geometry_mesh_metas[instance_index];
    {% endif %}

    var av_in: ApplyVertexInput;
    av_in.vertex_index = input.original_vertex_index;
    av_in.position = input.position;
    av_in.normal = input.normal;
    av_in.tangent = input.tangent;
    {% if instancing_transforms %}
        av_in.instance_transform_row_0 = input.instance_transform_row_0;
        av_in.instance_transform_row_1 = input.instance_transform_row_1;
        av_in.instance_transform_row_2 = input.instance_transform_row_2;
        av_in.instance_transform_row_3 = input.instance_transform_row_3;
    {% endif %}

    var vertex = av_in;
    if geometry_mesh_meta.morph_geometry_target_len != 0u {
        vertex = apply_position_morphs(vertex);
    }
    if geometry_mesh_meta.skin_sets_len != 0u {
        vertex = apply_position_skin(vertex);
    }

    {% if instancing_transforms %}
        let instance_transform = mat4x4<f32>(
            vertex.instance_transform_row_0,
            vertex.instance_transform_row_1,
            vertex.instance_transform_row_2,
            vertex.instance_transform_row_3,
        );
        var model_transform = get_model_transform(geometry_mesh_meta.transform_offset) * instance_transform;
    {% else %}
        var model_transform = get_model_transform(geometry_mesh_meta.transform_offset);
    {% endif %}

    // Skinned meshes are already in world space — drop the base model transform
    // so it isn't applied twice (matches the geometry + plain shadow pass).
    if (geometry_mesh_meta.skin_sets_len != 0u) {
        {% if instancing_transforms %}
            model_transform = instance_transform;
        {% else %}
            model_transform = mat4x4<f32>(
                vec4<f32>(1.0, 0.0, 0.0, 0.0),
                vec4<f32>(0.0, 1.0, 0.0, 0.0),
                vec4<f32>(0.0, 0.0, 1.0, 0.0),
                vec4<f32>(0.0, 0.0, 0.0, 1.0),
            );
        {% endif %}
    }

    let world_pos = model_transform * vec4<f32>(vertex.position, 1.0);

    var out: VertexOutput;
    out.clip_position = shadow_view.view_projection * world_pos;
    out.triangle_index = input.triangle_index;
    out.barycentric = input.barycentric;
    out.material_mesh_meta_offset = geometry_mesh_meta.material_mesh_meta_offset;
    return out;
}
