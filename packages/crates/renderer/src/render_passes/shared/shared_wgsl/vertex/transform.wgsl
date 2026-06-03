// Model matrix lookup for the geometry/shadow vertex shaders, which
// only need the model matrix (no normal). Reads from the packed
// transforms array — see `Transforms::BYTE_SIZE` (= stride 112).

fn get_model_transform(byte_offset: u32) -> mat4x4<f32> {
    return transforms[byte_offset / 112u].model_world;
}
