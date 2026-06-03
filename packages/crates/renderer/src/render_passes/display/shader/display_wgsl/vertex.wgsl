@vertex
fn vert_main(@builtin(vertex_index) vertex_index: u32) -> FragmentInput {
    var out: FragmentInput;

    // Generate oversized triangle vertices using bit manipulation
    // Goal: vertex 0→(-1,-1), vertex 1→(3,-1), vertex 2→(-1,3)

    // X coordinate generation:
    // vertex_index: 0 → 0<<1 = 0 → 0&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 1 → 1<<1 = 2 → 2&2 = 2 → 2*2-1 = 3  ✓
    // vertex_index: 2 → 2<<1 = 4 → 4&2 = 0 → 0*2-1 = -1 ✓
    let x = f32((vertex_index << 1u) & 2u) * 2.0 - 1.0;

    // Y coordinate generation:
    // vertex_index: 0 → 0&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 1 → 1&2 = 0 → 0*2-1 = -1 ✓
    // vertex_index: 2 → 2&2 = 2 → 2*2-1 = 3  ✓
    let y = f32(vertex_index & 2u) * 2.0 - 1.0;

    out.full_screen_quad_position = vec4<f32>(x, y, 0.0, 1.0);

    // Convert NDC coordinates (-1 to 1) to UV coordinates (0 to 1)
    // Note: Y is flipped because texture coordinates have origin at top-left
    // out.uv = vec2<f32>(
    //     (x + 1.0) * 0.5,     // -1→0, 1→1, 3→2 (off-screen)
    //     (1.0 - y) * 0.5      // -1→1, 1→0, 3→-1 (off-screen)
    // );

    return out;
}
