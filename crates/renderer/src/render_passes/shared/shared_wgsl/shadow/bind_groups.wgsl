// Shadow bind-group declarations. The bind-group slot is supplied by
// the containing template via `shadow_group_index` — opaque uses slot
// 3. Transparent is deferred to Phase 9.
//
// Bindings 0..=7 must stay in lockstep with
// `shared::material::bind_group::shadow_bind_group_layout_entries`.

const MAX_SHADOW_DESCRIPTORS: u32 = 32u;

struct ShadowDescriptor {
    // Light-space view-projection used at sample time.
    view_projection: mat4x4<f32>,
    // (atlas.x, atlas.y, atlas.w, atlas.h) in normalised UV space.
    atlas_rect: vec4<f32>,
    // (depth_bias, normal_bias, hardness, pcss_penumbra_scale)
    bias_params: vec4<f32>,
    // (split_far_view_z, cascade_index, cascade_count_in_light, evsm_flag)
    // `evsm_flag` is 1.0 when this cascade should sample EVSM moments
    // from `evsm_atlas` instead of the PCF depth atlas. Phase 5 lands
    // the flag + sample-site dispatch; the moment-write compute pass
    // and Gaussian blur are deferred — until they land, EVSM cascades
    // transparently fall back to PCF sampling on `shadow_atlas`.
    cascade_info: vec4<f32>,
};

struct ShadowGlobals {
    // (atlas.w, atlas.h, evsm.w, evsm.h)
    atlas_sizes: vec4<f32>,
    // (evsm_exponent, evsm_blur_radius, sscs_step_count, sscs_enabled)
    evsm_sscs: vec4<f32>,
    // (debug_cascade_colors, max_point_shadows, pad, pad)
    flags: vec4<u32>,
};

struct ShadowDescriptorArray {
    items: array<ShadowDescriptor, MAX_SHADOW_DESCRIPTORS>,
};

@group({{ shadow_group_index }}) @binding(0) var shadow_atlas: texture_depth_2d;
@group({{ shadow_group_index }}) @binding(1) var shadow_atlas_sampler: sampler_comparison;
@group({{ shadow_group_index }}) @binding(2) var shadow_cube_array: texture_depth_cube_array;
@group({{ shadow_group_index }}) @binding(3) var shadow_cube_sampler: sampler_comparison;
@group({{ shadow_group_index }}) @binding(4) var evsm_atlas: texture_2d<f32>;
@group({{ shadow_group_index }}) @binding(5) var evsm_atlas_sampler: sampler;
@group({{ shadow_group_index }}) @binding(6) var<uniform> shadow_globals: ShadowGlobals;
@group({{ shadow_group_index }}) @binding(7) var<uniform> shadow_descriptors: ShadowDescriptorArray;

// Sentinel for "no shadow" — packed into `LightPacked.row4.z`.
const SHADOW_INDEX_NONE: u32 = 0xFFFFFFFFu;

// 16 Poisson-distributed samples in `[-1, 1]^2`. Used by both the
// PCSS blocker search and the variable-kernel PCF pass. The same
// table doubled-up keeps the WGSL small; a per-pixel rotation breaks
// up the regular pattern.
const POISSON_DISK_16: array<vec2<f32>, 16> = array<vec2<f32>, 16>(
    vec2<f32>(-0.94201624, -0.39906216),
    vec2<f32>( 0.94558609, -0.76890725),
    vec2<f32>(-0.09418410, -0.92938870),
    vec2<f32>( 0.34495938,  0.29387760),
    vec2<f32>(-0.91588581,  0.45771432),
    vec2<f32>(-0.81544232, -0.87912464),
    vec2<f32>(-0.38277543,  0.27676845),
    vec2<f32>( 0.97484398,  0.75648379),
    vec2<f32>( 0.44323325, -0.97511554),
    vec2<f32>( 0.53742981, -0.47373420),
    vec2<f32>(-0.26496911, -0.41893023),
    vec2<f32>( 0.79197514,  0.19090188),
    vec2<f32>(-0.24188840,  0.99706507),
    vec2<f32>(-0.81409955,  0.91437590),
    vec2<f32>( 0.19984126,  0.78641367),
    vec2<f32>( 0.14383161, -0.14100790),
);

