//***** TAA MOTION: PREVIOUS-FRAME VERTEX EVALUATION *****
//
// Included by the geometry vertex shader in its TAA (velocity) variant only.
// Evaluates the SAME vertex with LAST frame's inputs (03 §2.1, §2.3): world
// matrix, instance transform, morph weights, joint matrices, frame time and,
// for billboards, camera position. The `_prev` functions below are copies of
// the current ones in morph.wgsl / skin.wgsl with only the buffer read
// swapped; `wgsl_validation::taa_prev_functions_mirror_current` keeps them in
// step.
//
// Previous-frame state: ONE storage buffer (the vertex stage of the
// custom-vertex variants is at Chrome-on-Apple's limit of ten storage
// buffers), with three regions of the same layout as the current buffers:
// transforms at 0, joint matrices and morph weights at the bases in the
// frame-globals uniform's spare words (`crate::buffer::history`). Copied
// current -> prev at the end of every frame.
@group(1) @binding(1) var<storage, read> prev_state: array<vec4<f32>>;

// Model matrix of the transform at `byte_offset` (the 112-byte layout of
// `transforms`; the model matrix is its first 64 bytes).
fn get_prev_model_transform(byte_offset: u32) -> mat4x4<f32> {
    let o = byte_offset / 16u;
    return mat4x4<f32>(prev_state[o], prev_state[o + 1u], prev_state[o + 2u], prev_state[o + 3u]);
}

// Joint matrix `index` (same indexing as `skin_joint_matrices`).
fn prev_skin_joint_matrix(index: u32) -> mat4x4<f32> {
    let o = frame_globals_raw._pad2.x + index * 4u;
    return mat4x4<f32>(prev_state[o], prev_state[o + 1u], prev_state[o + 2u], prev_state[o + 3u]);
}

// Morph weight `index` (same indexing as `geometry_morph_weights`).
fn prev_morph_weight(index: u32) -> f32 {
    let v = prev_state[frame_globals_raw._pad2.y + index / 4u];
    return v[index % 4u];
}

fn apply_position_morphs_prev(input: ApplyVertexInput) -> ApplyVertexInput {
    var output = input;

    let target_count = geometry_mesh_meta.morph_geometry_target_len;

    // Each target contributes 10 floats: position (3) + normal (3) + tangent (4)
    let floats_per_position = 3u;
    let floats_per_normal = 3u;
    let floats_per_tangent = 4u;
    let floats_per_target = floats_per_position + floats_per_normal + floats_per_tangent; // 10
    let total_floats_per_vertex = target_count * floats_per_target;

    // Calculate base offset for this vertex's morph data (indexed per original vertex)
    // NOTE: weights buffer format is [target_count, weight0, weight1, weight2, ...]
    // So we add 1 to skip the target_count stored at index 0
    let base_weights_offset = (geometry_mesh_meta.morph_geometry_weights_offset / 4) + 1u;
    let base_values_offset = (geometry_mesh_meta.morph_geometry_values_offset / 4) +  input.vertex_index * total_floats_per_vertex;

    // UNROLLED TARGETS for better performance
    {% for i in 0..max_morph_unroll %}
        if target_count >= {{ i+1 }}u {
            let weight_offset = base_weights_offset + {{ i }}u;
            let weight = prev_morph_weight(weight_offset);
            let value_offset = base_values_offset + ({{ i }}u * floats_per_target);
            // Position is at offset 0
            let morph_delta = vec3<f32>(
                geometry_morph_values[value_offset],
                geometry_morph_values[value_offset + 1u],
                geometry_morph_values[value_offset + 2u]
            );
            output.position += weight * morph_delta;
        }
    {% endfor %}

    // LOOP FOR REMAINING TARGETS
    if target_count > {{ max_morph_unroll }}u {
        for (var target_index = {{ max_morph_unroll }}u; target_index < target_count; target_index = target_index + 1u) {
            let weight_offset = base_weights_offset + target_index;
            let weight = prev_morph_weight(weight_offset);
            let value_offset = base_values_offset + (target_index * floats_per_target);
            // Position is at offset 0
            let morph_delta = vec3<f32>(
                geometry_morph_values[value_offset],
                geometry_morph_values[value_offset + 1u],
                geometry_morph_values[value_offset + 2u]
            );

            output.position += weight * morph_delta;
        }
    }

    return output;
}

