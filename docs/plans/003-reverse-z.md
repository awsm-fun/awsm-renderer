# 003 — Reverse-Z migration

**Order:** THIRD — early and deliberately BEFORE SSR verification (004), the test
scenes, and the optimization sweep. Rationale (David, 2026-07-10): **z-fighting did
NOT go away** with the bounded-ratio clip-plane fix — so this is now a **fix**, not a
someday precision-ceiling upgrade — and reverse-Z changes depth fundamentals
everywhere; landing it late would invalidate goldens, test scenes, SSR verification,
and every depth-adjacent optimization, forcing them all to be redone. Land it once,
early, and build everything downstream on the new convention.

Consequences of running early (overrides stale assumptions below):
- The doc's framing that the clip-plane fix "already eliminated the reported
  z-fighting" is **wrong in the field** — z-fighting still reproduces. **Repro
  decision (David, 2026-07-10): build SYNTHETIC repros** per §9.A (coplanar surfaces
  at far range, stacked thin platforms, extreme near:far, stacked concentric rings)
  and pin those as the before/after; do not block on a field scene.
- §9's golden-scene suite does not exist yet (it's plan 006 Phase 0). Verify with:
  ad-hoc scenes, the model-viewer (:9080), and the §9.E SSR scene — driven through
  the plan-002 clean-screenshot workflow. The permanent 006 test scenes are then
  AUTHORED under the new convention and become the lasting regression lock.
- **Ship decision:** at the end of this plan the `reverse_z` flag defaults **ON**
  (that is the point of doing it early); keep the flag for one release as the A/B and
  rollback lever per §10.
- SSR is implemented but not yet sign-off-verified (that's 004, after this): the
  LinearDda trace is value-agnostic (§9.E regression guard), the dormant min-Z/Hi-Z
  pyramid MUST be flipped in lockstep per §6.6/§12.6 even though it's gated off.

**SCOPE DECISIONS — LOCKED (David, 2026-07-10), superseding §7's recommendations:**
1. **Infinite-far reverse-Z** for the main perspective camera
   (`perspective_infinite_reverse_rh`) — maximize the precision win. This makes the
   §6.5 rework MANDATORY: thread explicit near/far to froxel z-slicing + cascade
   fitting instead of recovering them from the matrix (`proj[2][2]≈0` under
   infinite-far). Orthographic paths (editor ortho view, directional cascades) are
   inherently finite — they get finite reverse-Z (swapped near/far), not infinite.
2. **Shadows migrate IN THIS PASS** (§6.7 lockstep list is in scope, not optional):
   writers + receiver NDC + bounds + EVSM remap + compare + clear together. Stage
   them after the main-camera flip is verified so failures bisect cleanly.
3. **Mobile keeps `Depth24plus`** — correctness required on both formats; the
   precision win is desktop-only (forcing Depth32Float on mobile costs
   memory/bandwidth for nothing).
So §8's staged order applies with stages 7 (shadows) and 8 (infinite-far) PROMOTED to
mandatory, in that order: flag → finite main-camera flip → HZB/occlusion → frustum →
near/far threading → consumers/sentinels → shadows → infinite-far → relax
auto_clip_planes.

**STAGE PROGRESS (live, updated per commit):**
- [x] Stage 1a (96a4597c): `RendererFeatures.reverse_z` + `depth_convention.rs`
      (clear/compare/compare_strict/is_background/nearest/is_closer + mirror test);
      editor `?reversez` opt-in (main viewport + material preview share one source).
- [x] Stage 1b (f9eb4cdd): every main-camera depth site reads DepthConvention —
      geometry×4 + transparent pipeline builders, lines strict-compare chain,
      geometry+HUD clears, web-shared grid. Shadows pinned FORWARD until stage 7.
      Flag-off browser-verified identical.
- [x] Stage 2 (this commit): DepthConvention::{perspective, orthographic,
      near_ndc_z} helpers (+ matrix unit tests proving near→1/far→0 + ordering
      flip); renderer CameraMatrices::perspective takes the convention;
      compute_view_frustum_rays unprojects at the convention's near NDC z (z=0
      is the FAR plane under reverse — NaN rays under stage-8 infinite-far);
      web-shared FreeCamera.set_reverse_z + projection_matrix(convention);
      editor scene-camera matrices + viewport camera init + thumbnail camera
      wired to the ?reversez flag. model-tests + examples deliberately stay
      forward-Z (they don't opt into the feature; the RendererFeatures default
      remains false for library consumers). Flag-ON browser-verified: 3-object
      overlap scene renders with CORRECT depth ordering (front sphere occludes
      rear sphere occludes box; grid + axis lines correct; zero console
      errors) — occlusion culling did not mis-cull this scene pre-stage-3.
