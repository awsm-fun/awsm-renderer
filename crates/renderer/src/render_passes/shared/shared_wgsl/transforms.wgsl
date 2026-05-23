
struct Transforms {
    world_model: mat4x4<f32>,
    world_normal: mat3x3<f32>,
}

// Reads the packed transform (model + normal matrix) for the mesh.
// `transforms` is the global `var<storage, read> transforms:
// array<TransformPacked>` declared by each pass's bind-groups WGSL.
// The shader stride is 112 bytes (Transforms::BYTE_SIZE on the Rust
// side) — Option E packs model_world + normal_world into one struct
// so each pixel fetches both in a single buffer access.
fn get_transforms(material_mesh_meta: MaterialMeshMeta) -> Transforms {
    let entry = transforms[material_mesh_meta.transform_offset / 112u];
    return Transforms(entry.model_world, entry.normal_world);
}
