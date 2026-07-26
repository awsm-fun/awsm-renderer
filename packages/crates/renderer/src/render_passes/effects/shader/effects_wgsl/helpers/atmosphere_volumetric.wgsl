// Composite the pre-integrated froxel volume over the scene.
//
// All the work happened in the volumetrics pass; this is one fetch. The volume
// stores, per froxel, the in-scatter accumulated from the eye to that slice and
// the transmittance to it, so the composite is
//
//     rgb * transmittance + accumulated_in_scatter
//
// which is the same shape as the analytic `rgb * t + haze * (1 - t)` — except
// the haze term is what the lights actually put in the air along that path
// rather than a constant colour. That's the entire difference between the two
// modes at this point in the frame.

// Depth beyond which a pixel simply takes the volume's last slice. Sky pixels
// and anything past the froxel grid's far plane are both "as far as the volume
// goes"; there is no more medium described past that.
fn volumetric_slice_for_view_z(view_z: f32) -> i32 {
    let slices = i32(atmosphere_params.slice_count);
    // Inverse of the exponential slice mapping (same one froxel_walk.wgsl uses,
    // and the same z_near/z_far the light culling was built with — the volume
    // is registered to that grid, so this MUST agree with it).
    let z = max(view_z, atmosphere_params.froxel_z_near);
    let t = log(z / atmosphere_params.froxel_z_near)
        / max(atmosphere_params.froxel_log_far_over_near, 1e-6);
    return clamp(i32(t * f32(slices)), 0, slices - 1);
}

fn apply_atmosphere_volumetric(
    rgb: vec3<f32>,
    coords: vec2<i32>,
    pixel_center: vec2<f32>,
    screen_dims_f32: vec2<f32>,
    camera: Camera,
) -> vec3<f32> {
    let depth = load_depth(coords);
    // Sky takes the far slice: the volume ends where the froxel grid ends, and
    // everything beyond has crossed all the air the volume knows about.
    var slice = i32(atmosphere_params.slice_count) - 1;
    if (!is_sky_depth(depth)) {
        slice = volumetric_slice_for_view_z(linearize_depth(depth, camera));
    }

    // The froxel column for this pixel — the same 16px tiling the volume was
    // built with. Integer division, not a filtered sample: a sampler here would
    // be a second new binding, and the smoothing worth having is the temporal
    // pass's job.
    let dims = textureDimensions(volumetric_tex);
    let tile = vec2<i32>(pixel_center / FROXEL_TILE_PIXEL_SIZE);
    let clamped = clamp(tile, vec2<i32>(0), vec2<i32>(i32(dims.x) - 1, i32(dims.y) - 1));

    let integrated = textureLoad(volumetric_tex, vec3<i32>(clamped, slice), 0);
    return rgb * integrated.a + integrated.rgb;
}
