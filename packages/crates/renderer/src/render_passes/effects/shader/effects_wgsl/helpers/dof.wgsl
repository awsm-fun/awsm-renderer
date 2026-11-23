// Depth of Field.
//
// `linearize_depth` / `load_depth` live in `helpers/depth.wgsl` (shared with
// atmosphere), included by compute.wgsl ahead of this file.
//
// Two dispatches of this one module:
//
//  `dof_prepare` — one circle of confusion per pixel, from the nearest of its
//          depth samples, plus the largest one in each 8x8 tile. Both go into
//          `dof_data`, a buffer the effects pass owns.
//  `main`        — a tile with no disc over a pixel wide, next to it or
//          reaching it from in front, does nothing: its pixels leave exactly as
//          SMAA, haze and bloom made them. Elsewhere one gather reads the
//          buffer.
//
// The buffer is there for two reasons. A random load from a multisampled depth
// target costs several times a single-sampled one, and the gather makes dozens
// per pixel. And a tap must see the same depth a pixel sees for itself (the
// nearest sample): reading sample 0 made a silhouette pixel "background" to
// its neighbours, so its antialiased colour bled out as a jagged halo.

// Depth of Field constants
const DOF_MAX_BLUR: f32 = 20.0;          // Maximum blur radius in pixels
const SENSOR_HEIGHT: f32 = 0.024;        // 24mm full-frame sensor height (in meters)

// Calculate focal length from projection matrix in world units (meters)
// Matches Blender's camera model with 24mm sensor height
fn get_focal_length(camera: Camera) -> f32 {
    // proj[1][1] = 1 / tan(fov_y / 2)
    // focal_length = sensor_height / (2 * tan(fov_y / 2))
    return (SENSOR_HEIGHT * 0.5) * camera.proj[1][1];
}

// Physically-based circle of confusion (Blender-compatible), in pixels and
// SIGNED: positive in front of the focus plane, negative behind it. It only
// grows towards the camera, so a larger value also means nearer.
// aperture: f-stop number (e.g., 2.8, 5.6, 8.0) - lower = shallower DoF
// focus_distance: distance to focus plane in world units (meters)
fn calculate_signed_coc(linear_depth: f32, camera: Camera) -> f32 {
    let S = max(camera.focus_distance, 0.001);  // Focus distance
    let N = camera.aperture;                    // F-stop number
    let f = get_focal_length(camera);           // Focal length in world units
    let D = linear_depth;                       // Object distance
    if (D <= 0.0) {
        // `linearize_depth` is perspective-only and goes negative under an
        // orthographic camera: no blur, rather than all of it.
        return 0.0;
    }

    // Aperture diameter
    let A = f / max(N, 0.1);

    // CoC = A * f * |D - S| / (D * (S - f)), for S >> f: A * f * |1/S - 1/D|
    let coc_world = A * f * (1.0 / D - 1.0 / S);

    // Convert from world units to pixels
    let screen_height = camera.viewport_size.y;
    let coc_pixels = coc_world * screen_height / SENSOR_HEIGHT;

    return clamp(coc_pixels, -DOF_MAX_BLUR, DOF_MAX_BLUR);
}

// ── `dof_data` ──────────────────────────────────────────────────────────────
// Mirrors `dof_data_byte_size` in effects/bind_group.rs:
//   [0 .. tiles.x * tiles.y)  a word per 8x8 tile: low half the widest CoC in
//                             front of focus, high half the widest either side
//   then, row by row          the signed CoC, two pixels to a word
// All in fixed point, 1/1024 pixel.
const DOF_TILE: u32 = 8u;
const DOF_FIXED: f32 = 1024.0;

fn dof_tile_count(dims: vec2<u32>) -> vec2<u32> {
    return (dims + DOF_TILE - 1u) / DOF_TILE;
}

fn dof_pixel_word(coords: vec2<u32>, dims: vec2<u32>) -> u32 {
    let tiles = dof_tile_count(dims);
    return tiles.x * tiles.y + coords.y * ((dims.x + 1u) / 2u) + coords.x / 2u;
}

fn dof_load_coc(coords: vec2<i32>, dims: vec2<i32>) -> f32 {
    let c = vec2<u32>(coords);
    let word = bitcast<i32>(dof_data[dof_pixel_word(c, vec2<u32>(dims))]);
    return f32(extractBits(word, (c.x & 1u) * 16u, 16u)) / DOF_FIXED;
}

var<workgroup> dof_tile_front: atomic<u32>;
var<workgroup> dof_tile_widest: atomic<u32>;

