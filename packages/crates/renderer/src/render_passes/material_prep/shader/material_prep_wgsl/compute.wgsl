// Material prep compute pass (Plan B). Runs once per pixel over the visibility
// buffer, after classify and before per-material shading, and materializes the
// material-INDEPENDENT per-pixel data (world position, UVs, vertex colors, and —
// later — shadow visibility) so per-material kernels just read it.
//
// STAGE-1 SCAFFOLD: a valid, minimal `cs_prep` entry that wires the pass into the
// pipeline machinery. The real attribute reconstruction (perspective-correct
// vertex interpolation, shared with positions.wgsl) replaces the placeholder
// write in the next sub-stage; deferred-shadow sampling (via the shared
// froxel_walk.wgsl) lands in stage 3.

@compute @workgroup_size(8, 8)
fn cs_prep(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(world_pos_out);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coords = vec2<i32>(i32(gid.x), i32(gid.y));

    // Placeholder: read sample 0 of the visibility buffer (proves the binding +
    // dispatch path) and write a sentinel world position. Replaced by the
    // perspective-correct interpolation in the stage-1 attribute sub-stage.
    let _vis = textureLoad(visibility_data_tex, coords, 0);
    textureStore(world_pos_out, coords, vec4<f32>(0.0, 0.0, 0.0, 1.0));
}
