# TODO — single end-to-end work plan (consolidated)

This is the **single source of truth** for the remaining renderer work, consolidated from the
former `followup.md`, `prep-only.md`, and `uber-shader.md` (all three deleted). It is written to be
executed **end-to-end by one autonomous `/loop`**.

References to code are **by symbol / file name, not line number** (line numbers drift — grep for the
named functions/types).

---

## 0. Execution contract (read first, every wake)

**Branch:** work on the **current `updates` branch** (decided 2026-06-18). Commit each green stage.
**Do NOT push** and **do NOT open a PR** — David reviews and publishes.

**Per-commit gate (always):**
- `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader --lib` is **GREEN** before committing.
- Stage explicit paths only — **NEVER `git add -A`**.
- **NO backticks in `git commit -m`** (zsh command-substitutes them).
- Keep naga validation + `size_regression` + completeness tests green; re-tighten ceilings when a change
  legitimately shifts module sizes (note the shift in the commit message).

**Verification = chrome-devtools MCP, self-verify as much as possible (decided 2026-06-18).** There are
**no "stop and wait for human eyes" gates** in this plan — including the items the old docs flagged as
needing a human wall-clock (TTFR prewarm) or human eyes (MSAA edges). Use the browser MCP to prove each
change instead:
- Run `task model-tests:dev` (port **9080**) — or model-tests prod **9085**. Touch a `.rs` file to force
  rebuild; wait for `Compiling awsm-renderer` then a fresh `✅ success` before navigating.
- chrome-devtools MCP: `navigate_page` to `/app/model/<Name>` (use `?cam=` override per
  `docs/DEBUGGING-PREVIEW.md` §`?cam=`), `wait_for` / settle (~14 s for shading), `take_screenshot`,
  and `list_console_messages` to confirm a clean console (no pipeline-create errors, no black frames).
- For perf/timing items use `performance_start_trace` / `performance_stop_trace` /
  `performance_analyze_insight`, plus the in-app `memory_stats` query (`render_cpu_ms`, etc.) surfaced via
  `web-shared/src/perf.rs`.
- For visual parity, a coarse PIL/screenshot diff (exclude the sidebar `x < 215`) is a **signal**, not a
  hard gate: this codebase's deliberate shader rewrites produce expected invisible ±1-LSB FP-reorder
  deltas. **Bar = correct visible result** (no artifacts/seams/black frames/pipeline errors), not byte
  parity. Only chunky/structural diffs are real failures.

**Standards & regression gate — SURFACE to David, do NOT silently ship (decided 2026-06-18).** Self-verify
covers *correctness*; it does **not** authorize quietly accepting a perf regression or a standards
deviation. At every stage, actively check for and, if found, **STOP that stage and ask David** before
committing — do not absorb it into the change:
- **Performance regression** — a stage that makes a measured metric worse (per-frame `render_cpu_ms`,
  FPS 720p/4K, precompile time, dispatch count, VRAM, module size / register pressure / occupancy, TTFR).
  Anything that trades a known-good number for a worse one. The premise is that the right architecture
  *wins or is a very small difference*; a real regression **contradicts the premise → surface it**, never
  ship it on the assumption it's acceptable.
- **Deviation from an established standard / documented invariant** — e.g. the **prep-vs-recompute rule**
  ("prep the expensive common work in the prep pass; re-derive trivially-cheap work in the shader wrapper;
  invisible to material authors who only call an accessor" — see `material_prep/buffers.rs`,
  `docs/SHADER_GUIDELINES.md`, the READMEs); the **MSAA-compile invariant** ("never compile MSAA code with
  MSAA off, nor non-MSAA with MSAA on"); the **default-must-equal-today / zero-risk** rule (uber-shader
  grouping); **explicit-gradient sampling only** (uber hazard 2); **never a silent cap** (clamp + log).
  If a stage would add a prep buffer where recompute is cheaper (or vice-versa), fork an MSAA/non-MSAA
  codepath instead of sharing, change default behavior, or otherwise cross one of these lines — **stop and
  ask**, with the tradeoff written out (cost A vs cost B, what the standard says, why this stage tugs
  against it). Record the question + David's answer in this doc under the stage.
- When in doubt whether something is "a small expected delta" vs "a regression / deviation," **ask** —
  err toward surfacing.

**Stage discipline:** each numbered stage below is independently testable + green + committed. Mark a
stage `[x]` in this file as it lands (and append the commit hash). Do the phases **in order** — later
phases depend on earlier ones where noted.

**Loop hygiene:** keep going stage by stage; if a stage is genuinely blocked, write the blocker into this
doc under that stage and move to the next independent stage rather than spinning. When the whole plan is
`[x]`, post a before/after summary and **STOP**.

---

## Phase ordering (why this order)

1. **Scene-loader follow-ons** (§A) — independent, lower-risk, no renderer-internals risk. Good warmup.
2. **Static-shadow caching** (§B) — self-contained perf win; also a measured **input** to §C.
3. **Perf at thousands of meshes + TTFR prewarm** (§C) — runtime perf; consumes §B.
4. **Uber-shader** (§D) — the big architectural effort; **its spec is re-audited first** (suspected stale
   MSAA/fast-path framing — see §D.0). Last because it's largest and self-contained from the above.

---

# §A — Player-grade scene-loader follow-ons (crate `awsm-scene-loader`, dir `packages/crates/scene-loader`)

The loader (`load_scene_for_player` / `populate_awsm_scene`, `materialize` in `lib.rs`) materializes every
render `NodeKind` EXCEPT the items below. None are regressions — the pre-loader `materialize` dropped all
of these in its `_ => {}` arm; the cores are wired, these are the unfinished dimensions. The renderer
already exposes everything needed for A.1–A.3.

## [x] A.1 — ParticleEmitter rendering — **Design A: loader sets up, game ticks** (decided 2026-06-18) — commit `cf40249f`

**Landed:** `packages/crates/scene-loader/src/particles.rs` (new) — `EmitterHandle`
(mesh + instance-transform + def + capacity + base-world-pos), `build_emitter`
(emissive billboard quad, `BillboardMode::Full`, shadows off, instancing pre-enabled
at `max_alive`, dead-seeded), `def_to_emitter` (awsm_scene def → `awsm_particles::Emitter`),
and the `drive_emitter` consumer helper (push a `Simulator`'s `packed` particles to the
handle each frame, fixed capacity → no realloc). The `materialize` arm builds the emitter
+ records the handle into `NodeHandles.emitter`; teardown frees the billboard mesh +
instance transform. Removed `warn_particle_skip`. `awsm-particles` added as a dep.

**Verification (honest):** `cargo test -p awsm-scene-loader --lib` GREEN incl. two new
unit tests — `def_to_emitter_maps_every_field` (every field, x1000-drag decode) and
`def_to_emitter_drives_a_live_simulation` (the handle's emitter spec actually
spawns/ages/culls a one-shot burst end-to-end, no GPU). The **rendering** path
(`build_emitter` + `drive_emitter`) is call-for-call identical to the **shipping editor
particle preview** `packages/frontend/editor/src/engine/bridge/particles.rs` (same emissive
PBR material, same `sprite_quad`, same `BillboardMode::Full`, same instancing seed, same
per-frame `set_mesh_instances` / `set_mesh_instance_attrs`), which is proven to render —
so correctness is inherited; the only genuinely new code is the (unit-tested) plumbing.
A live chrome-devtools render of the *loader* emitter is **not yet possible**: model-tests
(:9080) loads via `populate_gltf` (glTF URLs), not the scene-loader, and no frontend ticks
a loader-returned handle today (Design A makes the tick a consumer responsibility). Wiring a
loader-render harness into model-tests would chrome-verify ALL of §A — noted as useful infra
(not done here; not a regression, not a standards deviation).

