
// make sure this matches MATERIAL_MESH_META_BYTE_ALIGNMENT in material_opaque_meta.rs
const META_SIZE_IN_BYTES = 256u;

// populated from `material_meta.rs`
struct MaterialMeshMeta {
    mesh_key_high: u32,
    mesh_key_low: u32,
    morph_material_target_len: u32,
    morph_material_weights_offset: u32,
    morph_material_values_offset: u32,
    morph_material_bitmask: u32,
    material_offset: u32,
    transform_offset: u32,
    normal_matrix_offset: u32,
    vertex_attribute_indices_offset: u32,
    vertex_attribute_data_offset: u32,
    vertex_attribute_stride: u32,
    uv_sets_index: u32,
    uv_set_count: u32,
    color_set_count: u32,
    visibility_geometry_data_offset: u32,
    is_hud: u32,
    // `1u` means the mesh's shading should be multiplied by shadow
    // visibility from the configured lights; `0u` short-circuits the
    // sample to "fully lit" inside `apply_lighting`. Matches
    // `Mesh::receive_shadows`. Filled from the corresponding `u32`
    // slot in `MaterialMeshMeta::to_bytes` — keep the byte offset in
    // lockstep when adding fields above.
    receive_shadows: u32,
    // `1u` means the mesh opts into projection decals;
    // `0u` skips the per-decal volume test in `material_decal`'s
    // compute. Matches `Mesh::receive_decals`.
    receive_decals: u32,
    // Per-frame coverage-driven shadow gate. Defaults to `1u`
    // (fully-lit / sample shadows). Set to `0u` by the CPU when
    // `LightMeshBuckets::mark_shadow_receivers` determines no
    // shadow-caster reaches this mesh this frame — the shading path
    // bitwise-ANDs this with the authored `receive_shadows` flag to
    // produce the effective gate, so callsites that read
    // `material_mesh_meta.receive_shadows` should AND in this field
    // before passing it to `apply_lighting*`. Filled from the
    // corresponding `u32` slot in `MaterialMeshMeta::to_bytes`
    // (offset `MATERIAL_MESH_META_SHADOW_RECEIVER_GATE_OFFSET`).
    shadow_receiver_gate: u32,
    // Reserved trailing u32s (indices 20-23). `_reserved2/3` formerly
    // held the per-mesh light slice (`offset`/`count`) for the old
    // per-mesh lighting path, removed when shading unified on the
    // per-pixel froxel light list; the slots are kept as inert tail
    // padding (each entry is 256-byte aligned, so removing them
    // reclaims nothing). The four u32s keep the populated region at a
    // vec4 boundary so `padding_4` lays out cleanly.
    _reserved0: u32,
    _reserved1: u32,
    _reserved2: u32,
    _reserved3: u32,
    padding_4: array<vec4<u32>, 10>,
}
