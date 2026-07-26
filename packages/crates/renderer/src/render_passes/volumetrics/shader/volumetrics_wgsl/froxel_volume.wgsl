// Froxel-volume geometry, shared by inject and integrate.
//
// The volume's X/Y grid is the light-culling tile grid, deliberately, so a
// froxel column lines up with a culling tile. Its Z slicing is NOT the culling
// grid's — see below — and it doesn't need to be: `froxel_base_for_pixel`
// takes a view DEPTH and does its own mapping, so the two grids only have to
// agree about metres, never about slice indices.

// View-space depth at the FRONT face of slice `s` (s == slice_count gives the
// far plane). UNIFORM across the volume's range:
//     z = z_near + (z_far - z_near) * s / slice_count
//
// The light-culling grid slices exponentially, and copying that was the
// original mistake. Exponential slicing equalizes SCREEN-space error over a
// 0.1 m -> 10 km binning range; the medium's error metric is world-space,
// because a beam is about a metre wide wherever it happens to be. Anchored at
// the camera's 0.1 m near plane, an exponential mapping spent HALF of 32
// slices between 0.1 m and 1.6 m — empty air in front of the lens — and left
// a whole stage at 5-9 m sharing about 3.4 slices. That is what made the beam
// cones read as hard straight-edged facets.
//
// The usual argument for exponential — keep froxels cubic so the trilinear
// filter is isotropic — does not apply at this budget. At 16 px columns and a
// 1080p-ish viewport a froxel is ~0.12 m across at 7 m but ~0.8 m deep: Z is
// already the starved axis by about 8x, so it should not also be the one
// being given away near the camera.
//
// This makes `volumetric_distance` matter more, which is affordable now that
// it no longer changes the look: set it to just past the subject and the
// slices land on the subject. Everything beyond is the analytic tail.
fn froxel_slice_view_z(s: f32) -> f32 {
    let t = s / max(volumetric_params.slice_count, 1.0);
    return mix(volumetric_params.z_near, volumetric_params.z_far, t);
}

// The screen pixel at the centre of froxel column (x, y).
fn froxel_center_pixel(xy: vec2<u32>) -> vec2<f32> {
    return (vec2<f32>(xy) + vec2<f32>(0.5)) * f32(FROXEL_TILE_PIXEL_SIZE);
}

// World-space position at the centre of froxel (xy, slice).
//
// Reconstructed through `inv_view_proj` from the pixel + a view depth, rather
// than by stepping a ray: the froxel grid is defined in screen × view-depth
// space, so going back through the same projection is what keeps the volume
// registered with the pixels that will sample it.
fn froxel_center_world(xy: vec2<u32>, slice: u32) -> vec3<f32> {
    let pixel = froxel_center_pixel(xy);
    let uv = pixel / vec2<f32>(f32(cull_params.viewport_w), f32(cull_params.viewport_h));
    let ndc_xy = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);

    // Midpoint of the slice in view depth. The ARITHMETIC mean now that the
    // slices are uniform — under the old exponential mapping this had to be
    // the geometric mean, since an arithmetic midpoint drifted toward the far
    // face and biased every froxel backwards.
    let view_z = froxel_slice_view_z(f32(slice) + 0.5);

    // Unproject: build a view-space ray through the pixel, scale it so its
    // forward component equals `view_z`, then take it to world space.
    let view_h = camera_raw.inv_proj * vec4<f32>(ndc_xy.x, ndc_xy.y, 1.0, 1.0);
    let view_dir = normalize(view_h.xyz / view_h.w);
    let view_pos = view_dir * (view_z / max(-view_dir.z, 1e-4));
    return (camera_raw.inv_view * vec4<f32>(view_pos, 1.0)).xyz;
}

// Thickness of slice `slice` in view depth — the path length a ray spends in
// that froxel, which is what turns per-froxel extinction into optical depth.
fn froxel_slice_thickness(slice: u32) -> f32 {
    return froxel_slice_view_z(f32(slice) + 1.0) - froxel_slice_view_z(f32(slice));
}

// The medium's density at a world height, matching the analytic path's profile:
// full below `base_height`, thinning exponentially above it.
fn froxel_medium_density(world_y: f32) -> f32 {
    let h = world_y - volumetric_params.base_height;
    if (volumetric_params.height_falloff <= 0.0) {
        return volumetric_params.density;
    }
    return volumetric_params.density * exp(-volumetric_params.height_falloff * max(h, 0.0));
}

// Henyey-Greenstein phase function. `cos_theta` is between the direction light
// travels and the direction we're looking. g > 0 forward-scatters, which is
// what makes a beam aimed at the camera flare instead of reading as a flat cone.
fn henyey_greenstein(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = 1.0 + g2 - 2.0 * g * cos_theta;
    return (1.0 - g2) / (4.0 * 3.14159265 * max(denom, 1e-4) * sqrt(max(denom, 1e-4)));
}
