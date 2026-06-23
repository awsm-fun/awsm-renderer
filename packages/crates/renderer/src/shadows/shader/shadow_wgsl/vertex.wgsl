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

{% if has_custom_vertex %}
{{ dynamic_vertex_struct_decl|safe }}
{{ dynamic_vertex_loader_decl|safe }}
{% include "shared_wgsl/frame_globals.wgsl" %}
{% include "shared_wgsl/vertex/custom_vertex.wgsl" %}
{% endif %}

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
    {% if !instancing_transforms %}
    // Load per-mesh meta from the storage array indexed by
    // `instance_index`. Mirrors the geometry pass's non-instanced
    // lookup; the CPU sets `first_instance = mesh_meta_idx` per
    // shadow draw.
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
    {% if has_custom_vertex %}
    {
        // The plain shadow path never compiles the hook today (`has_custom_vertex`
        // is hardwired off — custom-vertex casters use the dedicated
        // `shadow_custom_vertex` / masked-shadow templates which bind
        // `material_mesh_metas` + `visibility_data`). Pass a zero UV array so this
        // stays compilable if ever flipped on.
        var _cv_uv: array<vec2<f32>, 4>;
        let _disp = custom_displace_vertex(VertexDisplaceInput(
            vertex.position, vertex.normal, vertex.tangent, _cv_uv, 0u,
            vertex.vertex_index, 0u,
            material_data_load(geometry_mesh_meta.material_mesh_meta_offset),
            frame_globals_from_raw(frame_globals_raw),
        ));
        vertex.position = _disp.position;
    }
    {% endif %}
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

    // Skinned meshes are already in world space (the joint matrices fold in
    // every ancestor transform, incl. the Z-up→Y-up root). Drop the base model
    // transform so it isn't applied twice — matches the geometry pass. See the
    // note in shared_wgsl/vertex/apply_vertex.wgsl.
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
    return shadow_view.view_projection * world_pos;
}

