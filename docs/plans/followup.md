# Follow-up work (post-release)

The only intentionally-unfinished items, to pick up in a fresh session — possibly
long after this branch merges and other code lands. **References below are by
symbol/file name, not line number, on purpose: line numbers will drift; grep for
the named functions/types.** Everything else on this line shipped + was verified.
The maintainer publishes the release.

---

## 1. Static-shadow caching (perf)

**Goal:** skip re-rendering a light's shadow map when nothing affecting it changed
this frame, to cut per-frame cost on scenes with many static shadow-casters
(primary beneficiary: the runtime, not the editor).

**Most of the infra already exists** — do NOT rebuild it (crate `awsm-renderer`,
module `shadows/`):
- Persistent shadow textures (per cube face / cascade-array layer). Per-view
  attachments clear independently, so a *skipped* view keeps last frame's depth
  → caching is safe. Spot lights share the 2D atlas → always render (leave as is;
  guarded by the `has_own_attachment` check in the reconcile).
- `ShadowViewThrottle` (`shadows/record.rs`) persists `last_rendered_frame` /
  `last_view_projection` across the per-frame `records` rebuild.
- Per-view `should_render` skip in `shadows::render_pass::record` (it `continue`s
  when `!view.should_render`).
- The reconcile loop in `Shadows::write_gpu` (`shadows/state.rs`) already
  invalidates (sets `last_rendered_frame = u64::MAX`) on atlas-rect / cascade-layer
  change and on **`view_projection_drift`** (camera move → cascade VP changes →
  drift fires, so the camera/cascade dependency is handled for free), and on
  shadow-config / resolution change.

**The ONE missing input:** a "casters static this frame" signal. Today near
cascades (`update_period == 1`) + cube faces re-render every frame regardless.

**Implementation (per-view; do NOT skip the whole pass — forced re-renders must
still fire):** in the reconcile loop's `due` computation in `Shadows::write_gpu`,
split forced vs periodic and suppress only the periodic when static:
```rust
let forced   = t.last_rendered_frame == u64::MAX;            // rect/layer/drift/config
let periodic = frame >= t.last_rendered_frame.saturating_add(view.update_period);
let due      = forced || (periodic && !shadow_static);
```
Thread a `shadow_static: bool` param into `Shadows::write_gpu` (the call is in
`AwsmRenderer::render`). Note: light color/intensity do NOT affect the depth map —
only the light **transform** (→ dirty transforms) and config (existing
invalidation). So:
```
shadow_static = !caster_transforms_dirty_this_frame
             && !camera.camera_moved()
             && !caster_set_changed
             && !time_driven_shadow_present
```

