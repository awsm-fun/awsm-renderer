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

    // TWO positions, and the split is load-bearing.
    //
    // The LIGHTING is evaluated at this frame's jittered sample point: that is
    // the quantity with sub-froxel variation worth averaging, and jittering it
    // is the whole point of the temporal path.
    //
    // The MEDIUM is evaluated at the fixed CENTRE. Density is an analytic
    // function of height with no sampling noise to average, and — because the
    // alpha channel is deliberately NOT accumulated across frames — sampling
    // it at a moving point makes the extinction, and therefore every pixel's
    // transmittance, change every single frame. That is a whole-frame flicker,
    // and it was self-inflicted: the first version of this pass evaluated both
    // at the jittered point.
    let world_pos = froxel_sample_world(gid.xy, gid.z);
    let density = froxel_medium_density(froxel_center_world(gid.xy, gid.z).y);

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

        // 2. Per-froxel punctual list for THIS froxel's column + slice. The
        //    lookup goes through a view DEPTH, so the volume's uniform slicing
        //    and the culling grid's exponential slicing never have to agree on
        //    an index — only on metres.
        let view_z = froxel_slice_view_z(f32(gid.z) + 0.5);
        let base = froxel_base_for_pixel(froxel_center_pixel(gid.xy), view_z);
        let count = froxel_light_count(base);
        for (var i = 0u; i < count; i = i + 1u) {
            let index = lights_storage[base + 1u + i];
            scattered += froxel_light_contribution(index, world_pos, to_eye, g);
        }

        // In-scattered source term: sigma_s * (L_ambient + sum L_i).
        //
        // The AMBIENT term is what the first implementation was missing, and
        // its absence is why the two haze modes disagreed by 3.5x on the same
        // medium: air that no punctual light reaches was a pure ABSORBER, so
        // the volumetric path faded a distant surface to BLACK where the
        // analytic path fades it to `color`. Real media in-scatter the room
        // (sky, bounce, the walls), not only the lights the culling grid bins.
        //
        // The weight is exactly 1.0, and that is a derivation rather than a
        // taste call. With source S = color * density and extinction density,
        // `integrate`'s per-slice term S/sigma_t * (1 - exp(-sigma_t*d))
        // telescopes down the column to `color * (1 - T)` — the analytic
        // path's term, identically. So the two modes now agree in the limit
        // where no light participates in the medium, which is the invariant
        // `volumetric_matches_analytic_without_lights` pins.
        //
        // Note what is NOT here: `scattering_color` no longer tints the
        // lights. The medium's scattering albedo is neutral, so a beam is the
        // colour of its LIGHT — a white hazer does not turn a red beam blue.
        // `color` therefore keeps its one documented meaning in both modes:
        // the radiance unlit air glows at.
        // The ambient term rides INSIDE the density > 0 guard on purpose:
        // zero density means no medium, hence nothing to in-scatter. Adding
        // it unconditionally would paint haze above the layer's ceiling,
        // which is exactly what `base_height` + `height_falloff` prevent.
        scattered = (scattered + volumetric_params.scattering_color) * density;
    }

    var result = vec4<f32>(scattered, density);

{% if temporal %}
    // Temporal accumulation. One sample per froxel is far too few for a grid
    // this coarse — the fix is not more samples this frame but a different
    // sample EVERY frame, blended with where the same air was last frame.
    //
    // The froxel's centre (not its jittered sample point) is what gets
    // reprojected: the centre is the fixed thing both frames agree on, while
    // the jitter is deliberately different each frame.
    //
    // Only the in-scattered RADIANCE is accumulated. Extinction is an analytic
    // function of the froxel's height — no sampling noise to average away, and
    // blending it would smear the haze layer's own boundary across frames
    // whenever the camera moves vertically.
    let history = reproject_history(gid);

    // NO history clamp here, and that is a measured decision rather than an
    // omission.
    //
    // `ssr_wgsl/temporal.wgsl` fixes ITS ghosting with a 3x3 neighbourhood
    // AABB clamp, so the obvious move was an analogous one here — a froxel has
    // no cheap neighbourhood (each invocation computes exactly one and its
    // neighbours are in flight), so it was tried against the froxel's own
    // current value. It MADE THE FLICKER WORSE.
    //
    // The reason is worth recording. Where the current sample has high
    // variance, an asymmetric clamp ALTERNATES: on a bright frame the output
    // jumps up; on a dim frame the history is yanked down to a small multiple
    // of a small number; and the pair settles into a stable two-state
    // flip-flop instead of averaging. Measured on the truss band at 0.12x
    // playback, cells swung 2-4x frame to frame (18.9 -> 8.7 -> 12.9 -> 4.8)
    // with it in place.
    //
    // Plain exponential smoothing cannot oscillate — `r_n = a*c_n +
    // (1-a)*r_{n-1}` is monotone toward the mean for any bounded input. So the
    // right place to intervene was the INPUT's variance, which the finite
    // emitter radius above now bounds at source. Fix the sampling, then let
    // the filter be a filter.
    let weight = volumetric_params.history_blend * history.a;
    result = vec4<f32>(mix(result.rgb, history.rgb, weight), density);
{% endif %}

    textureStore(dst_volume, vec3<i32>(gid), result);
}