// Inter-leaved Gradient Noise — Jorge Jimenez's hash, returns a
// per-pixel angle in `[0, 2π]`. Used to rotate the Poisson disk so
// adjacent fragments don't sample identical patterns.
fn pcss_disk_angle(coords: vec2<f32>) -> f32 {
    let magic = vec3<f32>(0.06711056, 0.00583715, 52.9829189);
    let noise = fract(magic.z * fract(dot(coords, magic.xy)));
    return noise * 6.2831853;
}

fn pcss_rotate(v: vec2<f32>, sin_a: f32, cos_a: f32) -> vec2<f32> {
    return vec2<f32>(v.x * cos_a - v.y * sin_a, v.x * sin_a + v.y * cos_a);
}

// Screen-space contact shadows (SSCS). Short ray-march in view space
// from `world_pos` toward `light_dir` (the surface→light direction),
// using the already-bound depth buffer (`depth_tex`). Returns `[0, 1]`
// visibility — multiplied into the main shadow term to darken micro-
// occluders that the shadow map misses (gaps under feet, hair, etc.).
//
// `shadow_globals.evsm_sscs.w` is the master enable; `.z` is the step
// count. Phase 10 ships single-sample depth reads even when the
// geometry pass was rendered with MSAA (we read sample 0).
fn apply_sscs(world_pos: vec3<f32>, light_dir: vec3<f32>) -> f32 {
    let enabled = shadow_globals.evsm_sscs.w;
    if enabled < 0.5 {
        return 1.0;
    }
    let steps = u32(max(shadow_globals.evsm_sscs.z, 1.0));
    if steps == 0u {
        return 1.0;
    }
    // 5cm world-space steps. The total reach is `steps * step_len`
    // (e.g. 16 * 0.05 = 0.8 m) which matches the scale Drobot 2017
    // proposes for contact shadows.
    let step_len = 0.05;
    let step_world = light_dir * step_len;
    let viewport_size = camera_raw.viewport.zw;
    let depth_dim = vec2<i32>(viewport_size);

    var occluded: f32 = 0.0;
    var ray = world_pos;
    for (var i: u32 = 0u; i < steps; i = i + 1u) {
        ray = ray + step_world;
        let clip = camera_raw.view_proj * vec4<f32>(ray, 1.0);
        if clip.w <= 0.0 {
            continue;
        }
        let ndc = clip.xyz / clip.w;
        if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
            continue;
        }
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);
        let px = clamp(
            vec2<i32>(uv * viewport_size),
            vec2<i32>(0, 0),
            depth_dim - vec2<i32>(1, 1),
        );
        {% if multisampled_geometry %}
            let scene_depth = textureLoad(depth_tex, px, 0);
        {% else %}
            let scene_depth = textureLoad(depth_tex, px, 0);
        {% endif %}
        let ray_depth = ndc.z;
        // Hit window: scene depth is closer to the camera than the
        // ray by a small margin (`thickness_min`) but not by more than
        // `thickness_max` — the latter prevents distant geometry
        // behind the ray from registering as an occluder.
        let thickness_min = 0.0005;
        let thickness_max = 0.02;
        if scene_depth < ray_depth - thickness_min && scene_depth > ray_depth - thickness_max {
            occluded = occluded + 1.0;
            break;
        }
    }
    if occluded > 0.0 {
        return 0.0;
    }
    return 1.0;
}

// Cube near plane — MUST match the value used in `Mat4::perspective_rh`
// for cube face generation in `Shadows::write_gpu`.
const POINT_SHADOW_NEAR: f32 = 0.05;

