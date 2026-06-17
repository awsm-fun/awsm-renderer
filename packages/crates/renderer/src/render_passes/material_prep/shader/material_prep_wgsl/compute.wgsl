// Material prep compute pass (Plan B). Runs once per pixel over the visibility
// buffer, after classify and before per-material shading, materializing the
// material-INDEPENDENT geometry-pool attributes (UV0 + vertex color) so the slim
// per-material kernel reads them instead of recomputing. World position is NOT
// written here (the slim shader keeps the cheap depth-unprojection); shadow
// visibility (stage 3) + the compact edge buffer (stage 5) land later.
//
// `join32` / `U32_MAX` come from math.wgsl; `MaterialMeshMeta` /
// `META_SIZE_IN_BYTES` come from material_mesh_meta.wgsl (included by
// bind_groups.wgsl, concatenated before this).
{% include "shared_wgsl/math.wgsl" %}

// One vertex's UV set, read from the geometry pool at the given float offset
// (= uv_sets_index + set * 2). Mirrors texture_uvs.wgsl::_texture_uv_per_vertex.
// TODO(parity): factor the per-vertex attr fetch into a shared include consumed
// by both kernels.
fn prep_uv_at(data_off: u32, vert: u32, stride: u32, set_float_offset: u32) -> vec2<f32> {
    let o = data_off + vert * stride + set_float_offset;
    return vec2<f32>(visibility_data[o], visibility_data[o + 1u]);
}

// One vertex's COLOR set, at float offset (= color_sets_index + set * 4).
// Mirrors vertex_color_attrib.wgsl::_vertex_color_per_vertex.
fn prep_vcolor_at(data_off: u32, vert: u32, stride: u32, set_float_offset: u32) -> vec4<f32> {
    let o = data_off + vert * stride + set_float_offset;
    return vec4<f32>(visibility_data[o], visibility_data[o + 1u], visibility_data[o + 2u], visibility_data[o + 3u]);
}

