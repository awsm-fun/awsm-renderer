# Dynamic Materials Implementation Plan

## Instructions for the Implementor

This plan is meant to be followed **start to finish** in a single sustained effort.
The phases are ordered so each one leaves the renderer in a runnable (if visually-incomplete) state, but you should not try to ship intermediate phases as standalone PRs — there will be deliberate breaking changes along the way (the `MaterialShaderId` enum is rewritten as a `u32` newtype, the `Material` enum gains a `Custom` variant, the opaque-shading template substitution mechanism grows a second source of branches, etc.) and the goal is to keep the diff coherent rather than always shippable.

- **Commit frequently** at every natural checkpoint (e.g. after each phase, after each subsystem stands up green). Small commits make `git bisect` cheap when something regresses. Don't squash as you go.
- **Breaking changes are fine** mid-plan. If you need to change the shape of `MaterialShaderId`, the `Material` enum, the on-disk `project.json` schema, or the shader cache key, just do it — there's no migration story to preserve here yet. Update the test scene (`/Users/dakom/Documents/DAKOM/awsm-renderer-assets/world/project.json`) along with the change.
- **Update the tracking section at the bottom** as you go. Tick boxes when each item is done so a future session can resume cleanly if you stop mid-way.
- **Only after EVERYTHING below has landed and visually verified**, run:
  ```
  cargo fmt
  cargo clippy --workspace --all-targets
  ```
  Fix everything clippy turns up. Then the branch is ready to push.

### How to test

The primary verification surfaces are **two** browser apps:

1. **`material-editor`** — the new frontend crate this plan introduces. Start with:
   ```
   task material-editor:dev
   # served at http://localhost:9082 (pick the next free port)
   ```
   This is where you author and live-preview a custom material against a stub scene.

2. **`scene-editor`** — the existing app. Start with:
   ```
   task scene-editor:dev
   # served at http://localhost:9081
   ```
   This is where an imported custom material gets applied to a mesh in a real scene and verified under real lighting / shadows / etc.

Use the `preview_start` / `preview_screenshot` / `preview_snapshot` tools to drive each page in a Chromium preview. The renderer crate hot-reloads via Trunk's watch list, so editing renderer code and refreshing either preview is the fastest loop.

The test scene lives at `/Users/dakom/Documents/DAKOM/awsm-renderer-assets/world/project.json`. Extend it as you implement:

- Phase 4: a quad lit only by ambient, using a registered flowmap-style opaque dynamic material that scrolls a single texture's UVs by time. Visually confirms the opaque-compute injection path.
- Phase 6: a sphere with a registered soft-glass-style transparent dynamic material. Visually confirms the transparent-fragment injection path.
- Phase 9+: full scene with both a custom opaque and a custom transparent material, shadowed by the directional light from the shadows plan, under the standard PBR scene.

When testing, focus on:

1. **The golden path**: scene loads, dynamic materials render correctly, no GPU validation errors in the console.
2. **Authoring round-trip**: open a material in material-editor, edit a uniform default + the WGSL, save. Reopen — the values round-trip exactly. Switch to scene-editor, re-import, mesh still renders correctly.
3. **Hot recompile**: edit the WGSL in material-editor and save. The preview re-compiles (visible flash / log line). Introduce a syntax error — the editor shows the WGSL error inline / in the error console, the preview falls back to the last-good shader.
4. **Both alpha modes**: a material declared `alpha_mode: Opaque` routes to the compute kernel; declared `alpha_mode: Blend` routes to the transparent fragment shader. Switching alpha_mode in material-editor updates the contract-docs pane and the preview.
5. **First-party still works**: PBR, Unlit, Toon all render unchanged. Their shader-id constants survived the `MaterialShaderId` rewrite. Their shader cache keys don't depend on whether dynamic materials are registered (when none are).
6. **Promotion smoke test**: take the Phase-4 flowmap material's `material.json` + `shader.wgsl`, hand-port them to a first-party `materials/src/flowmap.rs` behind a Cargo feature (write a typed struct + manual `impl MaterialShader`). The visual output must be **bit-identical** to the dynamic version. The shader cache hash for that shader_id must match.

If you can't get something working through either editor, fall back to manually editing `project.json` and the material folder's `material.json`, but prefer the editors — that's also a smoke test for both UIs.

---

## Update (post material classify + indirect dispatch)

The opaque-shading compute kernel landscape has changed since this
plan was written. The material classify + indirect dispatch cluster
(see `docs/PERFORMANCE.md` §1 frame diagram + §15 row 6.1 in git
history) landed two related changes:

1. **Shader split.** PBR / Unlit / Toon each compile to their own
   specialized compute pipeline. The runtime `if (shader_id == X) {…}`
   dispatch chain in the opaque shader was replaced with an askama
   `{% match shader_id %}` template choice; only the matching
   material's WGSL fragment ends up in each pipeline.
2. **Material classify + indirect dispatch.** A new compute pass
   (`render_passes/material_classify/`) scans the visibility buffer
   per 8×8 tile and produces per-`MaterialShaderId` tile buckets +
   `dispatchWorkgroupsIndirect` args. Each pipeline now runs only
   over tiles its shader_id touches; mixed-material tiles are
   shaded by every pipeline whose shader_id is present (the
   per-pixel guard skips non-matching pixels).

This changes a few load-bearing assumptions of this plan. Read
together with the rest of the doc:

- **Each registered Custom material adds its own compute pipeline**,
  not a new branch in a shared dispatch chain. The classify-bucket
  count grows with the registered dynamic-material count; the
  classify shader gains a new bucket per `Custom` shader_id.
  Today's [`material_classify/buffers.rs`](../../crates/renderer/src/render_passes/material_classify/buffers.rs)
  uses a hard-coded `pub const BUCKET_COUNT: u32 = 3;` and the
  [`compute.wgsl`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl)
  routes via a fixed `BUCKET_BIT_PBR/UNLIT/TOON` if-else chain.
  Dynamic-materials registration must promote both of these into
  registry-driven values: `BUCKET_COUNT` becomes
  `enabled_materials().len() + registry.dynamic_entries.len()` and
  the WGSL bit chain is emitted from a template that walks the
  same source as `materials_wgsl`. This **must land alongside the
  registry plumbing in Phase 3** — without it the new dynamic
  shader_ids reach the opaque kernel but never get classified into
  the tiles they should shade.
- **`{{ shader_id_dispatch }}` is gone.** The template substitution
  site that hosted the dispatch chain is now `{% match shader_id %}`
  emitting exactly one material's shading body per pipeline. There
  is no shared first-party + dynamic dispatch site to "append" to.