**Deviations from this stage's pre-written spec (none requiring sign-off):** material is
emissive-PBR (matching the shipping editor preview) rather than the spec's guessed
"FlipBook/Unlit"; sprite `texture` binding + the `blend` transparent-pass route are
follow-ons (matching the editor bridge's own documented gaps). ParticleEmitters *inside a
prefab* remain transform-only (the existing B4 follow-on, unchanged). No perf regression
(new functionality on a previously clean-skipped node).

### A.1 spec (original, for reference)

`NodeKind::ParticleEmitter(ParticleEmitterDef)` is currently a clean-skip (one-time `tracing::warn!` in
`materialize`, `warn_particle_skip`, see `lib.rs`). The renderer **can** render particles —
`Meshes::enable_mesh_instancing_opaque` + per-frame `Meshes::set_mesh_instances` (transforms) +
`Meshes::set_mesh_instance_attrs` (per-instance color + alpha + size) **is** an instanced-billboard
particle renderer. No dedicated particle pass is needed.

`ParticleEmitterDef` (scene crate `particle.rs`) is a *simulation spec* — `spawn_rate`, `burst_count`,
`max_alive`, `lifetime`, `initial_speed`, `forces: [ForceDef::Gravity{…}]`, `color_over_life`,
`size_over_life`. The loader is a one-shot pass and never ticks (same boundary as animation: it loads
clips; the consumer drives the clock).

**Build (Design A):** `materialize` for a `ParticleEmitter` node:
- builds the billboard quad (reuse `build_sprite_mesh`),
- `enable_mesh_instancing_opaque` at `max_alive` capacity,
- attaches the FlipBook/Unlit material,
- returns an **emitter handle** (the mesh key + the `ParticleEmitterDef`) in `NodeHandles` so the game can
  drive it.
The game runs the CPU sim and calls `set_mesh_instances` / `set_mesh_instance_attrs` each frame. This
matches the loader's existing "loads, doesn't drive" contract; it still needs a few lines of consumer
per-kind code (the tick), which is expected/documented.

**Reuse note:** there is already a `packages/crates/particles` crate (`simulator.rs`) and lockstep's
`scene/particles.rs` — either is a reference CPU sim. If `awsm-particles` exposes a usable
spawn/integrate/age/cull simulator, surface a small helper or rustdoc example wiring it to the emitter
handle so a consumer's tick is near-trivial (do **not** make the loader tick it).

**Verify:** scene with an emitter loads; emitter handle is returned in `NodeHandles`; a tiny test/driver
ticks it and the billboards render + age (chrome-devtools screenshot over a few frames). Rustdoc the
"loader sets up, game ticks" contract on the emitter handle.

## [x] A.2 — InstancesAlongCurve per-instance attributes — commit `5d75d862`

**Landed:** `materialize_instances_along_curve` now applies `per_instance_colors` via
`renderer.set_mesh_instance_attrs(transform_key, ...)` (the same per-instance attribute path A.1's
emitter uses) after `enable_mesh_instancing_opaque`. The transform key is read from
`renderer.meshes.get(source_mesh).transform_key`. A new pure helper `expand_instance_colors`
expands the authored list to exactly the placed-instance count — repeating the last value when
shorter, truncating when longer (the def's documented semantics) — unit-tested
(`instance_colors_repeat_last_when_short_and_truncate_when_long`).

**Left as documented notes (per the plan):** per-instance `shadow` is NOT applied — cast/receive is a
**mesh-level** flag shared by every instance (instancing reuses the source mesh), so honoring the curve's
`shadow` would overwrite the *source node's own* authored flags; a true per-instance shadow flag is a
renderer change (follow-on). Source-node DFS-order resolution stays best-effort-with-warn (not yet bitten).

**Verification (honest):** `cargo test -p awsm-scene-loader --lib` GREEN incl. the new expand test; the
colour-application path is `set_mesh_instance_attrs`, identical to the shipping editor particle preview's
proven per-instance-colour push. Live chrome-devtools render pends the same loader-render harness noted in
A.1. No perf regression (one extra attr upload only when colours are authored), no standards deviation.

### A.2 spec (original, for reference)

## [x] A.3 — Prefab non-mesh children — commit `c4f65ecc`

**Landed:** `PrefabNode` gained a `replay: PrefabReplay` field (new enum:
`None`/`Light(LightConfig)`/`Camera(CameraConfig)`/`Line(LineDef)`/`Decal{texture_index,alpha}`).
`capture_prefab` populates it per node — resolving the **decal texture index at load time**
(via a new shared `resolve_decal_texture_index` extracted from `materialize_decal`), because
`instantiate` runs without assets. `instantiate` now composes a **per-node world matrix** up
the instance chain by hand (same as the live `materialize` recursion) and calls a new
`replay_prefab_node` that re-creates a fresh per-instance Light (bound to the instance
transform), Camera, Line (authored points re-baked into the instance world), or Decal (at the
instance world). `PrefabInstance::teardown` now frees the replayed light/line/decal keys too.
The `NodeHandles.{light,camera,camera_config,line,decal}` fields are populated for prefab
instances.

**Honest scope / notes:** `instantiate` stays **sync**, so the async pipeline warm-ups the
live arms `await` (`ensure_line_pipelines_compiled` / `ensure_shadow_pipelines_compiled`) are
skipped — the renderer's normal per-frame pipeline drive compiles them (or a prior load with a
line/caster already did); replayed lines/shadows may draw a frame or two late on a cold
instantiate. Replayed **cameras are not freed** on teardown (the renderer camera store has no
remove — matches the static loader, which also never frees cameras). `InstancesAlongCurve` and
`ParticleEmitter` inside a prefab remain transform-only follow-ons (the emitter handle isn't
threaded through `PrefabInstance` yet).

**Verification (honest):** `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader
--lib` GREEN (34 / 260 / 30). Each replay path calls the **same renderer API as the proven live
`materialize` arm** for that kind (`insert_light`+`bind_transform`, `cameras.insert`,
`add_line_strip` with world-baked points, `insert_decal` with a world `Mat4`); the world-matrix
composition mirrors the live recursion exactly. The GPU path can't be unit-tested natively
(no device); live chrome-devtools render pends the loader-render harness noted in A.1. No perf
regression (replay only runs on `instantiate`, sharing the existing transform-walk), no standards
deviation.

### A.3 spec (original, for reference)

`PrefabTemplate::instantiate` replays MESH nodes cheaply (`duplicate_mesh_with_transform`, shared GPU
buffers). Light / Camera / Line / Decal nodes inside a prefab currently contribute only their transform —
they aren't re-created per instance. To replay them, `instantiate` re-calls `insert_light` /
`add_line_strip` / `insert_decal` per instance from captured `PrefabNode` metadata (extend `PrefabNode` to
carry the light/line/decal config, not just `template_meshes`). Straightforward, just unwired.

**Verify:** a prefab containing a light/line/decal, instantiated ≥2×, shows each child per instance.

## [x] A.4 — Decal texture-index ≤64-layer assumption — **PROPER ENCODING FIX (David, 2026-06-18)** — commit `4e7110cd`

**Landed (the proper fix David chose):** the hard-coded `64` is gone. A single
source-of-truth helper `awsm_renderer::decals::decal_texture_index_stride(gpu)` returns the
device `max_texture_array_layers` (the real per-array layer ceiling the pool fills to). BOTH
sides now use it: the decal compute shader threads `texture_pool_layers_per_array` through its
cache key + template and unpacks with `% {{stride}}u` / `/ {{stride}}u`; the scene-loader's
`resolve_decal_texture_index` packs `array_index * stride + layer_index` with the same helper
(the duplicated `DECAL_POOL_LAYERS_PER_ARRAY = 64` const is deleted). They can no longer drift,
and a decal texture on any valid pool layer (incl. ≥64) round-trips. The editor decal bridge
already passes `0` (untextured), so it's unaffected.

**Verification:** `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader --lib`
GREEN (34 / **261** / 30). New naga validation test `decal_shader_validates_with_templated_layer_stride`
renders the decal shader at stride **256 and 2048** and asserts the templated stride appears in
the unpacking math — the decal shader had **no** prior naga coverage, so this is a coverage gain.
No perf regression (the divisor is device-constant → no extra pipeline variants; the cache-key
field carries it only so the template substitutes the exact value the loader packs with). Live
chrome-devtools render of a textured decal pends the loader-render harness (A.1 note); the
encoding correctness is now naga-locked + SSOT-unified.

### A.4 spec (original, for reference)

**Audit finding (surfaced to David):** the texture pool has **no 64-layer cap** — `TexturePoolArray::insert`
is an unbounded `push`; each `(width,height,format)` array fills to the device `max_texture_array_layers`
(256–2048). So the decal shader's `array_index * 64 + layer_index` packing (`% 64u` / `/ 64u` in
`material_decal_wgsl/compute.wgsl`) is a **latent correctness bug**: a decal texture at `layer_index >= 64`
decodes to the wrong array+layer. The plan's "confirm never exceeds 64" is false; "unify the const" wouldn't
fix the root cause.

