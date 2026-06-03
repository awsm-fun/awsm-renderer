//***** SKINS *****

// Each skin set has 8 f32 values (4 interleaved index/weight pairs)
const floats_per_set = 8u;

/// Applies skeletal skinning with support for multiple skin sets per vertex
fn apply_position_skin(input: ApplyVertexInput) -> ApplyVertexInput {
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

            let skin_mat_acc = joint_weight_0 * skin_joint_matrices[joint_index_0 + matrix_offset]
                            + joint_weight_1 * skin_joint_matrices[joint_index_1 + matrix_offset]
                            + joint_weight_2 * skin_joint_matrices[joint_index_2 + matrix_offset]
                            + joint_weight_3 * skin_joint_matrices[joint_index_3 + matrix_offset];

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

            let skin_mat_acc = joint_weight_0 * skin_joint_matrices[joint_index_0 + matrix_offset]
                             + joint_weight_1 * skin_joint_matrices[joint_index_1 + matrix_offset]
                             + joint_weight_2 * skin_joint_matrices[joint_index_2 + matrix_offset]
                             + joint_weight_3 * skin_joint_matrices[joint_index_3 + matrix_offset];

            skin_matrix = skin_matrix + skin_mat_acc;
        }
    }

    output.position = (skin_matrix * original_position).xyz;

    return output;
}

/// Applies skeletal skinning to normals and tangents (same weights as positions)
fn apply_normal_skin(input: ApplyVertexInput, normal: vec3<f32>) -> vec3<f32> {
    let skin_sets_count = geometry_mesh_meta.skin_sets_len;

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

            let skin_mat_acc = joint_weight_0 * skin_joint_matrices[joint_index_0 + matrix_offset]
                            + joint_weight_1 * skin_joint_matrices[joint_index_1 + matrix_offset]
                            + joint_weight_2 * skin_joint_matrices[joint_index_2 + matrix_offset]
                            + joint_weight_3 * skin_joint_matrices[joint_index_3 + matrix_offset];

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

            let skin_mat_acc = joint_weight_0 * skin_joint_matrices[joint_index_0 + matrix_offset]
                             + joint_weight_1 * skin_joint_matrices[joint_index_1 + matrix_offset]
                             + joint_weight_2 * skin_joint_matrices[joint_index_2 + matrix_offset]
                             + joint_weight_3 * skin_joint_matrices[joint_index_3 + matrix_offset];

            skin_matrix = skin_matrix + skin_mat_acc;
        }
    }

    // Transform normal (use mat3 to ignore translation)
    let normal_matrix = mat3x3<f32>(
        skin_matrix[0].xyz,
        skin_matrix[1].xyz,
        skin_matrix[2].xyz
    );

    return normal_matrix * normal;
}
