# Follow-up work (post-release)

The only intentionally-unfinished items, to pick up in a fresh session. Everything
else on the `mesh-authoring` line shipped + was verified. Branch is **not pushed**;
the maintainer publishes the release.

---

## 1. Static-shadow caching (perf) — task #50

**Goal:** skip re-rendering a light's shadow map when nothing affecting it changed
this frame, to cut per-frame cost on scenes with many static shadow-casters
(primary beneficiary: the runtime, not the editor).

**Most of the infra already exists** — do NOT rebuild it:
- Persistent shadow textures (per cube face / cascade-array layer); per-view
  attachments clear independently, so a *skipped* view keeps last frame's depth
  (caching is safe). Spot lights share the 2D atlas → always render (leave as is).
- `ShadowViewThrottle` (`shadows/record.rs`) persists `last_rendered_frame` /
  `last_view_projection` across the per-frame `records` rebuild.
- Per-view `should_render` skip already in `shadows/render_pass.rs` (~line 61).
- The reconcile (`shadows/state.rs` ~2426-2456) already invalidates on
  atlas-rect / cascade-layer / **view-projection drift** (camera move → cascade
  VP changes → drift fires, so the camera/cascade dependency is handled) and on
  shadow-config/resolution change (forces `last_rendered_frame = u64::MAX`).

**The ONE missing input:** a "casters static this frame" signal. Today near
cascades (period 1) + cube faces re-render every frame regardless.

**Implementation (per-view, NOT whole-pass-skip):** in the reconcile, change the
`due` computation (`state.rs` ~2438):
```rust
let forced   = t.last_rendered_frame == u64::MAX;          // rect/layer/drift/config
let periodic = frame >= t.last_rendered_frame.saturating_add(view.update_period);
let due      = forced || (periodic && !shadow_static);     // suppress periodic when static
```
Thread a `shadow_static: bool` param into `Shadows::write_gpu` (sig `state.rs:1519`;
call site `render.rs:378`). Light color/intensity do NOT affect the depth map —
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
   `update_transforms` (`transforms.rs:37`, the single point that consumes
   `take_dirty_meshes()` / `take_dirty_transforms()`); **accumulate** across the
   multiple per-frame `update_transforms` calls and reset after the shadow gate
   reads it. Need a transform-key → is-cast-shadow-mesh check (Meshes has
   mesh→transform_key, not reverse — add a reverse lookup or maintain a
   cast-shadow-transform-key set; the dirty set is small so iterating it + an
   O(1) lookup is cheap).

2. **Time-driven shadow materials.** FlipBook-masked (and any time-reading
   custom-masked) shadows change EVERY frame with NO transform dirty (the cutout
   is driven by `frame_globals.time`). A transform-quiet cache would FREEZE an
   animated cutout shadow. `time_driven_shadow_present = !dynamic_materials.is_empty()`
   (any custom — could read time) `|| Materials has any FlipBook` (add
   `Materials::has_flipbook()`). Opaque/alpha-tested-texture (PBR/Unlit/Toon)
   shadows are NOT time-driven (geometry-only) → cacheable.

3. **Caster-set change.** Add/remove a cast-shadow mesh must re-render. `meshes.len()`
   delta is a cheap proxy (HUD add/remove gives harmless false-positives → just
   re-renders); better is a cast-shadow-mesh count/revision.

**Conservative stance:** default to re-render; only skip on a provably-quiet
frame. Risk = a stale shadow if the signal misses a change.

**Verify (browser, the full matrix):**
- static opaque scene → shadows correct + `render_cpu_ms` drops (memory_stats)
- move a caster → its shadow updates; move camera → directional shadows update
- FlipBook-masked caster → shadow keeps animating while geometry is static
- editor with gizmo visible → still caches (HUD churn ignored)
- add/remove a caster → re-renders

---

## 2. Perf at thousands of meshes — task #51

Open-ended profile-and-optimize. Build a repeatable bench (instance a primitive
N thousand times via MCP), profile the per-frame CPU (`render_cpu_ms` via the
`memory_stats` query), find + fix the hotspots so a large scene stays
interactive. Record before/after numbers. Static-shadow caching (#50) is one
input; also look at per-frame renderable collection / classify / transform upload
scaling.

---

## 3. #31 TTFR prewarm-after-load — task #52 (needs maintainer wall-clock)

Time-to-first-render after a model load has a sub-frame-transient hitch that is
**unmeasurable from the agent side** (`scene_png` can't catch it; it's gone by
the time a capture lands). Implement a prewarm-after-load (compile the loaded
model's pipelines before the first frame that shows it), but **verifying it needs
a human wall-clock / eyes on a real cold load** — that's the agreed
human-in-the-loop step. Until measured, treat as: implement the prewarm + this
documented repro.

---

## 4. (Separate effort) MCP template repo

Not part of the renderer release. A tiny separate repo (working name `awsm-mcp`)
so anyone can drive the **hosted** editor from an MCP agent without cloning
`awsm-renderer` / building the WASM frontend: ship the prebuilt
`awsm-renderer-mcp` binary + a ready `.mcp.json`, open the hosted editor, click
**Connect**. Depends on the MCP work (already on the `mcp` line). (Folded here
from the deleted `template-repo.md`.)