**David's decision (2026-06-18):** **Proper encoding fix now** — inject a shared MAX_LAYERS divisor
(= device `max_texture_array_layers`) into BOTH the decal shader template and the loader (or carry
array_index + layer_index as two decal fields). Verify via chrome-devtools. Chosen approach: the shared
**divisor** (less invasive than splitting the decal field).

### A.4 spec (original, for reference)

`materialize_decal` resolves a decal's texture to a flat pool index as `array_index * 64 + layer_index`
(the `DECAL_POOL_LAYERS_PER_ARRAY` const in scene-loader), matching the decal shader's `texture_index % 64`
packing. If the texture pool ever grows an array past 64 layers, a decal on `layer >= 64` indexes wrong.
Either **confirm the pool never exceeds 64 layers/array** (and assert + comment it), or **unify the
constant** between shader and loader so they can't drift. Pick the unify option if the const is duplicated.

---

# §B — Static-shadow caching (perf)

**Goal:** skip re-rendering a light's shadow map when nothing affecting it changed this frame, to cut
per-frame cost on scenes with many static shadow-casters (primary beneficiary: the runtime, not the
editor). Crate `awsm-renderer`, module `shadows/`.

**Most infra already exists — do NOT rebuild it:**
- Persistent shadow textures (per cube face / cascade-array layer). Per-view attachments clear
  independently, so a *skipped* view keeps last frame's depth → caching is safe. Spot lights share the 2D
  atlas → always render (leave as is; guarded by `has_own_attachment` in the reconcile).
- `ShadowViewThrottle` (`shadows/record.rs`) persists `last_rendered_frame` / `last_view_projection` across
  the per-frame `records` rebuild.
- Per-view `should_render` skip in `shadows::render_pass::record` (it `continue`s when `!view.should_render`).
- The reconcile loop in `Shadows::write_gpu` (`shadows/state.rs`) already invalidates (sets
  `last_rendered_frame = u64::MAX`) on atlas-rect / cascade-layer change and on **`view_projection_drift`**
  (camera move → cascade VP changes → drift fires, so camera/cascade dependency is handled for free), and
  on shadow-config / resolution change.

**The ONE missing input:** a "casters static this frame" signal. Today near cascades (`update_period == 1`)
+ cube faces re-render every frame regardless.

**[x] B.1 — implement the static signal** — commit `7abac541`

**Landed exactly as specified** (split forced vs periodic in the `Shadows::write_gpu` reconcile;
`due = forced || (periodic && !shadow_static)`; threaded `shadow_static` from `AwsmRenderer::render`).
The gate state lives on `Shadows` (encapsulated, no `AwsmRenderer` constructor churn):
`take_shadow_static(mesh_count, external_static)` folds the caster-moved accumulator (set in
`update_transforms`, dirty transforms filtered to `cast_shadows && !hud && !hidden` via
`keys_by_transform_key` — HUD churn ignored) + the caster-set signature (mesh count + a revision
bumped in `set_mesh_shadow_flags` on a cast-flag toggle). `external_static` = camera still + no
time-driven material (`Materials::has_flipbook()` — added — or any custom material). All three "hard
parts" handled.