**Three hard parts (this is why it's deferred, not a quick task):**

1. **Editor HUD churn.** The editor re-anchors gizmo / light-icon / skeleton-overlay
   HUD meshes EVERY frame → dirties transforms every frame. A coarse "any
   transform dirty → don't cache" would therefore **never cache in the editor**.
   Must filter the dirty set to **cast-shadow** meshes (HUD is excluded from
   casters via `NodeFilter::shadow_caster` `exclude_hud`). Capture in
   `AwsmRenderer::update_transforms` (the single point that consumes
   `Transforms::take_dirty_meshes()` / `Instances::take_dirty_transforms()`);
   **accumulate** across the multiple per-frame `update_transforms` calls and
   reset after the shadow gate reads it. Need a transform-key → is-cast-shadow-mesh
   check (`Meshes` has mesh→transform_key, not reverse — add a reverse lookup or
   maintain a cast-shadow-transform-key set; the dirty set is small, so iterate it
   + an O(1) lookup).

2. **Time-driven shadow materials.** FlipBook-masked (and any time-reading
   custom-masked) shadows change EVERY frame with NO transform dirty (the cutout
   is driven by `frame_globals.time`). A transform-quiet cache would FREEZE an
   animated cutout shadow. `time_driven_shadow_present = !dynamic_materials.is_empty()`
   (any custom — could read time) `|| Materials has any FlipBook` (add a
   `Materials::has_flipbook()` helper — `Material::FlipBook`). Opaque /
   alpha-tested-texture (PBR/Unlit/Toon) shadows are NOT time-driven → cacheable.

3. **Caster-set change.** Add/remove a cast-shadow mesh must re-render. `Meshes`
   count delta is a cheap proxy (HUD add/remove gives harmless false-positives →
   just re-renders); better is a cast-shadow-mesh count/revision.

**Conservative stance:** default to re-render; only skip on a provably-quiet
frame. Risk = a stale shadow if the signal misses a change.

**Verify (browser, the full matrix):**
- static opaque scene → shadows correct + `render_cpu_ms` drops (`memory_stats` query)
- move a caster → its shadow updates; move camera → directional shadows update
- FlipBook-masked caster → shadow keeps animating while geometry is static
- editor with gizmo visible → still caches (HUD churn ignored)
- add/remove a caster → re-renders

---

## 2. Perf at thousands of meshes

Open-ended profile-and-optimize. Build a repeatable bench (instance a primitive
N thousand times via MCP), profile the per-frame CPU (`render_cpu_ms` via the
`memory_stats` query), find + fix the hotspots so a large scene stays
interactive. Record before/after numbers. Static-shadow caching (above) is one
input; also profile per-frame renderable collection, classify, and transform
upload scaling. Likely candidates: per-frame allocations in the renderable walk,
redundant GPU uploads, anything O(n) that could be incremental/dirty-tracked.

---

## 3. #31 TTFR prewarm-after-load (needs maintainer wall-clock)

Time-to-first-render after a model load has a sub-frame-transient hitch that is
**unmeasurable from the agent side** (`scene_png` can't catch it; it's gone by
the time a capture lands). Implement a prewarm-after-load (compile the loaded
model's pipelines before the first frame that shows it — `prewarm_pipelines` /
`wait_for_pipelines_ready` already exist as the building blocks), but **verifying
it needs a human wall-clock / eyes on a real cold load** — the agreed
human-in-the-loop step.

---

## 4. Player-grade scene-loader follow-ons (crate `awsm-scene-loader`)

The player-grade loader (`load_scene_for_player` / `populate_awsm_scene`, `materialize`
in `lib.rs`) materializes every render `NodeKind` EXCEPT the items below. None are
regressions — the pre-loader `materialize` dropped all of these in its `_ => {}`
arm; the core of each is now wired, these are the unfinished dimensions. The
renderer already exposes everything needed for (4.1)–(4.3); they were left as
documented follow-ons, not blocked.

### 4.1 ParticleEmitter rendering

`NodeKind::ParticleEmitter(ParticleEmitterDef)` is currently a clean-skip (one-time
`tracing::warn!` in `materialize`). **Correction to the earlier "no renderer particle
pass" framing:** the renderer CAN render particles — `Meshes::enable_mesh_instancing_opaque`
+ per-frame `Meshes::set_mesh_instances` (transforms) + `Meshes::set_mesh_instance_attrs`
(per-instance color + alpha + size) are exactly an instanced-billboard particle
renderer. There is no dedicated particle pass and one isn't needed.

What's actually unbuilt: `ParticleEmitterDef` (scene crate `particle.rs`) is a
*simulation spec* — `spawn_rate`, `burst_count`, `max_alive`, `lifetime`,
`initial_speed`, `forces: [ForceDef::Gravity{…}]`, `color_over_life`,
`size_over_life` — so particles spawn/integrate/age/die EVERY frame. The loader is
a one-shot pass and never ticks (same boundary as animation: it loads clips, the
consumer drives the clock). Two designs to pick from:
- **(A) Loader sets up, game ticks** — `materialize` builds the billboard quad
  (reuse `build_sprite_mesh`) + `enable_mesh_instancing_opaque` at `max_alive`
  capacity + the FlipBook/Unlit material, and returns an emitter handle (the mesh
  key + the `ParticleEmitterDef`) in `NodeHandles`; the game runs the CPU sim and
  calls `set_mesh_instances` / `set_mesh_instance_attrs` each frame. Matches the
  loader's existing "loads, doesn't drive" contract; still needs a few lines of
  consumer per-kind code (the tick).
- **(B) A renderer-side CPU particle component** that owns emitter state + ticks
  itself in the render loop (spawn/integrate/cull → writes the instance buffers).
  Net-new renderer module, but the only version that satisfies "renders with NO
  consumer per-kind code". Bigger effort; could be its own small plan.

(lockstep's `scene/particles.rs` is a reference CPU sim for either path.)

### 4.2 InstancesAlongCurve per-instance attributes

`materialize_instances_along_curve` places copies via `enable_mesh_instancing_opaque`
(transforms only). `InstancesAlongCurveDef.per_instance_colors` is NOT applied yet —
wire it through `Meshes::set_mesh_instance_attrs` (same API particles would use).
Per-instance `shadow` (`InstancesAlongCurveDef.shadow`) is genuinely limited: shadow
is a mesh-level flag, not per-instance — would need a renderer change to vary it per
instance (low priority). Also: source-node resolution relies on DFS order (source
materialized before the instances node) — currently best-effort with a warn; make it
order-independent if it bites.

### 4.3 Prefab non-mesh children

`PrefabTemplate::instantiate` replays MESH nodes cheaply (`duplicate_mesh_with_transform`,
shared GPU buffers). Light / Camera / Line / Decal nodes inside a prefab contribute
only their transform to each instance — they aren't re-created per instance (there's
no `duplicate_*` for them). To replay them, `instantiate` would re-call
`insert_light` / `add_line_strip` / `insert_decal` per instance from the captured
`PrefabNode` metadata (extend `PrefabNode` to carry the light/line/decal config, not
just `template_meshes`). Straightforward, just unwired.

### 4.4 Decal texture-index ≤64-layer assumption

`materialize_decal` resolves a decal's texture to a flat pool index as
`array_index * 64 + layer_index` (the `DECAL_POOL_LAYERS_PER_ARRAY` const in
scene-loader), matching the decal shader's `texture_index % 64` packing. If the
texture pool ever grows an array past 64 layers, a decal texture landing on
`layer >= 64` would index wrong. Either confirm the pool never exceeds 64
layers/array, or unify the constant between the shader and the loader so they can't
drift.

---

## 5. Shared prep pass (Plan B) — remaining items

The shared "prep" pass (every material-independent per-pixel computation runs once
into a buffer; per-material kernels read it) is **implemented + GPU-verified
byte-identical**, flag-gated behind `PrepPassConfig` / `AwsmRendererBuilder::with_prep_pass`
(crate `awsm-renderer`, `render_passes/material_prep/`). Per-shader size wins landed:
PBR no-MSAA −50 KB, MSAA −46 KB. Two items were intentionally left:

### 5.1 Flip the prep pass default-on (a decision, not code)

`with_prep_pass` currently defaults **OFF**. The intended direction is **default-on**
(keeping the A/B flag), but it was held for two reasons: (a) prep is the foundation
the uber-shader builds on, and the edge machinery it touches may be reshaped by the
uber-shader's single-pass model — so settle that direction first; (b) flipping the
default should be backed by a **runtime** measurement (prep should be win-or-tiny-diff
at runtime, not just a size win), and that sweep needs the
`experiments/compare-threejs-materials` benchmark infra (sibling repo, not in this
worktree). When both are settled: flip the builder default, and **re-tighten the
`size_regression` ceilings** (`material_opaque/shader/template.rs`) — today they're
sized for the non-prep variants; the prep deltas are only asserted by the naga tests.

### 5.2 5b-attrs — compact per-edge-sample UV/vcolor buffer (perf/VRAM tradeoff, deferred)

Under MSAA, the `5b-shadow` half shipped: a compact per-edge-sample SHADOW buffer
(`material_prep/buffers.rs` `EdgeShadowBuffer`, an `Rgba8unorm texture_2d_array` to
dodge the 10-storage-buffer cap; filled by `cs_prep_edge`) that the unified `cs_shade`
edge path reads via `PrepReadContext` EDGE mode — dropping inline shadow sampling from
the MSAA module (−46 KB). The **UV/vcolor sibling was deferred**: edge samples still
**recompute** UV0/vcolor0 (the RECOMPUTE path) rather than reading a prep buffer. A
`5b-attrs` buffer (same compact per-edge-sample shape as `EdgeShadowBuffer`, filled by
`cs_prep_edge`, read by `cs_shade`'s EDGE arm) would let edge samples read instead of
recompute. **Deliberately deferred — small win, large cost:** the UV-recompute helpers
are cheap (vs the ~50 KB shadow block that made 5b-shadow worth it), while the buffer
is big (~48 MB at `max_edge_budget` 512K for set 0 alone, more for multi-UV). Only do
it if profiling shows edge-sample attribute recompute is a real hotspot. NOTE: the
original spec wrote this against the now-deleted `cs_edge`; retarget to `cs_shade`'s
EDGE branch (the unified-edge kernel that replaced it).