- [x] Stage 3 (this commit): reverse_z axis on ShaderCacheKeyHzbSeed/HzbReduce/
      OcclusionCull → templates → WGSL. FIVE coupled sites flipped in lockstep
      (the doc's four + one it missed): seed MSAA sample-reduce max→min, mip
      reduce max→min, cull depth_min init 1.0→0.0 + nearest-corner min→max,
      footprint reduce max→min, in-front compare >→<, AND the clip.w<=0 bypass
      sentinel 0.0→1.0 (a forward-0.0 sentinel under the flipped compare would
      have force-CULLED every near-plane-clipped AABB). Browser A/B (wall + 20
      occluded spheres, gpu_culling on): front-view culled render is BYTE-
      IDENTICAL across conventions (26,506 B both); behind-view within 28 B;
      culled spheres reappear byte-identically after orbiting back (no stuck
      culls). Zero console errors both modes.
- [x] Stage 4 (this commit): CameraMatrices carries reverse_z (set by every
      producer; frustum extraction + future near/far recovery read it instead
      of guessing from the matrix). frustum.rs from_view_projection(vp,
      reverse_z) swaps the near/far rows under reverse (world-space planes
      identical — proven by a new equivalence unit test with relative f32
      tolerance); GPU extract_planes in cull.wgsl mirrors the swap via the
      stage-3 reverse_z axis, and its stale OpenGL row3+row2 comment is fixed.
      Shadow-path callers pin false until stage 7. The geometry crate's [-1,1]
      extractor has NO renderer consumers (doc note was stale — audited, left
      untouched). Browser A/B: 12-sphere ring, camera inside, 6 orbit angles —
      4/6 captures byte-identical across conventions, rest within 40 B; no
      popping at screen edges, zero console errors.
- [x] Stage 5 (this commit): CameraMatrices carries explicit near/far (every
      producer sets them — FreeCamera incl. clip_override, editor scene
      cameras, model-tests, examples). Froxel z-slicing (render.rs) + cascade
      fitting (shadows/state.rs) read the fields with infinite-far clamps
      (froxels: near*1e5 floor 10k; cascades: near*1e4 floor 1k) so stage 8's
      INFINITY sentinel is already handled. The two matrix-recovery fns
      (camera_near_far_from_projection, extract_near_far) are DELETED — the
      breaks-on-infinite-far algebra no longer exists to misuse. Browser A/B:
      3 colored point lights at different depths over floor+spheres — froxel
      lighting identical across conventions (PNG 326,786 vs 326,726 B, 0.02%).
- [x] Stage 6 (this commit): sentinel + reduce flips behind reverse_z template
      axes threaded through SEVEN pass families (decal, ssr, ssr_minz,
      material_prep, material_opaque, material_transparent, effects — the
      three material passes share the SSCS include): decal sky skip
      depth>=1.0→<=0.0; SSCS background reject scene_ndc_z>=1.0→<=0.0; DoF
      load_depth MSAA nearest reduce min→max (its linearize_depth was already
      reverse-aware); SSR trace sky bail flipped + ssr_minz seed/reduce
      min→max (Hi-Z traversal compares audited: they run in LINEAR view space
      via value-agnostic view_pos_from_depth — convention-independent, no flip
      needed; sky tiles under infinite-far unproject to inf cell_z = still
      correctly skipped). material_classify + opaque msaa.wgsl audited: all
      depth compares go through view-space unprojection — no changes needed.
      Browser A/B: SSR glossy-floor reflections identical across conventions
      (157,022 vs 157,266 B, 0.16%); bloom+DoF verified working under reverse.
      320 renderer tests green.
- [x] Stage 7 (this commit): full shadows lockstep. Writers via new
      Shadows.depth DepthConvention (cascade ortho, spot + point/cube persp
      incl. y_flip); pipeline compares + comparison sampler + clear via the
      convention; RASTERIZER DEPTH-BIAS SIGN flipped (with_depth_bias ±1,
      slope ±1.5 — a coupled site the original inventory missed: positive
      bias pushes casters TOWARD the light under reverse = acne). Receiver
      WGSL: 18 {% if reverse_z %} branches on the stage-6 axis — cube
      analytic NDC.z (exact writer mirror, ×4), ref-depth bias sign (×6),
      PCSS blocker tests (×3), PCSS penumbra math (×3), EVSM receiver remap
      (1−2·ndc.z); EVSM writer remap via /*EVSM_DEPTH_REMAP*/ marker
      substitution (guarded by a new test). Bounds checks / gradients (used
      as magnitudes) / POINT_SHADOW_NEAR / caster vertex shaders audited
      convention-agnostic with comments. NEW: wgsl_validation naga-compiles
      the reverse shadow arms (previously browser-only exposure). 322 tests
      green. Browser A/B: floor + 3 boxes + directional+spot+point — shadows
      present + equivalent both conventions (137,970 vs 138,334 B, 0.26%),
      no acne/peter-pan/missing.
- [ ] Stage 8: infinite-far main perspective camera
      (perspective_infinite_reverse_rh; ortho paths stay finite reverse).
- [ ] Stage 9: relax auto_clip_planes ratio cap; synthetic z-fight repros A/B
      (§9.A scenes); flag default ON (?noreversez rollback).

**Status:** stages 1a+1b landed — the sections below are the implementation-ready design.
**Scope:** core renderer (`packages/crates/renderer/`) + the shared cameras
(`web-shared`, editor, model-tests). Reverse-Z is a **global depth convention**;
it is all-or-nothing for the main camera path and cannot be scoped to just the
editor.

> Written 2026-07 after fixing the editor auto near/far z-fighting (see
> `web-shared/src/util/free_camera.rs::auto_clip_planes`). That fix — a bounded
> ~5000:1 far:near ratio — already eliminates the *reported* z-fighting on
> `Depth32Float`. **Reverse-Z is a precision *ceiling* upgrade, not a bug fix.**
> Do it when you want carefree near/far at extreme ranges (huge open worlds,
> tiny near planes), not because something is currently broken.

---

## 1. TL;DR

- Current renderer is **standard forward-Z**: glam `perspective_rh` / `orthographic_rh`,
  WebGPU NDC z ∈ [0,1] (**near→0, far→1**), depth **clear 1.0**, compare
  **`LessEqual`**, `Depth32Float` (desktop) / `Depth24plus` (mobile). "Closer" =
  smaller depth.
- Reverse-Z maps **near→1, far→0** and pairs the reversed distribution with
  float32's exponent bunching → near-uniform precision. The classic "float depth
  is basically free precision" win.
- It is **NOT** "strictly a win, only churn." Real caveats:
  1. **Benefit is conditional on a float depth buffer.** On the mobile
     `Depth24plus` profile the precision gain is ~zero (integer depth is already
     uniform); you pay the complexity for nothing there.
  2. **The risk is a depth-*consumer* audit, not just flipping producer states.**
     Every shader that reads depth and assumes near=0/far=1 silently mis-renders.
     Failures are visual, not crashes — easy to miss.
  3. **Infinite-far (the cleanest reverse-Z) drops the finite far plane** that
     froxel slicing, frustum far bounds, and shadow-cascade fitting rely on.
     Finite reverse-Z keeps them but returns some precision.
- **Recommendation:** worth doing eventually for the precision ceiling (it also
  helps the *game*, not just the editor — stud-scale arenas get rock-solid
  depth), but as its **own** feature-flagged, fully-regression-tested pass.
  Not urgent, not this session.

---

## 2. Why reverse-Z works (one paragraph)

Perspective projection crams almost all post-projection depth resolution near the
near plane: with a [0,1] buffer, ~99% of the depth range is spent in the first few
percent of view distance, starving far geometry → z-fighting. A **floating-point**
depth buffer also crams its precision near 0.0 (denormals/small exponents). Put
the two together in forward-Z and they *compound* the starvation. Reverse them —
near→1.0, far→0.0 — and the float precision near 0.0 now lands where the
projection is starved (far), while the projection's dense region near 1.0 gets the
float's coarse region. The two distributions **cancel** to near-uniform precision.
This only works with float depth; with UNORM/integer depth the buffer precision is
already uniform, so reversing gains almost nothing.

---

## 3. Preconditions & honest downsides

| Concern | Detail |
|---|---|
| **Float depth required** | Win only materializes on `Depth32Float`. Mobile `Depth24plus` (`profile.rs:154`) sees ~no benefit. Decide: force float depth everywhere (mobile memory/bandwidth cost) or accept "reverse-Z is a desktop-only precision win, correctness-neutral on mobile." Either way the *code* must be correct on both. |
| **Depth-consumer audit** | The mechanical producer flips (compares, clears) are easy. The dangerous surface is every shader reading depth: SSCS, decals, DoF MSAA reduce, HZB, "far==1.0" background sentinels. Miss one → silent visual bug. See §6. |
| **Infinite vs finite far** | `perspective_infinite_reverse_rh` gives the best precision but no finite far → froxel far slice, frustum far plane, shadow cascade far, and any far-based logic must be reworked. Finite reverse-Z avoids that but concedes precision. Pick per §7. |
| **Sticky global convention** | After migration, *every future depth-consuming feature* must be authored reverse-Z-aware. Permanent (small) cognitive tax. |
| **Two projection conventions already in-tree** | The renderer crate uses [0,1] Gribb-Hartmann frustum extraction; the `geometry` crate uses [-1,1] OpenGL extraction. Reverse-Z touches both differently. See §6.4. |
| **Not a fix for near-plane clipping** | Reverse-Z changes precision *distribution*, not clipping. Geometry in front of the near plane still clips; you still must choose a sane near. |

---

## 4. Current forward-Z baseline (what "correct after" must preserve)

- Projection: `Mat4::perspective_rh` / `orthographic_rh` (glam 0.32), [0,1] NDC.
- Depth compare: `LessEqual` (a few `Less`).
- Depth clear: `1.0`.
- Depth formats: `Depth32Float` desktop/high (`render_textures.rs:85`,
  `profile.rs:167/197`), `Depth24plus` mobile (`profile.rs:154`); shadow atlas
  always `Depth32Float`.
- Sky/background pixels carry the depth **clear value** (1.0) and are detected
  downstream via `depth >= 1.0`.
- "Closer" = **smaller** depth everywhere (HZB stores **max** = farthest as the
  conservative occluder bound).

Reverse-Z target: near→1, far→0, clear→0.0, compare→`GreaterEqual`, "closer" =
**larger** depth, HZB stores **min** = farthest, background sentinel `depth <= 0.0`.

---

## 5. What is SAFE and needs NO change (verified)

Call these out so the migrator does not waste time — or worse, "fix" something
that is already convention-agnostic and break it:

- **LOD selection** (`lod.rs:107-139`, call site `render.rs:2577-2581`) — pure
  **world-space distance** + `tan_half_fov_y` (Y-scale only). No depth. ✅
- **Clustered / Nanite-style virtual geometry** (`cluster_lod/…/cluster_cut.wgsl`,
  CPU `cluster_lod.rs:126-138`) — projected screen-space error from **world-space
  distance**; never binds the HZB or a depth texture. ✅
- **Light-culling froxels** (`light_culling/…`, `froxel_walk.wgsl:40-43`) — side
  planes fixed at z=0 (Z-independent), z-slicing in **linear view space**. ✅
  (But its z-slice *near/far inputs* come from the broken algebra in §6.5.)
- **Picking** (`picker/…/compute.wgsl`) — reads the **visibility-ID** texture, not
  depth. ✅ (Nearest-surface disambiguation already done by the geometry depth
  test upstream.) *Note: harmless `;;` typo at `picker_wgsl/compute.wgsl:40`.*
- **All `inv_proj` / `inv_view_proj` reconstruction** (material_prep
  `compute.wgsl:187-193,283-286`; shadow_blur `compute.wgsl:30-52`; classify;
  `viewSpaceDepth` in `material_opaque/…/helpers/msaa.wgsl:187-200`; decal
  world-pos; SSCS linearize `shadow/bind_groups.wgsl:358-360`) — **value-agnostic**:
  feed any depth value, it round-trips. ✅
- **`clip.w <= 0.0` near guards** (cull.wgsl, SSCS, decal_classify) — w-based,
  convention-safe. ✅

---

## 6. Migration inventory (what MUST change)

Paths relative to `packages/crates/renderer/src/` unless noted.

### 6.1 Depth-stencil pipeline states — compare `LessEqual`→`GreaterEqual`, clear `1.0`→`0.0`

Compares (all → `GreaterEqual`, `Less`→`Greater` for the line no-test-off variant):

| Pipeline | file:line |
|---|---|
| Geometry opaque | `render_passes/geometry/pipeline.rs:516-518` |
| Geometry masked | `render_passes/geometry/masked_pipeline.rs:231-233` |
| Geometry custom-vertex | `render_passes/geometry/custom_vertex_pipeline.rs:294-296` |
| Geometry masked custom-vertex | `render_passes/geometry/masked_custom_vertex_pipeline.rs:276-278` |
| Transparent | `render_passes/material_transparent/pipeline.rs:489-491` |
| Lines | `render_passes/lines/pipelines.rs:241-249` (`Always` off-variant unaffected; `Less`→`Greater`) |
| Shadow custom-vertex | `render_passes/shadow_custom_vertex/pipeline.rs:270-272` |
| Shadow masked custom-vertex | `render_passes/shadow_masked_custom_vertex/pipeline.rs:278-280` |
| Shadow base | `shadows/helpers.rs:287-289` |
| Shadow comparison sampler | `shadows/state.rs:692` |
| Editor grid | `web-shared/src/viewport3d/grid/pipelines.rs:98` (`Less`→`Greater`) |

Depth clears (→ `0.0`):
- Geometry `render_passes/geometry/render_pass.rs:162-164`
- HUD depth `render.rs:2398-2400` (see also comment `render.rs:2364`)
- Shadow `shadows/render_pass.rs:143-146`

Decal composite has **no** depth attachment (`material_decal/composite.rs:210`) — nothing to flip.

### 6.2 Projection matrix builders

Swap `perspective_rh` → reverse-Z (finite: hand-built reversed matrix or
`perspective_rh` with near/far swapped **and** verify glam's result; infinite:
`perspective_infinite_reverse_rh`). Ortho reverse-Z: swap near/far in
`orthographic_rh` (or negate the z-row).

- Main camera: `camera.rs:69`; **frustum-ray reconstruction** bakes near-at-z=0
  (`camera.rs:323-328`) → revisit; `inv_projection` upload `camera.rs:195,208-215`.
- Editor free camera: `editor/src/engine/render_loop.rs:386` (persp) / `:390` (ortho).
- web-shared orbit camera: `web-shared/src/util/free_camera.rs:491` (persp) /
  `:432` (ortho); `auto_clip_planes:495-520` — the 5000:1 ratio cap can be
  **relaxed/removed** under reverse-Z (that's the payoff), but keep a sane near.
- model-tests: `model-tests/…/camera/projection/{perspective.rs:65,orthographic.rs:86-93}`.
- Shadows: `shadows/cascade.rs:223` (ortho), `shadows/state.rs:2192` (spot persp),
  `shadows/state.rs:2338-2339` (point/cube persp) — see §6.7.
- Examples: `examples/multithreaded/src/*_demo.rs`, `examples/render-worker/src/worker.rs:353`.

### 6.3 HZB / occlusion culling — HIGHEST RISK (four coupled flips)

Forward-Z stores **max** depth (farthest) as the conservative occluder; occluded
when a bound's **nearest** (min) depth is *farther* than the HZB. Reverse-Z: near
is the max, so the conservative/farthest occluder is the **min**.

- Seed MSAA reduce `hzb/shader/hzb_wgsl/seed.wgsl:25-32` — `max`→`min`.
- Mip reduce `hzb/shader/hzb_wgsl/reduce.wgsl:42-43` — `max(max,max)`→`min(min,min)`.
- Occlusion test `occlusion/shader/occlusion_wgsl/cull.wgsl`:
  - `:121` `depth_min = 1.0` init → `0.0`.
  - `:142` `depth_min = min(depth_min, ndc.z)` (nearest) → `max`.
  - `:210` footprint `max(max,max)` → `min(min,min)`.
  - `:214` `if (screen.depth_min > hzb_depth) cull` → `<`.
- Compaction `occlusion/…/compaction.wgsl` — no depth, no change.

**These four (init, two min/max, one compare) are coupled. Flipping a subset
silently over- or under-culls** — the classic reverse-Z bug.

### 6.4 Frustum extraction — TWO conventions + a stale comment

- Renderer crate ([0,1] Gribb-Hartmann) `frustum.rs:40-62`: `near = row2`,
  `far = row3 - row2`. Reverse-Z: **swap** → `near = row3 - row2`, `far = row2`.
  Only these two lines; consumers (`renderable.rs:109`, `transforms.rs:122`,
  `scene_spatial/node.rs:63-73`, `shadows/importance.rs:80`) reuse the planes.
- GPU copy `occlusion/…/cull.wgsl:81-82` — same swap. **⚠️ Comment `:74` already
  says the stale OpenGL form `near = row3 + row2`; the comment is wrong today and
  will actively mislead the migrator.** Fix code AND comment.
- `geometry` crate ([-1,1] OpenGL) `packages/crates/geometry/src/frustum.rs:28-65`
  — different convention (`near = r3 + r2`). Used via `from_view_projection` in
  `shadows/importance.rs`. **Audit which matrices feed it** and apply the correct
  (different) swap; do not blindly copy the renderer-crate change here.

### 6.5 near/far recovery from the projection matrix — breaks outright

Both invert `perspective_rh`'s specific entries; both break on reverse-Z and on
infinite-far (`proj[2][2]≈0`):
- `render.rs:2447-2472` `camera_near_far_from_projection` — feeds froxel z-slicing
  (`light_culling/buffers.rs:407-426`). Currently falls back to hardcoded
  `(0.1,1000.0)` when `proj[2][2]≈0` → wrong slices under reverse-Z.
- `shadows/helpers.rs:351-363` `extract_near_far` — feeds cascade fitting.

Rework both with reverse-Z-aware algebra (mirror the DoF branch in §6.6), or —
cleaner — **thread the real near/far through explicitly** instead of recovering
them from the matrix.

### 6.6 Depth-consumer shaders (the easy-to-miss surface)

- **DoF** `effects/…/helpers/dof.wgsl` — `linearize_depth:7-24` is **already
  reverse-Z-aware** (branches on `abs(proj[2][2])<1e-4`, a ready template). BUT
  `load_depth:60-71` MSAA reduce uses `min` (nearest=min) → reverse-Z nearest=max.
  **Partial; finish it.**
- **Decal** `material_decal/…/compute.wgsl:34-38` — `if depth >= 1.0 {skybox skip}`
  → `<= 0.0`.
- **Decal classify** `material_decal/classify/…/compute.wgsl:133-138` — tracks
  `min_depth` as "closest" for an HZB-style test; align with §6.3 (min→max).
- **SSCS** `shared/shared_wgsl/shadow/bind_groups.wgsl:347-350` — background reject
  `if scene_ndc_z >= 1.0 continue` → `<= 0.0`. (The march itself is linear
  view-space, safe.)
- **Material classify / opaque MSAA** (`material_classify/…/compute.wgsl:414-513`,
  `material_opaque/…/helpers/msaa.wgsl:91-199`) — comparisons on **linear**
  view-space deltas → safe, but re-read to confirm no raw-depth `<`/`>`.
- **SSR Hi-Z min-Z pyramid (NEW consumer — added by the M2c SSR work)** —
  `render_passes/ssr_minz/shader/ssr_minz_wgsl/{seed,reduce}.wgsl` build a
  dedicated **min-Z** pyramid (per-tile NEAREST depth), and
  `render_passes/ssr/shader/ssr_wgsl/trace.wgsl` (`{% if hiz %}` block) marches
  it with `if (ray_z < cell_z) { skip }`. This is a **second, SSR-private HZB**
  and is convention-dependent EXACTLY like the occlusion HZB (§6.3): forward-Z
  nearest = **min**, reverse-Z nearest = **max**. Under a reverse-Z flip the
  seed/reduce op must flip `min`→`max` and the trace's in-front test
  (`ray_z < cell_z`, i.e. "closer = smaller depth") must flip with it, in
  lockstep with the reflection ray's own depth reconstruction
  (`view_pos_from_depth`, which is value-agnostic and safe). **Currently the
  SSR pyramid is only allocated/built when `SsrTrace::PRODUCTION` is `HiZ`
  (it is `LinearDda` today), so it is DORMANT — but it must be on this audit
  list before Hi-Z is ever promoted.** See the M2c finding below (§9.E) — the
  coarse-mip banding is partly forward-Z far-precision starvation, so reverse-Z
  is expected to *improve* Hi-Z quality, not just preserve it.

### 6.7 Shadows — migrate in lockstep, or keep forward-Z independently

Shadows use **separate** RH projections and a **separate** depth atlas, so they
**can stay forward-Z even if the main camera flips** (this is a legitimate,
lower-risk option — keep shadow z-fighting behavior identical). If you *do*
migrate them, everything below must move together (writer + receiver + compare +
clear):
- Writers: `cascade.rs:223` (ortho), `state.rs:2192` (spot), `state.rs:2338-2339`
  (point/cube, note the Y-flip), `consts.rs:78` `POINT_SHADOW_NEAR`.
- Receiver NDC.z formula (forward-Z, must match writer):
  `shadow/bind_groups.wgsl:458,492-494`, gradient `:514-515`, point mirror `:382`.
- Receiver bounds `ndc.z < 0 || ndc.z > 1` at `:825,887,1087,1381,1447`; ref-depth
  `ndc.z - bias·grad` at `:900,1097`.
- EVSM moment remap `evsm.rs:472` (`z = 2·depth - 1`) + receiver `bind_groups.wgsl:858`.
- `extract_near_far` §6.5. Compare `state.rs:692`, clear `render_pass.rs:146`.

**Decision to record at implementation time:** shadows-forward-Z + main-reverse-Z
is fully valid and cuts the blast radius roughly in half. Recommended for the
first landing; migrate shadows separately later if desired.

### 6.8 Skybox / background sentinels

- Skybox is **not** depth-tested — written where the visibility buffer is empty
  (`material_opaque/…/skybox_primary.wgsl:44-51,100-103,155`); ray reconstruction
  `helpers/skybox.wgsl:10-25` unprojects at z=0 (convention-safe for perspective).
  **No change to the skybox draw itself.**
- BUT sky pixels carry the depth **clear value**; flipping the clear 1.0→0.0 is
  what forces every `depth >= 1.0` background check (§6.6: decal, SSCS, DoF) to
  become `<= 0.0`. These sentinels are the most scattered, easiest-to-miss part.

---

## 7. Design decisions to lock before coding

1. **Infinite-far vs finite reverse-Z.** Recommend **finite reverse-Z first**
   (keeps froxel far, frustum far, cascade far working with minimal rework; still
   a large precision win with float32). Move to infinite-far only if a scene needs
   unbounded range — and then rework §6.5 far handling.
2. **Shadows scope.** Land main-camera reverse-Z with **shadows kept forward-Z**
   (§6.7). Smaller blast radius, independently verifiable.
3. **Mobile depth format.** Either (a) accept reverse-Z is precision-neutral on
   `Depth24plus` (correctness still required), or (b) force `Depth32Float`
   everywhere and eat the mobile cost. Recommend (a).
4. **near/far propagation.** Prefer **threading explicit near/far** to froxel and
   cascade code over matrix-inversion recovery (§6.5) — more robust and
   convention-independent for the future.
5. **Feature flag.** Gate the whole thing behind a build/profile flag
   (`reverse_z: bool`) so it can be toggled for A/B regression and rolled back.
   The projection builder, clear value, compare direction, HZB reduce op, frustum
   swap, and background sentinel all read one flag. This is the single most
   valuable de-risking step — do it first.

---

## 8. Implementation order (staged, each stage independently testable)

1. **Introduce the `reverse_z` flag** (default off) plumbed to: projection
   builders, depth clear, depth compare, HZB reduce op, frustum extraction,
   background-sentinel comparison, and DoF/decal/SSCS depth checks. No behavior
   change while off.
2. **Projection + clear + compare** (geometry only; shadows stay forward-Z).
   Verify a single opaque scene renders identically (visually) with the flag on.
3. **HZB + occlusion** (§6.3) — the four coupled flips together. Verify occlusion
   culling matches forward-Z (no missing/extra geometry) via the culling stats /
   heatmap.
4. **Frustum extraction** (§6.4) both crates + fix the stale GPU comment.
5. **near/far recovery** (§6.5) — froxel slices + (if migrating) cascade fitting.
6. **Depth-consumer sentinels** (§6.6) — decal, SSCS, DoF `load_depth`.
7. **(Optional, later) Shadows** (§6.7) in lockstep.
8. **(Optional, later) Infinite-far** + far-handling rework.
9. Relax `auto_clip_planes` ratio cap (`free_camera.rs`) to exploit the new
   precision (bigger far / smaller near).

---

## 9. Test plan

Run each with the flag **off** (baseline) then **on**; diff. Test on both a
`Depth32Float` and a `Depth24plus` profile.

**A. Precision / z-fighting (the point of the exercise)**
- Coplanar / near-coplanar surfaces at **far** range (decals on a distant wall,
  stacked thin platforms) — should stop z-fighting under reverse-Z where forward-Z
  fought. Capture at extreme near:far (e.g. near 0.05, far 50000).
- Thin close geometry at grazing angles.
- Stud-scale arena (the jetpack-knockout arena, ~440u) from many orbit distances.

**B. Occlusion culling correctness (HZB)**
- Culling-stats / heatmap parity vs forward-Z: no popping, no missing occludees,
  no over-culling. Camera behind, inside, and grazing large occluders.
- Fully-occluded object count matches forward-Z within tolerance.
- MSAA on/off (seed reduce path) and mobile `Depth24plus`.

**C. Frustum culling**
- Objects straddling near & far planes appear/disappear at the correct planes.
- Both extractors: renderer-crate consumers (culling, spatial) AND geometry-crate
  path (`shadows/importance.rs`).

**D. Shadows** (whether kept forward-Z or migrated)
- Directional cascades: no acne / peter-panning regression at cascade splits.
- Spot + point/cube: contact hardness, bias, EVSM moments unchanged.
- SSCS contact shadows land in the same screen positions (background reject flip).

**E. Depth consumers**
- Decals: project correctly, and **do not** bleed onto the skybox (background
  sentinel flip).
- DoF: focus plane + background bleed correct; MSAA `load_depth` nearest-pick
  correct.
- SSCS: no halos, background pixels excluded.
- Any position-from-depth reconstruction: world positions match forward-Z
  (should be identical — reconstruction is value-agnostic; this is a regression
  guard).
- **SSR trace (LinearDda, production):** view-space ray march + `view_pos_from_depth`
  reconstruction only — value-agnostic, so it is a regression guard, not an
  expected change. Confirm reflections are identical flag-off vs flag-on.
- **SSR Hi-Z min-Z pyramid (only if `SsrTrace::PRODUCTION` is promoted to `HiZ`):**
  after flipping seed/reduce `min`→`max` and the trace in-front test (§6.6),
  reflections must match the DDA reference. **Concrete scenario + the M2c
  finding this test exists to catch:** a dark near-mirror floor (dielectric
  roughness≈0.05, `ssr_max_distance`≈80, `ssr_max_steps`≈96) with three tall
  emissive objects (red/green/blue spheres+box) reflecting in it — the exact
  scene in `scratchpad/ssr_m2c_test.py`. Forward-Z Hi-Z shows **horizontal
  banding** in the reflected objects that DDA does not; an A/B proved the bands
  come entirely from the **coarse mip levels** (capping `max_lod` to 0 = pure
  fine march = smooth; any `max_lod > 0` bands). Root cause is two-fold: (1) the
  traversal advances a *fraction* of a tile instead of to the exact screen-space
  cell boundary (a traversal bug, fixable independent of reverse-Z — needs the
  McGuire/Mara 2014 cell-boundary DDA), AND (2) forward-Z's far-plane precision
  starvation coarsely quantizes the per-tile min-Z values at distance, which
  **amplifies** the banding. Reverse-Z is expected to *reduce* (not fully
  eliminate) it by restoring far-range precision, so this scene is the primary
  before/after visual for Hi-Z-under-reverse-Z: bands should shrink noticeably.
  Test from several orbit radii (near + far) since the effect is distance-driven.

**F. Cameras**
- Editor orbit (perspective + ortho): no clipping across zoom range; `Reset View`
  and framing correct.
- Manual clip override still works.
- Scene/game cameras (chase cam) unaffected or correctly reversed.

**G. Froxel light culling**
- Light assignment per froxel matches forward-Z (z-slice near/far correct after
  §6.5 rework). Test with many point/spot lights at varied depths.

**H. Picking**
- Pixel picks return the same front-most surface as forward-Z.

**I. Cross-cutting**
- Editor **and** game (jetpack-knockout) both regress-tested — the renderer is
  shared; the autonomous game build must not break.
- Transparent-over-opaque ordering unchanged.
- HUD layer (separate depth) composites correctly.
- Wireframe / gizmo / grid / line overlays depth-test correctly.

**J. Automated (where feasible)**
- Golden-image diffs for a handful of fixed scenes (opaque, transparent, shadowed,
  decaled, DoF) flag-off vs flag-on: expect **near-identical** output except
  reduced z-fighting. Any large diff = a missed consumer.

---

## 10. Rollback

Because everything is behind the §7.5 flag, rollback = flip the default off. Keep
the flag through at least one release after landing so field issues can be A/B'd.

---

## 11. Known-safe reference list (do not touch)

LOD (`lod.rs`), clustered/Nanite (`cluster_lod/…`), froxel side planes + linear
z-walk, picking (visibility-ID), all `inv_proj`/`inv_view_proj` reconstruction,
`clip.w<=0` guards. See §5 for file:line. Touching these to "make them reverse-Z"
is a mistake — they are already convention-independent.

---

## 12. Top-5 traps (from the audit)

1. HZB reduce + occlusion test direction — four coupled flips
   (`reduce.wgsl:42`, `seed.wgsl:25`, `cull.wgsl:121/142/210/214`).
2. Stale GPU frustum comment `cull.wgsl:74` (says `row3+row2`, code uses `row2`) —
   misleads the migrator.
3. Two CPU frustum conventions — `frustum.rs:57` ([0,1]) vs
   `geometry/src/frustum.rs:61` ([-1,1]) — different swaps.
4. Scattered `depth >= 1.0` background sentinels — decal `compute.wgsl:35`,
   SSCS `bind_groups.wgsl:347`, DoF `load_depth` min-reduce `dof.wgsl:60`.
5. `camera_near_far_from_projection` (`render.rs:2447`) + `extract_near_far`
   (`shadows/helpers.rs:351`) — forward-Z algebra, break on reverse-Z/infinite-far,
   silently mis-feed froxels + cascades.
6. **Second, easy-to-forget HZB:** the SSR **min-Z pyramid**
   (`ssr_minz/…/{seed,reduce}.wgsl` + `ssr/…/trace.wgsl` `{% if hiz %}`) is a
   convention-dependent nearest-depth pyramid just like the occlusion HZB (§6.3)
   and needs the same `min`→`max` + in-front-test flip. It is DORMANT today
   (`SsrTrace::PRODUCTION == LinearDda`), so it will not show up in a
   "what's-currently-bound" audit — flag it before promoting Hi-Z. (Bonus: its
   coarse-mip banding is partly forward-Z far-precision, so reverse-Z should
   help — see §9.E.)