// One workgroup per tile. An invocation owns two pixels side by side, so that
// no two ever write the same word.
@compute @workgroup_size(4, 8)
fn dof_prepare(
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(local_invocation_index) lidx: u32,
) {
    let camera = camera_from_raw(camera_raw);
    let dims = textureDimensions(composite_tex);
    let first = wid.xy * DOF_TILE + vec2<u32>(lid.x * 2u, lid.y);

    var word = 0u;
    for (var i = 0u; i < 2u; i = i + 1u) {
        let p = first + vec2<u32>(i, 0u);
        if (p.x < dims.x && p.y < dims.y) {
            let depth = load_depth(vec2<i32>(p));
            let coc = calculate_signed_coc(linearize_depth(depth, camera), camera);
            let fixed = i32(round(coc * DOF_FIXED));
            word = insertBits(word, bitcast<u32>(fixed), i * 16u, 16u);
            atomicMax(&dof_tile_front, u32(max(fixed, 0)));
            atomicMax(&dof_tile_widest, u32(abs(fixed)));
        }
    }
    if (first.x < dims.x && first.y < dims.y) {
        dof_data[dof_pixel_word(first, dims)] = word;
    }
    workgroupBarrier();

    if (lidx == 0u) {
        let tile = wid.y * dof_tile_count(dims).x + wid.x;
        dof_data[tile] = atomicLoad(&dof_tile_front) | (atomicLoad(&dof_tile_widest) << 16u);
    }
}

// How wide a gather this tile can need. Purely a cull: a tap beyond it has
// zero weight, so a pixel comes out the same for any larger value and the
// tiles cannot show.
struct DofReach {
    front: f32,  // widest CoC in front of focus whose disc reaches this tile
    widest: f32, // widest CoC in this tile and the eight around it
};

// Tiles either side that a disc can reach from: 3 tiles > DOF_MAX_BLUR pixels.
const DOF_REACH_TILES: i32 = 3;
const DOF_REACH_SPAN: u32 = 7u;
// A tap is placed (and rounded to a pixel) up to a pixel outside the disc
// that gives it weight, and the antialiasing also gathers for the pixels next
// to this one, which can be a pixel into the next tile.
const DOF_REACH_MARGIN: f32 = 2.5;

var<workgroup> dof_reach_front: atomic<u32>;
var<workgroup> dof_reach_widest: atomic<u32>;

// Must be called from uniform control flow, by every invocation, before any
// return: the 49 tile loads are shared out over the workgroup.
fn dof_tile_reach(wid: vec2<u32>, lidx: u32) -> DofReach {
    let tiles = vec2<i32>(dof_tile_count(textureDimensions(composite_tex)));
    let here = vec2<i32>(wid);
    for (var i = lidx; i < DOF_REACH_SPAN * DOF_REACH_SPAN; i = i + 64u) {
        let offset = vec2<i32>(i32(i % DOF_REACH_SPAN), i32(i / DOF_REACH_SPAN)) - DOF_REACH_TILES;
        let tile = here + offset;
        if (all(tile >= vec2<i32>(0)) && all(tile < tiles)) {
            let word = dof_data[u32(tile.y * tiles.x + tile.x)];
            let front = word & 0xFFFFu;
            // Pixels between the nearest pixels of the two tiles.
            let gap = vec2<f32>(max(abs(offset) * i32(DOF_TILE) - (i32(DOF_TILE) - 1), vec2<i32>(0)));
            if (length(gap) <= f32(front) / DOF_FIXED + DOF_REACH_MARGIN) {
                atomicMax(&dof_reach_front, front);
            }
            if (all(abs(offset) <= vec2<i32>(1))) {
                atomicMax(&dof_reach_widest, word >> 16u);
            }
        }
    }
    workgroupBarrier();
    return DofReach(
        f32(atomicLoad(&dof_reach_front)) / DOF_FIXED,
        f32(atomicLoad(&dof_reach_widest)) / DOF_FIXED,
    );
}

