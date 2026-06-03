// Renderer-wide variable-length per-material data pool.
//
// Bound alongside `materials_data` so any pass that includes
// `material.wgsl` also gets the extras helpers in scope. First-party
// materials are free to use it for variable-length data (none do
// today, but the binding is universal). Dynamic materials reach the
// pool via the auto-generated `<slot>_offset: u32` + `<slot>_length:
// u32` fields on `MaterialData`.
//
// CPU source lives in `crates/renderer/src/dynamic_materials/extras_pool.rs`.
// The bind-group declaration is in each pass's bind_groups.wgsl
// (alongside the materials binding) — this file is the helper-only
// half so the contract docs can reference one shared set of
// signatures.

fn extras_load_u32(index: u32) -> u32 {
    return bitcast<u32>(extras_pool[index]);
}

fn extras_load_f32(index: u32) -> f32 {
    return bitcast<f32>(extras_pool[index]);
}

fn extras_load_vec4_f32(index: u32) -> vec4<f32> {
    return vec4<f32>(
        bitcast<f32>(extras_pool[index + 0u]),
        bitcast<f32>(extras_pool[index + 1u]),
        bitcast<f32>(extras_pool[index + 2u]),
        bitcast<f32>(extras_pool[index + 3u]),
    );
}
