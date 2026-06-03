
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
    billboard_mode: u32,
    // Pad the struct out to `GEOMETRY_MESH_META_BYTE_ALIGNMENT` (256 B)
    // so the storage-array binding shape matches the CPU buffer
    // layout. The CPU side strides slots at 256 B because the
    // uniform-with-dynamic-offset path's dynamic-offset alignment
    // requires it; the same physical buffer backs both bindings, so
    // the WGSL struct stride must agree.
    //
    // The padding is `array<vec4<u32>, 13>` (not `array<u32, 52>`)
    // because when this struct is bound as a `uniform` (the
    // instanced path, and the non-instanced portable path when
    // `indirect_first_instance` is off), WGSL requires every array
    // inside the uniform address space to have a 16-byte-aligned
    // stride. Safari enforces this strictly; Chrome lets a
    // `u32`-stride pass. 13 × 16 B = 208 B padding + 48 B active =
    // 256 B total. Layout matches the prior `array<u32, 52>` shape
    // byte-for-byte from the CPU's perspective.
    _pad: array<vec4<u32>, 13>
}