// ── Sampling ────────────────────────────────────────────────────────────────
// One set of taps, feeding two layers:
//
//  SELF  — the pixel's own blur: taps inside ITS circle of confusion. This is
//          what blurs anything behind the focus plane (and the inside of a
//          near object).
//  NEAR  — everything IN FRONT of the focus plane that is blurred ONTO this
//          pixel and is blurrier than it: each tap accepted when that sample's
//          own circle of confusion reaches here (scatter-as-gather).
//
// The NEAR layer is what a single "blur by my own CoC" gather cannot do: an
// in-focus pixel next to a defocused foreground object has a CoC of zero and
// so never looked at its neighbours — the foreground blur stopped dead at the
// object's silhouette. A bright thin object close to the lens (a brass edge,
// a row of bulbs) therefore stayed a hard line however wide the aperture,
// instead of opening into soft discs.
//
// The mirror image happens INSIDE a defocused near object's silhouette: its
// blur makes it see-through there, and what shows through keeps that surface's
// own, smaller blur. How much shows is the share of the pixel's disc the
// farther surface fills; its colour is averaged near the pixel, over that
// surface's disc. Averaging it over the near object's wide disc instead drew a
// hard line along the silhouette, where the two blurs met.
//
// The taps cut the largest disc into rings that get wider outwards, and a
// tap's weight is the part of its ring that lies inside the disc in question.
// That is what makes a small disc smooth next to a large one:
//  * every pixel out to 3 is a tap of its own, at a fixed offset, so discs
//    that small have no noise at all;
//  * further out there is one tap per ring, turned per pixel, from 3 pixels of
//    area each growing to 28 at the rim;
//  * the weights of any disc sum to exactly its area, whatever its radius, so
//    a flat field has coverage exactly 1 at every pixel.
// Taps spread evenly over the 20 pixel disc left a 1-6 pixel one with 0-3 of
// them: random coverage, which showed as speckle along every soft edge.
//
// Areas are in units of PI square pixels, so a disc's area is its radius².
const DOF_INV_PI: f32 = 0.31830989;
const DOF_CENTER_AREA: f32 = DOF_INV_PI; // the pixel itself

// x, y: the offset. z: area inside this tap's ring. w: taps sharing the ring.
// Sorted by z, so a gather can stop at the first ring outside its reach.
const DOF_INNER_COUNT: u32 = 28u;
const DOF_INNER_PIXELS: f32 = 29.0;
const DOF_INNER = array<vec4<f32>, DOF_INNER_COUNT>(
    vec4<f32>( 1.0,  0.0,  1.0, 4.0), vec4<f32>(-1.0,  0.0,  1.0, 4.0),
    vec4<f32>( 0.0,  1.0,  1.0, 4.0), vec4<f32>( 0.0, -1.0,  1.0, 4.0),
    vec4<f32>( 1.0,  1.0,  5.0, 4.0), vec4<f32>(-1.0, -1.0,  5.0, 4.0),
    vec4<f32>( 1.0, -1.0,  5.0, 4.0), vec4<f32>(-1.0,  1.0,  5.0, 4.0),
    vec4<f32>( 2.0,  0.0,  9.0, 4.0), vec4<f32>(-2.0,  0.0,  9.0, 4.0),
    vec4<f32>( 0.0,  2.0,  9.0, 4.0), vec4<f32>( 0.0, -2.0,  9.0, 4.0),
    vec4<f32>( 2.0,  1.0, 13.0, 8.0), vec4<f32>(-2.0, -1.0, 13.0, 8.0),
    vec4<f32>( 2.0, -1.0, 13.0, 8.0), vec4<f32>(-2.0,  1.0, 13.0, 8.0),
    vec4<f32>( 1.0,  2.0, 13.0, 8.0), vec4<f32>(-1.0, -2.0, 13.0, 8.0),
    vec4<f32>( 1.0, -2.0, 13.0, 8.0), vec4<f32>(-1.0,  2.0, 13.0, 8.0),
    vec4<f32>( 2.0,  2.0, 21.0, 4.0), vec4<f32>(-2.0, -2.0, 21.0, 4.0),
    vec4<f32>( 2.0, -2.0, 21.0, 4.0), vec4<f32>(-2.0,  2.0, 21.0, 4.0),
    vec4<f32>( 3.0,  0.0, 25.0, 4.0), vec4<f32>(-3.0,  0.0, 25.0, 4.0),
    vec4<f32>( 0.0,  3.0, 25.0, 4.0), vec4<f32>( 0.0, -3.0, 25.0, 4.0),
);

// 56 rings of min(FIRST + GROWTH * i, WIDEST) pixels² fill the disc from the
// inner taps out to DOF_MAX_BLUR.
const DOF_OUTER_COUNT: u32 = 56u;
const DOF_OUTER_FIRST: f32 = 3.45;
const DOF_OUTER_GROWTH: f32 = 0.9;
const DOF_OUTER_WIDEST: f32 = 28.25;
const DOF_GOLDEN_TURN = vec2<f32>(-0.73736888, 0.67549029); // cos, sin of 2.39996323

// How much blurrier (pixels, and as a share of itself) a sample must be than
// the pixel it lands on to be laid OVER it. Below that the two are one
// surface, and SELF already blurs it.
const DOF_NEAR_START: f32 = 0.25;
const DOF_NEAR_FULL: f32 = 0.35;

