// Custom-vertex shadow-generation vertex shader (depth-only).
//
// Byte-for-byte the plain shadow vertex chain (`shadow_wgsl/vertex.wgsl`), with
// the `custom_displace_vertex` hook ALWAYS compiled — so a custom-vertex
// material's shadow is displaced IDENTICALLY to its geometry (same hook, same
// VertexDisplaceInput field values, same uv0 zero, same material_data_load
// offset, same instance_id, same frame_globals). The bind groups are the
// augmented custom-vertex shadow group 0 (materials + frame_globals_raw +
// texture pool, VERTEX-visible) reused from the masked-shadow bind group at draw
// time.
//
// Depth-only: there is no fragment stage (the plain caster path). A custom-vertex
// caster that is ALSO alpha-masked still casts via this pipeline today — its
// shadow is displaced but NOT cut out (rectangular hole) until the masked-custom-
// vertex variant lands; never worse than the pre-custom-vertex behavior.

{% include "shared_wgsl/vertex/geometry_mesh_meta.wgsl" %}
{% include "shadow_custom_vertex_wgsl/bind_groups.wgsl" %}
{% include "shared_wgsl/vertex/transform.wgsl" %}

// Slim ApplyVertexInput stub so the morph/skin helpers compile unchanged.
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

// Hook decls — always present (this is the custom-vertex variant). `frame_globals`
// (struct + `frame_globals_from_raw`) comes from the bind_groups include above,
// so it is NOT re-included here (avoids a redefinition).
{{ dynamic_vertex_struct_decl|safe }}
{{ dynamic_vertex_loader_decl|safe }}
{% include "shared_wgsl/vertex/custom_vertex.wgsl" %}

{% include "shared_wgsl/vertex/morph.wgsl" %}
{% include "shared_wgsl/vertex/skin.wgsl" %}

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) position: vec3<f32>,
    // Locations 1..=4 are declared because the visibility-geometry vertex buffer
    // layout includes them; only 0 + 5 are read.
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
    // The custom-vertex uv0 attribute (constant vec2(0.0) from the shared zero
    // buffer). Kept LAST so it never collides with instancing locations 6-9 —
    // matches the geometry custom-vertex layout (`@location(10)`).
    @location(10) uv0: vec2<f32>,
};

@vertex
fn vert_main(
    input: VertexInput,
    @builtin(instance_index) instance_index: u32,
) -> @builtin(position) vec4<f32> {
    {% if !instancing_transforms %}
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

    // instance_id resolved exactly as the geometry custom-vertex pass does
    // (`apply_vertex`): INSTANCE_ATTR_NONE (0xFFFFFFFF) when the mesh carries no
    // per-instance attribute base, else base + instance_index. The custom-vertex
    // shadow path is non-instanced today, so this is 0xFFFFFFFF — matching
    // geometry — but the general form keeps it identical if instancing lands.
    let _ia_base = geometry_mesh_meta.instance_attr_base;
    var instance_id: u32;
    if (_ia_base == 0xFFFFFFFFu) {
        instance_id = 0xFFFFFFFFu;
    } else {
        instance_id = _ia_base + instance_index;
    }

    var vertex = av_in;
    if geometry_mesh_meta.morph_geometry_target_len != 0u {
        vertex = apply_position_morphs(vertex);
    }
    // Custom displacement — IDENTICAL to the geometry custom-vertex pass: same
    // hook, same post-morph LOCAL position/normal/tangent, same uv0 (the shared
    // zero buffer → vec2(0.0)), same material_data_load offset, same instance_id,
    // same frame_globals. This is what keeps the shadow silhouette glued to the
    // lit geometry.
    {
        let _disp = custom_displace_vertex(VertexDisplaceInput(
            vertex.position, vertex.normal, vertex.tangent, input.uv0,
            vertex.vertex_index, instance_id,
            material_data_load(geometry_mesh_meta.material_mesh_meta_offset),
            frame_globals_from_raw(frame_globals_raw),
        ));
        vertex.position = _disp.position;
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
    return shadow_view.view_projection * world_pos;
}