**Refinement beyond the spec's literal list (a SAFETY ADDITION, not a regression/deviation — so not
gated to David):** the spec's `shadow_static` didn't mention **deformable geometry**. I verified the
shadow caster vertex shaders (`shadow_wgsl/vertex.wgsl`, `shadow_masked_wgsl/vertex.wgsl`) run
`apply_position_skin` + `apply_position_morphs` — so an animated **skinned or geometry-morph** caster's
shadow deforms every frame with NO root-transform move (joint / morph-weight changes don't dirty the
mesh transform, and aren't in `take_dirty_meshes`/`touched`). Without a guard the cache would FREEZE an
animated character's shadow. Added `deformable_present = !skins.is_empty() || !morphs.geometry.is_empty()`
to the not-static condition — conservative (any deformable mesh present, even idle, disables the cache),
which is fine: the §B target is static prop/terrain casters (no skins/morphs). Material morphs are
excluded (don't move vertices).

**Verification (honest):** `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader --lib`
GREEN (34 / 261 / 30). chrome-devtools on model-tests :9080: static (DamagedHelmet) AND animated-skinned
(Fox) models render correctly with the gate running every frame, clean console (only the pre-existing
benign `final_blend` pipeline-warmup warn). The shadow-CACHE behavioral matrix (static frame skips
re-render → `render_cpu_ms` drop; move caster → updates; FlipBook keeps animating; HUD ignored;
add/remove re-renders) **could not be exercised** — model-tests inserts its directional lights with
`None` shadow params (no shadow casting), so the shadow path runs zero views. Correctness rests on the
conservative design (any uncertainty → re-render; every non-static frame ≡ today's `forced || periodic`)
+ the green tests + the live render-loop integration check. No perf regression (non-static frames behave
identically to today; the added per-frame work — `has_flipbook` O(materials), two `is_empty()` O(1),
caster-moved O(dirty≈0 on static) — is negligible), no standards deviation (default-when-not-static ==
today).

### B.1 spec (original, for reference)

In the reconcile loop's `due` computation in `Shadows::write_gpu`, split forced vs periodic
and suppress only the periodic when static:
```rust
let forced   = t.last_rendered_frame == u64::MAX;            // rect/layer/drift/config
let periodic = frame >= t.last_rendered_frame.saturating_add(view.update_period);
let due      = forced || (periodic && !shadow_static);
```
Thread a `shadow_static: bool` param into `Shadows::write_gpu` (call site is in `AwsmRenderer::render`).
Light color/intensity do NOT affect the depth map — only light **transform** (→ dirty transforms) and
config (existing invalidation). So:
```
shadow_static = !caster_transforms_dirty_this_frame
             && !camera.camera_moved()
             && !caster_set_changed
             && !time_driven_shadow_present
```

**Three hard parts (why this is non-trivial):**
1. **Editor HUD churn.** The editor re-anchors gizmo / light-icon / skeleton-overlay HUD meshes EVERY
   frame → dirties transforms every frame. A coarse "any transform dirty → don't cache" would **never
   cache in the editor**. Filter the dirty set to **cast-shadow** meshes (HUD excluded from casters via
   `NodeFilter::shadow_caster` `exclude_hud`). Capture in `AwsmRenderer::update_transforms` (the single
   point consuming `Transforms::take_dirty_meshes()` / `Instances::take_dirty_transforms()`); **accumulate**
   across the multiple per-frame `update_transforms` calls and reset after the shadow gate reads it. Need a
   transform-key → is-cast-shadow-mesh check (`Meshes` has mesh→transform_key, not reverse — add a reverse
   lookup or maintain a cast-shadow-transform-key set; the dirty set is small → iterate it + O(1) lookup).
2. **Time-driven shadow materials.** FlipBook-masked (and any time-reading custom-masked) shadows change
   EVERY frame with NO transform dirty (cutout driven by `frame_globals.time`). A transform-quiet cache
   would FREEZE an animated cutout shadow. `time_driven_shadow_present = !dynamic_materials.is_empty()`
   (any custom — could read time) `|| Materials has any FlipBook` (add a `Materials::has_flipbook()` helper
   — `Material::FlipBook`). Opaque / alpha-tested-texture (PBR/Unlit/Toon) shadows are NOT time-driven →
   cacheable.
3. **Caster-set change.** Add/remove a cast-shadow mesh must re-render. `Meshes` count delta is a cheap
   proxy (HUD add/remove → harmless false-positive re-render); better is a cast-shadow-mesh count/revision.

**Conservative stance:** default to re-render; only skip on a provably-quiet frame. Risk = a stale shadow
if the signal misses a change.

**Verify (chrome-devtools MCP, full matrix):**
- static opaque scene → shadows correct + `render_cpu_ms` drops (`memory_stats` query)
- move a caster → its shadow updates; move camera → directional shadows update
- FlipBook-masked caster → shadow keeps animating while geometry is static
- editor with gizmo visible → still caches (HUD churn ignored)
- add/remove a caster → re-renders

---

# §C — Runtime perf

## [x] C.1 — Perf at thousands of meshes — commit `b84a95ca`

**Bench (the repeatable harness the spec asked for):** model-tests `?stress=N` (in `scene.rs`,
`stress_grid_count`) duplicates the loaded model's meshes into an N-cell grid — distinct renderables
sharing the source GPU buffers — so the per-frame path is exercised at thousands of meshes. Inert
without the param.

**Profiling (chrome-devtools, this desktop machine):**
| scene | rAF p50 | rAF p95 | CPU render | verdict |
|---|---|---|---|---|
| Box ×2000 | 8.3ms | 9.1ms | — | smooth, vsync headroom |
| Box ×15000 | 16.6ms | 17.4ms | **13.5ms** | CPU-bound at the 60fps edge; tail tight (p99 14.1) |
| DamagedHelmet ×2000 | 16.6ms | 33.4ms | **3.3ms** | **GPU-bound** |

**Key finding (answered David's "why do 2000 helmets drop frames"):** the helmet drops are **GPU in
the geometry/visibility pass** (~15K-tri helmet × 2000 ≈ 30M triangles), NOT shading and NOT CPU. The
per-pass CPU breakdown proves it: total `Render` CPU 3.3ms, `Material Opaque` (shading) **0.06ms** —
shading is fully decoupled/per-pixel as the deferred arch intends. So the renderer's CPU path is already
efficient at thousands of meshes; the limiter for high-poly scenes is GPU geometry (a content/LOD axis,
not a renderer-CPU hotspot).

**Optimization — per-frame allocation pooling (David: "avoid allocations — GC/fragmentation", keep all
three):** pooled the 3 mesh-count-scaling per-frame `Vec` allocs — the `visible` set in
`collect_renderables` (onto `RenderablePool`), and `opaque_snapshots` + the packed occlusion-instance
`bytes` in `render()` (into a reused `RenderFrameScratch`, `take`/restored across the frame). Removes
~208KB/frame at 2K meshes (~1.5MB/frame at 15K). **Honest A/B:** at Box ×15000 the CPU `Render` tail was
identical pooled vs unpooled (p50 13.5→13.6, p99 14.1→14.1, max 14.2→14.8 — noise) — these are
wasm-linear-memory allocs (dlmalloc freelist), not JS-GC, so no measurable pause here. Kept per David's
standard (sound hygiene; defensive for mobile / long sessions / fragmentation).

**Verification:** `cargo test` GREEN (34/261/30); static (DamagedHelmet ×2000) + animated-skinned (Fox)
stress scenes render correctly, clean console. No regression (A/B neutral), no standards deviation
(implements the no-per-frame-alloc standard). A future win would be incremental/dirty-tracked renderable
collection (the 13.5ms O(meshes) CPU walk at 15K), but it's a large refactor not warranted by current
evidence (15K meshes already holds 60fps).

### C.1 spec (original, for reference)

Open-ended profile-and-optimize. Build a repeatable bench (instance a primitive N-thousand times via MCP),
profile per-frame CPU (`render_cpu_ms` via `memory_stats`), find + fix hotspots so a large scene stays
interactive. **Record before/after numbers in this doc.** Static-shadow caching (§B) is one input; also
profile per-frame renderable collection, classify, and transform-upload scaling. Likely candidates:
per-frame allocations in the renderable walk, redundant GPU uploads, anything O(n) that could be
incremental / dirty-tracked. Verify via chrome-devtools `performance_*` traces + `memory_stats`.

## [ ] C.2 — #31 TTFR prewarm-after-load — **ROOT-CAUSED + DEFERRED (David, 2026-06-18). Pick up after §D.**

> **DEFERRED by David (2026-06-18):** the fix is larger than the initially-approved "finalize layout
> up-front" (see the refined finding below — the edge pipeline's inputs evolve through build → loading-render
> → model-load, so the prewarmed edge is repeatedly dropped; a correct fix means gating edge dispatch until
> inputs settle / not rendering edge frames until the bucket+pool set is final). It's a benign 1-frame
> un-anti-aliased transient **behind the loading overlay**. David chose to defer C.2 and move to §D
> (uber-shader) now; pick C.2 up as a focused renderer task after §D lands. Full diagnosis is preserved below.

**The hitch (reproduced, every cold load):** the first shown frame warn-skips the MSAA edge-resolve
`final_blend` pipeline (`render-frame preamble: pipeline not compiled at material_opaque::shade
(id=final_blend) — skipping`, `scheduler.rs:825` ← dispatch `material_opaque/render_pass.rs:179`,
`final_blend_pipeline_key == None`) → one un-anti-aliased reveal frame; installed (cache-hit) by frame 2.

**Root cause (debug breadcrumbs + a temporary `arrays_len`/`buckets` log, now removed):** the edge pipeline
is launched by `launch_edge_resolve_compile` (in `ensure_scene_pipelines`, inside `prewarm_pipelines`,
inside `wait_for_pipelines_ready`). Its compute cache key = `(shader_key, layout_key)` where `layout_key`
encodes the **texture-pool bind-group layout** (`texture_pool_arrays_len`) and the cache shader keys encode
**`bucket_entries`**. The diagnostic showed the **edge prewarm runs at `pool_arrays=0, buckets=5`** while
the **final state is `pool_arrays=2, buckets=7`** — i.e. the texture pool + variant bucket set are NOT yet
finalized when the prewarm (and the first render) launch the edge compile. The stale-layout edge promise
resolves but is correctly **dropped** (`apply_compile_resolution: edge resolution no longer desired —
dropped (slot FinalBlend)`, `launch.rs:935`) because the first render re-derives the desired edge-key set
against the FINAL layout; a later launch then finds the now-cached final pipeline and installs it (frame 2+).

**Why a pre-render prewarm can't fix it (tested):** I added a `wait_for_pipelines_ready` after `setup_all`
(reverted) — it found the edge compiles in-flight and changed nothing, because the determining state
(`pool_arrays`, `buckets`) only reaches its final shape after the textures/buckets finalize, which lags the
prewarm. So the edge layout the first render uses can't be known/compiled before the textures are in the
pool arrays. (Confirmed: the texture finalize flow `textures.rs` relayouts the **masked** geometry/shadow
pipelines on pool-grow but NOT the **edge** pipelines; the texture-pool bind-group *layout* is otherwise
rebuilt only by the render-time `bind_groups.recreate` drain of `BindGroupCreate::TexturePool`, `render.rs:564`.)

**David's decision (2026-06-18): renderer fix — finalize the layout up-front** (benefits the player/
scene-loader path too, not just model-tests). **Fix direction for the next iteration:** ensure the texture
pool + bucket set are finalized into their FINAL bind-group LAYOUT *before* the edge prewarm compiles — i.e.
make the pool-grow texture-finalize flow (`textures.rs`, alongside the masked relayout) ALSO relayout/relaunch
the edge pipelines (`launch_edge_resolve_compile`) so the edge `layout_key` tracks `texture_pool_arrays_len`
exactly like the masked pipelines do; OR drive the texture-pool bind-group **layout** rebuild during
`prewarm_pipelines` (the layout rebuild needs only the texture pool, not the viewport-dependent
`render_texture_views`, so it can run pre-first-render). Then `prewarm` compiles the edge pipeline against the
final layout and it installs (no drop). Verify: chrome-devtools cold load → no `final_blend` warn, clean
first visible frame; re-check the player/scene-loader path. Keep `cargo test` GREEN.

**Refined finding (2026-06-18, 2nd pass — DamagedHelmet, temp logs now removed):** the `final_blend`
skip fires on the **early render frames during loading, at the build/empty state** (`buckets=5
pool_arrays=0`) — NOT after the model loads. Sequence: build prewarm launches edge at `buckets=5,
pool_arrays=0` → render frames run at `buckets=5, pool_arrays=0` and `FINAL-BLEND-SKIP` → only later does
the state reach `buckets=6, pool_arrays=1` (model textures + its PBR feature-variant bucket). So the
renderer renders frames **before the model finalizes**, and the edge pipeline's inputs (`bucket_entries` +
`texture_pool_arrays_len`) **evolve through build → loading-render → model-load**; each change drops the
in-flight edge resolution (`apply_compile_resolution: … no longer desired — dropped`). It is therefore NOT
a single "finalize the layout once" fix: the edge can't be reliably prewarmed while its inputs are still
evolving and frames are being rendered against the intermediate states. A correct fix is larger than the
approved direction (e.g. gate edge dispatch until inputs settle, or don't render edge-resolve frames until
the scene's bucket/pool set is final, or make the prewarm's edge survive an idempotent re-derive). **Pending
David's prioritization** (deep fix for a 1-frame un-AA'd transient behind the loading overlay, vs defer and
do §D). See the AskUserQuestion answer recorded with the next commit.