{% if has_custom_vertex %}
fn apply_normal_morphs_prev(input: ApplyVertexInput, normal: vec3<f32>) -> vec3<f32> {
    var output = normal;

    let target_count = geometry_mesh_meta.morph_geometry_target_len;

    // Each target contributes 10 floats: position (3) + normal (3) + tangent (4)
    let floats_per_position = 3u;
    let floats_per_normal = 3u;
    let floats_per_tangent = 4u;
    let floats_per_target = floats_per_position + floats_per_normal + floats_per_tangent; // 10
    let total_floats_per_vertex = target_count * floats_per_target;

    // Calculate base offset for this vertex's morph data (indexed per original vertex)
    // NOTE: weights buffer format is [target_count, weight0, weight1, weight2, ...]
    // So we add 1 to skip the target_count stored at index 0
    let base_weights_offset = (geometry_mesh_meta.morph_geometry_weights_offset / 4) + 1u;
    let base_values_offset = (geometry_mesh_meta.morph_geometry_values_offset / 4) + input.vertex_index * total_floats_per_vertex;

    // UNROLLED TARGETS for better performance
    {% for i in 0..max_morph_unroll %}
        if target_count >= {{ i+1 }}u {
            let weight_offset = base_weights_offset + {{ i }}u;
            let weight = prev_morph_weight(weight_offset);
            let value_offset = base_values_offset + ({{ i }}u * floats_per_target);
            // Normal is at offset 3 (after position)
            let morph_delta = vec3<f32>(
                geometry_morph_values[value_offset + floats_per_position],
                geometry_morph_values[value_offset + floats_per_position + 1u],
                geometry_morph_values[value_offset + floats_per_position + 2u]
            );
            output += weight * morph_delta;
        }
    {% endfor %}

    // LOOP FOR REMAINING TARGETS
    if target_count > {{ max_morph_unroll }}u {
        for (var target_index = {{ max_morph_unroll }}u; target_index < target_count; target_index = target_index + 1u) {
            let weight_offset = base_weights_offset + target_index;
            let weight = prev_morph_weight(weight_offset);
            let value_offset = base_values_offset + (target_index * floats_per_target);
            // Normal is at offset 3 (after position)
            let morph_delta = vec3<f32>(
                geometry_morph_values[value_offset + floats_per_position],
                geometry_morph_values[value_offset + floats_per_position + 1u],
                geometry_morph_values[value_offset + floats_per_position + 2u]
            );

            output += weight * morph_delta;
        }
    }

    return output;
}

fn apply_tangent_morphs_prev(input: ApplyVertexInput, tangent: vec4<f32>) -> vec4<f32> {
    // Preserve the original w component (handedness) - morphs only affect xyz
    let original_w = tangent.w;
    var output_xyz = tangent.xyz;

    let target_count = geometry_mesh_meta.morph_geometry_target_len;

    // Each target contributes 10 floats: position (3) + normal (3) + tangent (4)
    // But tangent morphs only use the first 3 floats (xyz), the 4th is padding
    let floats_per_position = 3u;
    let floats_per_normal = 3u;
    let floats_per_tangent = 4u; // 3 used + 1 padding
    let floats_per_target = floats_per_position + floats_per_normal + floats_per_tangent; // 10
    let total_floats_per_vertex = target_count * floats_per_target;

    // Calculate base offset for this vertex's morph data (indexed per original vertex)
    // NOTE: weights buffer format is [target_count, weight0, weight1, weight2, ...]
    // So we add 1 to skip the target_count stored at index 0
    let base_weights_offset = (geometry_mesh_meta.morph_geometry_weights_offset / 4) + 1u;
    let base_values_offset = (geometry_mesh_meta.morph_geometry_values_offset / 4) + input.vertex_index * total_floats_per_vertex;

    // UNROLLED TARGETS for better performance
    {% for i in 0..max_morph_unroll %}
        if target_count >= {{ i+1 }}u {
            let weight_offset = base_weights_offset + {{ i }}u;
            let weight = prev_morph_weight(weight_offset);
            let value_offset = base_values_offset + ({{ i }}u * floats_per_target);
            // Tangent is at offset 6 (after position + normal), only read xyz
            let morph_delta = vec3<f32>(
                geometry_morph_values[value_offset + floats_per_position + floats_per_normal],
                geometry_morph_values[value_offset + floats_per_position + floats_per_normal + 1u],
                geometry_morph_values[value_offset + floats_per_position + floats_per_normal + 2u]
            );
            output_xyz += weight * morph_delta;
        }
    {% endfor %}

    // LOOP FOR REMAINING TARGETS
    if target_count > {{ max_morph_unroll }}u {
        for (var target_index = {{ max_morph_unroll }}u; target_index < target_count; target_index = target_index + 1u) {
            let weight_offset = base_weights_offset + target_index;
            let weight = prev_morph_weight(weight_offset);
            let value_offset = base_values_offset + (target_index * floats_per_target);
            // Tangent is at offset 6 (after position + normal), only read xyz
            let morph_delta = vec3<f32>(
                geometry_morph_values[value_offset + floats_per_position + floats_per_normal],
                geometry_morph_values[value_offset + floats_per_position + floats_per_normal + 1u],
                geometry_morph_values[value_offset + floats_per_position + floats_per_normal + 2u]
            );

            output_xyz += weight * morph_delta;
        }
    }

    // Return tangent with morphed xyz but original w (handedness)
    return vec4<f32>(output_xyz, original_w);
}
{% endif %}

