//***** INPUT/OUTPUT *****

struct ApplyVertexInput {
    vertex_index: u32,
    position: vec3<f32>,      // Model-space position
    normal: vec3<f32>,        // Model-space normal
    tangent: vec4<f32>,       // Model-space tangent (w = handedness)
    {% if instancing_transforms %}
        // instance transform matrix
        instance_transform_row_0: vec4<f32>,
        instance_transform_row_1: vec4<f32>,
        instance_transform_row_2: vec4<f32>,
        instance_transform_row_3: vec4<f32>,
    {% endif %}
}

struct ApplyVertexOutput {
    clip_position: vec4<f32>,
    world_normal: vec3<f32>,     // Transformed world-space normal
    world_tangent: vec4<f32>,    // Transformed world-space tangent (w = handedness)
    world_position: vec3<f32>,   // Transformed world-space position
}

fn apply_vertex(vertex_orig: ApplyVertexInput, camera: Camera) -> ApplyVertexOutput {
    var out: ApplyVertexOutput;

    var vertex = vertex_orig;
    var normal = vertex_orig.normal;
    var tangent = vertex_orig.tangent;

    // Apply morphs to position, normal, and tangent
    if geometry_mesh_meta.morph_geometry_target_len != 0 {
        vertex = apply_position_morphs(vertex);

        // Apply morphed normals (correct behavior)
        normal = apply_normal_morphs(vertex_orig, normal);
        tangent = apply_tangent_morphs(vertex_orig, tangent);
    }

    {% if has_custom_vertex %}
    {
        let _disp = custom_displace_vertex(VertexDisplaceInput(
            vertex.position, normal, tangent, vec2<f32>(0.0, 0.0),
            vertex.vertex_index, 0u,
            material_data_load(geometry_mesh_meta.material_mesh_meta_offset),
        ));
        vertex.position = _disp.position;
        normal = _disp.normal;
        tangent = _disp.tangent;
    }
    {% endif %}

    // Apply skinning to position, normal, and tangent
    if geometry_mesh_meta.skin_sets_len != 0 {
        vertex = apply_position_skin(vertex);
        normal = apply_normal_skin(vertex_orig, normal);
        tangent = vec4<f32>(apply_normal_skin(vertex_orig, tangent.xyz), tangent.w);
    }

    {% if instancing_transforms %}
        // Transform the vertex position by the instance transform
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

    // Skinned meshes are already in world space: `apply_position_skin` /
    // `apply_normal_skin` multiply by the joint matrices, each of which is
    // `jointWorld * inverseBind` and therefore folds in every ancestor of the
    // joint node — including the glTF Z-up→Y-up root conversion. Per the glTF
    // spec the skinned mesh node's own transform MUST NOT be applied on top, so
    // collapse the base model transform to identity (the per-instance transform,
    // if any, is preserved). Without this, models whose skinned mesh node sits
    // under a non-identity root (e.g. CesiumMan's `Z_UP`) get that rotation
    // applied twice and render lying flat.
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

    // Camera-facing override. Replaces the rotation portion of the model
    // matrix while preserving translation + per-instance scale (encoded as
    // column lengths). Uniform scale is recovered as `length(col[i].xyz)` so
    // the per-instance `size` (Stage 3) stays baked-in through this rewrite.
    let billboard_mode = geometry_mesh_meta.billboard_mode;
    if (billboard_mode != 0u) {
        let translation = model_transform[3].xyz;
        let scale_x = length(model_transform[0].xyz);
        let scale_y = length(model_transform[1].xyz);
        let scale_z = length(model_transform[2].xyz);

        let to_cam = camera.position - translation;
        // BillboardMode::YAxis (1): rotate only around world +Y so the local
        // +Z axis points at the camera in the XZ plane. Preserves upright
        // orientation for sprites that should not pitch.
        // BillboardMode::Full (2): build a full look-at basis with world up
        // as the reference; local +Z points at the camera in 3D.
        var forward: vec3<f32>;
        if (billboard_mode == 1u) {
            // YAxis: project onto XZ plane, fall back to +Z when degenerate.
            var xz = vec3<f32>(to_cam.x, 0.0, to_cam.z);
            let len_sq = dot(xz, xz);
            if (len_sq < 1e-8) {
                xz = vec3<f32>(0.0, 0.0, 1.0);
            } else {
                xz = xz * inverseSqrt(len_sq);
            }
            forward = xz;
        } else {
            // Full: world-space look-at; degenerate when the camera is exactly
            // above / below the sprite — fall back to +Z.
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
            // forward is collinear with world up; use world-X as a fallback
            // right vector so the basis stays orthonormal.
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

    let world_pos = model_transform * vec4<f32>(vertex.position, 1.0);
    out.clip_position = camera.view_proj * world_pos;


    // Transform normal/tangent to world space (ignore translation)
    let model_matrix3 = mat3x3<f32>(
        model_transform[0].xyz,
        model_transform[1].xyz,
        model_transform[2].xyz
    );
    // Correct normal transform for non-uniform scaling using an explicit
    // inverse-transpose (cofactor) path, avoiding WGSL inverse() support issues.
    let c0 = model_matrix3[0];
    let c1 = model_matrix3[1];
    let c2 = model_matrix3[2];
    let r0 = vec3<f32>(c0.x, c1.x, c2.x);
    let r1 = vec3<f32>(c0.y, c1.y, c2.y);
    let r2 = vec3<f32>(c0.z, c1.z, c2.z);

    let cof0 = cross(r1, r2);
    let cof1 = cross(r2, r0);
    let cof2 = cross(r0, r1);
    let det_model = dot(r0, cof0);

    let world_normal_unnormalized = select(
        model_matrix3 * normal,
        vec3<f32>(
            dot(cof0, normal),
            dot(cof1, normal),
            dot(cof2, normal),
        ) / det_model,
        abs(det_model) > 1e-8
    );
    let world_normal = normalize(world_normal_unnormalized);

    // Tangents transform with the model matrix, then must be re-orthonormalized against N.
    let tangent_raw = model_matrix3 * tangent.xyz;
    var tangent_ortho = tangent_raw - world_normal * dot(tangent_raw, world_normal);
    let tangent_len_sq = dot(tangent_ortho, tangent_ortho);
    if (tangent_len_sq > 1e-8) {
        tangent_ortho *= inverseSqrt(tangent_len_sq);
    } else {
        // Deterministic fallback tangent orthogonal to N.
        let fallback_axis = select(
            vec3<f32>(0.0, 0.0, 1.0),
            vec3<f32>(0.0, 1.0, 0.0),
            abs(world_normal.z) > 0.999
        );
        tangent_ortho = normalize(cross(fallback_axis, world_normal));
    }

    out.world_normal = world_normal;
    out.world_tangent = vec4<f32>(tangent_ortho, tangent.w);

    out.world_position = world_pos.xyz;

    return out;
}
