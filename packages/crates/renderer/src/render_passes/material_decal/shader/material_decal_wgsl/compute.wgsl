{% include "shared_wgsl/math.wgsl" %}

// Material decal compute pass.
//
// Per pixel: skip skybox + non-decal-receiver meshes; reconstruct
// world position from depth; iterate every active decal and
// alpha-blend each whose oriented unit cube contains the world
// point. The accumulated result is written to `transparent_tex_out`
// — the preceding opaque→transparent blit already populated that
// texture with the opaque shading, so any pixel we don't touch keeps
// its opaque value.
//
// v1 ships alpha-blend only. The per-decal `blend_mode` u32 stays in
// the layout so additional modes (additive, multiply) can join the
// `select` below without a layout change.

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coords = vec2<i32>(gid.xy);
    let dims = textureDimensions(opaque_tex_in);
    if coords.x >= i32(dims.x) || coords.y >= i32(dims.y) {
        return;
    }

    // This compute writes to a dedicated single-sample
    // `decal_color` (storage RGBA16float); a downstream composite
    // alpha-blits it onto `transparent` (multi- or single-sample).
    // Pixels with no decal hit *must* write 0 so the composite's
    // alpha test correctly skips them (the storage texture isn't
    // cleared between frames).

    // Skybox skip — depth == 1.0 means no geometry. Decals only
    // apply to opaque geometry; sky pixels stay untouched.
    let depth = textureLoad(depth_tex, coords, 0);
    if depth >= 1.0 {
        textureStore(transparent_tex_out, coords, vec4<f32>(0.0));
        return;
    }

    // `receive_decals` per-mesh opt-out. The visibility buffer holds
    // the per-pixel `material_meta_offset`; one extra storage load
    // gives us the flag at no observable cost in practice.
    let vis = textureLoad(visibility_data_tex, coords, 0);
    let tri = join32(vis.x, vis.y);
    if tri == U32_MAX {
        textureStore(transparent_tex_out, coords, vec4<f32>(0.0));
        return;
    }
    let meta_offset = join32(vis.z, vis.w);
    let mesh_meta = material_mesh_metas[meta_offset / META_SIZE_IN_BYTES];
    if mesh_meta.receive_decals == 0u {
        textureStore(transparent_tex_out, coords, vec4<f32>(0.0));
        return;
    }

    // Reconstruct world-space position from depth.
    //
    // NDC.x/y reverse the screen-space-to-clip remap (with WebGPU's
    // y-flip), NDC.z = depth directly (the depth texture stores
    // post-projection NDC z in [0, 1]). `inv_view_proj` lands us in
    // world space; the perspective divide finishes the unprojection.
    let uv = (vec2<f32>(gid.xy) + vec2<f32>(0.5)) / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = vec3<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth);
    let world_h = camera_raw.inv_view_proj * vec4<f32>(ndc, 1.0);
    let world_pos = world_h.xyz / world_h.w;

    // Pick decals from the per-tile bucket instead of the
    // global decals_buffer. Each workgroup is 8×8, so `gid.xy / 8`
    // identifies the tile. The bucket holds at most
    // `bucket_capacity` entries (overflow silently dropped on the
    // classify side).
    let tile_x = gid.x / 8u;
    let tile_y = gid.y / 8u;
    if (tile_x >= decal_buckets.tile_count_x || tile_y >= decal_buckets.tile_count_y) {
        textureStore(transparent_tex_out, coords, vec4<f32>(0.0));
        return;
    }
    let per_tile_stride = 1u + decal_buckets.bucket_capacity;
    let tile_base = (tile_y * decal_buckets.tile_count_x + tile_x) * per_tile_stride;
    let tile_count = min(decal_buckets.payload[tile_base], decal_buckets.bucket_capacity);
    if tile_count == 0u {
        textureStore(transparent_tex_out, coords, vec4<f32>(0.0));
        return;
    }
    let opaque_color = textureLoad(opaque_tex_in, coords, 0);
    var accum = opaque_color;
    var any_decal_hit = false;
    for (var i = 0u; i < tile_count; i = i + 1u) {
        let decal_index = decal_buckets.payload[tile_base + 1u + i];
        let decal = decals_buffer.items[decal_index];
        let local_h = decal.inverse_transform * vec4<f32>(world_pos, 1.0);
        let local = local_h.xyz / local_h.w;
        // Inside-the-oriented-unit-cube test. Decals project down the
        // local -Z axis; the unit-cube extent on each axis is `[-1, 1]`.
        if abs(local.x) > 1.0 || abs(local.y) > 1.0 || abs(local.z) > 1.0 {
            continue;
        }
        // local.xy in [-1, 1] → uv in [0, 1].
        let decal_uv = vec2<f32>(local.x * 0.5 + 0.5, 1.0 - (local.y * 0.5 + 0.5));
        let texel = sample_decal_texture(decal.texture_index, decal_uv);
        let blend_alpha = texel.a * decal.alpha;
        accum = vec4<f32>(mix(accum.rgb, texel.rgb, blend_alpha), accum.a);
        any_decal_hit = true;
    }

    if any_decal_hit {
        // Alpha = 1 marks "decal touched this pixel" for the composite's
        // discard test; the composite only ever writes back rgb.
        textureStore(transparent_tex_out, coords, vec4<f32>(accum.rgb, 1.0));
    } else {
        textureStore(transparent_tex_out, coords, vec4<f32>(0.0));
    }
}

// Sample a decal texture out of the renderer's texture pool by its
// flat `texture_index`. Mirrors the opaque pass's texture-pool
// indexing convention so the same texture handle can be reused for
// PBR base color and decal projection without re-importing.
fn sample_decal_texture(texture_index: u32, uv: vec2<f32>) -> vec4<f32> {
{% if texture_pool_arrays_len > 0 %}
    // Stride = device `max_texture_array_layers` (A.4) — the scene-loader packs
    // `texture_index` with the SAME value (`decal_texture_index_stride`), so a
    // decal texture on any valid pool layer round-trips. (Was a hard-coded `64u`
    // that mis-sampled once a pool array exceeded 64 layers.)
    let layer = texture_index % {{ texture_pool_layers_per_array }}u;
    let array_index = texture_index / {{ texture_pool_layers_per_array }}u;
    // Bilinear sample on mip 0 — decals are small; mipmap selection
    // would need ddx/ddy which compute shaders don't have without
    // workgroup gradient hacks. v1 doesn't bother.
    {% for i in 0..texture_pool_arrays_len %}
        if array_index == {{ i }}u {
            return textureSampleLevel(
                pool_tex_{{ i }},
                pool_sampler_0,
                uv,
                i32(layer),
                0.0,
            );
        }
    {% endfor %}
{% endif %}
    return vec4<f32>(1.0, 0.0, 1.0, 1.0); // magenta = unmapped
}
