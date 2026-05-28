# Renderer remaining work

PR #99 (the `more-optimizations` branch) landed essentially everything from the long-form `more-optimizations.md` plan that used to live next to this file. This document is the survivor — it captures **only** what's actually left to do, with no historical narrative.

Items are grouped by theme. Each has a one-line acceptance criterion. None block any consumer.

---

## Runtime correctness (Phase-6 extras-pool tail)

The extras-pool allocator's runtime path (`assign_or_update` → `slice_for` → `write_dirty_ranges`) is fully implemented and exercised. Two tail features remain:

### Pool resize on overflow

`assign_or_update` returns `AwsmDynamicMaterialError::OutOfCapacity` when the bump allocator runs out of room. The 1 MiB initial allocation (`DEFAULT_CAPACITY_WORDS = 262_144`) handles every authored material we've shipped, but a real-game consumer registering many `BufferSlot`-heavy materials would hit it.

**Acceptance**: `assign_or_update` re-allocates the GPU buffer with 2× capacity on overflow, marks bind groups for recreation, and copies existing data via `copyBufferToBuffer` so live slices stay valid.

### Fragmentation-triggered compaction

Bump-allocator + per-instance overrides means long edit → re-register cycles fragment the pool (each old slice leaks). Nothing reclaims those.

**Acceptance**: a free-list pass runs when fragmentation exceeds a threshold (e.g. >50% of allocated space unreferenced). Compacts surviving slices to the front via `copyBufferToBuffer`, updates `slice_for`, marks bind groups dirty.

Both are runtime-correctness items, not optimizations — current behavior is "errors out at capacity / leaks dead bytes," acceptable for the editor / authoring use case but not for shipping-game runtime.

---

## Asset authoring

The renderer code path for each of these is fully implemented and unit-tested. What's missing is the on-disk `material.json` + `shader.wgsl` + (where applicable) `frames.bin` content. **All three require user-side hand-authoring of artwork; the plan agent can stage the file scaffolds but not the textures themselves.**

### `irregular-atlas` test material

First material that exercises the `BufferSlot` + extras-pool path end-to-end.

- Hand-author `material.json` with one `BufferSlot { name: "frames" }`.
- Hand-author `frames.bin` (packed `Vec<vec4<f32>>` of UV rects).
- Hand-author `shader.wgsl` that reads `input.material.frames_offset` / `..._length`, indexes the per-instance frame from the extras pool via `extras_load_vec4_f32`, and samples the atlas at the resulting UV rect.

**Acceptance**: a scene with two `irregular-atlas` instances at different `buffer_overrides` renders independently. Both run through `prewarm_dynamic_pipelines`, both hit the extras-pool with distinct slices, both visually animate the right cells of the atlas.

### `soft-glass` test material

First **Blend** dynamic material in a real scene.

- Hand-author `material.json` with `alpha_mode: Blend`, `double_sided: true`, uniforms `[tint: Color3, edge_alpha: F32, face_alpha: F32]`.
- Author the WGSL fragment per the `contract-transparent.md` worked example.

**Acceptance**: a scene with a soft-glass sphere in front of an opaque ground plane renders correctly, sorted back-to-front against any first-party transparent material that shares the scene.

### Side-by-side test scene

A Phase-14 deliverable that's clearly content, not engineering: a test scene placing `scanline` (custom opaque), `soft-glass` (custom transparent), and the promoted `scanline` first-party material side-by-side.

**Acceptance**: visual diff between the dynamic + promoted scanlines is empty. The transparent material composes correctly with the opaque ones.

---

## material-editor UI niceties

### Quad / sphere / box mesh selector

Today the preview canvas ships with a fixed 2×2 plane. Materials whose visual behavior depends on geometry want a curved surface.

- Add a `preview_mesh: Mutable<PreviewMeshKind>` to `EditState`.
- Add a selector to the Preview pane header.
- In `RendererHost::apply_quad_for_current_registration`, regenerate the stub mesh when the selector changes.

**Acceptance**: switching the preview mesh updates the live preview within one debounce cycle. Plane remains the default.

### Buffer Converter modal

The schema + data shapes are in place (`BufferSlot` carries a `default: Option<String>` pointing at an asset filename). Today the material-editor doesn't have UI for adding/replacing buffer-default bytes; the user hand-authors `frames.bin` etc.

**Acceptance**: a modal in the Definition pane lets the user select a `.bin` / `.json` / drag-and-drop bytes, parse them per the slot's documented schema, and write to `assets/materials/<name>/<slot>.bin`. Required when an `irregular-atlas`-shaped material lands in scene-editor's Import Material flow.

### Error → cursor positioning ✅

**Done.** The Errors pane entries are clickable; clicking positions the WGSL textarea's caret at the reported line+column (best-effort via `setSelectionRange`). See `panes/errors.rs` + `panes/wgsl_editor.rs` — the textarea now has a stable `id` and the errors pane resolves line/column to character offset.

### File → New runnable stub

`EditState::new_scanline` seeds the scanline starter on boot. A `File → New` button that resets state to a different starter (a minimal "constant red" material, an unlit baseline, etc.) would help new authors get started without copying from an existing one.

**Acceptance**: a New button in the top-bar offers ≥2 starter templates; selecting one wipes the EditState and triggers the debounced recompile.

---

## scene-editor UI niceties

### Open in material-editor deep-link

The "Open in material-editor" link in the Custom Materials pane opens material-editor at its root URL. The material-editor doesn't consume the `folder` query param to pre-seed `EditState`; the user lands on the scanline starter and has to re-import.

