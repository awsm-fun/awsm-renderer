# Design + implementation spec: the load transaction (declare → commit → ready)

**Status: fully specced, ready to implement.** This is a standalone doc — an implementer can execute
it start-to-finish without re-deciding architecture. All open questions are resolved below with
concrete decisions grounded in the current code (file:line anchors throughout).

The goal: **declare all scene content, then ONE `commit()` finalizes + compiles everything
concurrently against the final state; the renderer does not render scene frames or reactively
compile until a commit is ready.** This replaces "mutate the live renderer as content streams in and
recompile on every input change," which is the root of the TTFR recompiles + the `remove_all` config
drift we hit.

**The primary win is making this easier to reason about, not the perf fix** (that's a consequence).
Today there are *several overlapping ways* to "get content compiled and onto the screen" —
`prewarm_pipelines`, `wait_for_pipelines_ready`, `wait_for_pipelines_ready_with_progress`,
`compile_material_variants`, and `finalize_gpu_textures` scattered across a dozen call sites, plus an
eager edge `ensure_compiled`. Even though the underlying compile machinery is sound, that surface is
confusing and *is itself how we keep getting perf bugs*. So this work is as much **consolidation +
naming + deleting call sites that shouldn't exist** as it is the transaction itself: when it's done
there should be **ONE obvious way** (`begin_load` → add → `commit_load`), the overlapping entry
points are internal or gone, and the load is something you can read top-to-bottom and trust.

---

## 0. Why (what this deletes)

Today the renderer reactively recompiles whenever an input changes (texture-pool array count, live
bucket set). Compiling against a *moving target* is the root complexity:
- **TTFR:** the MSAA edge pipeline recompiles ~3× per load because `finalize_gpu_textures`
  (`textures.rs:101`) runs several times during a load (gltf populate, scene-loader, IBL/skybox
  batches) and each eagerly `.await`s `edge_pipelines.ensure_compiled(...)` (`textures.rs:668-697`)
  at a different `texture_pool_arrays_len`. Loading-frame renders catch pipelines mid-compile →
  `not compiled, skipping`.
- **Config drift on recreate:** `remove_all` (`renderer.rs:420`) hand-copies live state into a fresh
  builder and silently dropped configs (bucket cap, shadow-K).

**Key fact (already true):** every content-add is ALREADY deferred — `add_image` (`textures.rs:966`)
just stages data; `register_material` (`registry.rs:1159`) just adds a bucket + scheduler entry +
marks `variants_dirty`; mesh `insert` (`meshes.rs:904`) stages CPU buffers; `set_skybox`
(`environment.rs:17`) just marks a bind-group dirty. The GPU/compile work is already batched into
`finalize_gpu_textures` + the render-driven `ensure_scene_pipelines`. **The machinery exists; it's
just driven reactively + repeatedly during load.** This design drives it ONCE, explicitly, at commit.

---

## 1. The model (resolved)

Two renderer flags drive everything:

- **`loading: bool`** — true between `begin_load()` and `commit_load()`. While true, the render
  preamble SKIPS the reactive `reconcile_material_variants` / `ensure_scene_pipelines` compile, so
  adds never compile against a moving target. **Applies to BOTH cold-load and live-add — same flag,
  same machinery.**
- **`committed_once: bool`** — false at build, set true by the first `commit_load()`. Drives the
  one-time cold-start render gate (below).

Derived render gate: **`first_load_pending = !committed_once && loading`** (an app that opted into a
load but hasn't committed its first one yet). When true, `render()` clears the framebuffer and skips
the scene pass. After the first commit, it's permanently false → live loads keep showing the existing
scene while their delta compiles in the background.

**Backward compatible:** an app that never calls `begin_load`/`commit_load` has `loading=false` +
`committed_once=false` → `first_load_pending=false` → renders exactly as today.

### Lifecycle (identical for cold-load and live — INVARIANT)

```
renderer.begin_load();                       // loading = true
//   add content via the EXISTING deferred APIs — no new wrappers needed:
renderer.populate_gltf(data).await?;         // textures staged, meshes staged, materials registered
renderer.add_image(...) / register_material(...) / set_skybox(...) / set_ibl(...);
let stats = renderer.commit_load(|s| { /* progress */ }).await?;   // THE single compile point
//   loading = false; committed_once = true; gate open; everything GPU-resident
```

- **Cold load:** the app `.await`s `commit_load` before revealing the scene (gate is closed
  meanwhile → clear color behind the loading overlay).
- **Live add:** the app does the *exact same* `begin_load → add → commit_load`, but the existing
  scene keeps rendering (gate already open since `committed_once`), and the app may choose not to
  block on the `commit_load` future (fire-and-let-it-land). The delta's meshes simply aren't drawn
  until its commit is ready (content-keyed compile → only the new pipelines compile; the rest are
  cache hits).

> **HARD INVARIANT:** `begin_load` / `commit_load` are the same code for cold and live. The ONLY
> differences are app-level: (a) whether the app awaits the commit before revealing, and (b) the
> one-time `first_load_pending` gate (driven by `committed_once`, not by the load path). **If
> implementation forces any other divergence between cold and live, STOP and raise it for
> discussion** — divergent paths for the same action is the exact trap this deletes.

---

## 1.5 Consolidate the surface (a primary goal — not a side effect)

When this lands there is **exactly one public way** to get content compiled and shown, and the
overlapping ways are gone. This is the point of the work; treat it as a deliverable, not cleanup.

**The one public surface:** `begin_load()` · `commit_load(on_progress)` · `loading_stats()`.
(Plus the unchanged per-kind deferred adds — `add_image`, `register_material`, `populate_gltf`,
`set_skybox`, `set_ibl`, mesh `insert`.)

**Make internal (`pub(crate)`/private) — callable ONLY from `commit_load`, never by embedders:**
- `prewarm_pipelines` (`renderer.rs:563`) — folded into `commit_load`.
- `wait_for_pipelines_ready` + `wait_for_pipelines_ready_with_progress` (`renderer.rs:2330/2340`) —
  collapse to ONE internal drain (`commit_load` calls it). Don't keep two near-identical entry
  points; the progress-callback form is the only one needed.
- `compile_material_variants` (`registry.rs:1381`) — its job (reconcile + wait) IS `commit_load` for
  materials; remove the separate public method, callers move to `commit_load`.
- `finalize_gpu_textures` (`textures.rs:101`) — internal; `commit_load` calls it exactly once.
  Embedders no longer call it directly.

**Delete outright (call sites that shouldn't exist):**
- The eager `edge_pipelines.ensure_compiled(...)` block inside `finalize_gpu_textures`
  (`textures.rs:668-697`) — the asymmetry that caused the 3× recompile.
- The boot prewarm at `canvas.rs:156` and the per-load `compile_materials()` →
  `compile_material_variants` at `scene.rs:877` — both replaced by one `commit_load`.
- The scattered editor `finalize_gpu_textures` calls (`thumbnail.rs:164,248`; `preview.rs:112,155`;
  `node_sync.rs:724,874,995`) — replaced by `begin_load → … → commit_load` brackets.

**Naming:** the kept names should say what they do. `begin_load` / `commit_load` for the bracket;
the internal drain reads as the commit's compile phase, not a free-floating `wait_for_pipelines_ready`.
If a kept name still implies "a thing you call ad-hoc mid-render," rename it. A reader scanning the
public surface should see one load story, not five.

**Acceptance for this goal:** grep the codebase — outside the renderer's own internals, the only
load/compile calls are `begin_load` / `commit_load` / `loading_stats`. No embedder calls
`finalize_gpu_textures`, `compile_material_variants`, `prewarm_pipelines`, or `wait_for_pipelines_ready`.

## 2. API (resolved)

On `AwsmRenderer` (no separate buffered transaction object — adds go through the existing deferred
APIs across any number of `Mutex` lock scopes, which a borrowing transaction object couldn't span):

```rust
/// Enter load mode: suppress the per-frame reactive compile so subsequent adds don't compile
/// against a moving target. Idempotent. Does NOT hide an already-shown scene (live-safe).
pub fn begin_load(&mut self);

/// THE single compile point. Finalizes the texture pool ONCE, kicks every needed pipeline compile,
/// drains them CONCURRENTLY (FuturesUnordered), uploads textures, reports progress, and on success
/// opens the render gate. Idempotent if nothing was added since the last commit (cheap no-op).
pub async fn commit_load(
    &mut self,
    on_progress: impl FnMut(LoadingStats),
) -> Result<LoadingStats>;

/// Imperative snapshot of the same data on_progress receives (for pollers).
pub fn loading_stats(&self) -> LoadingStats;
```

`commit_load` body (reuse what exists — see §1 "Key fact"):
1. `self.loading` is true (set by `begin_load`). Report `LoadingStats { phase: FinalizingTextures, .. }`.
2. `self.finalize_gpu_textures().await?` — ONCE (batches every staged texture). **Remove the eager
   `edge_pipelines.ensure_compiled(...)` block at `textures.rs:668-697`** — the edge will be compiled
   in step 3 against the now-final pool, once.
3. `phase = Compiling`. Run the concurrent drain that already exists:
   `self.wait_for_pipelines_ready_with_progress(|cp| on_progress(LoadingStats::from(cp, ...)))`
   (`renderer.rs:2340`) — its Phase-1 kicks `ensure_scene_pipelines` (compiles opaque + edge against
   final inputs), Phase-2 drains `inflight_compile` via `FuturesUnordered::next().await`. **This is
   the concurrent commit — do not reimplement it.**
4. `self.loading = false; self.committed_once = true;` → gate opens. Return final `LoadingStats`.

> Optional ergonomic wrapper (do AFTER the core works, only if it reads better): a
> `LoadTransaction<'a>` returned by `begin_load()` that derefs to `&mut AwsmRenderer` for adds and
> whose `commit(self, on_progress)` calls `commit_load`. The borrow-across-lock-scopes constraint
> means the bracket-on-the-renderer form above is the required baseline; the wrapper is sugar.

---

## 3. Render gate (resolved)

In `AwsmRenderer::render()` (`render.rs:75`), AFTER `poll_pipeline_scheduler()` (`render.rs:84`,
keep draining resolved compiles) and BEFORE the scene-pass chain:

```rust
let first_load_pending = !self.committed_once && self.loading;
if first_load_pending {
    // Cold start: nothing committed yet. Clear to clear_color (loading overlay draws on top), skip
    // the scene passes + the reconcile/ensure_scene_pipelines preamble.
    self.clear_framebuffer_only()?;   // (factor the existing clear out of the pass chain)
    return Ok(());
}
```

And in the preamble's reactive compile (`render.rs:293` `reconcile_material_variants` →
`ensure_scene_pipelines`): **gate it on `!self.loading`** so a live add accumulating across frames
doesn't compile incrementally. (The existing `variants_dirty` flag is preserved across the
suppression; `commit_load` consumes it via the wait path.)

- model-tests loading overlay (`canvas.rs:174-216`, CSS over the canvas) is unchanged — it now sits
  over a clear-color frame instead of a half-rendered scene.
- Editor (continuous render, never calls `begin_load` for its steady state) is unaffected — gate
  stays open. (Its load steps that DO adopt the transaction get the live-add behavior.)

---

## 4. LoadingStats (resolved)

Extend the existing `CompileProgress` (`scheduler.rs:144` — `materials_pending`, `materials_ready`,
`materials_failed`, `in_flight_subcompiles`) into `LoadingStats`:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadingStats {
    pub phase: LoadPhase,            // Idle | FinalizingTextures | Compiling | Ready
    pub textures_total: usize,
    pub textures_uploaded: usize,
    pub pipelines_pending: usize,   // = CompileProgress.materials_pending
    pub pipelines_ready: usize,     // = materials_ready
    pub pipelines_failed: usize,    // = materials_failed
    pub in_flight_subcompiles: u32,
}
pub enum LoadPhase { Idle, FinalizingTextures, Compiling, Ready }
```

- `finalize_gpu_textures` reports texture progress (it already iterates the staged pool — count
  total + emit per-upload).
- The compile drain already calls `on_progress(compile_progress())` per resolution
  (`renderer.rs:2374-2376`) — map `CompileProgress → LoadingStats`.
- `loading_stats()` returns the same struct from the same source (the scheduler snapshot + the
  texture counts). One struct, both paths.

---

## 5. `remove_all` / config-spec (resolved — do as the final step)

Capture all 18 build-time builder fields (`renderer.rs:790-1189`: logging, render_texture_formats,
brdf_lut_options, clear_color, skybox/ibl colors, anti_aliasing, post_processing, shadows_config,
features, max_edge_budget, bucket_config, prep_config, optimization_policy, scene_spatial_config,
recommended_shadow_quality_tier, depth override) into a `RendererConfigSpec` struct stored on the
renderer at `build()`. Then `remove_all` (`renderer.rs:420`) becomes:
`AwsmRendererBuilder::from_spec(self.gpu.clone(), self.config_spec.clone()).build().await` — one
line, no hand-copy, no drift (closes the `brdf_lut_options`/cap/K drift class permanently). This is
the config analog of the load transaction (declare→commit for config vs content). Layer them: the
builder/spec produces the renderer; transactions load content into it.

---

## 6. Implementation sequence (ordered; keep `cargo test … --lib` green + `task lint` clean per step)

1. **Flags + render gate.** Add `loading`/`committed_once` to `AwsmRenderer`; the render gate in
   `render()` (`render.rs` after :84) + the `!loading` guard on the preamble reconcile (`render.rs:293`);
   factor out `clear_framebuffer_only`. Default behavior unchanged (both flags false). Verify a normal
   model still renders.
2. **`LoadingStats`.** Add the struct + `LoadPhase`; map from `CompileProgress`; add texture
   counting to `finalize_gpu_textures`; add `loading_stats()`.
3. **`begin_load` / `commit_load`.** Implement per §2 (reusing `finalize_gpu_textures` +
   `wait_for_pipelines_ready_with_progress`). **Remove the eager edge `ensure_compiled` block at
   `textures.rs:668-697`** (the edge now compiles once in the commit drain). Re-verify the
   MSAA-change path (`set_anti_aliasing`) + cold-boot still compile their edge pipelines (they go
   through `ensure_scene_pipelines`/their own ensure — confirm, don't assume).
4. **Migrate model-tests.** Wrap the load (`canvas.rs` + `scene.rs`): `begin_load()` before the
   upload phase; keep the existing `populate_gltf`/`set_ibl`/`set_skybox` adds; replace
   `compile_materials()`'s `compile_material_variants()` (`scene.rs:877`) AND the boot prewarm
   (`canvas.rs:156`) with the single `commit_load(on_progress)`; drive the loading overlay
   (`context.rs` `LoadingStatus`) from `LoadingStats`. The rAF loop (`scene.rs:1164`) is unchanged —
   the render gate handles the cold frames.
5. **Migrate editor.** thumbnail (`thumbnail.rs:164,248`), preview (`preview.rs:112,155`), node_sync
   (`node_sync.rs:724,874,995`): wrap their per-node `finalize_gpu_textures` loads in
   `begin_load → adds → commit_load`; boot prewarm (`editor/main.rs:95`) → `commit_load`. These
   exercise the LIVE path → proves invariant ③.
6. **`RendererConfigSpec` + `remove_all`** (§5).
7. **Consolidate the surface (§1.5) — the forcing function.** Now that every app/editor call site is
   migrated, make `prewarm_pipelines`, `wait_for_pipelines_ready`,
   `wait_for_pipelines_ready_with_progress`, `compile_material_variants`, and `finalize_gpu_textures`
   `pub(crate)`/private. The compiler is the check: nothing outside the renderer crate may still call
   them — if it does, that call site was missed in steps 4-5, fix it. Collapse the two
   `wait_for_pipelines_ready*` into one internal drain. Rename anything whose public-looking name
   still implies "call me ad-hoc." End state = the §1.5 acceptance grep passes.
8. **Delete dead reactive machinery** once 1-7 land: confirm `last_ensured_bucket_layout` / the
   per-frame edge-compile path is now only used by genuine live AA/material changes (not load), and
   simplify whatever is now unreachable. Don't delete blindly — verify each removal.

---

## 7. Verification (per the standards gate: no perf regression; default-equals-today; MSAA-compile invariant)

- **Single edge compile per cold load:** console shows ONE `ensure_compiled … final_blend` (or the
  render-driven edge compile) per model load, not ~3×; no `not compiled, skipping` on the first
  interactive frame.
- **Screenshot-verify** (chrome-devtools :9080) several models incl. an MSAA model — MSAA edges
  intact, environment + shadows correct. ALWAYS screenshot before trusting console/timing.
- **Cold vs live use the same commit:** add an editor/live path test (or model-tests hook) that adds
  content after first reveal and confirm it routes through `commit_load` (invariant ③).
- **`remove_all`** preserves all config (load a model → `remove_all` → load again; bucket cap,
  shadow-K, brdf-lut all intact).
- **Load trace** shows reduced compile time (fewer total compiles).
- **One way only (§1.5 acceptance):** grep confirms no caller outside the renderer's own internals
  uses `finalize_gpu_textures` / `compile_material_variants` / `prewarm_pipelines` /
  `wait_for_pipelines_ready*`; the public load surface is `begin_load` / `commit_load` /
  `loading_stats` and nothing else. The load is readable top-to-bottom.
- `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader --lib` green + `task lint`
  clean throughout. Commit per step with explicit paths (NEVER `git add -A`, NO backticks in `-m`).

---

## Unrelated open issues (not part of this design)

- **Minor model-tests quirks (cosmetic).** `IridescenceDishWithOlives` renders black (camera
  framing / IBL — black in baseline too, not a renderer regression); a few model names in the picker
  route to "Not Found".
