// Decal classify.
//
// One thread per decal. Each thread:
//   1. Reconstructs the decal's world-space AABB from its
//      inverse_transform (the decal is an oriented unit cube; we
//      transform the 8 corners through `inverse(inverse_transform)`
//      to get world corners, then aabb-around).
//      Actually cheaper: re-derive the world transform on demand by
//      inverting the inverse_transform. The matrix inverse is heavy
//      per thread; v1 uses a simpler-but-conservative bound: project
//      the 8 corners of the *local* unit cube through `world_transform
//      = inverse(inverse_transform)` and take the AABB. With glam-side
//      `inverse_transform` already cached, the GPU inversion cost is
//      one mat4 inverse per decal — acceptable for 128 decals.
//   2. Projects the world AABB to screen space via `camera_raw.view_proj`.
//   3. Computes the tile range `[tile_min, tile_max]` (inclusive).
//   4. For each tile in the range, atomically appends the decal's
//      index to that tile's bucket. Overflow (bucket full) silently
//      drops the entry — the shading pass simply doesn't see it for
//      that tile.

{% include "shared_wgsl/camera.wgsl" %}

struct Decal {
    inverse_transform: mat4x4<f32>,
    texture_index: u32,
    alpha: f32,
    blend_mode: u32,
    _pad: u32,
};

struct DecalsBuffer {
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    items: array<Decal>,
};

struct Buckets {
    tile_count_x: u32,
    tile_count_y: u32,
    bucket_capacity: u32,
    _pad: u32,
    // Flat per-tile region — `(atomic<u32> count) + array<u32, capacity>`
    // laid out per tile. We index via manual offset math.
    payload: array<atomic<u32>>,
};

@group(0) @binding(0) var<storage, read> decals_buffer: DecalsBuffer;
@group(0) @binding(1) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(2) var<storage, read_write> buckets: Buckets;
{% if hzb_enabled %}
// HZB occlusion gate. Bound only when
// `features.gpu_culling && features.decals` — the HZB texture is
// itself gated on `gpu_culling`. Stores the conservative occluder
// bound per texel — the *farthest* depth in the reduced footprint:
// numerical MAX under forward-Z, numerical MIN under reverse-Z
// (see hzb_wgsl/reduce.wgsl). The gate fires when a decal's
// *closest* projected depth sits behind that bound for the tile.
@group(0) @binding(3) var hzb_texture: texture_2d<f32>;
{% endif %}

const TILE_PX: f32 = 8.0;

// Per-tile stride in `u32` units (count + capacity).
fn per_tile_stride_u32() -> u32 {
    return 1u + buckets.bucket_capacity;
}

fn tile_count_u32(tile_x: u32, tile_y: u32) -> u32 {
    let tile_idx = tile_y * buckets.tile_count_x + tile_x;
    return tile_idx * per_tile_stride_u32();
}