@compute @workgroup_size(8, 8)
fn cs_prep(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(uv_out);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coords = vec2<i32>(i32(gid.x), i32(gid.y));

    // Visibility: triangle id + per-mesh meta offset (split u32 via join32).
    let vis = textureLoad(visibility_data_tex, coords, 0);
    let triangle_index = join32(vis.x, vis.y);
    let material_meta_offset = join32(vis.z, vis.w);
    if (triangle_index == U32_MAX) {
        // Sky / no geometry — clear layer 0 (the slim shader never reads prep
        // for sky pixels, so the higher layers can be left untouched).
        textureStore(uv_out, coords, 0, vec4<f32>(0.0));
        textureStore(vcolor_out, coords, 0, vec4<f32>(0.0));
        return;
    }

    let mesh_meta = material_mesh_metas[material_meta_offset / META_SIZE_IN_BYTES];
    let stride = mesh_meta.vertex_attribute_stride / 4u;
    let idx_off = mesh_meta.vertex_attribute_indices_offset / 4u;
    let data_off = mesh_meta.vertex_attribute_data_offset / 4u;
    let uv_sets_index = mesh_meta.uv_sets_index;
    let color_sets_index = mesh_meta.color_sets_index;

    // Triangle vertex indices (bitcast f32 words → u32).
    let base_tri = idx_off + triangle_index * 3u;
    let ti = vec3<u32>(
        bitcast<u32>(visibility_data[base_tri]),
        bitcast<u32>(visibility_data[base_tri + 1u]),
        bitcast<u32>(visibility_data[base_tri + 2u]),
    );

    // Barycentric weights (same unpack as cs_opaque).
    let bary_raw = textureLoad(barycentric_tex, coords, 0);
    let bary_xy = vec2<f32>(f32(bary_raw.x), f32(bary_raw.y)) / 65535.0;
    let bary = vec3<f32>(bary_xy.x, bary_xy.y, 1.0 - bary_xy.x - bary_xy.y);

    // UV sets — materialize every present set into its own array layer
    // (clamped to the cap; sets beyond the cap are never written and clamp to
    // the last layer on read). `set * 2` floats per UV set within the packed
    // per-vertex block, starting at `uv_sets_index`.
    let uv_count = min(mesh_meta.uv_set_count, {{ max_prep_uv_sets }}u);
    for (var s: u32 = 0u; s < uv_count; s = s + 1u) {
        let off = uv_sets_index + s * 2u;
        let a = prep_uv_at(data_off, ti.x, stride, off);
        let b = prep_uv_at(data_off, ti.y, stride, off);
        let c = prep_uv_at(data_off, ti.z, stride, off);
        let uv = bary.x * a + bary.y * b + bary.z * c;
        textureStore(uv_out, coords, i32(s), vec4<f32>(uv, 0.0, 0.0));
    }

    // Vertex color sets — same per-layer materialization (`set * 4` floats per
    // color set, starting at `color_sets_index`).
    let color_count = min(mesh_meta.color_set_count, {{ max_prep_color_sets }}u);
    for (var s: u32 = 0u; s < color_count; s = s + 1u) {
        let off = color_sets_index + s * 4u;
        let a = prep_vcolor_at(data_off, ti.x, stride, off);
        let b = prep_vcolor_at(data_off, ti.y, stride, off);
        let c = prep_vcolor_at(data_off, ti.z, stride, off);
        let vc = bary.x * a + bary.y * b + bary.z * c;
        textureStore(vcolor_out, coords, i32(s), vc);
    }

{% if shadows %}
    // ── Per-pixel shadow visibility (Plan B Stage 3b) ───────────────────────
    // Walk the canonical froxel order (froxel_walk.wgsl SSOT) and, for each
    // shadowed light, sample its shadow map EXACTLY as `apply_lighting_per_froxel`
    // does (directional incl. `* apply_sscs(...)`, punctual WITHOUT sscs). Pack 4
    // visibility slots per Rgba8unorm texel (slot j -> layer j/4, channel j%4),
    // clamped to K. `receive_shadows` is NOT applied here (Stage 4 applies it at
    // read time so the slot model stays material-independent). INERT: nobody
    // reads this buffer yet.

    // World position reconstructed from depth (NOT materialized — decision #2).
    let cam = camera_from_raw(camera_raw);
    let depth = textureLoad(depth_tex, coords, 0);
    let pix_uv = (vec2<f32>(coords) + vec2<f32>(0.5, 0.5))
        / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = vec3<f32>(pix_uv.x * 2.0 - 1.0, 1.0 - pix_uv.y * 2.0, depth);
    let view_h = cam.inv_proj * vec4<f32>(ndc, 1.0);
    let world_pos = (cam.inv_view * vec4<f32>(view_h.xyz / max(view_h.w, 1e-8), 1.0)).xyz;
    let view_z = -(cam.view * vec4<f32>(world_pos, 1.0)).z;

    let nt = unpack_normal_tangent(textureLoad(normal_tangent_tex, coords, 0));
    let normal = nt.N;
    let pixel_xy = vec2<f32>(f32(coords.x), f32(coords.y));

    var slot: u32 = 0u;
    var acc = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    let k = {{ max_shadow_casters }}u;

    // Directional prefix.
    let n_dir = get_n_directional();
    for (var d = 0u; d < n_dir; d = d + 1u) {
        if (slot >= k) { break; }
        let light = get_light(get_directional_light_index(d));
        if (light.shadow_index != SHADOW_INDEX_NONE) {
            let ls = light_sample(light, normal, world_pos);
            var v = sample_shadow_directional(
                light.shadow_index,
                world_pos,
                shadow_normal_toward_light(normal, ls.light_dir),
                view_z,
            );
            v = v * apply_sscs(world_pos, normalize(-light.direction));
            acc[slot % 4u] = v;
            if (slot % 4u == 3u) {
                textureStore(shadow_visibility_out, coords, i32(slot / 4u), acc);
                acc = vec4<f32>(1.0, 1.0, 1.0, 1.0);
            }
            slot = slot + 1u;
        }
    }

    // Per-froxel punctual.
    let froxel_base = froxel_base_for_pixel(pixel_xy, view_z);
    let froxel_count = froxel_light_count(froxel_base);
    for (var i = 0u; i < froxel_count; i = i + 1u) {
        if (slot >= k) { break; }
        let li = lights_storage[froxel_base + 1u + i];
        let light = get_light(li);
        if (light.kind == 1u) { continue; }
        if (light.shadow_index != SHADOW_INDEX_NONE) {
            let ls = light_sample(light, normal, world_pos);
            let v = sample_shadow_directional(
                light.shadow_index,
                world_pos,
                shadow_normal_toward_light(normal, ls.light_dir),
                view_z,
            );
            acc[slot % 4u] = v;
            if (slot % 4u == 3u) {
                textureStore(shadow_visibility_out, coords, i32(slot / 4u), acc);
                acc = vec4<f32>(1.0, 1.0, 1.0, 1.0);
            }
            slot = slot + 1u;
        }
    }

    // Flush a partial layer (when the final slot didn't fall on a 4-boundary).
    if (slot % 4u != 0u) {
        textureStore(shadow_visibility_out, coords, i32(slot / 4u), acc);
    }
{% endif %}
}