// Point-light cube shadow sample.
//
// Each cube face stores perspective NDC.z written by the rasterizer
// (90° FOV, `near = POINT_SHADOW_NEAR`, `far = light_range`). The
// projection is post-multiplied by a Y-flip on the writer side so the
// rasterized image lines up with WebGPU's D3D-style cube sampling
// convention (texel `t=0` → world +Y on the +X face, etc.) — see
// `Shadows::write_gpu`. That flip doesn't change NDC.z, so the depth
// formula below stays the same on both sides.
//
// The receiver recreates that NDC.z by projecting `length(light, P)`
// onto the *dominant* cube axis of the light-to-surface direction:
//
//     view_depth = distance(light, P) · |dir.major|
//     ndc_z      = (far / (far - near)) · (1 - near / view_depth)
//
// Same formula generates both the rasterized atlas value and the
// receiver reference, so they compare directly — no linear-depth FS
// override, no per-tap face recompute, no seam math.
fn sample_shadow_cube(desc: ShadowDescriptor, world_pos: vec3<f32>, world_normal: vec3<f32>) -> f32 {
    let light_pos = desc.atlas_rect.xyz;
    let range = max(desc.atlas_rect.w, 0.01);
    let slot = i32(desc.cascade_info.y);

    let biased_pos = world_pos + world_normal * desc.bias_params.y;
    let light_to_surface = biased_pos - light_pos;
    let dist = length(light_to_surface);
    if dist >= range {
        return 1.0;
    }
    let dir = light_to_surface / max(dist, 1e-4);

    // Major-axis (cube-face) projected depth.
    let abs_d = abs(dir);
    let major = max(abs_d.x, max(abs_d.y, abs_d.z));
    let view_depth = dist * max(major, 1e-4);

    // Same perspective NDC.z formula as the rasterizer.
    let near = POINT_SHADOW_NEAR;
    let ndc_z = (range / (range - near)) * (1.0 - near / max(view_depth, near));

    // Slope-aware constant bias. Tight enough not to Peter-Pan
    // perpendicular surfaces; wide enough that grazing-angle plane
    // self-shadow doesn't reappear at the falloff radius.
    let n_dot_dir = abs(dot(dir, world_normal));
    let bias = max(desc.bias_params.x, 0.001) / max(n_dot_dir, 0.05);
    let ref_depth = clamp(ndc_z, 0.0, 1.0) - bias;
    let hardness = desc.bias_params.z;

    if hardness < 0.5 {
        return textureSampleCompareLevel(
            shadow_cube_array,
            shadow_cube_sampler,
            dir,
            slot,
            ref_depth,
        );
    }

    // Soft PCF: receiver-space jitter on the surface's tangent plane.
    // Each tap recomputes its own direction and NDC.z so a flat
    // receiver doesn't self-shadow into a kernel-shaped patch.
    let abs_n = abs(world_normal);
    let up_hint = select(
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(1.0, 0.0, 0.0),
        abs_n.y > 0.99,
    );
    let tangent = normalize(cross(up_hint, world_normal));
    let bitangent = cross(world_normal, tangent);

    let soft_world_radius = 0.02;
    let angle = pcss_disk_angle(
        biased_pos.xz * 137.0 + vec2<f32>(biased_pos.y * 31.0, biased_pos.y * 17.0),
    );
    let sin_a = sin(angle);
    let cos_a = cos(angle);

    // 8-tap rotated Poisson is the AAA-perf sweet spot for point
    // shadows. Each tap is already a 2x2 bilinear hardware compare,
    // so 8 calls ≈ 32 effective samples — visually close to 16-tap
    // while halving the texture bandwidth.
    var sum = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * soft_world_radius;
        let tap_pos = biased_pos + tangent * off.x + bitangent * off.y;
        let tap_to_light = tap_pos - light_pos;
        let tap_dist = length(tap_to_light);
        let tap_dir = tap_to_light / max(tap_dist, 1e-4);
        let tap_abs = abs(tap_dir);
        let tap_major = max(tap_abs.x, max(tap_abs.y, tap_abs.z));
        let tap_view_depth = tap_dist * max(tap_major, 1e-4);
        let tap_ndc_z =
            (range / (range - near)) * (1.0 - near / max(tap_view_depth, near));
        let tap_n_dot_dir = abs(dot(tap_dir, world_normal));
        let tap_bias = max(desc.bias_params.x, 0.001) / max(tap_n_dot_dir, 0.05);
        let tap_ref = clamp(tap_ndc_z, 0.0, 1.0) - tap_bias;
        sum += textureSampleCompareLevel(
            shadow_cube_array,
            shadow_cube_sampler,
            tap_dir,
            slot,
            tap_ref,
        );
    }
    return sum / 8.0;
}