// Interleaved gradient noise (Jimenez): a per-pixel value in [0,1) with good
// spatial distribution and no texture.
fn dof_noise(coords: vec2<i32>) -> f32 {
    let p = vec2<f32>(f32(coords.x), f32(coords.y));
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

struct DofGather {
    self_sum: vec3<f32>,
    self_weight: f32,
    near_sum: vec3<f32>,
    near_weight: f32,
    // Seen through the silhouette of a near object: the area, and its colour.
    revealed_area: f32,
    revealed_sum: vec3<f32>,
    revealed_weight: f32,
};

// One tap: the pixel at `coords`, standing for `share` of the ring between
// `ring_inner` and `ring_inner + ring_area`.
fn dof_tap(
    gather: ptr<function, DofGather>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>,
    center_coc: f32,
    ring_inner: f32,
    ring_area: f32,
    share: f32,
) {
    let p = clamp(coords, vec2<i32>(0), screen_dims - 1);
    let coc = dof_load_coc(p, screen_dims);
    let radius = abs(coc);
    let center_radius = abs(center_coc);

    var in_front = 0.0;
    if (coc > 0.0) {
        in_front = smoothstep(
            DOF_NEAR_START,
            max(1.0, DOF_NEAR_FULL * radius),
            coc - max(center_coc, 0.0),
        );
    }

    var near = 0.0;
    let disc = radius * radius;
    if (in_front > 0.0 && disc > ring_inner) {
        // The sample spreads its light over its own disc.
        near = in_front * share * min(disc - ring_inner, ring_area) / disc;
    }

    var own = 0.0;
    var revealed = 0.0;
    let center_disc = center_radius * center_radius;
    if (center_disc > ring_inner) {
        // What the NEAR layer lays over this pixel is not also under it.
        own = (1.0 - in_front) * share * min(center_disc - ring_inner, ring_area);
        // A sample sharper than this pixel and nearer must not smear across it
        // (an in-focus object bleeding into a blurred background), and one
        // with no disc of its own not at all; a sample behind this pixel is
        // what its blur reveals.
        if (coc > center_coc && radius < center_radius) {
            own *= radius / center_radius * smoothstep(0.0, min(1.5, center_radius), radius);
        }
        // The same test as `in_front`, seen from the other side.
        if (center_coc > 0.0) {
            let behind = smoothstep(
                DOF_NEAR_START,
                max(1.0, DOF_NEAR_FULL * center_radius),
                center_coc - max(coc, 0.0),
            );
            revealed = own * behind;
            own -= revealed;
        }
    }

    if (own + near + revealed <= 0.0) {
        return;
    }
    let color = textureLoad(composite_tex, p, 0).rgb;
    (*gather).self_sum += color * own;
    (*gather).self_weight += own;
    (*gather).near_sum += color * near;
    (*gather).near_weight += near;
    if (revealed > 0.0) {
        // Flat over the sample's disc, then falling fast: deep inside the
        // silhouette the nearest of what is behind stands in for the rest.
        let spread = (ring_inner + 0.5 * ring_area) / max(disc, DOF_CENTER_AREA);
        let weight = revealed / (1.0 + spread * spread);
        (*gather).revealed_area += revealed;
        (*gather).revealed_sum += color * weight;
        (*gather).revealed_weight += weight;
    }
}

struct DofBlurred {
    color: vec3<f32>,
    // false: no disc reaches the pixel and `color` is the composite, untouched.
    touched: bool,
};

// The composite at `coords`, blurred.
fn dof_blur(
    coords: vec2<i32>,
    screen_dims: vec2<i32>,
    center_coc: f32,
    reach_front: f32,
) -> DofBlurred {
    let center_composite = textureLoad(composite_tex, coords, 0).rgb;
    let gather_radius = max(reach_front, abs(center_coc));
    let gather_area = gather_radius * gather_radius;
    if (gather_area <= DOF_CENTER_AREA) {
        return DofBlurred(center_composite, false);
    }

    var gather: DofGather;

    for (var i = 0u; i < DOF_INNER_COUNT; i = i + 1u) {
        let tap = DOF_INNER[i];
        let ring_inner = tap.z * DOF_INV_PI;
        if (ring_inner >= gather_area) {
            break;
        }
        dof_tap(
            &gather, coords + vec2<i32>(tap.xy), screen_dims, center_coc,
            ring_inner, tap.w * DOF_INV_PI, 1.0 / tap.w,
        );
    }

    var ring_inner = DOF_INNER_PIXELS * DOF_INV_PI;
    if (ring_inner < gather_area) {
        let rotation = dof_noise(coords) * 6.2831853;
        var direction = vec2<f32>(cos(rotation), sin(rotation));
        for (var i = 0u; i < DOF_OUTER_COUNT; i = i + 1u) {
            if (ring_inner >= gather_area) {
                break;
            }
            let ring_area = min(DOF_OUTER_FIRST + DOF_OUTER_GROWTH * f32(i), DOF_OUTER_WIDEST) * DOF_INV_PI;
            let offset = direction * sqrt(ring_inner + 0.5 * ring_area);
            dof_tap(
                &gather, coords + vec2<i32>(round(offset)), screen_dims, center_coc,
                ring_inner, ring_area, 1.0,
            );
            ring_inner += ring_area;
            direction = vec2<f32>(
                direction.x * DOF_GOLDEN_TURN.x - direction.y * DOF_GOLDEN_TURN.y,
                direction.x * DOF_GOLDEN_TURN.y + direction.y * DOF_GOLDEN_TURN.x,
            );
        }
    }

    var result = center_composite;
    let own_area = gather.self_weight + gather.revealed_area;
    if (own_area > 0.0) {
        var sum = center_composite * DOF_CENTER_AREA + gather.self_sum;
        if (gather.revealed_weight > 0.0) {
            sum += gather.revealed_sum * (gather.revealed_area / gather.revealed_weight);
        }
        result = sum / (DOF_CENTER_AREA + own_area);
    }
    if (gather.near_weight > 0.0) {
        // The foreground layer is laid over the pixel's own result with the
        // coverage it accumulated (1 = fully inside a defocused near object).
        let near_color = gather.near_sum / gather.near_weight;
        result = mix(result, near_color, min(gather.near_weight, 1.0));
    }
    return DofBlurred(result, own_area + gather.near_weight > 0.0);
}

{% if smaa_anti_alias %}
// 1 for a disc inside its pixel, 0 from a 1.5 pixel one up.
fn dof_sharpness(coc: f32) -> f32 {
    return 1.0 - smoothstep(sqrt(DOF_CENTER_AREA), 1.5, abs(coc));
}
{% endif %}

// Apply depth of field.
//
// `color` is this pixel after the effects that run before DoF (SMAA, haze,
// bloom); the gather reads the raw composite. `effects_delta` is what haze and
// bloom alone added. It is carried across the blur unchanged, or bloom would
// vanish wherever DoF blurs.
//
// SMAA's share must NOT be carried like that. It is a correction to the raw
// pixel, and added to a blurred one it redraws the edge the blur removed, as a
// dotted one-pixel outline. Nor can it just be dropped where the pixel is
// blurred: half of an in-focus silhouette's antialiasing lives in the blurred
// pixels outside it. So the same blend is made again between the BLURRED
// colours of the pixels SMAA blended, and faded out where both are blurred.
fn apply_dof(
    color: vec3<f32>,
    effects_delta: vec3<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>,
    reach: DofReach,
) -> vec3<f32> {
    // Every disc here fits inside one pixel: nothing to load, nothing to do.
    let tile_reach = max(reach.front, reach.widest);
    if (tile_reach * tile_reach <= DOF_CENTER_AREA) {
        return color;
    }
    let center_coc = dof_load_coc(coords, screen_dims);
    let own = dof_blur(coords, screen_dims, center_coc, reach.front);
    var touched = own.touched;
    var antialiasing = vec3<f32>(0.0);

    {% if smaa_anti_alias %}
        let center_sharpness = dof_sharpness(center_coc);
        let base = smaa_compress(own.color);
        var blend = vec3<f32>(0.0);
        var blended = false;
        for (var side = 0; side < 2; side = side + 1) {
            let p = clamp(
                coords + smaa_blend_axis * (1 - 2 * side),
                vec2<i32>(0),
                screen_dims - 1,
            );
            var amount = smaa_blend.x;
            if (side == 1) {
                amount = smaa_blend.y;
            }
            if (amount <= 0.0) {
                continue;
            }
            let coc = dof_load_coc(p, screen_dims);
            amount *= max(center_sharpness, dof_sharpness(coc));
            if (amount <= 0.0) {
                continue;
            }
            let other = dof_blur(p, screen_dims, coc, reach.front);
            touched = touched || other.touched;
            blend += amount * (smaa_compress(other.color) - base);
            blended = true;
        }
        if (blended) {
            antialiasing = smaa_expand(base + blend) - own.color;
            {% if atmosphere || atmosphere_volumetric %}
                // Haze attenuated the pixel after SMAA.
                antialiasing *= atmosphere_scene_transmittance;
            {% endif %}
        }
    {% endif %}

    if (!touched) {
        return color;
    }
    return own.color + antialiasing + effects_delta;
}
