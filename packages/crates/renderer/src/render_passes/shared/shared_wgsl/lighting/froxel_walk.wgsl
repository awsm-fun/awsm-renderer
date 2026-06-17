// froxel_walk.wgsl — the SINGLE SOURCE OF TRUTH for how a pixel enumerates the
// lights (and therefore the shadow casters) that affect it.
//
// Included by BOTH `apply_lighting.wgsl` (per-material shading) and the Plan B
// prep pass (`docs/plans/deferred-shared-prep-pass.md`). The deferred-shadow
// stage relies on this: the prep pass writes shadow visibility for the j-th
// shadowed caster, and the per-material lighting loop reads layer j for its j-th
// shadowed caster — they MUST enumerate in the identical order, so that order is
// defined here, once.
//
// CANONICAL SHADOW-CASTER ENUMERATION ORDER (slot j increments per shadowed light):
//   1. Directional prefix — flat, NOT froxel-binned (directionals hit every
//      pixel): for d in 0..get_n_directional(): light = get_light(get_directional_light_index(d)).
//   2. Per-froxel punctual — for i in 0..froxel_light_count(base): light =
//      get_light(lights_storage[base + 1u + i]), base = froxel_base_for_pixel(...).
//   A light is a shadow caster iff `light.shadow_index != SHADOW_INDEX_NONE`.
//   Both passes must apply this exact predicate in this exact order.
//
// Per-froxel slice layout (see light_culling_wgsl/bind_groups.wgsl):
//   stride = cull_params.max_per_froxel_capacity + 1; slot 0 = count (clamp at
//   read time); slots 1..1+count = light indices.

const FROXEL_TILE_PIXEL_SIZE: u32 = 16u;
const FROXEL_SLICE_COUNT: u32 = {{ froxel_slice_count }}u;
// `max_per_froxel_capacity` is a runtime field on `cull_params` so the
// auto-grow path can bump the budget without recompiling.

// Maps a fragment's screen-space pixel coordinates + view-space depth
// (positive forward) into a froxel base index in `lights_storage`. The
// returned index already accounts for the head-region offset
// (`cull_params.mesh_indices_capacity_u32`) so callers can read
// `lights_storage[base]` for the count and `lights_storage[base + 1u + i]`
// for the i-th light index.
fn froxel_base_for_pixel(pixel_xy: vec2<f32>, view_z: f32) -> u32 {
    let tile_x = u32(pixel_xy.x) / FROXEL_TILE_PIXEL_SIZE;
    let tile_y = u32(pixel_xy.y) / FROXEL_TILE_PIXEL_SIZE;
    let tile_x_clamped = min(tile_x, max(cull_params.tiles_x, 1u) - 1u);
    let tile_y_clamped = min(tile_y, max(cull_params.tiles_y, 1u) - 1u);
    // Exponential z-slice mapping inverse:
    //   s = log(z / z_near) / log(z_far / z_near)
    let z = max(view_z, cull_params.z_near);
    let s = log(z / cull_params.z_near) / max(cull_params.log_far_over_near, 1e-6);
    let z_slice = clamp(u32(s * f32(FROXEL_SLICE_COUNT)), 0u, FROXEL_SLICE_COUNT - 1u);
    let tiles_per_layer = cull_params.tiles_x * cull_params.tiles_y;
    let froxel_idx = z_slice * tiles_per_layer + tile_y_clamped * cull_params.tiles_x + tile_x_clamped;
    let stride = cull_params.max_per_froxel_capacity + 1u;
    return cull_params.mesh_indices_capacity_u32 + froxel_idx * stride;
}

// The clamped per-froxel light count for `base` (slot 0, capped at the runtime
// per-froxel capacity). Centralized so prep + shading read it identically.
fn froxel_light_count(base: u32) -> u32 {
    return min(lights_storage[base], cull_params.max_per_froxel_capacity);
}