fn apply_position_skin_prev(input: ApplyVertexInput) -> ApplyVertexInput {
    var output = input;

    let skin_sets_count = geometry_mesh_meta.skin_sets_len;

    let original_position = vec4<f32>(input.position, 1.0);

    // Calculate base offset for this vertex's skin data (indexed per original vertex)
    let base_offset = (geometry_mesh_meta.skin_index_weights_offset / 4) + input.vertex_index * skin_sets_count * floats_per_set;

    let matrix_offset = geometry_mesh_meta.skin_matrices_offset / 64; // mat4x4<f32> is 64 bytes

    var skin_matrix: mat4x4<f32>;

    // UNROLLED SKIN SETS

    {% for i in 0..max_skin_unroll %}
        if skin_sets_count >= {{ i + 1}}u {
            let buffer_offset = base_offset + ({{ i }}u * floats_per_set);

            let joint_index_0 = bitcast<u32>(skin_joint_index_weights[buffer_offset]);
            let joint_weight_0 = skin_joint_index_weights[buffer_offset + 1u];

            let joint_index_1 = bitcast<u32>(skin_joint_index_weights[buffer_offset + 2u]);
            let joint_weight_1 = skin_joint_index_weights[buffer_offset + 3u];

            let joint_index_2 = bitcast<u32>(skin_joint_index_weights[buffer_offset + 4u]);
            let joint_weight_2 = skin_joint_index_weights[buffer_offset + 5u];

            let joint_index_3 = bitcast<u32>(skin_joint_index_weights[buffer_offset + 6u]);
            let joint_weight_3 = skin_joint_index_weights[buffer_offset + 7u];

            let skin_mat_acc = joint_weight_0 * prev_skin_joint_matrix(joint_index_0 + matrix_offset)
                            + joint_weight_1 * prev_skin_joint_matrix(joint_index_1 + matrix_offset)
                            + joint_weight_2 * prev_skin_joint_matrix(joint_index_2 + matrix_offset)
                            + joint_weight_3 * prev_skin_joint_matrix(joint_index_3 + matrix_offset);

            {% if i == 0 %}
                skin_matrix = skin_mat_acc;
            {% else %}
                skin_matrix = skin_matrix + skin_mat_acc;
            {% endif %}
        }
    {% endfor %}

    // LOOP FOR REMAINING SKIN SETS
    if skin_sets_count > {{ max_skin_unroll }}u {
        for (var skin_set_index = {{ max_skin_unroll }}u; skin_set_index < skin_sets_count; skin_set_index = skin_set_index + 1u) {
            let buffer_offset = base_offset + (skin_set_index * floats_per_set);

            let joint_index_0 = bitcast<u32>(skin_joint_index_weights[buffer_offset]);
            let joint_weight_0 = skin_joint_index_weights[buffer_offset + 1u];

            let joint_index_1 = bitcast<u32>(skin_joint_index_weights[buffer_offset + 2u]);
            let joint_weight_1 = skin_joint_index_weights[buffer_offset + 3u];

            let joint_index_2 = bitcast<u32>(skin_joint_index_weights[buffer_offset + 4u]);
            let joint_weight_2 = skin_joint_index_weights[buffer_offset + 5u];

            let joint_index_3 = bitcast<u32>(skin_joint_index_weights[buffer_offset + 6u]);
            let joint_weight_3 = skin_joint_index_weights[buffer_offset + 7u];

            let skin_mat_acc = joint_weight_0 * prev_skin_joint_matrix(joint_index_0 + matrix_offset)
                             + joint_weight_1 * prev_skin_joint_matrix(joint_index_1 + matrix_offset)
                             + joint_weight_2 * prev_skin_joint_matrix(joint_index_2 + matrix_offset)
                             + joint_weight_3 * prev_skin_joint_matrix(joint_index_3 + matrix_offset);

            skin_matrix = skin_matrix + skin_mat_acc;
        }
    }

    output.position = (skin_matrix * original_position).xyz;

    return output;
}

