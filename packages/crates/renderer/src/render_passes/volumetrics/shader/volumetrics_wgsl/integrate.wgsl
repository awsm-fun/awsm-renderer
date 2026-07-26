/*************** START froxel_volume.wgsl ******************/
{% include "volumetrics_wgsl/froxel_volume.wgsl" %}
/*************** END froxel_volume.wgsl ******************/

// Front-to-back accumulation down each froxel column.
//
// One invocation per COLUMN, not per froxel: the march is inherently serial in
// z, and doing it here means the effects pass pays one trilinear fetch instead
// of walking the volume per pixel.
//
// Writes `rgb = in-scatter accumulated from the eye to the FAR face of this
// slice, a = transmittance to that face`. A pixel then composites as
//
//     color * transmittance + accumulated_in_scatter
//
// which is the same shape as the analytic `rgb * t + haze * (1 - t)`, except
// the haze term is what the lights actually put in the air along that path
// rather than a constant colour.

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let grid = volumetric_params.grid_size;
    if (gid.x >= grid.x || gid.y >= grid.y) {
        return;
    }

    let slice_count = u32(volumetric_params.slice_count);
    var accumulated = vec3<f32>(0.0);
    var transmittance = 1.0;

    for (var slice = 0u; slice < slice_count; slice = slice + 1u) {
        let froxel = textureLoad(src_volume, vec3<i32>(vec3<u32>(gid.xy, slice)), 0);
        let extinction = froxel.a;
        let thickness = froxel_slice_thickness(slice);

        let optical_depth = extinction * thickness;
        let slice_transmittance = exp(-optical_depth);

        // Energy-conserving slice integral rather than `scatter * thickness`:
        // integrating the in-scatter against the transmittance falling off
        // across the slice gives `S/σt · (1 - exp(-σt·d))`. With plain
        // multiplication a thick slice over-contributes badly, which is
        // exactly where a 32-slice volume spends most of its depth range.
        let slice_scatter = froxel.rgb * (1.0 - slice_transmittance) / max(extinction, 1e-6);

        accumulated += transmittance * slice_scatter;
        transmittance *= slice_transmittance;

        textureStore(
            dst_volume,
            vec3<i32>(vec3<u32>(gid.xy, slice)),
            vec4<f32>(accumulated, transmittance),
        );

        // Once the air is opaque, every remaining slice contributes nothing —
        // `transmittance * slice_scatter` is already zero to the precision the
        // rgba16float target can hold. Fill the tail with the saturated value
        // and stop marching. In a dense scene this cuts most of the column.
        if (transmittance < 0.002) {
            for (var tail = slice + 1u; tail < slice_count; tail = tail + 1u) {
                textureStore(
                    dst_volume,
                    vec3<i32>(vec3<u32>(gid.xy, tail)),
                    vec4<f32>(accumulated, transmittance),
                );
            }
            return;
        }
    }
}