fn append_to_tile(tile_x: u32, tile_y: u32, decal_index: u32) {
    let base = tile_count_u32(tile_x, tile_y);
    // Atomic-bump the per-tile count; only write the entry if we got
    // a valid slot (overflow drops the append).
    let slot = atomicAdd(&buckets.payload[base], 1u);
    if (slot < buckets.bucket_capacity) {
        // The entries live immediately after the count.
        // Re-using `atomicStore` to satisfy the `read_write` typing —
        // the value is unique per slot so the atomic is just a typed
        // store at this point.
        atomicStore(&buckets.payload[base + 1u + slot], decal_index);
    }
}

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let decal_index = gid.x;
    let count = decals_buffer.count;
    if (decal_index >= count) {
        return;
    }

    // Recover the world transform from the inverse. Glam stores the
    // inverse already so this is the dual operation.
    let world_transform = inverse4(decals_buffer.items[decal_index].inverse_transform);

    // Project the 8 unit-cube corners through view_proj * world.
    let view_proj = camera_raw.view_proj;
    var uv_min = vec2<f32>(2.0, 2.0);
    var uv_max = vec2<f32>(-1.0, -1.0);
    // Closest projected corner depth across the 8 corners — the
    // convention-aware extreme (003): forward-Z closest = numerical
    // MIN (0 = near); reverse-Z closest = numerical MAX (1 = near).
    {% if reverse_z %}
    var closest_depth = 0.0;
    {% else %}
    var closest_depth = 1.0;
    {% endif %}
    var any_in_front = false;
    var any_behind_near = false;
    let xs = array<f32, 2>(-1.0, 1.0);
    let ys = array<f32, 2>(-1.0, 1.0);
    let zs = array<f32, 2>(-1.0, 1.0);
    for (var i = 0u; i < 8u; i++) {
        let lx = xs[i & 1u];
        let ly = ys[(i >> 1u) & 1u];
        let lz = zs[(i >> 2u) & 1u];
        let world_h = world_transform * vec4<f32>(lx, ly, lz, 1.0);
        let world_pos = world_h.xyz / world_h.w;
        let clip = view_proj * vec4<f32>(world_pos, 1.0);
        // Behind-near-plane corners — conservatively treat as
        // overlapping the whole screen so the shading pass still
        // tests this decal where needed (false positives are safe).
        if (clip.w <= 0.0) {
            uv_min = vec2<f32>(0.0, 0.0);
            uv_max = vec2<f32>(1.0, 1.0);
            any_behind_near = true;
            any_in_front = true;
            break;
        }
        let inv_w = 1.0 / clip.w;
        let ndc = clip.xy * inv_w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        uv_min = min(uv_min, uv);
        uv_max = max(uv_max, uv);
        // Track the *closest* screen-space depth across the 8
        // projected corners. WebGPU clip-space is `[0, 1]`; under
        // forward-Z 0 = near (closest = min), under reverse-Z
        // 1 = near (closest = max). The HZB stores the farthest
        // occluder bound per texel; the gate fires when this
        // closest depth sits behind that bound for the
        // screen-AABB's footprint.
        {% if reverse_z %}
        closest_depth = max(closest_depth, clip.z * inv_w);
        {% else %}
        closest_depth = min(closest_depth, clip.z * inv_w);
        {% endif %}
        any_in_front = true;
    }
    if (!any_in_front) {
        return;
    }

    // Clip the screen-AABB to the visible viewport.
    uv_min = clamp(uv_min, vec2<f32>(0.0), vec2<f32>(1.0));
    uv_max = clamp(uv_max, vec2<f32>(0.0), vec2<f32>(1.0));
    if (uv_min.x >= uv_max.x || uv_min.y >= uv_max.y) {
        return;
    }

    // Convert to tile-space inclusive [min, max].
    let tile_count_x_f = f32(buckets.tile_count_x);
    let tile_count_y_f = f32(buckets.tile_count_y);
    let viewport_w = tile_count_x_f * TILE_PX;
    let viewport_h = tile_count_y_f * TILE_PX;
    let tile_min_x = u32(floor(uv_min.x * viewport_w / TILE_PX));
    let tile_min_y = u32(floor(uv_min.y * viewport_h / TILE_PX));
    let tile_max_x = u32(floor((uv_max.x * viewport_w - 0.0001) / TILE_PX));
    let tile_max_y = u32(floor((uv_max.y * viewport_h - 0.0001) / TILE_PX));
    let tile_min_x_c = min(tile_min_x, buckets.tile_count_x - 1u);
    let tile_min_y_c = min(tile_min_y, buckets.tile_count_y - 1u);
    let tile_max_x_c = min(tile_max_x, buckets.tile_count_x - 1u);
    let tile_max_y_c = min(tile_max_y, buckets.tile_count_y - 1u);

    {% if hzb_enabled %}
    // HZB occlusion gate. Pick the smallest mip whose texel
    // covers the decal's screen-AABB so a single texel-load gives
    // a conservative "max depth across this footprint" reading,
    // then drop the decal when its closest-screen-depth sits
    // behind that max. Skipped for decals that straddle the near
    // plane (clip.w <= 0 for any corner) since the screen-AABB
    // was conservatively widened to the full viewport above and
    // the per-tile gate wouldn't be meaningful.
    if (!any_behind_near) {
        let mip0_dims = textureDimensions(hzb_texture, 0);
        let mip0_w = f32(mip0_dims.x);
        let mip0_h = f32(mip0_dims.y);
        let aabb_w_px = (uv_max.x - uv_min.x) * mip0_w;
        let aabb_h_px = (uv_max.y - uv_min.y) * mip0_h;
        // Mip M's texel covers 2^M pixels of mip 0. Pick the
        // smallest M such that one texel covers the AABB extent.
        let extent_px = max(aabb_w_px, aabb_h_px);
        let max_mip = textureNumLevels(hzb_texture) - 1u;
        var mip: u32 = 0u;
        if (extent_px > 1.0) {
            // ceil(log2(extent_px)) — `firstLeadingBit(e)` is
            // floor(log2(e)) for u32 (bit index of the MSB, LSB = 0);
            // bump by one when e isn't a power of two so a 3.x-pixel
            // AABB picks mip 2 (4-px texel), not mip 1. (This used to
            // compute `31 - firstLeadingBit` — the count-leading-zeros
            // dual — which always selected the coarsest mip.)
            let e_int = u32(ceil(extent_px));
            mip = min(firstLeadingBit(e_int), max_mip);
            if ((1u << mip) < e_int) {
                mip = min(mip + 1u, max_mip);
            }
        }
        let mip_dims = textureDimensions(hzb_texture, mip);
        let center_uv = (uv_min + uv_max) * 0.5;
        let center_px = vec2<u32>(
            min(u32(center_uv.x * f32(mip_dims.x)), mip_dims.x - 1u),
            min(u32(center_uv.y * f32(mip_dims.y)), mip_dims.y - 1u),
        );
        let hzb_bound = textureLoad(hzb_texture, center_px, mip).r;
        // Decal fully behind the farthest occluder bound covering the
        // screen-AABB footprint — drop it from every tile. "Behind"
        // is convention-aware (003): forward-Z behind = numerically
        // greater; reverse-Z behind = numerically smaller.
        {% if reverse_z %}
        if (closest_depth < hzb_bound) {
            return;
        }
        {% else %}
        if (closest_depth > hzb_bound) {
            return;
        }
        {% endif %}
    }
    {% endif %}

    for (var ty = tile_min_y_c; ty <= tile_max_y_c; ty++) {
        for (var tx = tile_min_x_c; tx <= tile_max_x_c; tx++) {
            append_to_tile(tx, ty, decal_index);
        }
    }
}