// Sample a single shadow descriptor (cascade / spot / face). Returns
// `[0, 1]` visibility (1.0 = lit, 0.0 = fully shadowed).
//
// Hardness branches:
//   0.0 = Hard, 1-tap.
//   1.0 = Soft, 3x3 PCF.
//   2.0 = PCSS (phase 6 — currently falls back to Soft).
fn sample_shadow_descriptor(
    descriptor_index: u32,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
) -> f32 {
    if descriptor_index >= MAX_SHADOW_DESCRIPTORS {
        return 1.0;
    }
    let desc = shadow_descriptors.items[descriptor_index];
    // cascade_info.w encodes the descriptor kind:
    //   0.0 = 2D PCF (directional cascade / spot)
    //   1.0 = 2D EVSM cascade (falls back to PCF until the moment
    //         writer lands — phase 5 deferred)
    //   2.0 = cube (point light)
    let kind = desc.cascade_info.w;
    if kind > 1.5 {
        return sample_shadow_cube(desc, world_pos, world_normal);
    }
    // EVSM cascades currently fall through to PCF — the moment write
    // pass / blur compute aren't online yet. The `cascade_info.w` flag
    // is preserved for the phase-5 follow-up that swaps the sample
    // call site to `sample_shadow_evsm`.

    // Offset the receiver along its surface normal by `normal_bias`
    // world-space units before projecting into shadow space. This
    // pushes the sample point *toward* the light, which is how we
    // dodge acne on slanted surfaces without relying solely on a
    // constant depth bias (cascade Z-ranges differ a lot, so a flat
    // depth bias is either too soft or too aggressive). The
    // pipeline's slope-scale bias and `bias_params.x` depth bias
    // handle the residual.
    let biased_pos = world_pos + world_normal * desc.bias_params.y;
    let clip = desc.view_projection * vec4<f32>(biased_pos, 1.0);
    if clip.w <= 0.0 {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let uv_local = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);
    let atlas_uv = desc.atlas_rect.xy + uv_local * desc.atlas_rect.zw;
    let ref_depth = ndc.z - desc.bias_params.x;
    let hardness = desc.bias_params.z;

    // PCF / PCSS taps must stay inside this cascade's tile of the
    // atlas. The tile-pack allocator places cascades edge-to-edge,
    // so a kernel that crosses the boundary samples a totally
    // unrelated cascade's depth (or another light's spot tile) and
    // produces a fringe of bogus shadow at the tile seam. The inset
    // is half a texel so bilinear PCF taps don't read past the edge
    // either.
    let inv_atlas = vec2<f32>(
        1.0 / shadow_globals.atlas_sizes.x,
        1.0 / shadow_globals.atlas_sizes.y,
    );
    let tile_min = desc.atlas_rect.xy + 0.5 * inv_atlas;
    let tile_max = desc.atlas_rect.xy + desc.atlas_rect.zw - 0.5 * inv_atlas;

    if hardness < 0.5 {
        return textureSampleCompareLevel(
            shadow_atlas,
            shadow_atlas_sampler,
            clamp(atlas_uv, tile_min, tile_max),
            ref_depth,
        );
    }
    if hardness < 1.5 {
        // Tap-rotated 16-sample Poisson disk PCF. The kernel is
        // sized in *world units* (`SOFT_WORLD_RADIUS`) and the
        // per-cascade texel-radius is recovered by dividing by the
        // cascade's `world_per_texel` (stored in `cascade_info.y`).
        // That keeps the perceived soft-edge width identical in every
        // cascade — without this, the near cascade's 2048 texels
        // covering a tiny world span produces razor-sharp shadows
        // while the far cascade's same 2048 texels covering a much
        // larger span produces soft ones, and the boundary between
        // is visible as a step in penumbra width.
        let world_per_texel = max(desc.cascade_info.y, 1e-4);
        let soft_world_radius = 0.06; // ≈ 6 cm penumbra at default light angles
        // Clamp at 2 texels minimum — anything tighter and the far
        // cascade reads from a single texel cluster (no PCF blur),
        // which makes the close→far cascade boundary look like a
        // step from "soft" to "razor". 6 texels max so the near
        // cascade doesn't burn a giant kernel that costs but barely
        // benefits at sub-millimetre world width.
        let radius_texels = clamp(soft_world_radius / world_per_texel, 2.0, 6.0);

        let angle = pcss_disk_angle(atlas_uv * shadow_globals.atlas_sizes.xy);
        let sin_a = sin(angle);
        let cos_a = cos(angle);
        var sum = 0.0;
        for (var i = 0u; i < 16u; i = i + 1u) {
            let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * radius_texels;
            sum += textureSampleCompareLevel(
                shadow_atlas, shadow_atlas_sampler,
                clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
                ref_depth,
            );
        }
        return sum / 16.0;
    }
    // PCSS — blocker-search + variable-kernel PCF. `pcss_scale`
    // (bias_params.w) tunes both the search radius and the apparent
    // light-source size.
    let pcss_scale = max(desc.bias_params.w, 0.01);
    let atlas_uv_to_texels = vec2<f32>(
        shadow_globals.atlas_sizes.x,
        shadow_globals.atlas_sizes.y,
    );
    let atlas_pixel = atlas_uv * atlas_uv_to_texels;
    let angle = pcss_disk_angle(atlas_pixel);
    let sin_a = sin(angle);
    let cos_a = cos(angle);
    let search_radius_texels = 3.0 * pcss_scale;

    var blocker_sum = 0.0;
    var blocker_count = 0u;
    let tile_min_px = vec2<i32>(tile_min * atlas_uv_to_texels);
    let tile_max_px = vec2<i32>(tile_max * atlas_uv_to_texels);
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * search_radius_texels;
        let sample_uv = atlas_uv + off * inv_atlas;
        let coord = vec2<i32>(sample_uv * atlas_uv_to_texels);
        // Clamp to the cascade's own tile so the blocker search
        // doesn't read from an adjacent cascade's depth values.
        let c = clamp(coord, tile_min_px, tile_max_px);
        let d = textureLoad(shadow_atlas, c, 0);
        if d < ref_depth - 0.0005 {
            blocker_sum = blocker_sum + d;
            blocker_count = blocker_count + 1u;
        }
    }
    if blocker_count == 0u {
        return 1.0; // fully lit fast path
    }
    let avg_blocker = blocker_sum / f32(blocker_count);
    let light_size_texels = 5.0 * pcss_scale;
    let penumbra_texels = max(
        (ref_depth - avg_blocker) * light_size_texels / max(avg_blocker, 1e-4),
        1.0,
    );
    var pcf_sum = 0.0;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * penumbra_texels;
        pcf_sum = pcf_sum + textureSampleCompareLevel(
            shadow_atlas,
            shadow_atlas_sampler,
            clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
            ref_depth,
        );
    }
    return pcf_sum / 16.0;
}

