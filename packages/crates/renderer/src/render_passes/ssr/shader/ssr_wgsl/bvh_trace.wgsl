// SSR software-BVH trace (docs/plans/bvh-reflections.md — Tier 7).
//
// Runs BEFORE the screen-space trace. For every NEAR-MIRROR pixel
// (spread < BVH_SPREAD_MAX and a live reflection descriptor) it traces ONE
// world-space reflection ray against the software BVH — BLAS per static
// mesh + a linear TLAS instance scan — and stores (constrained hit color,
// 1.0) into the `ssr_bvh` target, or (0,0,0,0) on a miss. The screen-space
// trace then uses this as its MISS FALLBACK (trace.wgsl's bvh template
// block): real off-screen geometry replaces the probe/env approximation for
// exactly the rays SSR cannot see, and the SSR hit path is untouched (an
// on-screen hit is always better data than the constrained shading here).
//
// Hit shading is deliberately CONSTRAINED (the plan's rule): premultiplied
// instance emissive + a dim probe-projected env term. No recursion, no
// punctual loops, no shadow rays. In an emissive-lit scene (the arena) that
// IS the reflected radiance; matte surfaces read as their dark selves.
//
// Rays are traced in OBJECT space per instance (origin/direction transformed
// by inv_world, direction left UNNORMALIZED) so the hit parameter `t` is
// shared across spaces and instances — nearest-t comparison needs no
// per-instance rescaling.

// CameraRaw + camera_from_raw (view / inv_view / inv_proj).
{% include "shared_wgsl/camera.wgsl" %}
// unpack_normal_tangent + box_project_env_dir.
{% include "shared_wgsl/math.wgsl" %}

// Eligibility: only near-mirror pixels get software rays (rougher pixels'
// cone blur hides the probe approximation — the fallback is fine there).
const BVH_SPREAD_MAX: f32 = 0.1;
// Matches the glossy HDR clamp in trace.wgsl: reflected luminance is capped
// so a bloom-hot emissive hit can't bloom AGAIN through the reflection.
const BVH_LUM_CLAMP: f32 = 3.0;

// Live tuning uniforms — layout MUST match `SsrParams` in trace.wgsl
// (they bind the same buffer). 80 bytes / 20xf32.
struct SsrParams {
    intensity: f32,
    max_distance: f32,
    thickness: f32,
    max_steps: f32,
    spread_cutoff: f32,
    edge_fade: f32,
    temporal_weight: f32,
    frame: f32,
    probe_center_enabled: vec4<f32>,
    probe_half_pad: vec4<f32>,
    // x = TLAS instance count, yzw pad.
    bvh_meta: vec4<f32>,
};

// 32-byte node, layouts match bvh.rs: leaf when (a & 0x80000000) != 0 —
// then first_tri = a & 0x7fffffff (LOCAL tri index), tri_count = b; else
// a/b = LOCAL child node indices.
struct BvhNode {
    min: vec3<f32>,
    a: u32,
    max: vec3<f32>,
    b: u32,
};

// 112-byte instance, layout matches bvh.rs::push_instance.
struct BvhInstance {
    inv_world: mat4x4<f32>,
    emissive: vec4<f32>,
    // xyz = world AABB min; w = bitcast<f32>(node base, in NODE elements)
    world_min: vec4<f32>,
    // xyz = world AABB max; w = bitcast<f32>(tri base, in VEC4 elements)
    world_max: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var<uniform> params: SsrParams;
{% if multisampled_geometry %}
@group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;
@group(0) @binding(3) var normal_tangent_tex: texture_multisampled_2d<f32>;
{% else %}
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var normal_tangent_tex: texture_2d<f32>;
{% endif %}
@group(0) @binding(4) var reflection_descriptor_tex: texture_2d<f32>;
@group(0) @binding(5) var out_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(6) var<storage, read> tlas: array<BvhInstance>;
@group(0) @binding(7) var<storage, read> bvh_nodes: array<BvhNode>;
@group(0) @binding(8) var<storage, read> bvh_tris: array<vec4<f32>>;
@group(0) @binding(9) var env_tex: texture_cube<f32>;
@group(0) @binding(10) var env_sampler: sampler;

fn view_pos_from_depth(uv: vec2<f32>, depth: f32, cam: Camera) -> vec3<f32> {
    let ndc = vec3<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth);
    let v = cam.inv_proj * vec4<f32>(ndc, 1.0);
    return v.xyz / v.w;
}

