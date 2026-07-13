# Software-BVH reflections (reflection-plan Tier 7) — design

Status: **SHIPPED** (2026-07-13, updates ..dfe1c77d) behind the default-off
`ssr.bvh_reflections` toggle (set_post_process `ssr_bvh_reflections`),
David's explicit greenlight ("if we can toggle it off/on, do it"). Deltas
from the design below, learned on-device:
- The bvh_trace pass runs BEFORE the screen-space trace and writes its own
  `ssr_bvh` target; the trace's miss fallback prefers a real BVH hit over
  the probe/env (no read-write hazard, SSR hits untouched).
- GRAZING GATE: far-pixel normal reconstruction noise tilts mirror rays
  below the reflector's tangent plane — the walk then returns the
  reflector's own far geometry as a false dark horizon band. Rays with
  dot(dir, n) < 0.005 fall through to the env fallback; the self-hit
  offset scales with camera distance.
- Verified: a mirrored torus reflects as a COMPLETE ring (camera-invisible
  underside supplied by the BVH); 60 fps at the full-screen-eligible mirror
  worst case; zero-cost off (pinned).
- Eligibility stayed spread < 0.1 — the arena floor (0.18) deliberately
  does NOT qualify (its occlusion gaps are already probe-anchored); raising
  the bound would need blur-matching the sharp BVH hit to the glossy cone.

Reference: `~/Downloads/awsmrenderer-reflection-plan.md` Tier 7. The layered
model it slots into (all shipped today): SSR trace confidence → box-projected
probe env fallback → raw env. BVH rays REPLACE the env fallback for the
pixels where it's weakest — SSR misses on near-mirror surfaces, where the
probe's blurred approximation is visibly not the scene (platform undersides,
off-screen occluded geometry with sharp detail).

## What it must fix (acceptance anchor)

The two artifact families the arena demonstrated that neither SSR nor the
global probe can close:

1. **Occluded-content misses at low spread**: a platform hides the wall pixels
   a floor ray needs; today the pixel takes probe glow (plausible but blurry —
   fine at arena floor spread 0.18, wrong for a spread<0.05 mirror).
2. **Camera-invisible geometry**: undersides / backfacing content (floor
   mirrors a pad's underside — never on screen, never in the probe).

If a scene has no near-mirror surfaces, BVH adds nothing the probe doesn't —
that's the device/content gate in one sentence.

## Eligibility (keep the ray count trivial)

Ray per pixel ONLY when ALL hold (evaluated in the trace pass, which already
knows every input):
- SSR miss (`hit == false` after the march), or refined hit rejected
- `spread < 0.1` (near-mirror band; everything rougher keeps the probe —
  the resolve's cone blur hides its inaccuracy)
- reflectivity above the existing 1/255 descriptor gate
- half-res (reuse `resolution_scale`; quarter-res knob later)

Arena-scale estimate: half-res 669×384 ≈ 257k pixels; near-mirror SSR-miss
subset is a few percent → **~5–15k rays/frame**. At ~40–80 BVH node visits
per ray that is well under a millisecond of ALU on the desktop tier; the risk
is bandwidth on divergent node fetches, not compute.

## Acceleration structure

- **BLAS per unique static mesh**, built CPU-side at mesh commit — the same
  hook where `source.positions` / `source.indices` are already resident for
  packing + LOD clusterization (`meshes.rs` commit path). Binary BVH, SAH or
  median split, flattened to a GPU storage buffer as 32-byte nodes
  (aabb_min+left, aabb_max+right/tri-offset). Skinned meshes: EXCLUDED from
  ray hits (plan's rule; their reflection stays probe/SSR).
- **TLAS over instances**: flat array of {world→object 3×4, BLAS offset,
  instance flags}; refit (AABB recompute + reupload) when a transform dirties
  — piggyback the existing transform-dirty walk that already feeds
  `update_from_transforms`. No TLAS tree for the MVP: a LINEAR instance scan
  is correct and fast at ≤ a few hundred instances (arena: ~40); add a real
  TLAS only when a scene proves it.
- Budget guard: per-mesh triangle cap (e.g. 64k) + total BLAS byte cap under
  the `MAX_GPU_BUFFER_BYTES` 1.9 GB create_buffer guard — refuse loudly, not
  abort.

## Traversal + shading (WGSL)

New compute entry in the SSR family (`ssr_wgsl/bvh_trace.wgsl`, own cache-key
axis so SSR-without-BVH compiles zero of it):
- Stackful BVH2 walk, fixed `array<u32, 24>` stack, front-to-back via
  slab test (the probe work just added the slab helper pattern in
  `math.wgsl`).
- Watertight-enough Möller–Trumbore; nearest hit wins.
- At the hit: fetch triangle attributes (position/normal from the packed
  visibility buffers by index; material id per instance), then shade
  CONSTRAINED per the plan: emissive + (probe-projected) env specular +
  optional single directional term. NO recursion, NO punctual loops, NO
  shadow rays in the MVP.
- Output: same `ssr` target texel format the trace writes (color + coverage
  alpha), so the resolve/temporal/composite chain — and the confidence
  blend — need zero new plumbing. hit_conf for a BVH hit is 1.0 × the same
  edge/travel fades; a BVH miss falls through to the probe env exactly as
  today.

## Integration order (when scheduled)

1. BLAS build + flatten behind a `bvh_reflections: bool` on `PostProcess.Ssr`
   (default OFF, structural axis like `debug`), buffers + bind group.
2. `bvh_trace` pass between trace and resolve, eligibility mask from the
   trace (pack "miss + low spread" into the ssr target alpha or a 1-byte
   sidecar).
3. TLAS refit on transform dirty; skinned/excluded flags.
4. wgsl_validation: naga-validate every axis combo; pin the eligibility
   gate + the constrained-shading (no-recursion) shape.
5. Device tiering: editor toggle first (like MSAA), auto-tier later
   (plan's mobile/mid/high policy).
6. Arena A/B at the pad-underside + platform-occlusion angles vs probe-only.

## Non-goals (MVP)

Transparent surfaces in rays (plan's transparency policy: opaque-only
sources); dynamic BLAS rebuilds; skinned hits; recursive bounces; denoiser
changes (reuse SSR's resolve/temporal as-is).
