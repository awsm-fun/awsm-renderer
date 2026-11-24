{% include "shared_wgsl/math.wgsl" %}{% include "shared_wgsl/motion_vector.wgsl" %}

//***** TAA DEFORMING-MESH VELOCITY OVERLAY (32 B/sample fallback) *****
//
// The prep pass reconstructed every pixel's vector from depth and the rigid
// object matrices. This pass re-draws only the meshes that deformed this
// frame (skin, morph, instance transforms, vertex hook) with the geometry
// vertex stage's two evaluations (current + previous, `motion_prev.wgsl`) and
// overwrites their pixels' vectors with the exact per-fragment ones (03 §2.2).
//
// Exactly one writer per pixel: a fragment writes only when it is the
// surface the geometry pass kept there, i.e. its triangle index, mesh meta
// offset and instance id equal what the geometry pass wrote into the
// visibility and barycentric targets. That makes the result independent of
// depth precision between the two pipelines and of the alpha test (a masked
// mesh's cut-out fragment never matches). Depth is attached read-only only
// to cull.
@group(2) @binding(1) var taa_overlay_visibility: texture_2d<u32>;
@group(2) @binding(2) var taa_overlay_barycentric: texture_2d<u32>;
@group(2) @binding(3) var taa_overlay_velocity: texture_storage_2d<rg32float, write>;{% if debug %}
// Debug check (`AwsmRenderer::set_taa_debug_overlay_check`): per material
// meta slot, [written, matched but depth != stored, matched but failing the
// production depth compare]. This variant tests depth `always`.
@group(2) @binding(4) var<storage, read_write> taa_overlay_check: array<atomic<u32>>;
@group(2) @binding(5) var taa_overlay_depth: texture_depth_2d;{% endif %}

struct TaaOverlayFragmentInput {
    @builtin(position) frag_coord: vec4<f32>,
    @location(0) @interpolate(flat) triangle_index: u32,
    @location(4) @interpolate(flat) instance_id: u32,
    @location(5) @interpolate(flat) material_mesh_meta_offset: u32,
    @location(7) @interpolate(perspective, center) clip_now: vec4<f32>,
    @location(8) @interpolate(perspective, center) clip_prev: vec4<f32>,
}

@fragment
fn fs_main(input: TaaOverlayFragmentInput) {
    let p = vec2<i32>(input.frag_coord.xy);
    // Same packing as the geometry fragment (geometry_wgsl/fragment.wgsl).
    let vis = textureLoad(taa_overlay_visibility, p, 0);
    let bary = textureLoad(taa_overlay_barycentric, p, 0);
    let ids = vec4<u32>(split16(input.triangle_index), split16(input.material_mesh_meta_offset));
    let same = all(vis == ids) && all(bary.zw == split16(input.instance_id));
    if (same) {
        textureStore(
            taa_overlay_velocity,
            p,
            vec4<f32>(taa_motion_vector(input.clip_now, input.clip_prev), 0.0, 0.0),
        );{% if debug %}
        let slot = (input.material_mesh_meta_offset / {{ meta_slot_bytes }}u) * 3u;
        atomicAdd(&taa_overlay_check[slot], 1u);
        let stored = textureLoad(taa_overlay_depth, p, 0);
        if (input.frag_coord.z != stored) {
            atomicAdd(&taa_overlay_check[slot + 1u], 1u);
        }
        if (!({% if reverse_z %}input.frag_coord.z >= stored{% else %}input.frag_coord.z <= stored{% endif %})) {
            atomicAdd(&taa_overlay_check[slot + 2u], 1u);
        }{% endif %}
    }
}
