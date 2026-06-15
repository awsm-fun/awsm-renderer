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
