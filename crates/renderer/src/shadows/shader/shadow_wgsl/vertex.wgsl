{% include "shared_wgsl/vertex/geometry_mesh_meta.wgsl" %}
{% include "shadow_wgsl/bind_groups.wgsl" %}
{% include "shared_wgsl/vertex/transform.wgsl" %}

// Slim ApplyVertexInput stub so the morph/skin helpers compile
// unchanged (they read `input.position`, `input.vertex_index`, etc).
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
    // The shadow VS only reads location 0 and 5; locations 1..=4 must
    // still be declared because the visibility-geometry vertex buffer
    // layout includes them, but they are unused.
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

@vertex
fn vert_main(
    input: VertexInput,
    @builtin(instance_index) instance_index: u32,
) -> @builtin(position) vec4<f32> {
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
        let model_transform = get_model_transform(geometry_mesh_meta.transform_offset) * instance_transform;
    {% else %}
        let model_transform = get_model_transform(geometry_mesh_meta.transform_offset);
    {% endif %}

    let world_pos = model_transform * vec4<f32>(vertex.position, 1.0);
    return shadow_view.view_projection * world_pos;
}
