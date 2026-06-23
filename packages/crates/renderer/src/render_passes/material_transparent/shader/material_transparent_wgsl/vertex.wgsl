//***** MAIN *****
struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) position: vec3<f32>,      // Model-space position
    @location(1) normal: vec3<f32>,        // Model-space normal
    @location(2) tangent: vec4<f32>,       // Model-space tangent (w = handedness)
    {% if instancing_transforms %}
    // instance transform matrix
    @location(3) instance_transform_row_0: vec4<f32>,
    @location(4) instance_transform_row_1: vec4<f32>,
    @location(5) instance_transform_row_2: vec4<f32>,
    @location(6) instance_transform_row_3: vec4<f32>,
    {% endif %}

    {% for i in 0..color_sets %}
        @location({{ in_color_set_start + i }}) color_{{ i }}: vec4<f32>,
    {% endfor %}

    {% for i in 0..uv_sets %}
        @location({{ in_uv_set_start + i }}) uv_{{ i }}: vec2<f32>,
    {% endfor %}
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,     // Transformed world position
    @location(1) world_normal: vec3<f32>,     // Transformed world-space normal
    @location(2) world_tangent: vec4<f32>,    // Transformed world-space tangent (w = handedness)
    // Per-fragment instance_id, plumbed through for the Stage-3b per-instance
    // tint applied at the end of fs_main. `INSTANCE_ATTR_NONE` (`u32::MAX`)
    // means "non-instanced" → identity tint.
    @location(3) @interpolate(flat) instance_id: u32,

    {% for i in 0..color_sets %}
        @location({{ out_color_set_start + i }}) color_{{ i }}: vec4<f32>,
    {% endfor %}

    {% for i in 0..uv_sets %}
        @location({{ out_uv_set_start + i }}) uv_{{ i }}: vec2<f32>,
    {% endfor %}
}

@vertex
fn vert_main(
    input: VertexInput,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    var out: VertexOutput;

    let camera = camera_from_raw(camera_raw);
    let frame_globals = frame_globals_from_raw(frame_globals_raw);

    // Per-fragment instance_id derived from geometry_mesh_meta.instance_attr_base
    // + the GPU's @builtin(instance_index). Non-instanced meshes carry the
    // sentinel through unchanged so the fragment side branches identically
    // to the opaque path's MSAA helper. Computed before `apply_vertex` so the
    // custom-vertex hook can receive it.
    let base = geometry_mesh_meta.instance_attr_base;
    var instance_id: u32;
    if (base == INSTANCE_ATTR_NONE) {
        instance_id = INSTANCE_ATTR_NONE;
    } else {
        instance_id = base + instance_index;
    }

    let applied = apply_vertex(ApplyVertexInput(
        input.vertex_index,
        input.position,
        input.normal,
        input.tangent,
        {% if instancing_transforms %}
            input.instance_transform_row_0,
            input.instance_transform_row_1,
            input.instance_transform_row_2,
            input.instance_transform_row_3,
        {% endif %}
    ), camera {% if has_custom_vertex %}, {% if uv_sets >= 1 %} input.uv_0 {% else %} vec2<f32>(0.0, 0.0) {% endif %}, instance_id, frame_globals {% endif %});

    out.clip_position = applied.clip_position;
    out.world_position = applied.world_position;
    out.world_normal = applied.world_normal;
    out.world_tangent = applied.world_tangent;

    out.instance_id = instance_id;

    {% for i in 0..color_sets %}
        out.color_{{ i }} = input.color_{{ i }};
    {% endfor %}

    {% for i in 0..uv_sets %}
        out.uv_{{ i }} = input.uv_{{ i }};
    {% endfor %}

    return out;
}