// World position of this vertex as of last frame. `vertex_orig` carries the
// PREVIOUS instance transform rows (when instanced). Mirrors `apply_vertex`
// for position only: morph -> custom hook -> skin -> model / instance ->
// billboard (facing last frame's camera position).
fn apply_vertex_prev(vertex_orig: ApplyVertexInput, camera: Camera {% if has_custom_vertex %}, custom_uv: array<vec2<f32>, 4>, custom_uv_count: u32, custom_instance_id: u32, custom_globals: FrameGlobals, custom_material_offset: u32 {% endif %}) -> vec3<f32> {
    var vertex = vertex_orig;
    {% if has_custom_vertex %}
    var normal = vertex_orig.normal;
    var tangent = vertex_orig.tangent;
    {% endif %}

    if geometry_mesh_meta.morph_geometry_target_len != 0 {
        vertex = apply_position_morphs_prev(vertex);
        {% if has_custom_vertex %}
        normal = apply_normal_morphs_prev(vertex_orig, normal);
        tangent = apply_tangent_morphs_prev(vertex_orig, tangent);
        {% endif %}
    }

    {% if has_custom_vertex %}
    {
        let _disp = custom_displace_vertex(VertexDisplaceInput(
            vertex.position, normal, tangent, custom_uv, custom_uv_count,
            vertex.vertex_index, custom_instance_id,
            material_data_load(custom_material_offset),
            custom_globals,
        ));
        vertex.position = _disp.position;
    }
    {% endif %}

    if geometry_mesh_meta.skin_sets_len != 0 {
        vertex = apply_position_skin_prev(vertex);
    }

    {% if instancing_transforms %}
        let instance_transform = mat4x4<f32>(
            vertex.instance_transform_row_0,
            vertex.instance_transform_row_1,
            vertex.instance_transform_row_2,
            vertex.instance_transform_row_3,
        );
        var model_transform = get_prev_model_transform(geometry_mesh_meta.transform_offset) * instance_transform;
    {% else %}
        var model_transform = get_prev_model_transform(geometry_mesh_meta.transform_offset);
    {% endif %}

    // Skinned: the joint matrices are already world space (see apply_vertex).
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

    // Billboards faced LAST frame's camera position last frame.
    let billboard_mode = geometry_mesh_meta.billboard_mode;
    if (billboard_mode != 0u) {
        let translation = model_transform[3].xyz;
        let scale_x = length(model_transform[0].xyz);
        let scale_y = length(model_transform[1].xyz);
        let scale_z = length(model_transform[2].xyz);

        let to_cam = camera.prev_position - translation;
        var forward: vec3<f32>;
        if (billboard_mode == 1u) {
            var xz = vec3<f32>(to_cam.x, 0.0, to_cam.z);
            let len_sq = dot(xz, xz);
            if (len_sq < 1e-8) {
                xz = vec3<f32>(0.0, 0.0, 1.0);
            } else {
                xz = xz * inverseSqrt(len_sq);
            }
            forward = xz;
        } else {
            let len_sq = dot(to_cam, to_cam);
            if (len_sq < 1e-8) {
                forward = vec3<f32>(0.0, 0.0, 1.0);
            } else {
                forward = to_cam * inverseSqrt(len_sq);
            }
        }

        let world_up = vec3<f32>(0.0, 1.0, 0.0);
        var right_unnorm = cross(world_up, forward);
        let right_len_sq = dot(right_unnorm, right_unnorm);
        var right: vec3<f32>;
        var up: vec3<f32>;
        if (right_len_sq < 1e-8) {
            right = vec3<f32>(1.0, 0.0, 0.0);
            up = cross(forward, right);
        } else {
            right = right_unnorm * inverseSqrt(right_len_sq);
            up = cross(forward, right);
        }

        model_transform = mat4x4<f32>(
            vec4<f32>(right * scale_x, 0.0),
            vec4<f32>(up * scale_y, 0.0),
            vec4<f32>(forward * scale_z, 0.0),
            vec4<f32>(translation, 1.0),
        );
    }

    return (model_transform * vec4<f32>(vertex.position, 1.0)).xyz;
}
