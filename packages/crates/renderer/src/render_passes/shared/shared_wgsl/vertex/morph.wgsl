//***** MORPHS *****


fn apply_position_morphs(input: ApplyVertexInput) -> ApplyVertexInput {
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
            let weight = geometry_morph_weights[weight_offset];
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
            let weight = geometry_morph_weights[weight_offset];
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

fn apply_normal_morphs(input: ApplyVertexInput, normal: vec3<f32>) -> vec3<f32> {
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
            let weight = geometry_morph_weights[weight_offset];
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
            let weight = geometry_morph_weights[weight_offset];
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

fn apply_tangent_morphs(input: ApplyVertexInput, tangent: vec4<f32>) -> vec4<f32> {
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
            let weight = geometry_morph_weights[weight_offset];
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
            let weight = geometry_morph_weights[weight_offset];
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