// Per-light cascade selection with smooth blending across split
// boundaries. `descriptor_base` points to the first cascade descriptor
// of a directional light; `cascade_info.z` gives the cascade count.
//
// We walk descriptors descriptor_base..base+count and pick the first
// whose `cascade_info.x` (split_far in world-space depth) exceeds
// `view_z`. To hide the abrupt softness jump that comes from each
// successive cascade halving its atlas resolution, the last
// `CASCADE_BLEND` fraction of every cascade's depth range linearly
// fades into the next cascade's sample (or to fully lit for the
// final cascade — receivers past the very end get no shadow).
//
// Returns 1.0 (no shadow) if `view_z` is beyond the last cascade.
// Fraction of each cascade's depth range that fades into the next
// cascade. Stretching this band wider spreads the (unavoidable)
// quality difference between cascades across a larger area, which
// the eye stops reading as a hard edge. 50% is the AAA default —
// the corresponding `BLEND_OVERLAP` in `fit_cascades` ensures the
// next cascade's frustum actually covers this whole band.
const CASCADE_BLEND: f32 = 0.5;

fn sample_shadow_directional(
    descriptor_base: u32,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
    view_z: f32,
) -> f32 {
    if descriptor_base == SHADOW_INDEX_NONE {
        return 1.0;
    }
    if descriptor_base >= MAX_SHADOW_DESCRIPTORS {
        return 1.0;
    }
    let cascade_count = u32(shadow_descriptors.items[descriptor_base].cascade_info.z);
    var picked: u32 = SHADOW_INDEX_NONE;
    var picked_local: u32 = 0u;
    for (var i = 0u; i < cascade_count; i = i + 1u) {
        let idx = descriptor_base + i;
        if idx >= MAX_SHADOW_DESCRIPTORS {
            break;
        }
        let split_far = shadow_descriptors.items[idx].cascade_info.x;
        if view_z <= split_far {
            picked = idx;
            picked_local = i;
            break;
        }
    }
    if picked == SHADOW_INDEX_NONE {
        return 1.0;
    }
    let split_far = shadow_descriptors.items[picked].cascade_info.x;
    var split_near: f32 = 0.0;
    if picked_local > 0u {
        split_near = shadow_descriptors.items[picked - 1u].cascade_info.x;
    }
    let span = max(split_far - split_near, 1e-4);
    let normalized = clamp((view_z - split_near) / span, 0.0, 1.0);

    let primary = sample_shadow_descriptor(picked, world_pos, world_normal);
    if normalized < 1.0 - CASCADE_BLEND {
        return primary;
    }
    let blend_t = (normalized - (1.0 - CASCADE_BLEND)) / CASCADE_BLEND;
    let next_local = picked_local + 1u;
    if next_local >= cascade_count {
        // Final cascade fades to fully lit at the very edge of the
        // light's max_distance so receivers don't pop from shadowed
        // to lit.
        return mix(primary, 1.0, blend_t);
    }
    let next_idx = descriptor_base + next_local;
    if next_idx >= MAX_SHADOW_DESCRIPTORS {
        return primary;
    }
    let secondary = sample_shadow_descriptor(next_idx, world_pos, world_normal);
    return mix(primary, secondary, blend_t);
}