**Acceptance**: query-param `?folder=<path>` on material-editor boot triggers a fetch + parse of `material.json` + `shader.wgsl` from the given folder (via the File System Access API path scene-editor's import flow already uses) and seeds `EditState` accordingly.

---

## Visual regression / CI tooling

### Headless screenshot harness

Manual browser verification is the current gate. For confidence in cross-platform rendering parity (and to catch "shader compiles but produces wrong output" failures), a headless screenshot harness would close the loop.

Shape:

- Headless Chrome with `--enable-unsafe-webgpu`.
- Drive each frontend's `task dev` target, wait for `phase = Ready`, capture canvas → PNG.
- Compare against a reference image with a per-pixel tolerance band.
- Wire into CI for the model-tests + material-editor preview canvas.

**Acceptance**: CI catches a visual regression in a known-good scene. Reference images live in the repo (or the awsm-renderer-assets sibling repo); diffs surface in PR checks.

Real infrastructure work — ~1-2 days, mostly diff-tolerance tuning + reference-image curation.

---

## Documentation polish

### `crates/renderer/README.md` walkthrough

The dynamic-materials public surface (`register_material`, `MaterialRegistration`, `Material::Custom`, etc.) doesn't have a top-level entry point in `crates/renderer/README.md`. The rustdoc + `crates/renderer/examples/dynamic_material.rs` cover the API; a README section makes it discoverable.

**Acceptance**: README section walks a fresh consumer through "register a dynamic material, build a `Material::Custom`, add a mesh that uses it, render." Cross-references `docs/dynamic-materials/contract-{opaque,transparent}.md`.

### Doc-link tail cleanup

`cargo doc --workspace --no-deps` runs warning-clean today (per Block E.6). Drive-by warning fixes belong here whenever a new one appears.

### Workspace-wide `-W missing_docs` gate

The `awsm-renderer::dynamic_materials` + `pipeline_scheduler` modules are missing-docs-clean today; the workspace-wide gate fails on pre-existing crates (scene-editor, gltf, etc.). Either:

- Keep the per-crate `#![warn(missing_docs)]` we already have on `awsm-materials` + `pipeline_scheduler` and add it incrementally as older crates clean up.
- Defer the workspace-wide version until those crates are docs-clean.

**Acceptance**: a CI step gates the public-API surface for missing-docs. Scope per the user's call.

---

## Beyond-the-plan optimizations

### Multithreading prep audit

`pipeline_scheduler` currently uses single-threaded `FuturesUnordered`. Audit for SharedArrayBuffer-readiness when wasm32-multithread lands. Document the boundaries.

### Test surface sweep

`wait_for_pipelines_ready()` test helper is in place; sweep `crates/renderer/tests/` and `crates/renderer/examples/` for sites that still rely on sync-insert-then-dispatch.

### Edge_resolve runtime profile sanity

At 1080p with the Fox scene, capture frame-time delta between the inline-path (pre-Stage-3) and the new edge_resolve path. Confirm parity or improvement. If a regression appears, investigate the per-frame `reset_header` cost vs classify's extra writes. Baseline lives at `docs/edge-resolve-baseline.md`.

### `MAX_EDGE_BUDGET` overflow atomic-add fallback

Currently the counter saturates and excess edges drop. Implement a small reserved accumulator region at the tail of `data_buffer` that overflow edges atomic-add into; final_blend reads it. ~50 lines of WGSL + 1 atomic counter. MVP diagnostic (boot-time log + one-shot `note_edge_overflow_observed` helper) ships today; behavior on pathological scenes is "drop edges silently, render with primary-sample shading" (degraded MSAA, not a crash). Operator-visible via `RUST_LOG=awsm_renderer::edge_resolve=info`.

### Build-time pipeline cache (parked)

Priority 4 from the old plan — parked waiting on Dawn pipeline-cache spec stabilization.

---

## Android device verification (out-of-scope for agent passes)

Requires a phone plugged in with `chrome://flags#enable-unsafe-webgpu`. Cannot be done from a developer machine.

- Plug in Android phone, run `task debug-mobile:chrome-check`.
- Confirm init reaches `phase = Ready` with no `VK_ERROR_INITIALIZATION_FAILED`. Capture boot-timing log for the eager batch — should show <500 ms total compile.
- Load a test scene with a PBR mesh. Skybox + camera UI within ~500 ms of Ready; PBR mesh within ~3 s; no watchdog kills; cross-material MSAA edges render correctly.
- Toggle MSAA off → on → off. Modal appears, scene recompiles, no driver rejection.
- Toggle bloom on. Bloom submits and resolves.
- Add a shadow-casting light. EVSM + ShadowGen submit and resolve.
- Register a dynamic material on desktop, save to project, load in scene-editor on Android. Dynamic material's pipelines compile on Android.
- Performance sanity: at 1080p with a moderate scene (~100k triangles, mixed materials), confirm 60 fps target is held.

---

## Cross-references

- Light culling design: [`light-culling.md`](light-culling.md).
- Material WGSL author contracts: [`../dynamic-materials/contract-opaque.md`](../dynamic-materials/contract-opaque.md), [`../dynamic-materials/contract-transparent.md`](../dynamic-materials/contract-transparent.md).
- Promotion walkthrough: [`../dynamic-materials/promotion.md`](../dynamic-materials/promotion.md).
- Integration example: [`../../crates/renderer/examples/dynamic_material.rs`](../../crates/renderer/examples/dynamic_material.rs).