### C.2 spec (original, for reference)


Time-to-first-render after a model load has a sub-frame-transient hitch. The old doc flagged this as
needing a human wall-clock; per the 2026-06-18 decision, **self-verify via chrome-devtools** instead.
Implement prewarm-after-load (compile the loaded model's pipelines before the first frame that shows it —
`prewarm_pipelines` / `wait_for_pipelines_ready` already exist as the building blocks). Verify with a
chrome-devtools `performance_start_trace` across a cold model load: confirm the first post-load frame has
no pipeline-compile stall (inspect the trace's long-task / `performance_analyze_insight` output) and the
console shows pipelines ready before first present. Capture the before/after trace evidence in this doc.

---

# §D — Uber-shader: selectable per-variant grouping (the partition is the design)

> **The grouping is a *partition of variants into groups*; each group compiles to one branching pipeline.
> Group-of-1 = today's per-bucket pipeline; group-of-all = one global uber-shader; the useful configs are
> in between and chosen per game.** This makes that partition a first-class, authored, schema-persisted
> input. **Default = all-split = today's exact behavior** (zero-risk; a scene specifying nothing compiles
> and renders identically to today).

## [x] D.0 — RE-AUDIT & UPDATE THIS SPEC (done 2026-06-18; commit `26456aac`)

**Audit outcome:** edge model verified against code — `cs_shade` IS the one unified MSAA kernel/bucket
(interior→`opaque_tex`, edge→`accumulator`); `MaterialEdgeBuffers`(`edge_slot_map`+4-slot `accumulator`) +
`final_blend` cross-pipeline combine; skybox = own bucket pipeline (`skybox_primary.wgsl`); `cs_edge` is dead
comment-naming (`unified-edge-shading` folded it into `cs_shade`). All prep buffers exist as named
(`material_prep` per-pixel + per-edge-sample shadow, `froxel_walk.wgsl` SSOT, depth→world recompute,
in-register edge UV/vcolor, no prep flag). **D4 rewritten** to current truth; **D.6 fast-MSAA path deferred /
measurement-gated** (coherent but NOT subsumed — skybox-separate keeps the accumulator live at one material
pipeline — and too narrow/speculative to build pre-measurement; build only if D.5 shows the one-pipeline
accumulator overhead matters). Below is the (now-superseded by D4/D.6) original D.0 mandate, kept for history.

David folded the whole uber-shader plan in **but flagged it needs updating** — specifically the
**"fast-MSAA" framing is suspected outdated** after `unified-edge-shading` landed (and subsequent edge
work). **Before implementing any D-stage**, re-audit the spec against current code and **edit §D in place**
to match reality, then proceed:
- Confirm the current MSAA edge model: `cs_shade` is the one kernel per material pipeline doing interior
  *and* edge work (write-target = `opaque_tex` interior vs `accumulator` edge); the cross-pipeline combine
  is `MaterialEdgeBuffers` (`edge_slot_map` + 4-slot `accumulator`) + `final_blend`. (All these symbols
  still exist — verified — but re-read them; the "fast-MSAA single-pipeline path" claims in **D4 / Stage
  D.6** are the suspect part: re-derive whether bypassing the accumulator at exactly-one-opaque-pipeline is
  still coherent and worth specifying, or whether the landed edge model already subsumes it. Rewrite D4 +
  Stage D.6 to the current truth, or mark them dropped with the reason.)
- Confirm the prep buffers this consumes still exist as named (UV/vcolor arrays, K-layer shadow visibility,
  the per-edge-sample shadow buffer, `froxel_walk.wgsl` SSOT, depth→world_pos recompute). Plan B + prep-only
  landed: opaque is **unconditionally prep**, the prep on/off flag is gone, and edge UV/vcolor is
  **recomputed in-register, not buffered** (the prep-vs-recompute rule). Update any D-text that assumes a
  prep flag or an edge UV/vcolor buffer (the old Stage-5 "Option B" edge attribute buffer was resolved
  WON'T-DO — see history below; D's fast path must read what actually exists).
- Commit the spec update as its own commit before the first implementation stage.

## The idea

Today shading is **N specialized compute dispatches** — one per bucket (`(shader_id, pbr_features)` tuple)
— issued by `MaterialOpaqueRenderPass::render` looping over `bucket_entries_cached()`, each an indirect
dispatch over that bucket's classify-produced tile list. Every bucket is its own compiled pipeline (lean,
DCE'd to exactly its feature-set). The uber-shader lets a **set of variants collapse into one branching
pipeline**:
```
read prep buffers (UV/vcolor arrays, K-layer shadow, depth→world_pos)
  → switch(shader_id) { case PBR: …; case TOON: …; case CUSTOM_n: …; }    // runtime branch
  → write opaque_tex
```

**Why this is the win vector vs three.js:** three does N forward passes (one draw per `mesh.material`);
awsm does N compute dispatches (one per bucket) **plus** a geometry pass + G-buffer bandwidth — both O(N)
in variant count, awsm carrying strictly more, so losing is structurally expected. Collapsing N shadings
into **one branching dispatch** is the move three **cannot** make (its shading is welded to per-material
draws). awsm's deferred decoupling (Plan B) is what makes one shading pass possible. Secondary wins:
**precompile collapse** (N specialized modules → one per group; the ~230 s / 1024-module compile is the
unbounded custom-material axis — grouping customs bites hardest there); MSAA edge machinery already shrank
to one kernel (`unified-edge-shading`), and the cross-pipeline combine shrinks further only at the
single-pipeline extreme (the deferred/measurement-gated fast path, D.6).

## Locked decisions

### D1 — The grouping is a *partition*, decided at pipeline-batch-submission time, and rides in the schema
The grouping policy does **not** live on `AwsmRendererBuilder`. It is **input to the pipeline scheduler's
batch submission** (`AwsmRenderer::submit_pipeline_group_batch` / `pipeline_scheduler::types::PipelineGroupDef`),
driven by `ensure_scene_pipelines`. Rationale: grouping isn't needed until pipelines are submitted for
compile, so the **editor** can recompute + resubmit → recompile on author change (the scheduler already
transitions affected materials `Ready → Pending` on config drift — same mechanism), and the grouping
becomes **part of the loadable schema** a player consumes (ships with the scene like `ShaderIncludes`
opt-in does today). Concretely: today each material maps 1 `MaterialDef` → 1 `PipelineGroupDef::Material` →
1 pipeline (`MaterialId`); the policy changes the mapping so several `MaterialDef`s resolve to one shared
group. The scheduler/`MaterialId` model already supports a group owning multiple sub-pipelines + multiple
materials charged to one group → extension of the existing batch shape, not a new subsystem.
**Default = all-split = today's exact behavior.** Grouping is opt-in; unspecified → bit-identical to today.

### D2 — PBR uses a per-feature SPLIT / UBER partition (the PBR-split answer)
PBR's "variant" is the 17-bit `PbrFeatures` mask (`awsm_materials::pbr::PbrFeatures`); today each distinct
mask is its own pipeline (feeds `ShaderCacheKeyMaterialOpaque::pbr_features`, compile-time gated via
`{% if pbr_features.x %}` + DCE). A runtime `switch(shader_id)` can't have an arm per *unknown*
feature-combo, so folding PBR into the uber converts compile-time feature gates to **runtime** gates.
**Feasible with zero new per-instance data:** per-material feature presence is *already* in the material
storage buffer read by `pbr_get_material(byte_offset)` — each extension is an absolute index where
`0 == absent` (`clearcoat_index`, `sheen_index`, `iridescence_index`, `anisotropy_index`, `ior_index`,
`specular_index`, `emissive_strength_index`, `vertex_color_info_index`, …); each texture is a `TextureInfo`
with detectable presence (sentinel index). So a runtime-gated PBR arm branches on `if (m.clearcoat_index != 0u) {…}`.
Today's compile-time `pbr_features` gating is therefore **purely a DCE / register-pressure optimization**,
not a data dependency — which is what makes the partition a free knob.

