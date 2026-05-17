// Per-instance attribute block — color + alpha + size — read by the shading
// passes to tint the resolved material color. Mirrors the Rust struct in
// `crates/renderer/src/instances.rs` (16 bytes, `InstanceAttr::BYTE_SIZE`).
//
// `color_packed` is an RGBA8 unorm value packed into a `u32` (low byte = R).
// `size` is reserved for a future GPU-side per-instance scale and is currently
// baked into the per-instance transform on the CPU side. `alpha` multiplies on
// top of the material's own alpha.

struct InstanceAttr {
    color_packed: u32,
    size: f32,
    alpha: f32,
    _pad: u32,
}

// Sentinel base used in `geometry_mesh_meta.instance_attr_base` to mean "this
// mesh has no per-instance attributes" — the shading pass treats the matching
// per-fragment `instance_id` as identity tint.
const INSTANCE_ATTR_NONE: u32 = 0xFFFFFFFFu;
