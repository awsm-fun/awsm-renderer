
struct GeometryMeshMeta {
    mesh_key_high: u32,
    mesh_key_low: u32,
    morph_geometry_target_len: u32,
    morph_geometry_weights_offset: u32,
    morph_geometry_values_offset: u32,
    skin_sets_len: u32,
    skin_matrices_offset: u32,
    skin_index_weights_offset: u32,
    transform_offset: u32,
    // this is not the offset of the material
    // it's the offset of the mesh_meta data in the material *pass*
    material_mesh_meta_offset: u32,
    // First index into the per-instance attribute storage buffer for this
    // mesh's instances. The vertex shader adds `@builtin(instance_index)`
    // to this to derive the per-fragment instance_id that's packed into
    // barycentric_tex's BA channels and looked up by the shading compute
    // pass. U32_MAX sentinel means "this mesh isn't instanced or has no
    // per-instance attributes" — the shading pass treats that as an
    // identity tint.
    instance_attr_base: u32,
    // Camera-facing rotation override applied in `apply_vertex` after morphs +
    // skinning. 0 = none (default), 1 = Y-axis (yaw-around-up only — sprite
    // stays upright), 2 = full (rotates the local basis so the model +Z axis
    // points at the camera). See `BillboardMode` in `meshes/mesh.rs`.
    billboard_mode: u32
}