// DEBUG: returns the picked cascade index (0..3) as a float, or
// 4.0 if no cascade was picked. Used for debug tinting.
fn debug_picked_cascade(
    descriptor_base: u32,
    view_z: f32,
) -> f32 {
    if descriptor_base == SHADOW_INDEX_NONE || descriptor_base >= MAX_SHADOW_DESCRIPTORS {
        return 4.0;
    }
    let cascade_count = u32(shadow_descriptors.items[descriptor_base].cascade_info.z);
    for (var i = 0u; i < cascade_count; i = i + 1u) {
        let idx = descriptor_base + i;
        if idx >= MAX_SHADOW_DESCRIPTORS {
            break;
        }
        if view_z <= shadow_descriptors.items[idx].cascade_info.x {
            return f32(i);
        }
    }
    return 4.0;
}

// Debug-overlay tint for cascade visualisation. Driven by
// `shadow_globals.flags.x` (`debug_cascade_colors`). Returns the
// cascade-tinted color if enabled, otherwise the input unchanged.
fn debug_cascade_tint(
    base_color: vec3<f32>,
    descriptor_base: u32,
    view_z: f32,
) -> vec3<f32> {
    if shadow_globals.flags.x == 0u {
        return base_color;
    }
    if descriptor_base == SHADOW_INDEX_NONE || descriptor_base >= MAX_SHADOW_DESCRIPTORS {
        return base_color;
    }
    let cascade_count = u32(shadow_descriptors.items[descriptor_base].cascade_info.z);
    var picked_idx: u32 = cascade_count;
    for (var i = 0u; i < cascade_count; i = i + 1u) {
        let idx = descriptor_base + i;
        if idx >= MAX_SHADOW_DESCRIPTORS {
            break;
        }
        if view_z <= shadow_descriptors.items[idx].cascade_info.x {
            picked_idx = i;
            break;
        }
    }
    let palette = array<vec3<f32>, 4>(
        vec3<f32>(1.0, 0.4, 0.4),
        vec3<f32>(0.4, 1.0, 0.4),
        vec3<f32>(0.4, 0.5, 1.0),
        vec3<f32>(1.0, 1.0, 0.4),
    );
    if picked_idx >= 4u {
        return base_color;
    }
    return mix(base_color, palette[picked_idx], 0.35);
}