{% if temporal %}
// Last frame's value for the air now sitting in froxel `gid`, or a zero vec4
// when that air was off-screen / outside the volume last frame.
//
// `w` carries validity rather than a second return value: 1 = the sample is
// real, 0 = there was nothing to reproject.
fn reproject_history(gid: vec3<u32>) -> vec4<f32> {
    // The UNJITTERED centre — see the note at the call site.
    let center_view_z = froxel_slice_view_z(f32(gid.z) + 0.5);
    let pixel = froxel_center_pixel(gid.xy);
    let uv = pixel / vec2<f32>(f32(cull_params.viewport_w), f32(cull_params.viewport_h));
    let ndc_xy = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let view_h = camera_raw.inv_proj * vec4<f32>(ndc_xy.x, ndc_xy.y, 1.0, 1.0);
    let view_dir = normalize(view_h.xyz / view_h.w);
    let view_pos = view_dir * (center_view_z / max(-view_dir.z, 1e-4));
    let world_pos = (camera_raw.inv_view * vec4<f32>(view_pos, 1.0)).xyz;

    // Into LAST frame's clip space. `prev_view_projection` is the same matrix
    // SSR's temporal path reprojects through, so the two features cannot
    // disagree about where the camera was.
    let prev_clip = camera_raw.prev_view_projection * vec4<f32>(world_pos, 1.0);
    if (prev_clip.w <= 1e-4) {
        // Behind last frame's eye — no history.
        return vec4<f32>(0.0);
    }
    let prev_ndc = prev_clip.xyz / prev_clip.w;
    let prev_uv = vec2<f32>(prev_ndc.x * 0.5 + 0.5, 0.5 - prev_ndc.y * 0.5);

    // For a standard perspective projection the clip `w` IS the view-space
    // depth, which is what the slice mapping wants. This holds under reverse-Z
    // too: reverse-Z rewrites the z row, never the w row.
    let prev_slice_t = (prev_clip.w - volumetric_params.z_near)
        / max(volumetric_params.z_far - volumetric_params.z_near, 1e-6);

    // Off-screen or outside the volume's depth range last frame: nothing to
    // blend against. Rejecting is what stops the edges of the screen smearing
    // stale medium inward as the camera turns.
    if (prev_uv.x < 0.0 || prev_uv.x > 1.0 || prev_uv.y < 0.0 || prev_uv.y > 1.0
        || prev_slice_t < 0.0 || prev_slice_t > 1.0) {
        return vec4<f32>(0.0);
    }

    // No half-texel fudge needed here, and adding one is a real bug: froxel i's
    // CENTRE sits at slice coordinate s = i + 0.5, so the normalized coordinate
    // is s/n — which is exactly `prev_slice_t`. `textureSampleLevel` then reads
    // texel `prev_slice_t * n - 0.5 = i`. (The composite in the effects pass
    // DOES carry an offset, because the volume it samples stores accumulation
    // to each slice's FAR face rather than a value at its centre.)
    let sample = textureSampleLevel(
        src_volume, history_sampler,
        vec3<f32>(prev_uv, clamp(prev_slice_t, 0.0, 1.0)), 0.0);
    return vec4<f32>(sample.rgb, 1.0);
}
{% endif %}

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

    // FINITE EMITTER RADIUS — the fix for flicker at the fixtures.
    //
    // `inverse_square` guards its denominator at 1e-4, which is fine for
    // surfaces (a surface is never INSIDE a light) and badly wrong for a
    // medium (the fixtures hang in it). A froxel 0.1 m from a 1100-intensity
    // spot samples radiance ~1e5, and the sub-froxel jitter moves that sample
    // several-fold every frame — an unbounded variance the temporal filter
    // cannot average, which is exactly the flicker seen along the truss.
    //
    // Rather than capping the symptom with a magic luminance, give the light
    // the finite radius a real fixture has: rescale 1/max(d^2, 1e-4) into
    // 1/max(d^2, r^2). Outside `r` this is exactly 1.0 and the beams are
    // untouched; inside it the irradiance goes CONSTANT, so the jitter has
    // nothing left to vary. Directionals have no position and are skipped.
    var contribution = sample.radiance;
    if (light.kind != LIGHT_KIND_DIRECTIONAL) {
        let d2 = max(dot(light.position - world_pos, light.position - world_pos), 1e-4);
        contribution *= d2 / max(d2, VOLUMETRIC_LIGHT_RADIUS * VOLUMETRIC_LIGHT_RADIUS);
    }

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

    // Phase: cos between the direction the light TRAVELS (-light_dir, since
    // light_dir points from the froxel toward the light) and the direction the
    // scattered light travels to reach the eye (to_eye, froxel -> eye). With
    // g > 0, a beam propagating toward the camera has cos_theta -> 1 (the HG
    // forward lobe) and flares; negating either vector inverts the knob.
    let cos_theta = dot(-sample.light_dir, to_eye);
    let phase = henyey_greenstein(cos_theta, g);

    return contribution * shadow * phase * volumetric_intensity;
}
