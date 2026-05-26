# Renderer remaining work (non-optimization)

## Purpose

The dynamic-materials PR (#98) is functionally complete and merging. Some loose ends remain. The cold-boot / pipeline-compile / lazy-pool ones live in [`more-optimizations.md`](more-optimizations.md). This file captures everything else — asset authoring, UI niceties, test scenes, docs polish, and pre-existing items from the dynamic-materials plan that don't fit under "optimization".

Items are grouped by area; each has a one-line acceptance criterion. None of these block any consumer of the dynamic-materials surface; they're all "would be nicer" / "would close out the verification story" items.

---

## Asset authoring (gates the end-to-end visual verification)

The renderer code path for each of these is fully implemented and unit-tested. What's missing is the on-disk `material.json` + `shader.wgsl` + (where applicable) `frames.bin` content.

### `irregular-atlas` test material

The first material that exercises the **`BufferSlot` + extras-pool** path end-to-end.

- Hand-author a `material.json` with one `BufferSlot { name: "frames" }`.
- Hand-author `frames.bin` (a packed `Vec<vec4<f32>>` of UV rects).
- Hand-author `shader.wgsl` that reads `input.material.frames_offset` / `..._length`, indexes the per-instance frame from the extras pool via `extras_load_vec4_f32`, and samples the atlas at the resulting UV rect.

**Acceptance**: a scene with two `irregular-atlas` instances at different `buffer_overrides` renders independently. Both run through `prewarm_dynamic_pipelines`, both hit the extras-pool with distinct slices, both visually animate the right cells of the atlas. Verifies Phase 6's `assign_or_update` + per-instance override flow live.

### `soft-glass` test material

The first **Blend** dynamic material in a real scene.

- Hand-author `material.json` with `alpha_mode: Blend`, `double_sided: true`, uniforms `[tint: Color3, edge_alpha: F32, face_alpha: F32]`.
- Author the WGSL fragment per the `contract-transparent.md` worked example (Schlick-style view-angle alpha tint, no opaque-background sampling).

**Acceptance**: a scene with a soft-glass sphere in front of an opaque ground plane renders correctly, sorted back-to-front against any first-party transparent material that happens to share the scene. Verifies the transparent template's `dispatch_hash` + `dynamic_shader_id` cache-key extensions live, against a real per-mesh transparent pipeline (not just the `prewarm_dynamic_pipelines` stub compile).

### Side-by-side test scene

One Phase-14 deliverable that's clearly content, not engineering:

- A test scene that places `scanline` (custom opaque), `soft-glass` (custom transparent), and the promoted `scanline` first-party material side-by-side.

**Acceptance**: visual diff between the dynamic + promoted scanlines is empty (byte-identical packer guarantees identical render). The transparent material composes correctly with the opaque ones. Drives the worked-example in `docs/dynamic-materials/promotion.md`.

---

## UI niceties (material-editor)

The material-editor app is functional today; these are polish items that improve the authoring experience without changing capability.

### Quad / sphere / box mesh selector

Today the preview canvas ships with a fixed 2×2 plane. Materials whose visual behavior depends on geometry (anything that reads `world_normal` or `world_tangent` non-trivially) want a curved surface. The plumbing is straightforward:

- Add a `preview_mesh: Mutable<PreviewMeshKind>` to `EditState`.
- Add a selector to the Preview pane's header.
- In `RendererHost::apply_quad_for_current_registration`, regenerate the stub mesh when the selector changes.

**Acceptance**: switching the preview mesh updates the live preview within one debounce cycle. The plane variant remains the default.

### Buffer Converter modal

The schema + data shapes are in place (`BufferSlot` carries a `default: Option<String>` that points at an asset filename). Today the material-editor doesn't have a UI for adding/replacing the buffer-default bytes; the user authors `frames.bin` etc. by hand.

**Acceptance**: a modal in the Definition pane that lets the user select a `.bin` / `.json` / drag-and-drop bytes, parse them per the slot's documented schema, and write to the project's `assets/materials/<name>/<slot>.bin`. Required when an `irregular-atlas`-shaped material lands in the scene-editor's Import Material flow.

### Error → cursor positioning

The `CompileError { line, column }` shape carries the data; the parser is in place. The remaining piece is a click handler on each entry in the Errors pane that calls `setSelectionRange` on the WGSL textarea at the right offset.

**Acceptance**: clicking a parsed error in the Errors pane positions the WGSL editor's caret at the reported line+column.

### File → New runnable stub

`EditState::new_scanline` seeds the scanline starter on boot. A `File → New` button that resets the state to a different starter (a minimal "constant red" material, an unlit baseline, etc.) would help new authors get started without copying from an existing one.

**Acceptance**: a New button in the top-bar offers ≥2 starter templates; selecting one wipes the EditState and triggers the debounced recompile.

---

## UI niceties (scene-editor)

### Open in material-editor deep-link

The "Open in material-editor" link in the Custom Materials pane today opens the material-editor at its root URL (config-driven base + `/?folder=<...>`). The material-editor doesn't currently consume the `folder` query param to pre-seed its `EditState`; the user lands on the scanline starter and has to re-import.

**Acceptance**: query-param `?folder=<path>` on material-editor boot triggers a fetch + parse of `material.json` + `shader.wgsl` from the given folder (via the File System Access API path the scene-editor's import flow already uses) and seeds `EditState` accordingly. Cross-port deep link is then end-to-end.

---

## Visual regression / CI tooling

### Headless screenshot harness

Manual browser verification is the current gate — `task material-editor:dev`, eyeball the canvas, check `task lint`. For confidence in cross-platform rendering parity (and to catch the kind of "shader compiles but produces wrong output" failure that the byte-identical promotion smoke test catches at the byte layer but not at the pixel layer), a headless screenshot harness would close the loop.

Realistic shape:

- Use a headless Chrome with `--enable-unsafe-webgpu`.
- Drive each frontend's `task dev` target, wait for `phase = Ready`, capture canvas → PNG.
- Compare against a reference image with a per-pixel tolerance band (GPU-driver-dependent rounding makes exact matches infeasible).
- Wire into CI for the model-tests + material-editor preview canvas.

**Acceptance**: CI catches a visual regression in a known-good scene. Reference images live in the repo (or the awsm-renderer-assets sibling repo); diffs surface in PR checks.

This is a real piece of infrastructure work — ~1-2 days, mostly the diff-tolerance tuning + reference-image curation.

---

## Documentation polish

### `crates/renderer/README.md` walkthrough

The dynamic-materials public surface (`register_material`, `MaterialRegistration`, `Material::Custom`, etc.) doesn't have a top-level entry point in `crates/renderer/README.md`. The rustdoc + `crates/renderer/examples/dynamic_material.rs` cover the API; a README section makes it discoverable.

**Acceptance**: README section walks a fresh consumer through the canonical "register a dynamic material, build a Material::Custom, add a mesh that uses it, render" sequence. Cross-references `docs/dynamic-materials/contract-{opaque,transparent}.md` for the WGSL contract.

### Doc-link tail cleanup

`cargo doc --workspace --no-deps` produces 47 warnings on this branch vs 51 on `main` (net improvement from the lazy-pool cleanup pass). Most remaining ones are pre-existing unresolved intra-doc-links in `render-worker`, `web-shared`, `renderer-gltf`. None are in the dynamic-materials surface. Resolve them as drive-by cleanup; not a blocker.

### Public API gate — `-W missing_docs`

Phase 14's tracking calls out `cargo clippy --workspace --all-targets -- -W missing_docs` as a documentation-coverage gate. The dynamic-materials module surface is missing-docs-clean today; the workspace-wide check fails on pre-existing crates (scene-editor, gltf, etc.). Either:

- Land the gate just for `awsm-materials` + `awsm-renderer::dynamic_materials` via per-crate `#![warn(missing_docs)]`.
- Defer the workspace-wide version until the older crates are docs-clean.

**Acceptance**: a CI step gates the public-API surface for missing-docs. Scope to be decided.

---

## Phase 6 extras-pool follow-ups

The extras-pool allocator's runtime path (`assign_or_update` → `slice_for` → `write_dirty_ranges`) is fully implemented and exercised. Two tail features remain on the Phase 6 tracking section:

### Pool resize on overflow

`assign_or_update` today returns `AwsmDynamicMaterialError::OutOfCapacity` when the bump allocator runs out of room. The 1 MiB initial allocation (`DEFAULT_CAPACITY_WORDS = 262_144`) handles every authored material we've shipped so far, but a real-game consumer registering many `BufferSlot`-heavy materials would hit it.

**Acceptance**: `assign_or_update` re-allocates the GPU buffer with `2× capacity` when needed (same growth strategy as the classify buffer), marks bind groups for recreation, and copies existing data via `copyBufferToBuffer` so live slices stay valid.

### Fragmentation-triggered compaction

Bump-allocator + per-instance overrides means a long session of edit → re-register cycles can fragment the pool (each old slice leaks). Today nothing reclaims those.

**Acceptance**: a free-list pass runs when fragmentation exceeds a threshold (e.g. >50% of allocated space unreferenced). Compacts surviving slices to the front via `copyBufferToBuffer`, updates the `slice_for` table, marks bind groups dirty. The unit-test surface is the same as resize (verify post-compaction reads return the right bytes).

Both are runtime-correctness items, not optimizations — current behavior is "errors out at capacity / leaks dead bytes," which is acceptable for the editor / authoring use case but not for shipping-game-runtime use.

---

## Cross-references

- Cold-boot / pipeline / shader / lazy-pool work: [`more-optimizations.md`](more-optimizations.md).
- Material WGSL author contracts: [`../dynamic-materials/contract-opaque.md`](../dynamic-materials/contract-opaque.md), [`../dynamic-materials/contract-transparent.md`](../dynamic-materials/contract-transparent.md).
- Promotion walkthrough (dynamic → first-party): [`../dynamic-materials/promotion.md`](../dynamic-materials/promotion.md).
- Integration example: [`../../crates/renderer/examples/dynamic_material.rs`](../../crates/renderer/examples/dynamic_material.rs).