- **`MaterialShaderId` newtype rewrite** (this plan's §"`MaterialShaderId`
  partitioning") is still load-bearing — classify uses it as the
  bucket key. The newtype shape goes from `enum { Pbr=1, Unlit=2,
  Toon=3 }` to `struct MaterialShaderId(u32)` with `PBR` / `UNLIT` /
  `TOON` consts + a `DYNAMIC_START = 10_000` reserved range.
- **Storage budget watch.** The opaque main bind group is at 9 of
  10 storage bindings after classify. Adding `extras_pool` (this
  plan's "Storage strategy") pushes it to 10/10 — the absolute cap.
  No headroom for further additions without an earlier pack. The
  cap is also enshrined in
  [`PERFORMANCE.md §11`](../PERFORMANCE.md) ("Don't bump
  `with_max_storage_buffers_per_shader_stage` past 10") — devices
  that exactly meet the declared limit fail pipeline validation if
  we exceed it.
- **Skybox ownership.** PBR pipeline retains the skybox-fallback
  block; non-PBR pipelines early-return on skybox without writing.
  A `Custom` opaque shader inherits the non-PBR rule by default —
  it must not write skybox tiles unless the material is explicitly
  registered as a skybox-owner (a future "skybox bucket"
  extension).
- **Per-frame upload path.** Every renderer-owned per-frame
  `queue.writeBuffer` site routes through `MappedUploader` now
  ([`PERFORMANCE.md §5b`](../PERFORMANCE.md)). The `extras_pool` is
  *renderer-owned per-frame writable* data (each `register_material`
  / instance change updates a slice), so it falls under that rule.
  See §"Extras pool" below for the corrected wiring.
- **Always-in-scope helpers.** The `frame_globals` uniform
  (`time` / `delta_time` / `frame_count` / `resolution`) ships
  bound alongside `camera` in every pass that does material shading
  — see [`TEMPORAL_SHADERS.md`](../TEMPORAL_SHADERS.md). Its
  `shared_wgsl/frame_globals.wgsl` is part of the always-in-scope
  helper set for both `contract-opaque.md` and
  `contract-transparent.md` and the contract docs should list it
  alongside the other shared helpers.

The rest of this plan still applies as the implementation brief
for everything else; the §"Storage strategy", §"Render-graph
slot", and §"Extras pool" sections are the only ones whose
details substantively shift. Treat the per-section text below as
**mostly correct, with the dispatch-chain references swapped for
per-pipeline match choices** at implementation time.

---

## High-Level Direction

We're adding a runtime registration path for **custom materials** to a visibility-buffer deferred renderer with a forward transparent pass. The motivating intent:

> Custom shaders should be authored as data (a `material.json` + a `shader.wgsl`) without requiring a fork of the `materials` crate. They register against the renderer at startup and route through exactly the same template-injection sites the first-party materials use. When a custom material proves itself, it gets **promoted** to first-party by porting the JSON layout to a typed Rust struct and the WGSL to a `&str` constant — no runtime change, no shader change, no GPU-layout change.

The renderer keeps **two** classes of material:

- **First-party (static, fast path).** PBR, Unlit, Toon. Declared in `crates/materials/src/`, feature-gated, compiled into `enabled_materials()`. Each has its own typed Rust struct + hand-rolled `impl MaterialShader` + `WGSL_FRAGMENT` constant. Static dispatch, exhaustive `Material` match arms for them.
- **Dynamic (runtime-registered).** One generic `DynamicMaterial` in `crates/materials/src/dynamic.rs` interprets a `MaterialDefinition` (data) from `scene-schema`. Registered against the renderer at app startup (or anytime before first frame; mid-frame registration is allowed but forces a shader recompile). The same trait, the same write-bytes contract, the same template-injection site.

The key architectural assertion is that the **public contract for `MaterialShader`** is the same surface both paths write against. First-party materials are not privileged in *capability* — they're privileged in *dispatch cost* (statically dispatched, branch known at compile time) and *type-safety* (Rust struct vs. opaque byte buffer driven by runtime layout).

### Render-graph slot

No new passes. Custom materials inject into the existing template-substitution sites:

```
geometry pass                  →  visibility / normal / depth targets
[shadow generation]            ← when shadows plan lands
light culling
opaque clear
material_opaque (compute)      →  contains `{{ materials_wgsl|safe }}` + dispatch chain
                                 ← inject custom opaque fragments + dispatch branches here
opaque mipgen (if transmissive)
blit opaque → transparent
material_transparent           →  contains material fragment substitution + dispatch
                                 ← inject custom transparent fragments + dispatch branches here
display
```

The opaque-shading compute kernel and the transparent fragment shader both use Askama templates. Each has a `{{ materials_wgsl|safe }}` (or equivalent) substitution slot above a `{{ shader_id_dispatch|safe }}` chain. First-party materials feed both substitutions today via `enabled_materials()`. The plan extends each substitution to **also** consume the renderer's runtime dynamic-material registry. When that registry is empty, the compiled WGSL is bit-identical to what's produced today — important for the guarantee that first-party-only consumers pay nothing for this feature.

### On-disk format

A custom material is a **folder**, not a single file. Self-contained, portable, importable:

```
flipbook/
├── material.json
├── shader.wgsl
└── assets/
    └── sprite-sheet.png
```

`material-editor` exports a folder of this shape. `scene-editor` imports a folder into a project, copying it under `assets/materials/<name>/`:

```
my-game/
├── project.json
└── assets/
    ├── model.glb
    └── materials/
        └── flipbook/
            ├── material.json
            ├── shader.wgsl
            └── assets/
                └── sprite-sheet.png
```

The convention is unconditional — the WGSL file is **always** `shader.wgsl` inside the folder. The schema doesn't carry a path field.

### The author's contract (public surface)

This is the load-bearing public surface of this plan. Whatever shape it takes after the Phase-1 audit is the **stable** contract — both for custom-material authors AND for the first-party PBR/Unlit/Toon refactor that audits against it. Promotion stays mechanical only as long as the contract doesn't churn underneath.

The contract differs by `alpha_mode`:

- `Opaque` | `Mask { cutoff }` → the WGSL fragment is a function injected into the **opaque-shading compute kernel** at `{{ materials_wgsl|safe }}`. It is called from the kernel's per-pixel dispatch when the kernel decodes a visibility-buffer sample whose material has this material's `shader_id`.
- `Blend` → the WGSL fragment is a function injected into the **forward transparent fragment shader**. It is called from the fragment's dispatch when the fragment runs for a transparent draw whose material has this `shader_id`.

Both contracts share these guarantees:

- The author's WGSL fragment is preceded by **all helpers in `shared_wgsl/`**: `math`, `color_space`, `textures`, `transforms`, `camera`, `material_mesh_meta`, `lighting/brdf`, `lighting/lights`, `lighting/unlit`, `shadow/bind_groups` (when shadows are enabled). Any symbol declared in those files is callable from a custom fragment. Do not redefine symbols from those files.
- The texture pool is bound and accessible. A `TextureSlot` named e.g. `"flow_map"` becomes a `flow_map_index: u32` in the material's WGSL uniform struct; the author samples via the existing texture-pool helpers using that index.
- Per-material uniform data lives in a storage / uniform buffer at an offset known to the kernel; the author's WGSL fragment receives it as a typed struct of the layout they declared in `material.json`.
- Output: whatever the existing first-party materials of the same `alpha_mode` already produce. The Phase 1 audit locks this down precisely — `shading_result` shape, exact field names, what's already converted to HDR vs. linear, etc.

These contracts are documented in **`docs/dynamic-materials/contract-opaque.md`** and **`docs/dynamic-materials/contract-transparent.md`**. Phase 1 produces these files; later phases keep them in sync.

### Storage strategy

- **Static dispatch chain (first-party).** Unchanged. Generated from `enabled_materials()` at template-render time. Static `if shader_id == PBR { … }` branches. Bit-identical to today.
- **Dynamic dispatch chain (custom).** Appended after the static chain at template-render time, from a snapshot of the renderer's dynamic registry. Same shape (`if shader_id == NNNN { … }`), just from a runtime list rather than a Cargo-feature list.
- **Per-material data buffer.** The existing storage buffer pattern (each material packs into bytes via `write_uniform_buffer`, indexed by `(shader_id, byte_offset)`) is unchanged. `DynamicMaterial::write_uniform_buffer` walks its `MaterialDefinition.uniforms` in declaration order, respecting WGSL alignment, then appends `u32` texture-pool indices for each `TextureSlot`, then appends `(offset, length)` u32 pairs for each `BufferSlot` (see below). The kernel reads via the same byte_offset mechanism — it doesn't care that the bytes came from a generic packer.
- **Extras pool (for variable-length data per material).** A new renderer-wide `extras_pool: array<u32>` storage buffer mirrors the existing `materials: array<u32>` pool. Each declared `BufferSlot` on a dynamic material gets a contiguous slice in this pool; the per-material data buffer carries `(offset, length)` indices that the author reads as `extras_load_f32(material.<slot>_offset + i)` / `extras_load_u32(...)` — the same bitcast convention `material_load_f32` / `material_load_u32` already establish for the materials pool. One shared binding regardless of how many dynamic materials register or how many buffer slots they each declare; per-material slices are managed by a free-list/bump allocator in the renderer. Data on disk is always `.bin` (raw little-endian u32 words); a converter tool in material-editor produces `.bin` from human-readable JSON arrays.
- **Shader cache.** The opaque compute kernel's cache key is currently `hash(enabled_materials())`. It becomes `hash(enabled_materials(), dynamic_registry_snapshot)` where `dynamic_registry_snapshot` is a stable hash of `[(shader_id, wgsl_source, layout)]` for all currently-registered dynamic materials. Same for the transparent fragment cache key. When the dynamic set changes, the cache key changes, and next use triggers recompile. **When no dynamic materials are registered, the cache key matches today's exactly.**

### `MaterialShaderId` partitioning

Today this is `#[repr(u32)] enum { Pbr = 1, Unlit = 2, Toon = 3 }`. That shape can't extend to runtime values. It becomes a `#[repr(transparent)] struct MaterialShaderId(u32)` with associated constants for first-party and a documented dynamic range:

```rust
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MaterialShaderId(u32);

impl MaterialShaderId {
    pub const PBR:   Self = Self(1);
    pub const UNLIT: Self = Self(2);
    pub const TOON:  Self = Self(3);
    // 4..=9999 reserved for future first-party materials.

    pub const DYNAMIC_START: u32 = 10_000;

    pub fn is_dynamic(self) -> bool { self.0 >= Self::DYNAMIC_START }
    pub fn as_u32(self) -> u32 { self.0 }

    /// SAFETY-free constructor for the renderer's dynamic allocator only.
    /// Game code never builds these directly.
    pub(crate) fn from_raw(raw: u32) -> Self { Self(raw) }
}
```

GPU representation unchanged — still a `u32`. Pattern matches on the old enum become `if id == MaterialShaderId::PBR { … }`. The dynamic range gives effectively unlimited room.

### Why these choices

- **Folder format over single-file.** WGSL is meaningfully large per material (often 50–300 lines), embedding it in JSON makes it unreadable and hostile to source control. A folder is git-diffable, editor-friendly, and lets the material own its texture assets without an external manifest.
- **One generic `DynamicMaterial`, not `Box<dyn ...>` per-material.** The author already produces all the per-material customisation as data (layout + WGSL); a typed Rust struct per dynamic material would be redundant. One generic interpreter avoids `Box<dyn MaterialShader>` overhead and keeps the static `Material` enum closed (only adds a single `Custom` variant).
- **Dispatch chain over indirection.** The existing pattern is already a `shader_id` dispatch table compiled into the WGSL. Extending it is the lowest-friction integration — no new pipelines, no indirection, no perf cost on the first-party path. Cost on the dynamic path is one extra `if` branch per registered material, which is well within the budget for an "experimentation" feature.
- **Both passes in v1, opaque-first in implementation order.** The transparent path is the same template-injection shape as opaque, just with a different signature in the contract. Doing both ensures the contract is forced to generalize. Implementing opaque-first means the contract bugs get debugged in isolation; transparent then comes online following the proven shape.
- **Material folder is `scene-schema`'s problem.** No new shared crate. `MaterialDefinition` is data, lives next to `MaterialDef` in `scene-schema/src/`, gets the same back-compat serde discipline as everything else there. Third-party scene players that deserialize a project also get custom-material deserialisation for free.
- **`material-editor` is a separate frontend crate.** Pulling CodeMirror / Monaco into `scene-editor` would meaningfully grow its bundle for a feature most scenes don't author. Separate crate keeps the scene-editor lean and creates a natural place for a future node-graph authoring frontend (which would generate `shader.wgsl` from a graph, but otherwise produce the same on-disk format).
- **Browser tabs for isolation.** Rather than building a second app with a stub scene, the user opens material-editor in a new browser tab when they want isolated iteration. Same code path; the OS provides the isolation for free.

### True non-goals

These are not in v1 and are not deferred — they're genuinely the wrong fit for this iteration.

- **Node-graph authoring.** A visual graph (à la Unity Shader Graph, Blender shader nodes) is a long-arc product, not an experimentation tool. The data model leaves room for one — the `wgsl_fragment` field is an opaque string; a future node-graph frontend would emit into the same field — but building one now is premature.
- **GLSL input.** The renderer is WGPU/WGSL. Translating GLSL via naga is mechanically possible but adds a second mental model the contract docs would have to cover. WGSL only.
- **Custom render passes / non-shading compute jobs.** Dynamic materials inject into the *material shading* slot of the opaque kernel and the transparent fragment. They do not let authors add their own bind groups, their own pipeline stages, or their own compute dispatches. That's a separate feature ("plugins for the render graph") that's out of scope here.
- **Materials that switch `alpha_mode` at runtime.** A material is one alpha_mode. Want both? Author two materials. Matches first-party convention.
- **Material inheritance / variants.** No inheritance, no parameter override of one material from another. Two materials sharing 80% of their WGSL just share it via copy-paste for now. A factoring mechanism (shared `.wgsl` modules importable from a custom material's `shader.wgsl`) is plausible future work but not v1.
- **Hot-reload via filesystem watch.** Save-driven recompile (user hits Ctrl-S, recompile fires) is the v1 UX. A filesystem watcher is convenient but not load-bearing.
- **Tagged-type buffer data formats (e.g. `[1.5, {"u32": 5}, 2.5]`).** The Buffer Converter accepts a flat (or nested-then-flattened) JSON array of numbers, each written as 4 bytes of `f32`. Authors who want u32 semantics either value-cast in WGSL (`u32(extras_load_f32(i))` — lossless up to 2^24) or true-bitcast (`bitcast<u32>(extras_load_f32(i))`). If a real consumer hits the 2^24 ceiling, tagged-type syntax can be added as a focused follow-up; not in v1.
- **Buffer data formats other than `.bin`.** No JSON-on-disk for buffers, no PNG-as-buffer, no in-place editing of buffer contents in scene-editor. The renderer reads `.bin` only; material-editor authors `.bin` via the Buffer Converter. One format end-to-end.

---

## Editor UX

Two editors. `material-editor` authors custom materials standalone. `scene-editor` imports and applies them.

### `material-editor` (new app)

Single-window app with the following panes:

**Top bar**
- File: New / Open Folder… / Save / Save As…
- Preview mesh selector: `Quad` / `Sphere` / `Box` / `Custom glTF…` (loads any local glTF for preview)
- Recompile button (manual trigger; Ctrl-S also recompiles)
- Tools: **Buffer Converter…** — opens a modal that converts a JSON array of numbers (e.g. `[1, 2.5, 3, 4.0]` or nested arrays for readability) into a `.bin` file and downloads it. The author drops the resulting file into their material folder's `assets/` and points a buffer slot at it. See "Buffer Converter" below.

**Left pane — Definition**
- `Name` — string, must be a valid folder name (kebab-case enforced).
- `Version` — integer, manually bumped by the author when they ship a breaking layout change.
- `Alpha mode` — segmented toggle: `Opaque` / `Mask` / `Blend`. When `Mask` is selected, a `Cutoff` slider appears (0.0–1.0, default 0.5).
- `Double-sided` — bool toggle.
- `Uniforms` — table of `(name, type, default)` rows. Types: `F32`, `Vec2`, `Vec3`, `Vec4`, `U32`, `IVec2`, `IVec3`, `IVec4`, `Mat3`, `Mat4`, `Color3` (vec3 with color-picker UI), `Color4` (vec4 with color-picker UI), `Bool` (becomes `u32` 0/1 in WGSL). `+` button adds a row; rows are reorderable; `−` deletes. Editing a row's type updates the WGSL struct preview pane immediately.
- `Textures` — table of `(name, default-asset)` rows. Each row picks a texture asset (PNG / KTX2) from a local file dialog; the asset gets copied into the material folder's `assets/` on save. Reorderable, deletable. Note that `name` becomes `<name>_index: u32` in the material's WGSL struct (the index into the texture pool).
- `Buffers` — table of `(name, default-asset)` rows. Each row picks a `.bin` file from the material folder's `assets/` (or from anywhere via file dialog, in which case it's copied in on save). Reorderable, deletable. `name` becomes two fields in the material's WGSL struct: `<name>_offset: u32` and `<name>_length: u32` — indices into the renderer-wide `extras_pool: array<u32>` storage buffer. The author reads via `extras_load_f32(material.<name>_offset + i)` or `extras_load_u32(...)`. Hand-authored buffer data is produced via the **Buffer Converter** tool (see Top bar).

**Buffer Converter (modal)**
- A textarea accepting a JSON array of numbers. Nested arrays are flattened (e.g. `[[1,2,3,4], [5,6,7,8]]` is treated as 8 sequential values — useful for hand-authoring tabular data with one row per line).
- Filename input (e.g. `frames.bin`).
- Download button: parses the textarea as JSON, recursively flattens to `Vec<f32>`, writes each value as 4 bytes little-endian, triggers a browser download.
- Error display for parse failures or non-numeric values.
- An explanatory note documenting the format convention: "All numbers are written as 32-bit floats. In WGSL, read floats via `extras_load_f32(idx)`; for small integers (< 2^24), value-cast with `u32(extras_load_f32(idx))`."

**Center pane — WGSL editor**
- CodeMirror 6 (or whichever WASM-compatible editor you choose) with WGSL syntax highlighting.
- An auto-generated read-only preview at the top showing the `struct MaterialData { … }` declaration the renderer will inject above the author's fragment, derived from the current uniform / texture layout. Updates live as the author edits the Definition pane.
- Below it, the author's WGSL function body. The cursor starts inside a stub function whose signature matches the current `alpha_mode`'s contract.

**Right pane — Contract**
- Read-only documentation pane that shows the active contract for the current `alpha_mode`:
  - Helpers in scope (with anchor links into the rendered contract docs)
  - Function signature the author's WGSL fragment must match
  - Output struct shape

The pane swaps contents when `alpha_mode` switches between `Opaque|Mask` and `Blend`. This is the same surface as `docs/dynamic-materials/contract-{opaque,transparent}.md` — same source, rendered inline.

**Bottom pane — Preview + Errors**
- Left half: 3D preview viewport rendering the selected preview mesh under a default 3-point lighting rig + a ground plane. Updates immediately on recompile.
- Right half: Error console. WGSL compile errors with line/column from naga. Inline gutter markers in the WGSL pane when error positions are available.

**Recompile behavior**
- On save (Ctrl-S or File → Save) and on explicit Recompile button.
- A failed compile keeps the preview running on the **last-good** shader and surfaces the error. The author can keep editing.
- Recompile takes ~50–500ms; show a spinner overlay on the preview while it's pending.

### `scene-editor` (existing app)

Two surface additions:

**Project pane — "Materials" section**
- Lists all custom materials currently imported into the project. Each row: name + an "Open in material-editor" link (opens a new browser tab to the material-editor URL with a query param identifying the folder).
- `Import Material…` button: file-picker for a folder. Copies the folder into the project's `assets/materials/<name>/`, adds an entry to `project.json::custom_materials`, and registers it with the renderer immediately.
- `Remove Material` per row: confirms, then de-references it from `project.json` and removes the folder. (Renderer recompiles the dispatch chain.)

**Per-mesh material editor**
- The existing material-picker dropdown (currently shows PBR / Unlit / Toon variants) gains entries for every imported custom material under a "Custom" sub-section. Picking a custom material populates the per-mesh material instance with the layout's defaults; per-instance values become editable in the property panel using the same UI primitives material-editor uses for uniform defaults (drag floats, color pickers, asset references for textures).

No changes to the lighting / camera / scene-tree UI. Custom materials participate in lighting / shadows / etc. on equal footing with first-party ones (because they execute inside the same opaque kernel / transparent fragment).

---

## Schema Changes

### `crates/scene-schema/src/material.rs` (or new `dynamic_material.rs`)

```rust
/// On-disk shape of a custom material. Lives in `material.json` at the
/// root of a material folder. Companion `shader.wgsl` and `assets/` are
/// loaded separately by the folder loader.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MaterialDefinition {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub alpha_mode: MaterialAlphaMode,        // reuses existing enum
    #[serde(default)]
    pub double_sided: bool,
    #[serde(default)]
    pub uniforms: Vec<UniformField>,
    #[serde(default)]
    pub textures: Vec<TextureSlot>,
    #[serde(default)]
    pub buffers: Vec<BufferSlot>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UniformField {
    pub name: String,                          // becomes the field name in WGSL
    pub ty: FieldType,
    pub default: UniformValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    F32, Vec2, Vec3, Vec4,
    U32, IVec2, IVec3, IVec4,
    Mat3, Mat4,
    Color3,                                    // vec3 with color-picker UI
    Color4,                                    // vec4 with color-picker UI
    Bool,                                      // becomes u32 in WGSL
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum UniformValue {
    F32(f32),
    Vec2([f32; 2]), Vec3([f32; 3]), Vec4([f32; 4]),
    U32(u32),
    IVec2([i32; 2]), IVec3([i32; 3]), IVec4([i32; 4]),
    Mat3([f32; 9]), Mat4([f32; 16]),
    Color3([f32; 3]), Color4([f32; 4]),
    Bool(bool),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TextureSlot {
    /// Becomes `<name>_index: u32` in the material's WGSL uniform struct.
    pub name: String,
    /// Path relative to the material folder root (typically inside `assets/`).
    /// Optional — slots without a default require a binding at instance time.
    #[serde(default)]
    pub default: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BufferSlot {
    /// Becomes `<name>_offset: u32` and `<name>_length: u32` in the
    /// material's WGSL uniform struct. The author reads via
    /// `extras_load_f32(material.<name>_offset + i)` etc.
    pub name: String,
    /// Path to a `.bin` file (raw little-endian u32 words) relative to the
    /// material folder root, typically inside `assets/`. Optional — slots
    /// without a default require a binding at instance time.
    #[serde(default)]
    pub default: Option<PathBuf>,
}
```

### `crates/scene-schema/src/project.rs` (or wherever the project root lives)

Add a `custom_materials` field to the project root: a list of `(name, folder_path)` pointers into `assets/materials/`. `name` matches the folder's `material.json::name` (cross-check on load); `folder_path` is project-relative.

```rust
#[serde(default)]
pub custom_materials: Vec<CustomMaterialRef>,

pub struct CustomMaterialRef {
    pub name: String,
    pub folder: PathBuf,                       // e.g. "assets/materials/flipbook"
}
```

### Per-mesh material reference

Wherever a mesh today carries a material selection (likely a tagged enum like `Material::Pbr(...)` / `Unlit(...)` / `Toon(...)` in `scene-schema/src/material.rs`), add a `Custom { name: String, values: HashMap<String, UniformValue>, textures: HashMap<String, TextureRef> }` variant. `name` matches a `CustomMaterialRef::name`; `values` / `textures` carry per-instance overrides of the layout's defaults.

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MaterialRef {
    Pbr(PbrMaterialDef),
    Unlit(UnlitMaterialDef),
    Toon(ToonMaterialDef),
    Custom(CustomMaterialInstance),
}

pub struct CustomMaterialInstance {
    pub material: String,                      // matches CustomMaterialRef::name
    #[serde(default)]
    pub uniform_overrides: HashMap<String, UniformValue>,
    #[serde(default)]
    pub texture_overrides: HashMap<String, TextureRef>,
    #[serde(default)]
    pub buffer_overrides: HashMap<String, BufferRef>,
}

/// Thin newtype mirroring `TextureRef`. Points at a `.bin` file relative to
/// the project root. Future extensibility (e.g. format hints, sub-ranges)
/// stays additive.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BufferRef {
    pub path: PathBuf,
}
```

All new fields use `#[serde(default)]` so old projects round-trip cleanly without `custom_materials`.

### Folder loader

`crates/scene-schema/src/dynamic_material.rs` (or wherever fits) exposes:

```rust
pub struct LoadedMaterialFolder {
    pub definition: MaterialDefinition,
    pub wgsl_source: String,
    pub texture_data: HashMap<PathBuf, Vec<u8>>,  // resolved texture file contents
    pub buffer_data: HashMap<PathBuf, Vec<u32>>,  // resolved .bin file contents (validated u32-aligned)
}

pub fn load_material_folder(root: &Path) -> Result<LoadedMaterialFolder, MaterialFolderError>;
```

`MaterialFolderError` covers: `material.json` missing/invalid, `shader.wgsl` missing, a `TextureSlot::default` pointing to a nonexistent file, a `BufferSlot::default` pointing to a nonexistent file, a `.bin` file whose size is not a multiple of 4, layout name collisions, reserved names (e.g. `material`, `texture_pool`, `extras_pool`).

This loader is the **only** schema-side logic. It's used by both `material-editor` (loading the current edit), `scene-editor` (loading on project import), and any third-party scene player (loading on project init). The renderer doesn't depend on `scene-schema`; the bridge code in each consumer converts `LoadedMaterialFolder` → renderer-side `MaterialRegistration` (defined below).

---

## Public API Surface

The `awsm-renderer` crate is a library; `scene-editor` and `material-editor` are two consumers, but a game runtime / model-tests frontend / standalone tool must also be able to register a custom material without reverse-engineering either editor. The API below is the contract — implementors must keep it stable across phases, document every public item with rustdoc, and ensure a non-editor consumer can register a dynamic material end-to-end using only `pub` symbols from `awsm-renderer` + `awsm-renderer-materials`.

### Design principles

- **Mirror existing material patterns.** The `MaterialShader` trait is unchanged in shape; the `Material` enum gains a `Custom` variant. Registration goes through one method on `AwsmRenderer`.
- **One way to do each thing.** Registration is one call. Updating an instance's uniform values is the same path first-party materials use (write through the existing storage buffer).
- **Schema vs. runtime separation.** `scene-schema::MaterialDefinition` is the on-disk format. The renderer takes its own `MaterialRegistration` (essentially the same data plus the loaded WGSL string). The consumer converts; the renderer never depends on `scene-schema`.
- **Lazy, dirty-flag-driven shader compile.** Registration marks the dispatch dirty; the next `render()` call regenerates the cache key and recompiles if needed. No synchronous compile.
- **Errors via a single `AwsmDynamicMaterialError` enum.** All fallible methods return `Result<T, AwsmDynamicMaterialError>`; this enum flows into `AwsmError` like the other subsystem errors.
- **Every public item has a rustdoc comment.** Type-level doc explains what it represents; method-level doc explains effect, when it takes effect, and when it can fail. Examples for non-obvious methods.

### Types (`awsm_renderer_materials::dynamic` + `awsm_renderer`)

```rust
/// Runtime registration payload for a custom material. The renderer's
/// counterpart to `scene_schema::MaterialDefinition` + the loaded WGSL.
/// Consumers convert from the schema; the renderer does not depend on
/// scene-schema.
#[derive(Clone, Debug)]
pub struct MaterialRegistration {
    pub name: String,
    pub alpha_mode: MaterialAlphaMode,
    pub double_sided: bool,
    pub layout: MaterialLayout,
    pub wgsl_fragment: String,
}

/// The uniform + texture + buffer layout of a registered material. Drives:
/// 1. The generated WGSL struct declaration injected above the fragment.
/// 2. The byte packing in `DynamicMaterial::write_uniform_buffer`.
/// 3. The property-editor UI in material-editor / scene-editor.
#[derive(Clone, Debug)]
pub struct MaterialLayout {
    pub uniforms: Vec<UniformFieldRuntime>,
    pub textures: Vec<TextureSlotRuntime>,
    pub buffers: Vec<BufferSlotRuntime>,
}

pub struct UniformFieldRuntime { pub name: String, pub ty: FieldType, pub default: UniformValue }
pub struct TextureSlotRuntime { pub name: String, pub default: Option<TextureKey> }
pub struct BufferSlotRuntime { pub name: String, pub default: Option<Vec<u32>> }

/// One generic dynamic-material implementation. All registered materials
/// share this type — what differs per material is the `layout` and
/// `wgsl_fragment` reachable via the assigned `shader_id`.
pub struct DynamicMaterial {
    pub shader_id: MaterialShaderId,           // assigned at registration
    pub values: Vec<UniformValue>,             // current per-instance values
    pub textures: Vec<Option<TextureKey>>,     // per texture slot
    pub buffers: Vec<Option<Vec<u32>>>,        // per buffer slot; raw u32 words
}

impl MaterialShader for DynamicMaterial { /* see below */ }
```

### Methods on `AwsmRenderer`

```rust
impl AwsmRenderer {
    /// Registers a custom material. Returns an opaque `MaterialShaderId` in the
    /// dynamic range (>= MaterialShaderId::DYNAMIC_START). Takes effect on the
    /// next `render()` call (the shader cache key changes; the affected pipeline
    /// recompiles on first dispatch).
    ///
    /// Idempotent on `(name, layout_hash, wgsl_hash)`: re-registering the same
    /// material returns the same id without recompiling.
    pub fn register_material(
        &mut self,
        registration: MaterialRegistration,
    ) -> Result<MaterialShaderId, AwsmDynamicMaterialError>;

    /// Removes a previously-registered dynamic material. Returns an error if
    /// any live mesh still references it. Triggers a shader recompile on next
    /// render.
    pub fn unregister_material(
        &mut self,
        shader_id: MaterialShaderId,
    ) -> Result<(), AwsmDynamicMaterialError>;

    /// Returns the registration record for a previously-registered id.
    pub fn dynamic_material_registration(
        &self,
        shader_id: MaterialShaderId,
    ) -> Option<&MaterialRegistration>;

    /// Iterator over all currently-registered dynamic materials. Useful for
    /// the editor when listing what's available to assign to a mesh.
    pub fn dynamic_materials(&self) -> impl Iterator<Item = (MaterialShaderId, &MaterialRegistration)>;
}
```

### Error type

```rust
#[derive(thiserror::Error, Debug)]
pub enum AwsmDynamicMaterialError {
    #[error("[dynamic-material] duplicate name `{0}` already registered")]
    DuplicateName(String),
    #[error("[dynamic-material] unknown shader id {0:?}")]
    UnknownShaderId(MaterialShaderId),
    #[error("[dynamic-material] cannot unregister `{name}`: {instance_count} live instances")]
    InUse { name: String, instance_count: usize },
    #[error("[dynamic-material] reserved field name `{0}` (collides with kernel-provided symbol)")]
    ReservedName(String),
    #[error("[dynamic-material] WGSL compile failed: {0}")]
    WgslCompile(String),
    #[error("[dynamic-material] {0}")]
    Core(#[from] awsm_renderer_core::error::AwsmCoreError),
}
```

Added to top-level `AwsmError` like the other subsystem errors.

### Minimal integration example (game runtime, no editor)

This is the smallest end-to-end snippet that should compile against the public API. Include it verbatim as a rustdoc example on `register_material` or in `crates/renderer/examples/dynamic_material.rs`.

```rust
use awsm_renderer::AwsmRenderer;
use awsm_renderer_materials::{
    dynamic::{MaterialRegistration, MaterialLayout, UniformFieldRuntime, FieldType, UniformValue},
    alpha_mode::MaterialAlphaMode,
};

// 1. Build a registration (this is what a game would normally load from a
//    material folder via the scene-schema folder loader; here we hand-build
//    the equivalent).
let reg = MaterialRegistration {
    name: "flowmap".into(),
    alpha_mode: MaterialAlphaMode::Opaque,
    double_sided: false,
    layout: MaterialLayout {
        uniforms: vec![
            UniformFieldRuntime { name: "speed".into(), ty: FieldType::F32, default: UniformValue::F32(0.5) },
            UniformFieldRuntime { name: "tint".into(),  ty: FieldType::Color3, default: UniformValue::Color3([1.0, 1.0, 1.0]) },
        ],
        textures: vec![ /* TextureSlotRuntime { name: "flow".into(), default: None } */ ],
    },
    wgsl_fragment: include_str!("../shaders/flowmap.wgsl").into(),
};

// 2. Register it. Returns a stable id usable to assign instances.
let shader_id = renderer.register_material(reg)?;

// 3. Render as usual; on first frame after registration the opaque kernel
//    recompiles to include the new dispatch branch.
renderer.render(None)?;
```

### Documentation requirements

For every phase that introduces or modifies a public-API item, the implementor MUST:

1. **Add a rustdoc comment** to every new `pub` type, `pub` field, `pub` method, `pub` enum variant. Comments answer: what is this, when does it take effect, what can go wrong.
2. **Run `cargo doc --workspace --no-deps`** at the end of each phase that touches the API. Fix any broken intra-doc links.
3. **Update the integration example** in `crates/renderer/examples/dynamic_material.rs` so it reflects the current shape of the API as it grows.
4. **Update `docs/dynamic-materials/contract-opaque.md` and `docs/dynamic-materials/contract-transparent.md`** whenever a helper signature, an injection-site convention, or a kernel-provided symbol changes.
5. **Run `cargo clippy --workspace -- -W missing_docs`** as a periodic check. This should be **clean at Phase 13** even if intermediate phases haven't caught up.

The "Public API gate" tracking checkboxes at the bottom gate the final ship.

---

## Renderer / Materials Changes

### New module: `crates/materials/src/dynamic.rs`

The generic `DynamicMaterial` interpreter:

```rust
pub struct DynamicMaterial { … }              // shape above

impl MaterialShader for DynamicMaterial {
    fn shader_id(&self) -> MaterialShaderId { self.shader_id }
    fn alpha_mode(&self) -> MaterialAlphaMode { /* from registration */ }
    fn wgsl_fragment(&self) -> &str { /* from registration, looked up by id */ }
    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, out: &mut Vec<u8>) {
        // walks layout.uniforms in declared order, packs each `UniformValue`
        // respecting WGSL alignment rules (see crates/materials/src/dynamic_layout.rs)
        // then appends one u32 per texture slot: ctx.resolve_texture_index(self.textures[i])
    }
}
```

### New module: `crates/materials/src/dynamic_layout.rs`

The shared WGSL-alignment-and-packing helper. Two outputs from one source of truth (the `MaterialLayout`):

```rust
/// Generate the WGSL struct declaration that goes above the author's fragment,
/// e.g. `struct MaterialData { speed: f32, tint: vec3<f32>, flow_index: u32, }`.
/// Respects WGSL alignment (vec3 → 16-byte align, etc.) and inserts padding
/// fields where needed.
pub fn generate_wgsl_struct(struct_name: &str, layout: &MaterialLayout) -> String;

/// Pack a uniform value into a byte buffer at the correct WGSL-aligned offset.
/// Walks the layout in declared order. The texture-index tail is appended by
/// the caller via `pack_texture_indices`.
pub fn pack_uniform_values(layout: &MaterialLayout, values: &[UniformValue], out: &mut Vec<u8>);

pub fn pack_texture_indices(layout: &MaterialLayout, indices: &[u32], out: &mut Vec<u8>);

/// Total size (with tail padding) — useful for size-of checks and for the
/// per-material byte_offset table.
pub fn layout_size(layout: &MaterialLayout) -> usize;
```

Unit tests in this module are load-bearing: they verify the generated struct and the packed bytes agree exactly with the WGSL spec for representative layouts (every `FieldType`, plus mixed-alignment cases). When alignment math is wrong, materials silently render garbage — these tests are the first line of defense.

### `crates/materials/src/registry.rs` — dual-mode registry

Today `enabled_materials() -> Vec<MaterialEntry>` returns a Cargo-feature-driven static list. Becomes:

```rust
pub struct MaterialRegistry {
    static_entries: Vec<MaterialEntry>,        // from enabled_materials(), unchanged
    dynamic_entries: SecondaryMap<MaterialShaderId, DynamicEntry>,
    next_dynamic_id: u32,
}

pub struct DynamicEntry {
    pub registration: MaterialRegistration,
    pub layout_hash: u64,
    pub wgsl_hash: u64,
}

impl MaterialRegistry {
    pub fn new() -> Self;                      // populates from enabled_materials()
    pub fn register(&mut self, reg: MaterialRegistration) -> Result<MaterialShaderId, AwsmDynamicMaterialError>;
    pub fn unregister(&mut self, id: MaterialShaderId) -> Result<(), AwsmDynamicMaterialError>;
    pub fn all_entries(&self) -> Vec<RegistryEntry<'_>>;   // static followed by dynamic
    pub fn dispatch_hash(&self) -> u64;        // for shader cache invalidation
}
```

### `Material` enum gains `Custom`

```rust
pub enum Material {
    Pbr(Box<PbrMaterial>),
    Unlit(UnlitMaterial),
    Toon(Box<ToonMaterial>),
    Custom(Box<DynamicMaterial>),              // new
}
```

Every pattern-match against `Material` becomes non-exhaustive in the same release; add the `Custom` arm to each. The renderer's per-frame material packing dispatches on the variant the same way it does today.

### `crates/renderer/src/dynamic_materials/` — new module

```
dynamic_materials/
  mod.rs                  ← entry point, pub struct DynamicMaterials
  registry_view.rs        ← snapshot of the registry for template rendering
  cache_key.rs            ← dispatch_hash → cache_key extension
```

The actual `MaterialRegistry` lives in `awsm-renderer-materials`; this module is the renderer-side facade that integrates it with the shader cache + the bind-group machinery.

### Template substitution

Both:

- `crates/renderer/src/render_passes/shared/shared_wgsl/material.wgsl` (the `{{ materials_wgsl|safe }}` site for the opaque compute kernel)
- The corresponding site in the transparent fragment shader template

…are extended so the substitution iterates **both** `static_entries` and `dynamic_entries` from the current `MaterialRegistry` snapshot. The Askama context gains:

```
materials_wgsl: String       (concatenation of static + dynamic fragments + per-material struct decls)
shader_id_dispatch: String   (concatenation of static + dynamic dispatch branches)
shader_id_consts: String     (named consts for first-party; dynamic uses literal numeric ids in its branches)
```

For each dynamic entry, the substitution emits:

1. A `struct CustomMaterialData_NNNN { … }` declaration generated via `generate_wgsl_struct`. Fields are emitted in this order:
   - All `UniformField` entries in declared order (alignment-respected).
   - A `<name>_index: u32` per `TextureSlot` in declared order.
   - A `<name>_offset: u32` and `<name>_length: u32` pair per `BufferSlot` in declared order.
2. The author's WGSL fragment, wrapped in a function `fn custom_shade_NNNN(…) -> …` with the contract's signature.
3. A dispatch branch `else if shader_id == NNNNu { return custom_shade_NNNN(…); }` appended to the dispatch chain.

`NNNN` is the dynamic shader_id assigned by the registry. The `_NNNN` suffix avoids symbol collisions if multiple authors picked the same struct field names.

### Bind groups

**One new binding**: the `extras_pool` storage buffer (`var<storage, read> extras_pool: array<u32>`), bound alongside the existing `materials` pool in the bind group already carrying it (group 0 binding TBD — pick the next free slot in the bind group that carries `materials: array<u32>` at binding 2 in `material_transparent/.../bind_groups.wgsl`, and the corresponding binding in the opaque compute kernel). The binding is **shared across all dynamic materials** regardless of how many register or how many buffer slots each declares.

Apart from `extras_pool`, no new bind groups. Custom materials read uniform data from the existing per-material storage / uniform buffer (the same one PBR/Unlit/Toon read from) and texture data via the existing texture pool binding. The `Material::Custom` instance carries texture keys and buffer slices; the per-frame upload resolves texture keys to texture-pool indices and appends them after the uniform tail in `write_uniform_buffer`, then appends `(offset, length)` u32 pairs for each buffer slot (offsets assigned by the extras-pool allocator at instance-upload time).

### Extras pool (variable-length per-material data)

A new module `crates/renderer/src/dynamic_materials/extras_pool.rs` owns:

- A `web_sys::GpuBuffer` of `extras_pool_capacity` u32 words (storage-mode, read-only from shaders). Capacity is configurable via `AwsmRenderer::new` options; default 1 MiB (262 144 u32s). Resizable on overflow with a `BindGroupRecreate` event (mirrors how the texture pool / shadow atlas handle resizes).
- A **free-list allocator** keyed by `(MaterialShaderId, slot_name)` → contiguous slice. On insert/update of a `DynamicMaterial` instance, the allocator finds (or coalesces) a slice that fits the slot's u32 words, records the offset, and **uploads the bytes via the renderer's mapped-buffer ring** — not raw `gpu.write_buffer`. See "Upload path" below. On removal of an instance, the slice is returned to the free list.
- Compaction: when fragmentation exceeds a threshold (e.g. free space > 25% of capacity but the largest free run is < 50% of total free space), the allocator runs a compaction pass that re-packs all live slices and updates every affected `DynamicMaterial`'s `(offset, length)` pairs. Compaction is a per-frame cap-limited operation (e.g. move at most 64 KiB of data per frame) to avoid hitching. Most scenes won't trigger compaction at all.

**Upload path.** Per
[`PERFORMANCE.md §5b`](../PERFORMANCE.md), every renderer-owned
per-frame upload goes through a
[`MappedUploader`](../../crates/renderer/src/buffer/mapped_uploader.rs)
companion. The extras pool fits both halves of that split:

- **Per-frame dirty-range writes** (the common path — author
  edited a uniform-override in the editor, a slice's bytes need
  re-uploading): use `MappedUploader::write_dirty_ranges`. One
  slot per material × buffer-slot pair acquires a dirty range; the
  per-frame upload batches them.
- **Foreign-bytes ingestion** (initial registration of a buffer
  slot from a `.bin` file loaded via `scene-schema`'s
  `load_material_folder`, or the first-time copy of an
  instance-override `BufferRef`): use
  `MappedUploader::ingest_foreign` — the bytes arrive as a
  `Vec<u32>` from outside the renderer's CPU-authoritative state,
  matching the same convention as glTF buffer + texture
  ingestion. Counted under `bytes_uploaded_via_writebuffer` in the
  upload-ring telemetry.

The allocator's `write_slice(material, slot, &[u32])` method is
the single entrypoint that picks the right one of the two based
on whether the slice was already in the allocator's tracked-Vec
shadow or is being freshly inserted.

The corresponding WGSL helper module `crates/renderer/src/render_passes/shared/shared_wgsl/extras.wgsl` mirrors `material.wgsl`'s pattern exactly:

```wgsl
@group(N) @binding(M) var<storage, read> extras_pool: array<u32>;

fn extras_load_u32(index: u32) -> u32 {
    return bitcast<u32>(extras_pool[index]);
}
fn extras_load_f32(index: u32) -> f32 {
    return bitcast<f32>(extras_pool[index]);
}
fn extras_load_vec4_f32(index: u32) -> vec4<f32> {
    return vec4<f32>(
        bitcast<f32>(extras_pool[index + 0u]),
        bitcast<f32>(extras_pool[index + 1u]),
        bitcast<f32>(extras_pool[index + 2u]),
        bitcast<f32>(extras_pool[index + 3u]),
    );
}
```

Included in every pass that includes `material.wgsl` — the symmetry is deliberate. First-party materials are free to use the extras pool too if they ever want variable-length data (none do today, but the binding is universal).

### Pipeline layouts

Unchanged. The opaque compute pipeline and the transparent fragment pipeline have the same layout regardless of how many dynamic materials are registered — what changes is the WGSL source, not the binding interface.

### Shader cache integration

The opaque kernel's existing cache key (and the transparent fragment shader's, separately) gains an extra component:

```rust
struct OpaqueShadingCacheKey {
    // … existing fields …
    dispatch_hash: u64,    // = registry.dispatch_hash()
}
```

When `register_material` / `unregister_material` runs, the registry's `dispatch_hash` changes; the next render's cache lookup misses and triggers compile. **When no dynamic materials are registered, the `dispatch_hash` returns a stable constant identical to today's implicit value.**

---

## New crate: `crates/frontend/material-editor/`

Mirrors the shape of `crates/frontend/scene-editor/` and `crates/frontend/model-tests/`.

### Cargo.toml

Dependencies:
- `awsm-renderer` (the renderer library)
- `awsm-renderer-materials` (for `MaterialRegistration`, etc.)
- `scene-schema` (for `MaterialDefinition` + folder loader)
- `web-shared` (theme, DOM helpers, dominator setup)
- A WGSL-capable code editor — recommend `codemirror` via wasm-bindgen / a thin JS shim. Confirm bundle size impact before committing.
- `wasm-bindgen-futures`, `web-sys` (file system access API for folder open/save), `serde_json`

### Module layout

```
crates/frontend/material-editor/
├── Cargo.toml
├── index.html
├── Trunk.toml
└── src/
    ├── main.rs                ← entry: starts renderer, mounts UI
    ├── app.rs                 ← top-level dominator component
    ├── panes/
    │   ├── mod.rs
    │   ├── definition.rs      ← left pane (uniforms, textures, alpha_mode, …)
    │   ├── wgsl_editor.rs     ← center pane (CodeMirror wrapper)
    │   ├── contract.rs        ← right pane (renders contract docs by alpha_mode)
    │   ├── preview.rs         ← bottom-left (renderer viewport)
    │   └── errors.rs          ← bottom-right (compile errors)
    ├── state.rs               ← Mutable<EditState>: current file, layout, wgsl
    ├── preview_scene.rs       ← stub scene construction (quad/sphere/box/glTF)
    ├── recompile.rs           ← orchestrates: layout → MaterialRegistration → register_material → record errors
    └── fs.rs                  ← File System Access API: open/save folder, copy texture assets
```

### Task / dev server

Add a `task material-editor:dev` rule mirroring `task scene-editor:dev`, on the next free port (9082 or 9083 depending on what's already taken).

---

## Implementation Phases

Each phase is a runnable checkpoint — commit after each. Lower phases assume upper phases compiled.

### Phase 0 — Scaffolding & wiring

1. **Rewrite `MaterialShaderId`** in `crates/materials/src/shader_id.rs` from `#[repr(u32)] enum` to `#[repr(transparent)] struct(u32)` with associated `PBR` / `UNLIT` / `TOON` consts and a `DYNAMIC_START` const. Every pattern-match like `match id { MaterialShaderId::Pbr => …, … }` becomes `if id == MaterialShaderId::PBR { … } else if id == MaterialShaderId::UNLIT { … } else …`. There are <10 call sites — `grep -rn 'MaterialShaderId::' crates/` to find them all.
2. **Add `Material::Custom(Box<DynamicMaterial>)`** variant. Stub `DynamicMaterial` as `pub struct DynamicMaterial { pub shader_id: MaterialShaderId, … }` with a temporary `impl MaterialShader` that panics on every method (will be fleshed out in Phase 2). Add the `Custom` arm to every existing `Material` pattern-match (also <10 call sites). For now the arm can `unreachable!()` since no `Custom` instance exists yet.
3. **Stand up `crates/renderer/src/dynamic_materials/`** with empty `mod.rs`, `pub struct DynamicMaterials` that holds nothing. Add `pub dynamic_materials: DynamicMaterials` to `AwsmRenderer`.
4. **Add stub `register_material` / `unregister_material` / `dynamic_materials()` methods** on `AwsmRenderer` returning placeholder values + `AwsmDynamicMaterialError::WgslCompile("unimplemented".into())` for `register_material`. The signatures are the public surface; the bodies come later.
5. **Add `AwsmDynamicMaterialError`** to `crates/renderer/src/error.rs` and into the top-level `AwsmError` enum.

Expected outcome: scene-editor + model-tests still build and render identically to before. No `Material::Custom` instances exist yet. Commit.

### Phase 1 — Schema additions + contract audit

1. **Add `MaterialDefinition`, `UniformField`, `FieldType`, `UniformValue`, `TextureSlot`, `BufferSlot`** in `crates/scene-schema/src/material.rs` (or a new `dynamic_material.rs` if the file is getting long). Match the shapes in **Schema Changes** above. Every field uses `#[serde(default)]` where reasonable.
2. **Add `CustomMaterialRef`** to the project root struct and `MaterialRef::Custom(CustomMaterialInstance)` to the material variant enum, both with `#[serde(default)]`. The instance struct includes `buffer_overrides: HashMap<String, BufferRef>` alongside the existing uniform / texture overrides.
3. **Implement `load_material_folder`** with full error variants. Cover: `material.json` missing, JSON parse error, `shader.wgsl` missing, asset file missing, `.bin` file size not a multiple of 4, reserved-name collision (`material`, `texture_pool`, `extras_pool`, `frag`, `vert`).
4. **Round-trip test**: write a hand-built `MaterialDefinition` (including a `BufferSlot` with a default `.bin` reference) to a temp folder, load it back, assert deep equality on both the layout and the resolved buffer bytes.
5. **Audit the first-party shading contract.** Read every first-party material WGSL (`materials/src/wgsl/pbr/*`, `unlit_material.wgsl`, `toon_material.wgsl`) and the compute-kernel template (`shared_wgsl/material.wgsl` + the opaque-compute pass shaders) and the transparent fragment shader. Document precisely:
   - Function signature each first-party fragment exposes (input struct, output struct, name pattern).
   - Helpers reachable from inside the fragment (every symbol from `shared_wgsl/`). This includes `shared_wgsl/frame_globals.wgsl` (`frame_globals.time` / `delta_time` / `frame_count` / `resolution`) — see [`TEMPORAL_SHADERS.md`](../TEMPORAL_SHADERS.md) for the full surface.
   - Per-material storage-buffer convention (byte_offset table, how `shader_id` indexes in, where texture indices live).
   - Output expectations for each pass (HDR linear, alpha handling, etc.).
6. **Write the docs.** Produce `docs/dynamic-materials/contract-opaque.md` and `docs/dynamic-materials/contract-transparent.md`. Each begins with the exact function signature an author writes, followed by sections on helpers in scope, per-material data access, and texture-pool access. Cross-reference into the relevant `shared_wgsl/` files by line range.
7. **Refactor first-party materials if needed** so they conform to the documented contract. The goal: a future promoted material (a custom material baked to first-party) is bit-identical to a hand-written one in shape. If PBR has an idiosyncratic input struct that no custom material could plausibly match, normalize it.
8. **Update the dynamic-materials plan in this file** with any contract details that emerged in the audit.

Expected outcome: contract docs exist; first-party materials conform; schema types serialize/deserialize cleanly. No rendering changes. Commit.

### Phase 2 — Layout helpers + DynamicMaterial impl

1. **Implement `crates/materials/src/dynamic_layout.rs`** with `generate_wgsl_struct`, `pack_uniform_values`, `pack_texture_indices`, `pack_buffer_offsets`, `layout_size`. Match the WGSL alignment rules from the W3C spec. Reference: `vec3<f32>` aligns to 16 bytes but only occupies 12 bytes of payload (4 bytes trailing padding); `mat3<f32>` aligns to 16 bytes and occupies 48 bytes; `mat4<f32>` aligns to 16 bytes and occupies 64 bytes. `generate_wgsl_struct` emits fields in the documented order: uniforms first, then `<texture>_index: u32` per texture slot, then `<buffer>_offset: u32` + `<buffer>_length: u32` per buffer slot.
2. **Unit tests covering every `FieldType`** + mixed-alignment cases:
   - `[F32, Vec3, F32]` → struct should have padding between F32 and Vec3 (Vec3 needs 16-byte align); generated bytes must match.
   - `[Vec3, Vec3]` → 12 bytes data + 4 padding + 12 bytes data + 4 padding = 32 bytes total.
   - `[Mat3, F32]` → Mat3 is 48 bytes (three column-vec3s, each 16-byte aligned, 4 bytes each tail-padded), F32 right after.
   - `[Bool, F32]` → Bool becomes U32 (4 bytes), F32 right after.
   - A layout with `[F32 "a"]` uniforms + `[TextureSlot "tex"]` + `[BufferSlot "buf"]` → struct is `{ a: f32, tex_index: u32, buf_offset: u32, buf_length: u32 }` (16 bytes total, naturally tight).
   These tests are the **first line of defense** against silent rendering garbage. Don't skimp.
3. **Implement `DynamicMaterial::write_uniform_buffer`** using the layout helpers. Pull `(layout, wgsl_fragment)` for the `shader_id` from a `&'a MaterialRegistry` passed through the `TextureContext` trait (extend `TextureContext` with a `material_layout(shader_id)` accessor if needed). Buffer slot `(offset, length)` pairs are passed in by the renderer at write time (they don't exist on the `DynamicMaterial` itself — the extras-pool allocator assigns them per-instance). For Phase 2 they're stub zeros; Phase 6 wires the real allocator.
4. **Implement the rest of `impl MaterialShader for DynamicMaterial`**: `shader_id()`, `alpha_mode()`, `wgsl_fragment()`. All look up from the registry by `shader_id`.

Expected outcome: `DynamicMaterial` instances can be constructed and `write_uniform_buffer` produces correctly-aligned bytes. No rendering integration yet. Commit.

### Phase 3 — Registry + dispatch-hash plumbing

1. **Implement `MaterialRegistry`** in `crates/materials/src/registry.rs` per the shape above. `register` assigns the next `DYNAMIC_START + N` shader_id, records the entry, increments. `dispatch_hash` is a stable hash over `[(shader_id, name, layout_hash, wgsl_hash)]` (sort by shader_id for stability).
2. **Wire `MaterialRegistry`** into the renderer: `AwsmRenderer::dynamic_materials` becomes `pub struct DynamicMaterials { registry: MaterialRegistry, … }`. The stub `register_material` from Phase 0 calls through.
3. **Extend the opaque compute kernel's cache key** with `dispatch_hash`. Verify (via a test or a debug print) that the hash is constant when no dynamic materials are registered.
4. **Extend the transparent fragment shader's cache key** the same way.
5. **Idempotency**: `register_material` checks `(name, layout_hash, wgsl_hash)` against existing entries; if all three match, return the existing id without changing the dispatch hash.
6. **Promote `material_classify::BUCKET_COUNT` and the WGSL bit-table to registry-driven.** Today both are hard-coded for PBR/UNLIT/TOON (see [`material_classify/buffers.rs`](../../crates/renderer/src/render_passes/material_classify/buffers.rs) `pub const BUCKET_COUNT: u32 = 3;` and the `BUCKET_BIT_*` consts + if-else chain in [`compute.wgsl`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl)). Both become functions of `registry.all_entries().len()`. The classify WGSL is now an askama template that walks the registry to emit:
   - `const BUCKET_BIT_<name>: u32 = (1u << index);` for each entry.
   - `const SHADER_ID_<name>: u32 = N;` for each entry (already exists as `shader_id_consts`).
   - The shader_id → bit if-else chain.
   - The per-bucket extract block (around lines 89-103 of the current `compute.wgsl`).
   Without this, dynamic shader_ids reach the opaque kernel but never get classified, so they fail to dispatch over any tile. **Verify**: register a dynamic material against a one-quad scene; confirm the classify pass writes its bucket non-zero (via `read_render_pass_timings` showing the per-shader_id pipeline runs).

Expected outcome: registering and unregistering a dynamic material changes `dispatch_hash`; the cache invalidates; recompile fires on next render (but produces the same WGSL since the substitution hasn't been wired yet). Commit.

#### Phase 3 cross-link: shader-cache warmup API

The dispatch-hash machinery this phase lands plugs into the
already-extant
[`AwsmRenderer::prewarm_pipelines`](../../crates/renderer/src/lib.rs)
API (see also [`docs/PERFORMANCE.md` §5g](../PERFORMANCE.md) for the
batched `ensure_keys` plumbing the warmup rides on). That method
already walks the live mesh set and warms every transparent
pipeline variant the scene needs; the dynamic-materials extension
is to additionally iterate the registry's enabled set so newly
registered materials' opaque + transparent pipelines compile
through the same batched `ensure_keys` path. Shipping that
extension alongside Phase 4 (when the first dynamic material
actually compiles) avoids surfacing a per-frame "registered a
material → recompile stutter mid-game" hazard to player-shipped
consumers.

Idempotent and cheap when the GPU disk cache is warm (<5 ms on
Chrome); ~50–500 ms per N variants on a cold cache. Game-init
code calls it after the burst of `register_material` calls
finishes; mid-gameplay code calls it after each new burst of
runtime registrations (e.g. streamed-in level packs).

### Phase 4 — Opaque template substitution + first dynamic render

1. **Generate WGSL for dynamic entries.** In whatever module currently produces `materials_wgsl` and `shader_id_dispatch` for the opaque kernel template, extend the producer to iterate dynamic entries after static ones. Per dynamic entry, emit:
   - `struct CustomMaterialData_<id> { … }` from `generate_wgsl_struct`
   - The author's `wgsl_fragment` wrapped in `fn custom_shade_<id>(input: <ContractInput>) -> <ContractOutput> { <fragment body> }`
   - A dispatch branch `else if shader_id == <id>u { return custom_shade_<id>(input); }`
2. **Plumb per-material data** so the dynamic material's `write_uniform_buffer` output gets written into the same per-material storage / uniform buffer first-party materials use, at the same byte_offset table location.
3. **Texture indices**: when the per-frame upload runs `write_uniform_buffer` for a `Material::Custom`, resolve each texture key to a texture-pool index via the existing `TextureContext` resolver, and append the indices as u32 in the layout's texture order.
4. **First test material**: hand-build a `MaterialRegistration` for a simple flowmap-style material:
   - Uniforms: `speed: f32 = 0.5`, `tint: vec3 = [1,1,1]`
   - Textures: `flow` (point to any RGB texture in the test assets)
   - WGSL: scrolls the flow texture by `time * speed`, tints by `tint`, returns it as the diffuse contribution under a basic lighting term using the shared lighting helpers.
5. **Test scene**: a quad in the world scene at known coordinates, with `MaterialRef::Custom { material: "flowmap", … }`. Load scene; verify the material renders. Toggle the `tint` value in the project.json; verify it updates after reload.

Expected outcome: a hand-registered opaque dynamic material renders correctly in the test scene, indistinguishable from a first-party material with equivalent behavior. Commit.

### Phase 5 — Mesh / material reference plumbing in scene-editor

1. **Bridge updates** (`crates/frontend/scene-editor/src/renderer_bridge/`): on project load, walk `project.custom_materials`, call `load_material_folder` for each, convert to `MaterialRegistration`, call `renderer.register_material`. Cache the assigned `MaterialShaderId` per name so mesh material refs can resolve.
2. **`MaterialRef::Custom` → renderer instance**: when a mesh has `MaterialRef::Custom { material, uniform_overrides, texture_overrides }`, construct a `DynamicMaterial { shader_id, values, textures }` where `values` start from the layout defaults overlaid with `uniform_overrides`, and `textures` resolve via the asset system.
3. **scene-editor "Materials" pane**: lists `project.custom_materials`. For Phase 5, read-only is fine — Import/Remove buttons land in Phase 11.
4. **Per-mesh material picker** in the scene-editor's property panel gains a "Custom" submenu listing all registered dynamic materials. Picking one populates the mesh's `MaterialRef::Custom` with the layout defaults.

Expected outcome: a custom material defined manually in `project.json` (in `assets/materials/flowmap/`) loads on scene open, attaches to a mesh via the property panel, and renders. Commit.

### Phase 6 — Extras pool + buffer slots

1. **Stand up `crates/renderer/src/dynamic_materials/extras_pool.rs`**: 1 MiB `array<u32>` storage buffer (configurable via `AwsmRendererOptions::extras_pool_capacity`), free-list allocator keyed by `(MaterialShaderId, slot_name)` → contiguous slice. The pool owns a `MappedUploader` companion (see [`crates/renderer/src/buffer/mapped_uploader.rs`](../../crates/renderer/src/buffer/mapped_uploader.rs) — `instances.transforms` is a good precedent for a "single big mutable slice" upload pattern). Methods: `allocate(material, slot, words)`, `free(material, slot)`, `write_slice(material, slot, &[u32])` (routes through `write_dirty_ranges` for tracked slices or `ingest_foreign` for first-time inserts; see §"Upload path" in **Renderer / Materials Changes**).
2. **Add `shared_wgsl/extras.wgsl`** with `extras_pool` binding declaration and `extras_load_u32` / `extras_load_f32` / `extras_load_vec4_f32` helpers. Include in every pass that includes `material.wgsl` — they're peer modules.
3. **Bind group plumbing**: add `extras_pool` to the same bind group that already carries `materials: array<u32>` (both in opaque-compute and transparent-fragment passes — see `material_transparent_wgsl/bind_groups.wgsl` for the existing binding). Pipeline layouts grow by one binding entry each. Verify the layout doesn't push past `maxStorageBuffersPerShaderStage`.
4. **Per-frame upload**: when packing a `Material::Custom` instance into the materials pool, for each declared buffer slot:
   - Resolve the slot's data: `buffer_overrides.get(slot.name)` first, else `slot.default` from the registration.
   - Call `extras_pool.allocate_or_update(material_id, slot_name, &data)` and obtain `(offset, length)`.
   - Append `offset` and `length` u32s to the material's uniform tail (after texture indices). The auto-generated WGSL struct's `<slot>_offset` / `<slot>_length` fields naturally line up.
5. **Resize on overflow**: if `extras_pool.allocate` fails (no contiguous slice large enough), grow the pool (double capacity), fire a `BindGroupRecreate::ExtrasPoolResize` event, re-upload all live slices into the new buffer, re-write all affected `(offset, length)` pairs.
6. **Compaction**: when fragmentation exceeds the threshold (free space > 25% but largest free run < 50% of total free), run a per-frame-capped compaction (move ≤ 64 KiB per frame, update affected `(offset, length)` pairs as slices move).
7. **Second test material**: a flipbook-with-irregular-cells dynamic material:
   - Uniforms: `fps: f32`, `frame_count: u32`, `tint: vec3<f32>`
   - Textures: `atlas` (the sprite-sheet image)
   - Buffers: `frames` — each "frame" is 4 f32s (cell `x`, `y`, `w`, `h` in UV space)
   - WGSL: reads `frame_globals.time` (in scope on every material-shading pass — see [`TEMPORAL_SHADERS.md`](../TEMPORAL_SHADERS.md)), computes `frame_idx`, reads the cell rect from `frames` via `extras_load_f32(material.frames_offset + frame_idx * 4u + i)`, computes the cell UV, samples the atlas, multiplies by tint.
8. **Author the test material's `.bin`**: use the Buffer Converter (or, for Phase 6 since material-editor may not yet exist, a one-off Rust helper script in `crates/renderer/examples/make_flipbook_bin.rs`) to produce `frames.bin` from a JSON array of cell rects.
9. **Test scene**: add a quad with the irregular-flipbook material; verify cells play back correctly. Add a second instance with `buffer_overrides` pointing at a different `frames.bin` (different cell layout); verify both render independently with no aliasing.

Expected outcome: a custom material reading variable-length data from `extras_pool` renders correctly. Two instances with different buffer data render independently. Pool resize works end-to-end (force it by setting the initial capacity low). Commit.

### Phase 7 — Transparent path

1. **Audit transparent contract** (already documented in Phase 1 — verify nothing's drifted). Confirm signature and helpers-in-scope for the transparent fragment shader site.
2. **Same template substitution mechanism** as Phase 4, but in the transparent fragment shader's template. Same `struct + fn + dispatch-branch` triple per dynamic entry, but with the transparent contract's input/output signature.
3. **Cache key invalidation** for the transparent fragment shader on dispatch-hash change — already wired in Phase 3.
4. **Second test material**: a soft-glass-style material with `alpha_mode: Blend`, samples a tint texture, uses `sample_transmission_background` from the existing transparent helpers to produce a refracted background, multiplies by tint with alpha based on view angle.
5. **Test scene**: a sphere with the glass material in front of one of the opaque cubes. Verify the cube is visible through the glass, that it's affected by lighting, etc.
6. **Sorting**: transparent meshes are already back-to-front sorted by the existing transparent pass — no per-shader-id changes needed. Verify the custom transparent renders in the right order relative to first-party transparents.

Expected outcome: a hand-registered transparent dynamic material renders correctly, sorts correctly, samples the opaque background correctly. Commit.

### Phase 8 — `material-editor` crate scaffolding

1. **Create `crates/frontend/material-editor/`** with the file layout above. Empty implementations / placeholder UIs are fine.
2. **`task material-editor:dev` target** in the workspace Taskfile.
3. **Boot the renderer** in `main.rs` with a 1×1 canvas and a stub scene that draws nothing. Verify the page loads in the browser, the renderer initializes, no GPU validation errors.
4. **Skeleton UI** with dominator: top bar (File / Preview mesh / Recompile placeholders), four-pane grid for Definition / WGSL / Contract / (Preview + Errors). No interactivity yet.
5. **Hard-coded test material**: load a `LoadedMaterialFolder` for the Phase-4 flowmap as the initial edit state. Confirm Definition pane shows its uniforms, WGSL pane shows its WGSL source (read-only for now), Preview pane is blank.

Expected outcome: material-editor app boots, displays a hard-coded material's metadata, renders nothing. Commit.

### Phase 9 — material-editor preview + recompile

1. **Stub scene** in `preview_scene.rs`: a quad / sphere / box mesh on a neutral background with a default lighting rig (one directional + ambient). Selectable via the top bar.
2. **Preview render**: bind the renderer to a `<canvas>` in the Preview pane; render the stub scene every frame.
3. **Apply the loaded material** to the preview mesh: call `renderer.register_material(reg)` once on load, assign the returned `shader_id` to the mesh's material slot.
4. **Recompile path**: when the user edits the layout (Definition pane) or the WGSL (CodeMirror pane), debounce ~500ms, then:
   - Re-build a `MaterialRegistration` from the current state.
   - Call `renderer.unregister_material(old_id)` and `renderer.register_material(new_reg)` to get a fresh id.
   - Assign the new id to the preview mesh.
   - On next render, the shader cache invalidates and recompiles.
5. **Failed compile fallback**: if `register_material` returns `AwsmDynamicMaterialError::WgslCompile`, keep the previous registration active; surface the error string to the Errors pane.

Expected outcome: editing the WGSL or layout of the loaded material updates the preview within ~1s; compile failures keep the previous material running and surface in the error console. Commit.

### Phase 10 — material-editor definition pane

1. **Uniforms table** in `panes/definition.rs`: a dominator table whose rows are `Mutable<UniformFieldRuntime>`. Add row, delete row, reorder via drag-handle. Each row has a name input, a `FieldType` dropdown, and a default-value editor whose shape depends on the type (number drag for F32, color picker for Color3/Color4, vector inputs for VecN, etc.).
2. **Textures table**: similar shape. Default-texture picker uses the File System Access API to import a local image file into the in-memory `LoadedMaterialFolder`'s `texture_data` map; the path is stored relative to the folder root.
3. **Render-state controls**: `alpha_mode` segmented toggle (Opaque/Mask/Blend) — with a cutoff slider that appears for Mask, `double_sided` toggle.
4. **Auto-generated WGSL struct preview**: above the user's WGSL in the editor pane, show a read-only block displaying the current `generate_wgsl_struct("MaterialData", &layout)` output. Updates live as the Definition pane changes.

Expected outcome: the entire `MaterialDefinition` is editable through the UI, and edits drive recompile via the Phase 8 path. Commit.

### Phase 11 — Error reporting + contract pane

1. **Contract pane** (`panes/contract.rs`): renders `docs/dynamic-materials/contract-opaque.md` when the current `alpha_mode` is Opaque/Mask, `contract-transparent.md` when Blend. Pull-in markdown rendering via a lightweight WASM markdown library (or pre-bake the HTML at build time and `include_str!` it).
2. **WGSL error parsing**: when `register_material` returns `WgslCompile(msg)`, parse the error message for line/column (naga's error format is reasonably structured). Surface to the Errors pane as a clickable entry that focuses the WGSL editor at the position.
3. **Inline gutter markers**: if the code editor supports it, draw an error marker on the line. CodeMirror supports a `lint` extension that takes a list of diagnostics.
4. **Stub-fragment-on-new**: when the user clicks File → New, populate the WGSL pane with a minimal stub matching the current `alpha_mode`'s contract:
   ```wgsl
   // Opaque/Mask stub:
   fn shade(input: OpaqueShadingInput) -> OpaqueShadingOutput {
       // your code here
       return OpaqueShadingOutput(/* default-bright */);
   }
   ```

Expected outcome: compile errors point at lines; the contract pane swaps based on alpha_mode; New produces a runnable stub. Commit.

### Phase 12 — scene-editor import flow

1. **`Import Material…` button** in the scene-editor's Materials pane: opens a folder picker, validates the structure (`material.json` + `shader.wgsl` present), copies the folder into `<project>/assets/materials/<name>/`, appends a `CustomMaterialRef` to `project.custom_materials`, saves the project, and registers the material with the renderer immediately so it's available for assignment without a reload.
2. **`Remove Material`** per-row: confirms, then verifies no live meshes reference it (else: error). Removes the folder, the project entry, calls `renderer.unregister_material`.
3. **`Open in material-editor`** link: opens `http://localhost:9082/?folder=<path>` (or whatever the URL scheme is) in a new tab. material-editor's `main.rs` checks the URL parameter and loads the folder on boot.
4. **Save/reload round-trip**: edit a custom material's per-instance values on a mesh via the property panel, save the project, reload — values round-trip.

Expected outcome: full authoring loop: write a material in material-editor → export folder → import into scene-editor → assign to a mesh → render → edit values → save → reload. Commit.

### Phase 13 — Promotion documentation + example

1. **Write `docs/dynamic-materials/promotion.md`**: a step-by-step walkthrough of porting a dynamic material to first-party. Use the Phase-4 flowmap as the worked example.
2. **Land a promoted material** in `crates/materials/src/flowmap.rs` behind a `flowmap` Cargo feature: `struct FlowmapMaterial`, `impl MaterialShader`, `WGSL_FRAGMENT` constant. The struct fields, the `write_uniform_buffer` byte order, and the WGSL fragment must produce **bit-identical** output to the dynamic version.
3. **Add a "Promotion smoke test"** in the materials crate's tests: build both a `DynamicMaterial` and a `FlowmapMaterial` with the same inputs; call `write_uniform_buffer` on each; assert byte-equal output. Hash the WGSL fragments and assert equal.
4. **Update the schema-side support** so a project that previously referenced `MaterialRef::Custom { material: "flowmap", … }` can transparently load against the promoted first-party material when the feature is enabled — i.e. the registration step recognizes the name collision and prefers the typed first-party impl. (Alternatively: leave them as separate concepts, and document that promotion requires editing the scene to switch from `Custom("flowmap")` to `Flowmap { … }`. Pick one and document it.)

Expected outcome: a real material has walked the entire path from dynamic to first-party; the docs prove it's mechanical. Commit.

### Phase 14 — Final pass

1. Update `docs/ROADMAP.md`: tick the "Dynamic Materials" line item.
2. Update the test scene one final time so it shows off:
   - At least one custom opaque material under direct lighting
   - At least one custom transparent material in front of an opaque object
   - The promoted first-party flowmap alongside the dynamic flowmap (visually identical)
   This becomes the visual regression baseline.
3. Run all of `material-editor`'s round-trip tests:
   - New material, edit layout + WGSL, save, reopen → identical
   - Import to scene-editor, assign to mesh, edit instance values, save, reload → identical
   - Promotion smoke test → byte-identical bytes, WGSL-identical hashes
4. `cargo fmt`
5. `cargo clippy --workspace --all-targets` — fix everything.
6. `cargo doc --workspace --no-deps` — fix every broken intra-doc link.
7. `cargo clippy --workspace -- -W missing_docs` — every public item in the new surface area has a rustdoc.
8. Re-run all the test scenarios from **How to test**. Take screenshots for the visual regression baseline.

Done.

---

## Key References

- **WGSL specification** — particularly memory layout rules. <https://www.w3.org/TR/WGSL/#memory-layouts> — the alignment rules in `dynamic_layout.rs` MUST match this section exactly.
- **WGSL uniform vs storage buffer rules** — <https://gpuweb.github.io/gpuweb/wgsl/#address-space-layout-constraints> — relevant if dynamic materials' data ever moves to a `var<storage>` binding instead of `var<uniform>`.
- **Naga (WGSL compiler used by wgpu/WebGPU)** — error format reference for the error-parsing in `panes/errors.rs`. <https://github.com/gfx-rs/naga>
- **Askama template engine** — the Rust template engine used for all shader composition. <https://djc.github.io/askama/>
- **CodeMirror 6** — recommended in-browser WGSL editor. <https://codemirror.net/docs/>
- **File System Access API** — for material-folder open/save in material-editor. <https://developer.mozilla.org/en-US/docs/Web/API/File_System_Access_API>
- **Internal**: `docs/SHADOWS.md` — shadow subsystem (preceded this one) established the schema → bridge → renderer pattern this plan extends. The "Player / runtime integration" and "Configuration surface" sections show the same discipline applied to a feature that's already shipped.
- **Internal**: `crates/materials/src/shader.rs` — current `MaterialShader` trait definition; the contract the Phase 1 audit anchors against.
- **Internal**: `crates/renderer/src/render_passes/shared/shared_wgsl/` — every file in this directory is in-scope for custom-material WGSL fragments; the contract docs reference all of them.

---

## Tracking

Tick items as they land. A future session can resume by reading this list.

### Phase 0 — Scaffolding
- [ ] `MaterialShaderId` rewritten as `#[repr(transparent)] struct(u32)` with `PBR` / `UNLIT` / `TOON` consts + `DYNAMIC_START`
- [ ] All `MaterialShaderId::X` pattern-match sites updated
- [ ] `Material::Custom(Box<DynamicMaterial>)` variant added; all match sites updated
- [ ] `crates/renderer/src/dynamic_materials/` module skeleton
- [ ] `dynamic_materials` field on `AwsmRenderer`
- [ ] Stub `register_material` / `unregister_material` / `dynamic_materials()` methods (return placeholder errors)
- [ ] `AwsmDynamicMaterialError` added to top-level `AwsmError`

### Phase 1 — Schema + contract audit
- [ ] `MaterialDefinition`, `UniformField`, `FieldType`, `UniformValue`, `TextureSlot`, `BufferSlot` in scene-schema
- [ ] `CustomMaterialRef` on project root; `MaterialRef::Custom` variant; `CustomMaterialInstance.buffer_overrides` + `BufferRef`
- [ ] `load_material_folder` with full error variants (including `.bin` size-not-multiple-of-4 and reserved-name `extras_pool`)
- [ ] `LoadedMaterialFolder.buffer_data` populated from `.bin` files
- [ ] Round-trip test for a hand-built material (including a `BufferSlot` with a `.bin` default)
- [ ] First-party WGSL audited; function signatures + helpers-in-scope documented
- [ ] `docs/dynamic-materials/contract-opaque.md` written
- [ ] `docs/dynamic-materials/contract-transparent.md` written
- [ ] First-party PBR/Unlit/Toon refactored to conform if needed
- [ ] This plan updated with any contract details that emerged

### Phase 2 — Layout helpers + DynamicMaterial impl
- [ ] `crates/materials/src/dynamic_layout.rs` with `generate_wgsl_struct`, `pack_uniform_values`, `pack_texture_indices`, `pack_buffer_offsets`, `layout_size`
- [ ] `generate_wgsl_struct` emits fields in the documented order (uniforms → `<tex>_index` → `<buf>_offset`/`<buf>_length`)
- [ ] Unit tests covering every `FieldType` + mixed-alignment cases (Vec3 padding, Mat3 stride, mixed Bool/F32, mixed uniform-texture-buffer slot layouts)
- [ ] `impl MaterialShader for DynamicMaterial` complete (with stub `(0, 0)` buffer-offset writes; Phase 6 wires the allocator)

### Phase 3 — Registry + dispatch-hash
- [ ] `MaterialRegistry` in `crates/materials/src/registry.rs`
- [ ] Renderer-side `DynamicMaterials` facade wraps the registry
- [ ] Opaque kernel cache key includes `dispatch_hash`
- [ ] Transparent fragment shader cache key includes `dispatch_hash`
- [ ] Idempotent registration on `(name, layout_hash, wgsl_hash)`
- [ ] Verified: empty registry → `dispatch_hash` matches today's compiled WGSL
- [ ] **`material_classify::BUCKET_COUNT` is registry-driven**, not a hard-coded `3`
- [ ] **Classify `compute.wgsl` is an askama template** emitting `BUCKET_BIT_<name>` / shader_id → bit mapping / per-bucket extract — walked from the same registry as `materials_wgsl`
- [ ] Verified: registering a dynamic material against a one-quad scene shows its bucket bit set + its pipeline dispatched (via `read_render_pass_timings`)

### Phase 4 — Opaque template substitution
- [ ] Substitution emits `struct CustomMaterialData_<id>` per dynamic entry
- [ ] Substitution emits wrapped `fn custom_shade_<id>` per dynamic entry
- [ ] Substitution appends dispatch branches per dynamic entry
- [ ] Per-material storage / uniform buffer carries dynamic-material bytes
- [ ] Texture indices resolved via `TextureContext` and appended after uniforms
- [ ] Phase-4 flowmap registration renders on a test-scene quad

### Phase 5 — scene-editor instance plumbing
- [ ] Bridge registers all `project.custom_materials` on project load
- [ ] `MaterialRef::Custom` → `Material::Custom` runtime conversion in the bridge
- [ ] `buffer_overrides` round-trip through the bridge (data path; no editor UI yet)
- [ ] scene-editor "Materials" pane lists registered customs (read-only)
- [ ] Per-mesh material picker shows a "Custom" submenu

### Phase 6 — Extras pool + buffer slots
- [ ] `crates/renderer/src/dynamic_materials/extras_pool.rs` — 1 MiB default `array<u32>` storage buffer with free-list allocator
- [ ] **Pool owns a `MappedUploader` companion**; per-frame edits go through `write_dirty_ranges`, first-time inserts go through `ingest_foreign` (NOT raw `gpu.write_buffer`)
- [ ] **`bytes_uploaded_via_writebuffer` telemetry counts initial buffer-slot loads**; `bytes_uploaded_via_ring` counts edits — verify via `read_upload_ring_stats()` after a material registration + edit cycle
- [ ] `shared_wgsl/extras.wgsl` with `extras_load_u32` / `extras_load_f32` / `extras_load_vec4_f32` helpers
- [ ] `extras_pool` bound alongside `materials` in opaque-compute and transparent-fragment passes
- [ ] Per-frame upload resolves each `Material::Custom`'s buffer slots and writes `(offset, length)` u32 pairs after the texture-index tail
- [ ] Auto-generated WGSL struct fields `<slot>_offset` / `<slot>_length` line up byte-for-byte with the packer
- [ ] Pool resize on overflow (double capacity + `BindGroupRecreate::ExtrasPoolResize`) — confirm the `MappedUploader` rebuilds at the new size cleanly (same path the `Dynamic*Buffer` resize uses today)
- [ ] Fragmentation-triggered compaction (per-frame cap; moves ≤ 64 KiB per frame)
- [ ] Second test material: irregular-flipbook reading `frames` from extras pool, with hand-authored `frames.bin`
- [ ] Two instances with different `buffer_overrides` render independently

### Phase 7 — Transparent path
- [ ] Transparent fragment shader template grows the same substitution mechanism
- [ ] Second test material (transparent glass-style) renders correctly
- [ ] Test scene confirms sort order with first-party transparents

### Phase 8 — material-editor scaffolding
- [ ] `crates/frontend/material-editor/` crate exists, builds
- [ ] `task material-editor:dev` runs
- [ ] Renderer boots with a stub scene
- [ ] Four-pane skeleton UI mounts
- [ ] Hard-coded flowmap material displayed (read-only)

### Phase 9 — Preview + recompile
- [ ] Stub scene with quad/sphere/box selector
- [ ] Loaded material applies to the preview mesh
- [ ] Debounced recompile on layout / WGSL edits
- [ ] Failed-compile fallback keeps the previous material live and surfaces the error

### Phase 10 — Definition pane
- [ ] Uniforms table: add/delete/reorder rows, type dropdown, default editor per type
- [ ] Textures table: import local image into folder's `texture_data`
- [ ] Buffers table: pick `.bin` file (copy into folder's `assets/` on save); name becomes `<name>_offset` / `<name>_length` in struct preview
- [ ] **Buffer Converter modal**: JSON-array textarea → flatten → write f32 little-endian bytes → download as `.bin`
- [ ] Render-state controls (alpha_mode + cutoff + double_sided)
- [ ] Auto-generated WGSL struct preview above the user's WGSL (includes buffer offset/length fields)

### Phase 11 — Errors + contract pane
- [ ] Contract pane renders the appropriate markdown by alpha_mode
- [ ] WGSL compile errors parsed for line/column
- [ ] Inline gutter markers in the code editor
- [ ] File → New produces a runnable stub matching the current alpha_mode

### Phase 12 — scene-editor import flow
- [ ] Import Material button: folder picker → validate → copy → register
- [ ] Remove Material with reference-counting safety
- [ ] Open in material-editor link (URL with folder param)
- [ ] Save/reload round-trip preserves per-instance values

### Phase 13 — Promotion
- [ ] `docs/dynamic-materials/promotion.md` walks through flowmap promotion
- [ ] `crates/materials/src/flowmap.rs` lands behind a Cargo feature
- [ ] Promotion smoke test: dynamic vs. promoted produce byte-identical buffers + WGSL-identical fragments
- [ ] Scene-side migration documented (or auto-detected at load)

### Phase 14 — Ship
- [ ] `docs/ROADMAP.md` updated
- [ ] Test scene shows custom opaque + custom transparent + promoted side-by-side
- [ ] Material-creator round-trip tests pass
- [ ] `cargo fmt` clean
- [ ] `cargo clippy --workspace --all-targets` clean
- [ ] `cargo doc --workspace --no-deps` clean
- [ ] Visual regression screenshots taken

### Public API gate (must pass at ship)
The public API surface defined in **Public API Surface** above is the contract for non-editor consumers. Tick these before declaring done.

- [ ] Every `pub` type, field, method, and enum variant in `awsm_renderer_materials::dynamic` and `awsm_renderer::dynamic_materials` has a rustdoc comment
- [ ] `AwsmRenderer::{register,unregister}_material`, `dynamic_material_registration`, `dynamic_materials` all documented
- [ ] `AwsmDynamicMaterialError` integrated into top-level `AwsmError`
- [ ] Integration example (`crates/renderer/examples/dynamic_material.rs` or rustdoc example) compiles, runs, and produces a visible custom material with NO scene-schema or editor dependency
- [ ] `cargo doc --workspace --no-deps` produces no warnings
- [ ] `cargo clippy --workspace --all-targets -- -W missing_docs` produces no warnings on `awsm-renderer` / `awsm-renderer-materials` dynamic-material items
- [ ] `crates/renderer/README.md` (or a dedicated docs file) walks through the minimal "register and use a custom opaque material" recipe
- [ ] `docs/dynamic-materials/contract-opaque.md` and `contract-transparent.md` are the single source of truth for the author-facing contract; no duplicate or conflicting documentation lives elsewhere
- [ ] `docs/dynamic-materials/promotion.md` describes the dynamic → first-party promotion path with a worked example
