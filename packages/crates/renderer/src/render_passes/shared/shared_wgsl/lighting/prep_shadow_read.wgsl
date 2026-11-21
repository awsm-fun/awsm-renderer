{% if prep_present %}
// Plan B (stage 4/5a/5b): read a prep pass's packed shadow-visibility buffer
// instead of sampling shadow maps inline. `slot` is the j-th shadowed light in
// the canonical froxel walk (directional prefix then per-froxel punctual) — the
// SAME order `cs_prep` / `cs_prep_edge` wrote, so slot j here matches layer j/4 /
// channel j%4 there. Slots >= K (clamped per-pixel caster cap) overflow to lit
// (1.0), matching prep's clamp-and-skip.
//
// The SOURCE is runtime-selected by the per-thread PrepReadContext mode:
//   PRIMARY (cs_opaque) → full-screen prep_shadow_visibility at `pixel_xy`.
//   EDGE    (cs_edge)   → compact prep_edge_shadow at `g_prep_ctx.edge_shadow_xy`
//                         (the 2D texel for this edge_pixel × sample, set per
//                         sample in cs_edge — Stage 5b-shadow).
fn prep_shadow_read(pixel_xy: vec2<f32>, slot: u32) -> f32 {
    if (slot >= {{ max_shadow_casters }}u) {
        return 1.0;
    }
{% if multisampled_geometry %}
    if (g_prep_ctx.mode == PREP_MODE_EDGE) {
        let texel = textureLoad(prep_edge_shadow, g_prep_ctx.edge_shadow_xy, i32(slot / 4u), 0);
        return texel[slot % 4u];
    }
{% endif %}
    let c = vec2<i32>(i32(pixel_xy.x), i32(pixel_xy.y));
    let texel = textureLoad(prep_shadow_visibility, c, i32(slot / 4u), 0);
    return texel[slot % 4u];
}
{% endif %}