// Manual 4×4 matrix inverse — adapted from MESA's gluInvertMatrix.
// Used to recover `world_transform` from the cached `inverse_transform`
// the CPU already computed; doing it on GPU avoids a second per-decal
// upload while keeping the classify dependency-free.
fn inverse4(m: mat4x4<f32>) -> mat4x4<f32> {
    let a00 = m[0][0]; let a01 = m[0][1]; let a02 = m[0][2]; let a03 = m[0][3];
    let a10 = m[1][0]; let a11 = m[1][1]; let a12 = m[1][2]; let a13 = m[1][3];
    let a20 = m[2][0]; let a21 = m[2][1]; let a22 = m[2][2]; let a23 = m[2][3];
    let a30 = m[3][0]; let a31 = m[3][1]; let a32 = m[3][2]; let a33 = m[3][3];

    let b00 = a00*a11 - a01*a10;
    let b01 = a00*a12 - a02*a10;
    let b02 = a00*a13 - a03*a10;
    let b03 = a01*a12 - a02*a11;
    let b04 = a01*a13 - a03*a11;
    let b05 = a02*a13 - a03*a12;
    let b06 = a20*a31 - a21*a30;
    let b07 = a20*a32 - a22*a30;
    let b08 = a20*a33 - a23*a30;
    let b09 = a21*a32 - a22*a31;
    let b10 = a21*a33 - a23*a31;
    let b11 = a22*a33 - a23*a32;

    let det = b00*b11 - b01*b10 + b02*b09 + b03*b08 - b04*b07 + b05*b06;
    let inv_det = 1.0 / det;

    return mat4x4<f32>(
        vec4<f32>(
            ( a11*b11 - a12*b10 + a13*b09) * inv_det,
            (-a01*b11 + a02*b10 - a03*b09) * inv_det,
            ( a31*b05 - a32*b04 + a33*b03) * inv_det,
            (-a21*b05 + a22*b04 - a23*b03) * inv_det,
        ),
        vec4<f32>(
            (-a10*b11 + a12*b08 - a13*b07) * inv_det,
            ( a00*b11 - a02*b08 + a03*b07) * inv_det,
            (-a30*b05 + a32*b02 - a33*b01) * inv_det,
            ( a20*b05 - a22*b02 + a23*b01) * inv_det,
        ),
        vec4<f32>(
            ( a10*b10 - a11*b08 + a13*b06) * inv_det,
            (-a00*b10 + a01*b08 - a03*b06) * inv_det,
            ( a30*b04 - a31*b02 + a33*b00) * inv_det,
            (-a20*b04 + a21*b02 - a23*b00) * inv_det,
        ),
        vec4<f32>(
            (-a10*b09 + a11*b07 - a12*b06) * inv_det,
            ( a00*b09 - a01*b07 + a02*b06) * inv_det,
            (-a30*b03 + a31*b01 - a32*b00) * inv_det,
            ( a20*b03 - a21*b01 + a22*b00) * inv_det,
        ),
    );
}