// Slab test against an AABB; returns (t_enter, t_exit) — hit when
// t_enter <= t_exit && t_exit >= 0. `inv_d` precomputed by the caller.
fn slab(mn: vec3<f32>, mx: vec3<f32>, ro: vec3<f32>, inv_d: vec3<f32>) -> vec2<f32> {
    let t0 = (mn - ro) * inv_d;
    let t1 = (mx - ro) * inv_d;
    let tmin = min(t0, t1);
    let tmax = max(t0, t1);
    let enter = max(max(tmin.x, tmin.y), tmin.z);
    let exit = min(min(tmax.x, tmax.y), tmax.z);
    return vec2<f32>(enter, exit);
}

// Möller–Trumbore, no backface cull (open shells must reflect from both
// sides). Returns t (in the caller's ray parameter) or -1.0.
fn tri_hit(ro: vec3<f32>, rd: vec3<f32>, v0: vec3<f32>, v1: vec3<f32>, v2: vec3<f32>) -> f32 {
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let p = cross(rd, e1);
    let det = dot(e2, p);
    if (abs(det) < 1e-9) {
        return -1.0;
    }
    let inv_det = 1.0 / det;
    let tv = ro - v0;
    let u = dot(tv, p) * inv_det;
    if (u < 0.0 || u > 1.0) {
        return -1.0;
    }
    let q = cross(tv, e2);
    let v = dot(rd, q) * inv_det;
    if (v < 0.0 || u + v > 1.0) {
        return -1.0;
    }
    return dot(e1, q) * inv_det;
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_dims = textureDimensions(out_tex);
    let coords = vec2<i32>(gid.xy);
    if (coords.x >= i32(out_dims.x) || coords.y >= i32(out_dims.y)) {
        return;
    }
    let uv = (vec2<f32>(coords) + vec2<f32>(0.5)) / vec2<f32>(out_dims);
    let full_dims = textureDimensions(depth_tex);
    let fcoords = vec2<i32>(uv * vec2<f32>(full_dims));

    let cam = camera_from_raw(camera_raw);
    let depth = textureLoad(depth_tex, fcoords, 0);
    {% if reverse_z %}
    let is_sky = depth <= 0.0;
    {% else %}
    let is_sky = depth >= 1.0;
    {% endif %}

    let descriptor = textureLoad(reflection_descriptor_tex, fcoords, 0);
    let reflectivity = descriptor.rgb;
    let spread = descriptor.a;
    let reflect_strength = max(reflectivity.r, max(reflectivity.g, reflectivity.b));
    if (is_sky || reflect_strength < (1.0 / 255.0) || spread > BVH_SPREAD_MAX) {
        textureStore(out_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 0.0));
        return;
    }

    // World-space mirror ray from the shaded surface.
    let p_view = view_pos_from_depth(uv, depth, cam);
    let world_pos = (cam.inv_view * vec4<f32>(p_view, 1.0)).xyz;
    let tbn = unpack_normal_tangent(textureLoad(normal_tangent_tex, fcoords, 0));
    let n_world = normalize(tbn.N);
    let cam_pos_w = cam.inv_view[3].xyz;
    let incident = normalize(world_pos - cam_pos_w);
    let dir_w = normalize(reflect(incident, n_world));
    // Nudge off the surface — the reflector itself must not self-hit.
    let origin = world_pos + n_world * 0.02 + dir_w * 0.01;

    var best_t = params.max_distance;
    var best_inst: u32 = 0xffffffffu;
    var best_n = vec3<f32>(0.0, 1.0, 0.0);

    let n_inst = u32(params.bvh_meta.x);
    // Near-zero direction components: clamp for a finite inv (the huge |t|
    // slab crossings can never win the min/max chain — the right limit).
    let safe_w = select(dir_w, vec3<f32>(1e-7), abs(dir_w) < vec3<f32>(1e-7));
    let inv_w = vec3<f32>(1.0) / safe_w;

    for (var i: u32 = 0u; i < n_inst; i = i + 1u) {
        let inst = tlas[i];
        // World AABB early reject (also rejects anything beyond best_t).
        let se = slab(inst.world_min.xyz, inst.world_max.xyz, origin, inv_w);
        if (se.x > se.y || se.y < 0.0 || se.x > best_t) {
            continue;
        }
        // Object-space ray; direction unnormalized so t is shared.
        let ro = (inst.inv_world * vec4<f32>(origin, 1.0)).xyz;
        let rd = (inst.inv_world * vec4<f32>(dir_w, 0.0)).xyz;
        let safe_o = select(rd, vec3<f32>(1e-7), abs(rd) < vec3<f32>(1e-7));
        let inv_o = vec3<f32>(1.0) / safe_o;
        let node_base = bitcast<u32>(inst.world_min.w);
        let tri_base = bitcast<u32>(inst.world_max.w);

        var stack: array<u32, 28>;
        var sp: i32 = 0;
        stack[0] = 0u;
        sp = 1;
        // Bounded loop: the builder caps depth at 28 and each iteration pops
        // one node, so the walk terminates; the extra guard keeps a corrupt
        // buffer from hanging the GPU.
        var guard: u32 = 0u;
        while (sp > 0 && guard < 4096u) {
            guard = guard + 1u;
            sp = sp - 1;
            let node = bvh_nodes[node_base + stack[sp]];
            let nse = slab(node.min, node.max, ro, inv_o);
            if (nse.x > nse.y || nse.y < 0.0 || nse.x > best_t) {
                continue;
            }
            if ((node.a & 0x80000000u) != 0u) {
                let first = node.a & 0x7fffffffu;
                for (var k: u32 = 0u; k < node.b; k = k + 1u) {
                    let tb = tri_base + (first + k) * 3u;
                    let v0 = bvh_tris[tb].xyz;
                    let v1 = bvh_tris[tb + 1u].xyz;
                    let v2 = bvh_tris[tb + 2u].xyz;
                    let t = tri_hit(ro, rd, v0, v1, v2);
                    if (t > 1e-4 && t < best_t) {
                        best_t = t;
                        best_inst = i;
                        best_n = cross(v1 - v0, v2 - v0);
                    }
                }
            } else {
                if (sp < 26) {
                    stack[sp] = node.a;
                    stack[sp + 1] = node.b;
                    sp = sp + 2;
                }
            }
        }
    }

    if (best_inst == 0xffffffffu) {
        textureStore(out_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 0.0));
        return;
    }

    // Constrained hit shading: premultiplied instance emissive + a dim
    // probe-projected env term (so matte hits aren't pure black). The
    // trace applies fresnel × intensity × crossfade when it consumes this,
    // exactly as it weights its own env fallback.
    let inst = tlas[best_inst];
    let inv3 = mat3x3<f32>(inst.inv_world[0].xyz, inst.inv_world[1].xyz, inst.inv_world[2].xyz);
    var n_hit = normalize(transpose(inv3) * best_n);
    if (dot(n_hit, dir_w) > 0.0) {
        n_hit = -n_hit;
    }
    let hit_pos = origin + dir_w * best_t;
    let hit_refl = reflect(dir_w, n_hit);
    let env_dir = box_project_env_dir(
        hit_refl,
        hit_pos,
        params.probe_center_enabled,
        params.probe_half_pad.xyz,
    );
    let env_mip = 0.6 * f32(textureNumLevels(env_tex) - 1u);
    let env_c = textureSampleLevel(env_tex, env_sampler, env_dir, env_mip).rgb;
    var color = inst.emissive.rgb + env_c * 0.15;
    let lum = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    color = color * min(1.0, BVH_LUM_CLAMP / max(lum, 1e-4));
    textureStore(out_tex, coords, vec4<f32>(color, 1.0));
}
