/*************** START froxel_volume.wgsl ******************/
{% include "volumetrics_wgsl/froxel_volume.wgsl" %}
/*************** END froxel_volume.wgsl ******************/

// Per-froxel medium + in-scattered light.
//
// Writes `rgb = in-scattered radiance, a = extinction` — the UNintegrated
// medium. What arrives at this point in the air, and how opaque the air is
// here. The marching happens in `integrate`.
//
// The light walk is the canonical `froxel_walk` order: directional prefix
// first, then the per-froxel punctual list. Same order, same lights, same
// shadow lookups as the surfaces in this column — the volume can't disagree
// with the geometry about what is lit.

@compute @workgroup_size(4, 4, 4)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let grid = volumetric_params.grid_size;
    let slice_count = u32(volumetric_params.slice_count);
    if (gid.x >= grid.x || gid.y >= grid.y || gid.z >= slice_count) {
        return;
    }

    let world_pos = froxel_center_world(gid.xy, gid.z);
    let density = froxel_medium_density(world_pos.y);

    // Direction from the froxel back toward the eye — the outgoing direction
    // for the phase function.
    let to_eye = normalize(camera_raw.position.xyz - world_pos);

    var scattered = vec3<f32>(0.0);

    // Zero-density froxels (above the haze layer) still cost the walk unless we
    // skip: the scattered radiance would be multiplied to nothing anyway, and a
    // scene with a low haze layer is mostly empty air.
    if (density > 0.0) {
        let g = volumetric_params.anisotropy;

        // 1. Directional prefix — flat, not froxel-binned (directionals hit
        //    every froxel), exactly as froxel_walk.wgsl documents.
        let n_directional = get_n_directional();
        for (var d = 0u; d < n_directional; d = d + 1u) {
            let index = get_directional_light_index(d);
            scattered += froxel_light_contribution(index, world_pos, to_eye, g);
        }

        // 2. Per-froxel punctual list for THIS froxel's column + slice.
        let view_z = sqrt(
            froxel_slice_view_z(f32(gid.z)) * froxel_slice_view_z(f32(gid.z) + 1.0)
        );
        let base = froxel_base_for_pixel(froxel_center_pixel(gid.xy), view_z);
        let count = froxel_light_count(base);
        for (var i = 0u; i < count; i = i + 1u) {
            let index = lights_storage[base + 1u + i];
            scattered += froxel_light_contribution(index, world_pos, to_eye, g);
        }

        scattered *= volumetric_params.scattering_color * density;
    }

    textureStore(dst_volume, vec3<i32>(gid), vec4<f32>(scattered, density));
}

// One light's contribution to the medium at `world_pos`.
//
// Deliberately NOT the surface path: there is no normal in the air, so no
// `n_dot_l` — `light_sample`'s cosine term is discarded and the phase function
// takes its place. Shadowing uses the same `sample_shadow_descriptor` the
// surfaces call, so a beam is occluded by exactly the geometry that casts a
// shadow on the floor.
fn froxel_light_contribution(
    index: u32,
    world_pos: vec3<f32>,
    to_eye: vec3<f32>,
    g: f32,
) -> vec3<f32> {
    // `volumetric_intensity` rides the free word of the packed light's 4th row.
    // 0 ⇒ this light lights surfaces only and is invisible in the air, which is
    // the usual setting for a key light.
    let volumetric_intensity = lights[index].kind_outer_pad.w;
    if (volumetric_intensity <= 0.0) {
        return vec3<f32>(0.0);
    }

    let light = get_light(index);
    // `light_sample` wants a normal for its cosine term; hand it the outgoing
    // direction and drop `n_dot_l`. What we keep is `radiance` (distance
    // attenuation + cone falloff) and `light_dir`.
    let sample = light_sample(light, to_eye, world_pos);

    var shadow = 1.0;
    if (light.shadow_index != SHADOW_INDEX_NONE) {
        // The HARD path by construction: per-froxel PCSS blocker search is
        // unaffordable and pointless, since the volume integral blurs the
        // result anyway. `sample_shadow_descriptor` takes a world normal for
        // its normal-offset bias; the medium has none, so pass the direction
        // toward the light — the offset then pushes the sample point along the
        // shadow ray, which is the degenerate-but-correct choice here.
        shadow = sample_shadow_descriptor(light.shadow_index, world_pos, sample.light_dir);
    }

    // Phase: cos between the direction light TRAVELS (-light_dir, since
    // light_dir points from the froxel toward the light) and the direction we
    // look FROM (-to_eye).
    let cos_theta = dot(-sample.light_dir, -to_eye);
    let phase = henyey_greenstein(cos_theta, g);

    return sample.radiance * shadow * phase * volumetric_intensity;
}