**The knob:** partition the PBR feature set into **SPLIT** (compile-time gated → keys the pipeline; lean,
DCE'd, no register cost for absent features) and **UBER** (runtime-gated inside the shared arm → all group
members share one pipeline; feature code compiled in, register cost paid by every pixel, skipped at runtime
via `*_index != 0`). Spectrum with one mechanism: SPLIT = all 17 → exactly today; SPLIT = ∅ → one PBR
pipeline, all features runtime-gated; any mix in between.

**Scope < 17 — transmission-family excluded:** materials with alpha-blend OR transmission route to the
**transparent forward pass** (`MaterialShader::is_transparency_pass = has_alpha_blend() || has_transmission()`),
out of scope for both Plan B and the uber. So the opaque uber-PBR arm never compiles `transmission` /
`volume` / `dispersion`. Opaque-routed axis: base-color / metallic-roughness / normal / occlusion /
emissive textures, `vertex_color`, `emissive_strength`, `ior`, `specular`, `clearcoat`, `sheen`,
`anisotropy`, `iridescence`, `diffuse_transmission` (opaque unless paired with transmission).

**Recommended default partition** (scene opts PBR into a group but doesn't specify per-feature): UBER the
**common core** — base-color tex, metallic-roughness tex, normal tex, occlusion tex, emissive tex,
`vertex_color`, `emissive_strength`, `ior`, `specular`; SPLIT the **rare + register-heavy lobes** —
`clearcoat`, `sheen`, `anisotropy`, `iridescence`, `diffuse_transmission`. Editor exposes this as a named
**"PBR Default" preset** (one click), every feature individually overridable; the chosen partition persists
in the schema.

### D2b — Unlit / Toon / FlipBook have no compile-time feature axis — base-level membership only
Verified: Unlit, Toon, FlipBook each compile to a **single program** with runtime uniform params (Toon's
`diffuse_bands`/`rim_*`/`specular_steps`/`shininess`, FlipBook's `cols`/`rows`/`frame_count`/`fps`/`mode`/
`flip_y`, base-color factors) — no `*Features` mask, so no SPLIT/UBER decision within them (`toon.rs` says
so: "If Toon ever gains texture sampling, add a `ToonFeatures` mirroring `PbrFeatures`…"). ⇒ For these the
only control is **base-level membership** (which group + per-group opt-out, D3) — each is one `case` in its
group's `switch(shader_id)`. Schema is nonetheless **per-base-general**: every base carries an optional
feature-partition slot (empty for Unlit/Toon/FlipBook today), so "Toon gains textures → `ToonFeatures`"
drops into the same mechanism with no schema/editor redesign.

### D3 — Custom/dynamic material grouping is author-controlled (same partition, at material granularity)
Custom materials (`shader_id >= DYNAMIC_START`, via `MaterialRegistration`) are the unbounded axis (the
1024-unique case). Grouping is **explicit + author-controlled**, exposed like `ShaderIncludes` opt-in:
author assigns materials to a named **shading group**; all members compile into one branching pipeline (a
`switch(shader_id)` over members, each member's author WGSL wrapped as its own `case` exactly as the
dispatch-table wrapper does today). **Default = group-of-1** (unassigned custom → own pipeline = today).
Surface in **editor + MCP**. Assignment is part of the scene schema (D1), recompiles on change. **No
automatic heuristic in v1** (deferred). Custom materials keep **Tier-B protection**: a grouped custom
pipeline still forces `BRDF`/`APPLY_LIGHTING`/`MATERIAL_COLOR_CALC` off per `ShaderIncludeFlags::for_custom`,
each member compiles only what it declares (grouping must not leak first-party shading into a custom arm or
one member's includes into another); the group's include-set is the **union** of members' declared
includes. **Per-group opt-out:** a group may be flagged to stay separate pipelines when profiling says
branching loses. **Overflow / cap:** a max members-per-group (register pressure / module size); exceeding it
is **clamped + logged** (never a silent cap) and overflow members fall back to their own pipelines (hybrid).

### D4 — MSAA: the accumulator path is THE path; fast-MSAA is a measurement-gated option **(D.0 RE-AUDITED 2026-06-18 — current truth)**

**Current edge model (verified against code, D.0):** the MSAA opaque kernel is **one unified `cs_shade`
per bucket** (`material_opaque_wgsl/compute.wgsl::cs_shade`, shared module with the bucket's `cs_opaque`
non-MSAA kernel; `cs_opaque` is gated to non-MSAA per the MSAA-compile invariant). `cs_shade` does interior
AND edge work via a write-target branch: interior pixels → `opaque_tex`; edge samples → the `accumulator`.
The cross-pipeline combine is `MaterialEdgeBuffers` (`edge_slot_map` + the 4-slot `accumulator`, see
`render_passes/material_opaque/edge_buffers.rs`) + `final_blend.wgsl`. The **skybox is its own bucket
pipeline** (`skybox_primary.wgsl`, its own `cs_opaque`/`cs_shade`) — it is NOT a groupable material (hazard
6). The per-shader edge `cs_shade` pipelines are built by `launch_edge_resolve_compile`
(`pipeline_scheduler/launch.rs`). *(Naming note: stale `cs_edge` comments survive in `compute.wgsl`; the
landed `unified-edge-shading` replaced `cs_opaque + cs_edge` with the single `cs_shade` — see
`edge_pipeline.rs` "in place of cs_opaque + cs_edge". The model, not the comment, is authoritative.)*

This machinery exists because shading is split across pipelines: at an edge pixel the 4 samples can belong
to materials in **different** pipelines, and a pipeline's `cs_shade` only shades its own samples → a
cross-pipeline accumulate-then-combine is mandatory. **The instant there is >1 opaque pipeline that owns
edge samples, this is required and does NOT simplify.** Real scenes almost always have some pipeline
separation → **the accumulator path is the default and stays.**

**Fast-MSAA single-pipeline path — D.0 verdict: COHERENT but NOT subsumed, and DEFERRED (measurement-gated).**
- *Coherent:* when the grouping collapses to exactly one opaque **material** pipeline, a fast path that
  bypasses `accumulator`/`final_blend`/`edge_slot_map` is logically sound. Its `cs_shade` edge branch would
  resolve all 4 samples itself, writing the blend straight to `opaque_tex`.
- *NOT subsumed by the landed model:* the precondition is real — **skybox is always a separate pipeline**,
  so "one material pipeline" still means ≥2 pipelines at silhouette edges (material samples mix with sky
  samples, which the skybox bucket's `cs_shade` writes to the accumulator today). So even at one material
  pipeline the accumulator IS still used; the landed model does not auto-collapse it. The fast path only
  works if the single material kernel's edge branch **samples the skybox/environment inline** for sky
  samples (feasible — the material kernel already binds environment/IBL — but it duplicates
  `skybox_primary`'s projection logic into the uber kernel).
- *Why DEFERRED, not built (the cost/benefit):* the saving (accumulator VRAM — up to ~37 MB at 512k edges,
  far less typically — + the `final_blend` dispatch + `edge_slot_map`) accrues ONLY at the **global-uber
  extreme** (exactly one non-skybox opaque pipeline), which is the rarest grouping config. Against that: a
  new inline-skybox `cs_shade` edge variant (added register pressure on the already-heavy uber kernel),
  a submit-time single-pipeline detector, and a dual-path kept behind a flag for visual-only (non-naga)
  parity. The uber-shader's PRIMARY wins (dispatch collapse, precompile collapse) are **independent** of it.
  ⇒ Per "measure, don't guess": do NOT build it speculatively. Build the fast path ONLY if **D.5's
  accumulator-path-for-groups measurements** show the one-material-pipeline accumulator/`final_blend`
  overhead is a real, worth-eliminating cost on a global-uber scene. See Stage **D.6** (now gated on that).
- If/when built: the fast `cs_shade` edge branch consumes Plan B's per-edge-sample shadow buffer +
  recomputes edge UV/vcolor in-register (the landed prep-vs-recompute rule — **NOT** a per-edge-sample
  attribute buffer; the old "Option B" buffer was resolved WON'T-DO). Correctness is visual-only (MSAA edges
  can't be naga-checked): match the accumulator path exactly, keep it behind a flag, verify model-tests
  MSAA-on via chrome-devtools.
- The forward transparent path keeps its own MSAA handling (`EdgeResolveBlend`) — unaffected.

### D5 — Defaults summary (zero-risk)
| axis | default | opt-in |
|------|---------|--------|
| material→group mapping | all-split (1 pipeline per bucket = today) | grouping spec in schema |
| PBR per-feature SPLIT/UBER | all-SPLIT when ungrouped; core-UBER/heavy-SPLIT when PBR is grouped | per-feature override |
| custom grouping | group-of-1 | author assigns groups (editor/MCP) |
| MSAA | accumulator path (one `cs_shade` kernel/pipeline; cross-pipeline combine) | fast path DEFERRED — measurement-gated, build only if D.5 shows accumulator overhead matters (D.0 verdict, D.6) |

## Authoring surface + schema (every material kind is controllable)

Grouping must be fully expressible in the schema (ships with the scene) and fully authorable in editor +
MCP. **Schema shape — `ShadingGroupSpec` (per scene):**
```
ShadingGroupSpec {
  groups: [ ShadingGroup {
    id, name,
    members: [ MaterialRef ],            // first-party bases AND/OR custom shader_ids
    opt_out: bool,                       // force members to stay group-of-1 (D3)
    cap: u32,                            // max members; overflow → own pipelines + log (D3)
  } ],
  feature_partitions: { base: FeaturePartition { uber: [Feature], split: [Feature] } },  // per-base; only PBR non-empty today (D2b)
  // unlisted material → group-of-1, all-split (default = today)
}
```
A `MaterialRef` is a first-party base (PBR/UNLIT/TOON/FLIPBOOK) or a custom `shader_id` (`>= DYNAMIC_START`);
a group may mix bases + customs (one `switch(shader_id)` module). Round-trips with the rest of the scene
schema; `ensure_scene_pipelines` reads it to build the batch.

**Per-kind controllability:** PBR — group membership ✅, full 14-feature partition + "PBR Default" preset ✅,
opt-out ✅, cap ✅ (only base with a feature axis). Unlit/Toon/FlipBook — membership ✅, feature slot reserved
empty (D2b), opt-out ✅, cap ✅. Custom — membership ✅ (editor + MCP), author WGSL is the unit, opt-out ✅,
cap ✅, Tier-B protection, include-set = union.

**Editor surfaces:** (1) Group manager (create/rename/delete groups; drag any material in; group spanning
bases shows its member `switch` set). (2) PBR feature partition editor (per-feature SPLIT/UBER toggles +
"PBR Default" preset button + per-feature override; generic per-base widget, empty for Unlit/Toon/FlipBook).
(3) Per-group opt-out toggle + cap field. (4) Live recompile (any change resubmits affected groups
`Ready → Pending → Ready`; status via `pipeline_group_status` / `drain_pipeline_status_events`; no renderer
rebuild). (5) Diagnostics: cap overflow ("N exceeded cap → M fell back"), single-pipeline / fast-MSAA
indicator (D4; only if the deferred D.6 fast path is built), divergence hint for spatially-interleaved divergent grouping.

**MCP parity:** every editor op has an MCP equivalent (create/edit/delete groups, assign materials, set PBR
partition incl. apply-default, set opt-out/cap) — agent can author + measure headlessly.

**Player / runtime:** the loaded schema's `ShadingGroupSpec` flows into `ensure_scene_pipelines` → scheduler
batch (D1). A player never re-derives grouping; absent a spec → all-split → identical to today.

## The variant space (what a group's `switch` branches over)
**Force separate pipelines** (change bind-group layout, raster/sample state, or sampling intrinsics):
`msaa_sample_count` (None/2/4), `mipmaps`, `texture_pool_arrays_len` / `texture_pool_samplers_len`, PBR
**SPLIT** features (D2), group-overflow members (D3). **Runtime `switch`/`if` within one pipeline:**
`shader_id` (top-level switch), PBR **UBER** features (`if (m.*_index != 0u)`). So a group's module is a
`switch(shader_id)` over members, the PBR arm itself an `if`-ladder over UBER features, with SPLIT features
+ non-switchable dims having already partitioned which pipeline this is.

## Implementation hazards (pre-resolved — silently break if missed)
1. **Grouped custom members collide on WGSL symbol names.** The dynamic-material generator emits **fixed**
   names — `struct MaterialData`, `fn material_data_load` (literal in `dynamic_materials/registry.rs`), one
   `custom_shade_dynamic` wrapper. Two customs in one module redefine all three → compile error. **Fix:**
   namespace per `shader_id` when grouping — `MaterialData_<id>`, `material_data_load_<id>`,
   `custom_shade_<id>` (cache_key comments already anticipate `custom_shade_<id>`; parameterise the
   generator by id, not hardcoded `"material_data_load"`/`"dynamic"`). Same care for any other top-level
   decl a custom fragment emits.
2. **Per-pixel/per-sample divergence is only safe because sampling uses EXPLICIT gradients.** A grouped
   kernel's `switch(shader_id)` + UBER `if`s are non-uniform across a tile. Implicit-LOD `textureSample` in
   non-uniform control flow is a WGSL hazard (undefined gradients). Already safe in the opaque path (samples
   with explicit gradients `texture_pool_sample_grad` / `mipmap_pbr.wgsl` over prep-materialized UVs) — the
   uber kernel **must preserve** that invariant: no implicit-LOD sampling anywhere reachable under the
   variant/feature branches. State + test it (naga won't catch it; visual artifacts at variant boundaries
   will).
3. **The group's pipeline cache key must encode group composition.** Add the **ordered** member `shader_id`
   list + per-base SPLIT/UBER partition to `ShaderCacheKeyMaterialOpaque` (alongside `pbr_features` /
   `dispatch_hash` / `bucket_entries`). Two groupings must not alias one cached pipeline; a membership change
   must invalidate. **Order stable** (sort by `shader_id`) so the same group hashes identically + arm order
   is deterministic.
4. **A group needs ONE bind-group layout covering all members.** A group's pipeline binds a single layout
   (main + lights + texture_pool + shadows). Members already share registry-managed `materials` storage +
   bindless texture pool → normally unify, but a member needing a binding the others lack **cannot join**
   that group. Enforce at grouping time (diagnostic "material X can't join group G — incompatible
   bindings"), don't silently miscompile.
5. **Classify groups tiles by GROUP; per-pixel `shader_id` drives the `switch`.** Today `material_classify`
   appends a tile to a bucket's list if any pixel matches, keyed by `MaterialBucketLut` (shader_id→bucket).
   For grouping: LUT becomes shader_id→**group**, a tile joins a group's list if any pixel matches any
   member, the group gets **one** indirect-args slot. Tiles are then **heterogeneous** — the kernel reads
   each pixel's `shader_id` (from the visibility buffer) and switches. Nothing reads "the tile's material";
   there isn't one.
6. **Skybox is not a groupable material.** Bucket 0 / `SKYBOX` (the `OpaqueEmpty` / uncovered-pixel path)
   stays special, never a group member; participates as a lean `cs_shade` arm (uncovered/sky samples write
   the accumulator at edges), and — only under the deferred D.6 fast path — re-entering inline in the single material kernel's `cs_shade` edge branch (D4).

## Costs / risks to design against
- **Branch divergence:** a wavefront straddling two `switch` arms (or two UBER branches) runs both
  serially. Mitigate with material-coherent tiling (`material_classify` already groups tiles by bucket → mostly
  one variant; coherence holds spatially). A group **should be coherent** — grouping spatially-interleaved
  divergent materials is the author's footgun (per-group opt-out, D3, is the escape hatch).
- **Register pressure / occupancy:** every UBER feature + every member compiles into the module → compiler
  allocates for the union → lower occupancy for all pixels in the group. Central tradeoff the SPLIT/UBER
  knob (D2) + membership (D3) exist to tune. Stance: trading register pressure to unlock fast MSAA is an
  acceptable per-game choice.
- **Module size:** bounded only if the group is bounded (hence the cap, D3).
- **Bandwidth at 4K is orthogonal:** one dispatch or N, G-buffer + prep-buffer read traffic is identical.
  The uber does NOT fix bandwidth; the win is dispatch/draw-bound regimes (high instance count, moderate
  res) — most real content.

## Implementation stages (each independently testable + green; default-off / all-split until a stage proves parity)
Per stage: `cargo test -p awsm-renderer -p awsm-materials --lib` green (naga + size_regression +
completeness) and model-tests render correctly (PBR/IBL dish, alpha, shadows, MSAA on/off) with a clean
console, **verified via chrome-devtools MCP**.

- **[x] D.0 — re-audit & update this spec** (above) — commit `26456aac`. Edge model verified; D4 rewritten; D.6 fast-MSAA deferred/measurement-gated.
- **[ ] D.1 — Grouping spec plumbing (inert).** Add `ShadingGroupSpec` types (groups + members + opt-out +
  cap + per-base `feature_partitions`, kept per-base-general per D2b) to the scene schema +
  `pipeline_scheduler` batch input. `ensure_scene_pipelines` reads it; **default produces the exact same
  `PipelineGroupDef::Material` set as today** (all-split, all-SPLIT, group-of-1). No behavior change.
  Tests: schema round-trips; default batch byte-identical to current.
- **[ ] D.2 — Runtime-gated PBR arm (single-member group, UBER core).** Add a PBR template path reading
  feature presence at runtime (`m.*_index != 0u`, texture sentinels) instead of `{% if pbr_features %}`, for
  the **UBER** features only; SPLIT features still key the pipeline. Behind the grouping spec; a PBR-only
  scene with the default core-UBER partition now compiles **one** PBR pipeline. Validate visual parity
  (Iridescence/clearcoat dish, normal/emissive/occlusion variants, vertex-color, MSAA off) vs the
  specialized path; measure register pressure / module size / occupancy. **This is the PBR-split proof — do
  it before any multi-member grouping.**
- **[ ] D.3 — Multi-member groups (first-party). HIGHEST-RISK — split into sub-commits.** Allow
  PBR+Toon+Unlit+FlipBook (or subset) to compile into one `switch(shader_id)` pipeline. Per hazard 5:
  `MaterialBucketLut` → shader_id→**group**, a tile joins if any pixel matches any member, one indirect-args
  slot per group; kernel reads per-pixel `shader_id` and switches (heterogeneous tiles). Carry ordered
  member list + partition in the cache key (hazard 3); one unified bind-group layout (hazard 4). Visual
  parity, no-MSAA. Measure dispatch-count drop. Sub-commits: (a) classify group LUT + per-group args, inert;
  (b) merged `switch` kernel for a 2-member first-party group; (c) extend to all four bases.
- **[ ] D.4 — Custom-material groups + full authoring surface.** Wrap N custom members into one group
  pipeline (each a `case`, Tier-B protected, include-set = union). Build the complete authoring surface:
  group manager (every kind — bases AND customs), PBR partition editor with "PBR Default" preset, per-group
  opt-out + cap, live-recompile status, diagnostics (cap-overflow, single-pipeline indicator; fast-MSAA only if D.6 built).
  **MCP parity** for all of it. Schema persistence + player load (D1). naga over the union; visual parity for
  a 2–3 custom-material group and a mixed base+custom group.
- **[ ] D.5 — MSAA accumulator path for groups.** Make the edge machinery group-aware: a group's `cs_shade`
  edge branch shades its members' samples; `edge_slot_map` keys by group not bucket; `final_blend` combines
  across groups. The general MSAA path with grouping. Visual MSAA-on parity (chrome-devtools).
- **[ ] D.6 — Fast-MSAA single-pipeline path (D4) — DEFERRED / MEASUREMENT-GATED (D.0 verdict 2026-06-18).**
  D.0 re-audited D4 and found the fast path **coherent but NOT subsumed and NOT worth building speculatively**
  (narrow: global-uber extreme only; skybox stays a separate pipeline so the accumulator is still used at one
  material pipeline; needs a new inline-skybox `cs_shade` variant + dual-path). **Do NOT build in this pass.**
  Build ONLY if **D.5's** accumulator-path-for-groups measurements show the one-material-pipeline
  accumulator/`final_blend` overhead is a real, worth-eliminating cost on a global-uber MSAA scene. If D.5
  shows that: when the scheduler reports exactly one opaque material pipeline, compile a `cs_shade` edge branch
  that resolves all 4 samples (material + skybox sampled inline) and writes final directly to `opaque_tex`,
  skipping `accumulator`/`final_blend`/`edge_slot_map`; gated, accumulator path kept behind the flag; visual-only
  parity vs the accumulator path on a PBR-only MSAA scene via chrome-devtools; measure VRAM + dispatch savings +
  edge-divergence cost. **Otherwise mark `[x]` as "evaluated → deferred (D.0); accumulator path is sufficient".**
- **[ ] D.7 — Finalize.** Decide per-default partition tuning from measurements; document the editor/MCP
  grouping recipe; re-dump `reports/awsm-dumps/`; update `report.md`; tighten ceilings.

## Measurement gates (record before/after in this doc; AA off AND on; 1280×720 AND 3840×2160)
1. Per-group module size + register pressure / occupancy (the central tradeoff). 2. Precompile time
(pipeline-count × module-size; big drop on grouped-custom axis). 3. Dispatch count (N buckets → #groups).
4. Runtime FPS 720p AND 4K (dispatch-bound win vs bandwidth-bound wash). 5. Edge divergence cost (MSAA) +
VRAM/dispatch the fast path saves. 6. Correctness — naga (non-MSAA + accumulator MSAA); model-tests visual
parity incl. fast-MSAA single-pipeline; clean console. **Useful experiment:** a many-custom scene, sweep
group size N from 1 (today) to all (one global) — directly measures the partition sweet spot.

## Open questions (small, decide empirically — do NOT guess)
- Per-feature partition tuning (D2 core-UBER/heavy-SPLIT is a starting guess; stage-D.2 register/occupancy
  measurements may move `specular`/`ior` or pull `clearcoat` in).
- Group cap value (D3) — set from D.3/D.4 register-pressure measurements.
- Auto-grouping heuristic — explicitly deferred (D3); only build if author-controlled grouping proves too
  tedious in practice.

## Out of scope (uber-shader)
- Transparent-path slimming / grouping (transmission/blend stay forward, D2).
- Auto-grouping heuristics (deferred, D3).

---

# Final step

When **every** `[ ]` above is `[x]` + green + committed on `updates`: post a before/after summary
(scene-loader kinds covered; static-shadow `render_cpu_ms` delta; perf-at-thousands numbers; TTFR trace
evidence; uber-shader dispatch/precompile/module-size/FPS measurements + the chosen PBR partition + group
cap), then **STOP**. Do not push; David publishes.

---

# Appendix — already landed (context, not work)

- **Plan B (deferred-shared-prep-pass)** — COMPLETE + merged. Shared `material_prep` compute pass (UV/vcolor
  array textures + per-pixel + per-edge-sample shadow visibility, `froxel_walk` SSOT); opaque reads them
  (no-MSAA + MSAA via `PrepReadContext`). Per-shader PBR size −46…−53 KB.
- **unified-edge-shading** — COMPLETE + merged. MSAA shading is ONE `cs_shade` kernel/pipeline (interior →
  `opaque_tex`; edge samples → `accumulator`, via a write-target branch) + `final_blend`. Deleted legacy
  `cs_edge`/`skybox_edge_resolve`/per-bucket edge-sample lists. The `accumulator` + `edge_slot_map` +
  `MaterialEdgeBuffers` were KEPT (cross-pipeline combine still needs them).
- **prep-only** — COMPLETE + merged (PR #130). **P1** removed the `PrepPassConfig.enabled` / `with_prep_pass`
  flag entirely — opaque is **unconditionally prep**; the opaque variant axis collapsed; `size_regression`
  ceilings re-set to prep-on sizes (commit `2d459e6a`). **P2** (per-edge-sample UV/vcolor buffer) resolved
  **WON'T-DO by design** (commit `cd3d05de`): edge samples recompute UV/vcolor in-register (the edge arm
  already holds per-sample triangle + barycentric) — cheaper than a buffer's write+read + ~16–48 MB VRAM,
  with no bulky code to evict. This is the documented **prep-vs-recompute rule** (in `material_prep/buffers.rs`,
  `helpers/texture_uvs.wgsl`, `helpers/vertex_color_attrib.wgsl`, `material_prep/.../compute.wgsl`,
  READMEs, `docs/SHADER_GUIDELINES.md`). Transparent stays forward (`prep_present=false`) — the shared
  recompute WGSL is emitted only for the transparent module; a true deferred-transparency unification is a
  separate, much larger project (not planned here).
- **Renderer MSAA-compile cleanup** — "never compile MSAA code with MSAA off, nor non-MSAA with MSAA on";
  `cs_opaque` gated to non-MSAA, `cs_shade` to MSAA, naga asserts the invariant.
