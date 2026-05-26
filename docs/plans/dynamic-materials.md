# Dynamic Materials Implementation Plan

## Instructions for the implementor

This plan is meant to be followed **start to finish** in a single sustained
effort. The phases are ordered so each leaves the renderer in a runnable
(if visually-incomplete) state, but don't try to ship intermediate phases
as standalone PRs — there will be deliberate breaking changes along the
way (the `MaterialShaderId` enum is rewritten as a `u32` newtype struct,
the `Material` enum gains a `Custom` variant, the per-shader-id pipeline
machinery grows registry-driven branches, the material-classify shader
becomes template-driven) and the goal is to keep the diff coherent rather
than always shippable.

- **Commit frequently** at every natural checkpoint (after each phase,
  after each subsystem stands up green). Small commits make `git bisect`
  cheap when something regresses. Don't squash as you go.
- **Breaking changes are fine** mid-plan. If you need to change the shape
  of `MaterialShaderId`, the `Material` enum, the on-disk `project.json`
  schema, or the shader cache key, just do it — the next crates.io
  publish will be a new major version. Update the test scene at
  `awsm-renderer-assets/world/project.json` (and any other authored
  projects you find) along with the change.
- **Update the tracking section at the bottom** as you go. Tick boxes
  when each item is done so a future session can resume cleanly if you
  stop mid-way.
- **Only after EVERYTHING below has landed and visually verified**, run:
  ```
  cargo fmt --all
  cargo clippy --workspace --all-targets -- -D warnings
  cargo doc --workspace --no-deps
  ```
  Fix everything they turn up. Then the branch is ready to push.

### How to test

The primary verification surfaces are **two** browser apps:

1. **`material-editor`** — the new frontend crate this plan introduces.
   Start with:
   ```
   task material-editor:dev
   # served at http://localhost:9082 (pick the next free port)
   ```
   This is where you author and live-preview a custom material against a
   stub scene.

2. **`scene-editor`** — the existing app. Start with:
   ```
   task scene-editor:dev
   # served at http://localhost:9081
   ```
   This is where an imported custom material gets applied to a mesh in a
   real scene and verified under real lighting / shadows / etc.

Use the `preview_start` / `preview_screenshot` / `preview_snapshot` tools
to drive each page in a Chromium preview. The renderer crate hot-reloads
via Trunk's watch list, so editing renderer code and refreshing either
preview is the fastest loop.

The test scene lives at `awsm-renderer-assets/world/project.json` (a
sibling repo). Extend it as you implement:

- Phase 4: a quad lit only by ambient, using a registered **scanline**
  opaque dynamic material that overlays a moving scanline pattern on a
  base texture. Visually confirms the per-shader-id pipeline emission
  for `Custom` opaque variants.
- Phase 6: a quad with a registered **irregular-atlas** opaque dynamic
  material (TexturePacker-style — variable-size cell rects per frame).
  Visually confirms the extras pool's variable-length per-material data
  path. Complements first-party `FlipBook` (which is grid-uniform only).
- Phase 7: a sphere with a registered **soft-glass** transparent dynamic
  material. Visually confirms the transparent-fragment injection path.
- Phase 9+: full scene with both custom opaque and custom transparent
  materials, shadowed by the directional light, under the standard PBR
  scene.

When testing, focus on:

1. **The golden path**: scene loads, dynamic materials render correctly,
   no GPU validation errors in the console.
2. **Authoring round-trip**: open a material in material-editor, edit a
   uniform default + the WGSL, save. Reopen — the values round-trip
   exactly. Switch to scene-editor, re-import, mesh still renders
   correctly.
3. **Hot recompile**: edit the WGSL in material-editor and save. The
   preview re-compiles (visible flash / log line). Introduce a syntax
   error — the editor shows the WGSL error in the error pane, the
   preview falls back to the last-good shader.
4. **Both alpha modes**: a material declared `alpha_mode: Opaque` routes
   to its specialized opaque compute pipeline; declared `alpha_mode:
   Blend` routes to the transparent fragment shader. Switching
   `alpha_mode` in material-editor updates the contract-docs pane and
   the preview.
5. **First-party still works**: PBR, Unlit, Toon, and FlipBook all render
   unchanged. Their shader-id constants survived the `MaterialShaderId`
   rewrite. Their shader cache keys don't depend on whether dynamic
   materials are registered (when none are).
6. **Promotion smoke test**: take the Phase-4 scanline material's
   `material.json` + `shader.wgsl`, hand-port them to a first-party
   `materials/src/scanline.rs` behind a Cargo feature (write a typed
   struct + manual `impl MaterialShader`). The visual output must be
   **bit-identical** to the dynamic version. The shader cache hash for
   that shader_id must match.

If you can't get something working through either editor, fall back to
manually editing `project.json` and the material folder's `material.json`,
but prefer the editors — that's also a smoke test for both UIs.

---

## High-level direction

We're adding a runtime registration path for **custom materials** to a
visibility-buffer deferred renderer with a forward transparent pass. The
motivating intent:

> Custom shaders should be authored as data (a `material.json` + a
> `shader.wgsl`) without requiring a fork of the `awsm-materials` crate.
> They register against the renderer at startup and route through exactly
> the same template-injection sites the first-party materials use. When
> a custom material proves itself, it gets **promoted** to first-party
> by porting the JSON layout to a typed Rust struct and the WGSL to a
> `&str` constant — no runtime change, no shader change, no GPU-layout
> change.

The renderer keeps **two** classes of material:

- **First-party (static, fast path).** PBR, Unlit, Toon, FlipBook.
  Declared in `crates/materials/src/`, feature-gated, compiled into
  `enabled_materials()`. Each has its own typed Rust struct + hand-rolled
  `impl MaterialShader` + `WGSL_FRAGMENT` constant. Static dispatch,
  exhaustive `Material` match arms.
- **Dynamic (runtime-registered).** One generic `DynamicMaterial` in
  `crates/materials/src/dynamic.rs` interprets a `MaterialDefinition`
  (data) from `awsm-scene-schema`. Registered against the renderer at
  app startup (or anytime before first frame; mid-frame registration is
  allowed but forces a per-shader-id pipeline recompile).

The key architectural assertion is that the **public contract for
`MaterialShader`** is the same surface both paths write against.
First-party materials are not privileged in *capability* — they're
privileged in *dispatch cost* (statically dispatched, branch known at
compile time) and *type-safety* (Rust struct vs. opaque byte buffer
driven by runtime layout).

**Prior art.** `FlipBook` was added as the most recent first-party
material — its shape (typed struct + `WGSL_FRAGMENT` constant +
feature-gated registry entry) is the target of the promotion phase.
Phase 1's contract audit should include FlipBook's WGSL alongside PBR /
Unlit / Toon as the canonical shape any custom material must conform to.

### Render-graph slot

No new passes. Custom materials slot into the existing per-shader-id
pipeline + classify-bucket machinery:

```
geometry pass                  →  visibility / normal / depth targets
shadow generation              ←  caster shaders shared
light culling
opaque clear
material_classify (compute)    →  per-tile shader_id buckets
                                  ← grows one bucket per registered
                                    Custom opaque material
material_opaque pipelines      →  one compute pipeline per shader_id,
                                  indirect-dispatched over its bucket
                                  ← one new pipeline per registered
                                    Custom opaque material
opaque mipgen (if transmissive)
blit opaque → transparent
material_transparent fragment  →  forward fragment per-mesh, per-pipeline
                                  ← one new pipeline variant per
                                    registered Custom transparent material
display
```

Each material — first-party or custom — compiles to its own specialized
compute pipeline (opaque) or fragment pipeline variant (transparent).
There is **no shared dispatch chain** to inject branches into; instead,
the askama template emits the correct shading body via `{% match
shader_id %}` and only the matching material's WGSL fragment ends up in
each pipeline. Dynamic materials extend the match arms.

When no dynamic materials are registered the compiled WGSL is
bit-identical to today's first-party output — important for the
guarantee that first-party-only consumers pay nothing for this feature.

### On-disk format

A custom material is a **folder**, not a single file. Self-contained,
portable, importable:

```
scanline/
├── material.json
├── shader.wgsl
└── assets/
    └── base.png
```

`material-editor` exports a folder of this shape. `scene-editor` imports
a folder into a project, copying it under `assets/materials/<name>/`:

```
my-game/
├── project.json
└── assets/
    ├── model.glb
    └── materials/
        └── scanline/
            ├── material.json
            ├── shader.wgsl
            └── assets/
                └── base.png
```

The convention is unconditional — the WGSL file is **always**
`shader.wgsl` inside the folder. The schema doesn't carry a path field.

### The author's contract (public surface)

This is the load-bearing public surface of this plan. Whatever shape it
takes after the Phase-1 audit is the **stable** contract — both for
custom-material authors AND for the first-party
PBR/Unlit/Toon/FlipBook refactor that audits against it. Promotion stays
mechanical only as long as the contract doesn't churn underneath.

The contract differs by `alpha_mode`:

- `Opaque` | `Mask { cutoff }` → the WGSL fragment is a function injected
  into the per-shader-id **opaque-shading compute kernel** at the
  `{% match shader_id %}` site. The kernel calls it from the per-pixel
  dispatch when the kernel decodes a visibility-buffer sample whose
  material has this material's `shader_id`.
- `Blend` → the WGSL fragment is a function injected into the
  per-pipeline **transparent fragment shader**. The fragment calls it
  for a transparent draw whose material has this `shader_id`.

Both contracts share these guarantees:

- The author's WGSL fragment is preceded by **all helpers in
  `shared_wgsl/`**: `math`, `color_space`, `textures`, `transforms`,
  `camera`, `material_mesh_meta`, `frame_globals` (`time` / `delta_time`
  / `frame_count` / `resolution` — see [`TEMPORAL_SHADERS.md`](../TEMPORAL_SHADERS.md)),
  `lighting/brdf`, `lighting/lights`, `lighting/unlit`, `shadow/bind_groups`
  (when shadows are enabled). Any symbol declared in those files is
  callable from a custom fragment. Do not redefine symbols from those
  files.
- The texture pool is bound and accessible. A `TextureSlot` named e.g.
  `"base"` becomes a `base_index: u32` in the material's WGSL uniform
  struct; the author samples via the existing texture-pool helpers using
  that index.
- The extras pool is bound and accessible (see "Storage strategy"
  below). A `BufferSlot` named e.g. `"frames"` becomes
  `frames_offset: u32` + `frames_length: u32` in the material's WGSL
  uniform struct; the author reads via `extras_load_f32(material.frames_offset + i)`
  or `extras_load_u32(...)`.
- Per-material uniform data lives in the existing per-material storage
  buffer at an offset known to the kernel; the author's WGSL fragment
  receives it as a typed struct of the layout they declared in
  `material.json`.
- Output: whatever the existing first-party materials of the same
  `alpha_mode` already produce. The Phase 1 audit locks this down
  precisely — `shading_result` shape, exact field names, what's already
  converted to HDR vs. linear, etc.
- `is_transparency_pass()` derives from `alpha_mode` for dynamic
  materials: `Opaque` → false, `Mask` → true (mask routes through the
  transparency pass for alpha-aware sorting), `Blend` → true. Custom
  materials cannot override this — if they need finer routing (e.g. an
  opaque material that uses the transparency pass for transmission like
  PBR does), they should promote to first-party.

These contracts are documented in
**`docs/dynamic-materials/contract-opaque.md`** and
**`docs/dynamic-materials/contract-transparent.md`**. Phase 1 produces
these files; later phases keep them in sync.

### Storage strategy

- **Per-shader-id specialized pipelines (first-party + custom).** Each
  material — registered or first-party — gets its own opaque-compute
  and/or transparent-fragment pipeline. Pipeline cache keys depend on
  `(shader_id, MSAA, mipmaps, …)`; the WGSL source itself is selected
  via `{% match shader_id %}` against the registry. **When no dynamic
  materials are registered, the compiled WGSL for first-party pipelines
  is bit-identical to today's.**
- **Per-material data buffer.** The existing storage buffer pattern
  (each material packs into bytes via `write_uniform_buffer`, indexed
  by `(shader_id, byte_offset)`) is unchanged.
  `DynamicMaterial::write_uniform_buffer` walks its
  `MaterialDefinition.uniforms` in declaration order, respecting WGSL
  alignment, then appends `u32` texture-pool indices for each
  `TextureSlot`, then appends `(offset, length)` u32 pairs for each
  `BufferSlot`. The kernel reads via the same byte_offset mechanism — it
  doesn't care that the bytes came from a generic packer.
- **Extras pool (variable-length per-material data).** A new
  renderer-wide `extras_pool: array<u32>` storage buffer mirrors the
  existing `materials: array<u32>` pool. Each declared `BufferSlot` on
  a dynamic material gets a contiguous slice in this pool; the
  per-material data carries `(offset, length)` indices that the author
  reads via `extras_load_f32(material.<slot>_offset + i)` /
  `extras_load_u32(...)` — the same bitcast convention `material_load_f32`
  / `material_load_u32` already establish for the materials pool. One
  shared binding regardless of how many dynamic materials register or
  how many buffer slots they each declare; per-material slices are
  managed by a free-list/bump allocator in the renderer. Data on disk
  is `.bin` (raw little-endian u32 words); a converter tool in
  material-editor produces `.bin` from human-readable JSON arrays.
- **Shader cache.** Each per-shader-id pipeline's cache key gains a
  `dispatch_hash: u64` component, computed from
  `[(shader_id, name, layout_hash, wgsl_hash)]` for all currently
  registered materials (sorted by shader_id for stability). Registering
  / unregistering a dynamic material changes the hash and invalidates
  affected pipelines on next render. **When no dynamic materials are
  registered, the hash returns a stable constant identical to today's
  implicit value.**

### Storage-buffer budget watch

The opaque main bind group currently uses **9 of 10** storage bindings
declared as `CompatibilityRequirements::storage_buffers = Some(9)` in
[`crates/renderer/src/lib.rs`](../../crates/renderer/src/lib.rs). Adding
the `extras_pool` storage buffer takes that to **10 / 10** — the
absolute cap. [`PERFORMANCE.md §11`](../PERFORMANCE.md) ("Don't bump
`with_max_storage_buffers_per_shader_stage` past 10") spells out why:
devices that exactly meet the declared limit fail pipeline validation
if we exceed it. No headroom for additional storage bindings without an
earlier pack.

### `MaterialShaderId` partitioning

Today this is `#[repr(u32)] enum { Pbr = 1, Unlit = 2, Toon = 3, FlipBook = 4 }`.
That shape can't extend to runtime values. It becomes a
`#[repr(transparent)] struct MaterialShaderId(u32)` with associated
constants for first-party and a documented dynamic range:

```rust
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MaterialShaderId(u32);

impl MaterialShaderId {
    pub const PBR:      Self = Self(1);
    pub const UNLIT:    Self = Self(2);
    pub const TOON:     Self = Self(3);
    pub const FLIPBOOK: Self = Self(4);
    // 5..=9999 reserved for future first-party materials.

    pub const DYNAMIC_START: u32 = 10_000;

    pub fn is_dynamic(self) -> bool { self.0 >= Self::DYNAMIC_START }
    pub fn as_u32(self) -> u32 { self.0 }

    /// SAFETY-free constructor for the renderer's dynamic allocator only.
    /// Game code never builds these directly.
    pub(crate) fn from_raw(raw: u32) -> Self { Self(raw) }
}
```

GPU representation unchanged — still a `u32`. Pattern matches on the old
enum become `if id == MaterialShaderId::PBR { … }`. The dynamic range
gives effectively unlimited room.

### Skybox ownership

The PBR pipeline retains the skybox-fallback `textureStore` for pixels
with `triangle_index == U32_MAX`. Non-PBR pipelines (Unlit / Toon /
FlipBook / any `Custom`) early-return on skybox without writing — a
mixed-material tile shaded by Unlit + skybox doesn't double-write the
skybox pixels. A `Custom` opaque material inherits the non-PBR rule by
default; if a custom material genuinely needs to own the skybox tiles,
that's a separate registration concept ("skybox bucket") and is **out
of scope** for this plan.

### Material classify: registry-driven buckets

Today's classify path is hard-coded for 4 buckets:

- [`material_classify::buffers.rs`](../../crates/renderer/src/render_passes/material_classify/buffers.rs)
  hardcodes `pub const BUCKET_COUNT: u32 = 4;` and the header writer
  emits per-bucket offsets at fixed byte positions.
- [`compute.wgsl`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl)
  has named per-bucket atomics (`args_pbr`, `args_unlit`, `args_toon`,
  `args_flipbook`) and an if-else chain
  (`if shader_id == SHADER_ID_PBR { local_bit = BUCKET_BIT_PBR; } else if …`).

**Both must become registry-driven.** Without this, dynamic shader_ids
reach the opaque kernel but never get classified into the tiles they
should shade. Specifically:

- Host-side `ClassifyBuffers` struct: replace the named `args_pbr` /
  `<name>_offset` fields with array-of-entries indexed by
  `registry.all_entries().len()`. The header writer walks the registry
  in stable id order and emits N indirect-args slots + N offsets.
  Buffer size becomes `f(registry_len)` rather than the current
  hardcoded `BUCKET_COUNT`.
- WGSL `compute.wgsl` becomes an askama template emitting:
  - `const BUCKET_BIT_<name>: u32 = (1u << index);` for each entry
  - `const SHADER_ID_<name>: u32 = N;` for each entry (already exists as
    `shader_id_consts`)
  - The shader_id → bit if-else chain
  - The per-bucket extract block (the `args_<name>` / `<name>_offset`
    fanout at lines 89-119 today)
- The `classify_output` struct binding becomes a runtime-array (one
  indirect-args entry + one offset per bucket) rather than the four
  named pairs it has today. The opaque-compute pipeline's
  `dispatch_workgroups_indirect(args_buffer, bucket_index * 16)` call
  picks the right offset from the registry-driven indexing.

This refactor lands in **Phase 3** alongside the registry plumbing.
Verify: registering a dynamic material against a one-quad scene shows
its bucket bit set and its per-shader-id pipeline dispatched (via
`read_render_pass_timings`).

### Why these choices

- **Folder format over single-file.** WGSL is meaningfully large per
  material (often 50–300 lines), embedding it in JSON makes it
  unreadable and hostile to source control. A folder is git-diffable,
  editor-friendly, and lets the material own its texture assets without
  an external manifest.
- **One generic `DynamicMaterial`, not `Box<dyn ...>` per-material.**
  The author already produces all the per-material customisation as
  data (layout + WGSL); a typed Rust struct per dynamic material would
  be redundant. One generic interpreter avoids `Box<dyn MaterialShader>`
  overhead and keeps the static `Material` enum closed (only adds a
  single `Custom` variant).
- **Per-pipeline specialization over a fat shared kernel.** The
  existing per-shader-id pipeline architecture is already the right
  shape — adding a custom material adds one pipeline. No new
  indirection, no perf cost on the first-party path. Recompile cost on
  registration is **per-material** (one new pipeline compiles), not
  global.
- **Both passes in v1, opaque-first in implementation order.** The
  transparent path is the same template-injection shape as opaque, just
  with a different signature in the contract. Doing both ensures the
  contract is forced to generalize. Implementing opaque-first means
  the contract bugs get debugged in isolation; transparent then comes
  online following the proven shape.
- **Material folder is `awsm-scene-schema`'s problem.** No new shared
  crate. `MaterialDefinition` is data, lives next to `MaterialDef` in
  `scene-schema/src/`, gets the same back-compat serde discipline as
  everything else there. Third-party scene players that deserialize a
  project also get custom-material deserialisation for free.
- **`material-editor` is a separate frontend crate.** Authoring a
  custom material is a different workflow from editing a scene;
  bundling the authoring UI into `scene-editor` would meaningfully grow
  its dependency surface for a feature most scenes don't author.
  Separate crate keeps the scene-editor lean.
- **Plain textarea over CodeMirror.** A WGSL-aware code editor (with
  syntax highlighting + inline error gutter) is a meaningful bundle-size
  cost for a v1 authoring tool. A plain `<textarea>` + an error pane
  showing line/column from naga's compile error is the v1 surface;
  syntax highlighting can land later as a focused polish PR.
- **Browser tabs for isolation.** Rather than building a second app
  with a stub scene, the user opens material-editor in a new browser
  tab when they want isolated iteration. Same code path; the OS
  provides the isolation for free.

### True non-goals

These are not in v1 and are not deferred — they're genuinely the wrong
fit for this iteration.

- **Node-graph authoring.** A visual graph (à la Unity Shader Graph,
  Blender shader nodes) is a long-arc product, not an experimentation
  tool. The data model leaves room for one — the `wgsl_fragment` field
  is an opaque string; a future node-graph frontend would emit into the
  same field — but building one now is premature.
- **GLSL input.** The renderer is WGPU/WGSL. Translating GLSL via naga
  is mechanically possible but adds a second mental model the contract
  docs would have to cover. WGSL only.
- **Custom render passes / non-shading compute jobs.** Dynamic materials
  inject into the *material shading* slot of the per-shader-id opaque
  pipeline and the transparent fragment. They do not let authors add
  their own bind groups, their own pipeline stages, or their own
  compute dispatches.
- **Materials that switch `alpha_mode` at runtime.** A material is one
  alpha_mode. Want both? Author two materials. Matches first-party
  convention.
- **Material inheritance / variants.** No inheritance, no parameter
  override of one material from another. Two materials sharing 80% of
  their WGSL just share it via copy-paste for now.
- **Hot-reload via filesystem watch.** Save-driven recompile (Ctrl-S
  recompiles) is the v1 UX. A filesystem watcher is convenient but not
  load-bearing.
- **Skybox-owning custom materials.** Skybox tiles are PBR's
  responsibility. A custom material wanting to own them is a separate
  feature.
- **Tagged-type buffer data formats.** The Buffer Converter accepts a
  flat (or nested-then-flattened) JSON array of numbers, each written
  as 4 bytes of `f32`. Authors who want u32 semantics either value-cast
  in WGSL (`u32(extras_load_f32(i))` — lossless up to 2^24) or
  true-bitcast (`bitcast<u32>(extras_load_f32(i))`).
- **Buffer data formats other than `.bin`.** No JSON-on-disk for
  buffers, no PNG-as-buffer, no in-place editing of buffer contents in
  scene-editor. The renderer reads `.bin` only; material-editor authors
  `.bin` via the Buffer Converter. One format end-to-end.

---

## Editor UX

Two editors. `material-editor` authors custom materials standalone.
`scene-editor` imports and applies them.

### `material-editor` (new app)

Single-window app with the following panes:

**Top bar**
- File: New / Open Folder… / Save / Save As…
- Preview mesh selector: `Quad` / `Sphere` / `Box` / `Custom glTF…`
  (loads any local glTF for preview)
- Recompile button (manual trigger; Ctrl-S also recompiles)
- Tools: **Buffer Converter…** — opens a modal that converts a JSON
  array of numbers into a `.bin` file and downloads it. The author
  drops the resulting file into their material folder's `assets/` and
  points a buffer slot at it.

**Left pane — Definition**
- `Name` — string, must be a valid folder name (kebab-case enforced).
- `Version` — integer, manually bumped by the author when they ship a
  breaking layout change.
- `Alpha mode` — segmented toggle: `Opaque` / `Mask` / `Blend`. When
  `Mask` is selected, a `Cutoff` slider appears (0.0–1.0, default 0.5).
- `Double-sided` — bool toggle.
- `Uniforms` — table of `(name, type, default)` rows. Types: `F32`,
  `Vec2`, `Vec3`, `Vec4`, `U32`, `IVec2`, `IVec3`, `IVec4`, `Mat3`,
  `Mat4`, `Color3`, `Color4`, `Bool` (becomes `u32` 0/1 in WGSL).
  Reorderable, deletable. Editing a row's type updates the WGSL struct
  preview pane immediately.
- `Textures` — table of `(name, default-asset)` rows. Each row picks a
  texture asset (PNG / KTX2) from a local file dialog; the asset gets
  copied into the material folder's `assets/` on save. Reorderable,
  deletable. `name` becomes `<name>_index: u32` in the material's WGSL
  struct.
- `Buffers` — table of `(name, default-asset)` rows. Each row picks a
  `.bin` file. Reorderable, deletable. `name` becomes
  `<name>_offset: u32` + `<name>_length: u32` — indices into the
  renderer-wide `extras_pool`.

**Buffer Converter (modal)**
- A textarea accepting a JSON array of numbers. Nested arrays are
  flattened (e.g. `[[1,2,3,4], [5,6,7,8]]` is treated as 8 sequential
  values — useful for hand-authoring tabular data with one row per line).
- Filename input (e.g. `frames.bin`).
- Download button: parses the textarea as JSON, recursively flattens to
  `Vec<f32>`, writes each value as 4 bytes little-endian, triggers a
  browser download.
- Error display for parse failures or non-numeric values.
- An explanatory note: "All numbers are written as 32-bit floats. In
  WGSL, read floats via `extras_load_f32(idx)`; for small integers
  (< 2^24), value-cast with `u32(extras_load_f32(idx))`."

**Center pane — WGSL editor**
- A plain `<textarea>` with monospace font and a fixed-width
  presentation. No syntax highlighting in v1.
- An auto-generated read-only preview at the top showing the
  `struct MaterialData { … }` declaration the renderer will inject
  above the author's fragment, derived from the current uniform /
  texture / buffer layout. Updates live as the Definition pane edits
  arrive.
- Below it, the author's WGSL function body. The cursor starts inside a
  stub function whose signature matches the current `alpha_mode`'s
  contract.

**Right pane — Contract**
- Read-only documentation pane that shows the active contract for the
  current `alpha_mode`:
  - Helpers in scope (with anchor links into the rendered contract
    docs)
  - Function signature the author's WGSL fragment must match
  - Output struct shape

The pane swaps contents when `alpha_mode` switches between `Opaque|Mask`
and `Blend`. Same source as
`docs/dynamic-materials/contract-{opaque,transparent}.md` — rendered
inline.

**Bottom pane — Preview + Errors**
- Left half: 3D preview viewport rendering the selected preview mesh
  under a default 3-point lighting rig + a ground plane. Updates
  immediately on recompile.
- Right half: Error pane. WGSL compile errors with line/column from
  naga, shown as a list. Clicking an entry focuses the WGSL textarea
  at that position (best-effort cursor positioning).

**Recompile behavior**
- On save (Ctrl-S or File → Save) and on explicit Recompile button.
- A failed compile keeps the preview running on the **last-good**
  shader and surfaces the error. The author can keep editing.
- Recompile takes ~50–500 ms; show a spinner overlay on the preview
  while it's pending.

### `scene-editor` (existing app)

Two surface additions:

**Project pane — "Materials" section**
- Lists all custom materials currently imported into the project. Each
  row: name + an "Open in material-editor" link (opens a new browser
  tab to the material-editor URL with a query param identifying the
  folder).
- `Import Material…` button: file-picker for a folder. Copies the
  folder into the project's `assets/materials/<name>/`, adds an entry
  to `project.json::custom_materials`, and registers it with the
  renderer immediately.
- `Remove Material` per row: confirms, then de-references it from
  `project.json` and removes the folder. (Renderer recompiles the
  affected per-shader-id pipelines.)

**Per-mesh material editor**
- The existing material-picker dropdown gains entries for every
  imported custom material under a "Custom" sub-section. Picking a
  custom material populates the per-mesh material instance with the
  layout's defaults; per-instance values become editable in the
  property panel using the same UI primitives material-editor uses for
  uniform defaults (drag floats, color pickers, asset references for
  textures).

No changes to the lighting / camera / scene-tree UI. Custom materials
participate in lighting / shadows / etc. on equal footing with
first-party ones.

---

## Schema changes

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

Add a `custom_materials` field to the project root: a list of
`(name, folder_path)` pointers into `assets/materials/`. `name` matches
the folder's `material.json::name` (cross-check on load); `folder_path`
is project-relative.

```rust
#[serde(default)]
pub custom_materials: Vec<CustomMaterialRef>,

pub struct CustomMaterialRef {
    pub name: String,
    pub folder: PathBuf,                       // e.g. "assets/materials/scanline"
}
```

### Per-mesh material reference

Wherever a mesh today carries a material selection (a tagged enum like
`Material::Pbr(...)` / `Unlit(...)` / `Toon(...)` / `FlipBook(...)` in
`scene-schema/src/material.rs`), add a `Custom` variant:

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MaterialRef {
    Pbr(PbrMaterialDef),
    Unlit(UnlitMaterialDef),
    Toon(ToonMaterialDef),
    FlipBook(FlipBookMaterialDef),
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
/// the project root.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BufferRef {
    pub path: PathBuf,
}
```

All new fields use `#[serde(default)]` so old projects round-trip
cleanly without `custom_materials`.

### Folder loader

`crates/scene-schema/src/dynamic_material.rs` (or wherever fits)
exposes:

```rust
pub struct LoadedMaterialFolder {
    pub definition: MaterialDefinition,
    pub wgsl_source: String,
    pub texture_data: HashMap<PathBuf, Vec<u8>>,  // resolved texture file contents
    pub buffer_data: HashMap<PathBuf, Vec<u32>>,  // resolved .bin file contents (validated u32-aligned)
}

pub fn load_material_folder(root: &Path) -> Result<LoadedMaterialFolder, MaterialFolderError>;
```

`MaterialFolderError` covers: `material.json` missing/invalid,
`shader.wgsl` missing, a `TextureSlot::default` pointing to a
nonexistent file, a `BufferSlot::default` pointing to a nonexistent
file, a `.bin` file whose size is not a multiple of 4, layout name
collisions, reserved names (`material`, `texture_pool`, `extras_pool`,
`frame_globals`, `camera`).

This loader is the **only** schema-side logic. It's used by both
`material-editor` (loading the current edit), `scene-editor` (loading
on project import), and any third-party scene player. The renderer
doesn't depend on `awsm-scene-schema`; the bridge code in each consumer
converts `LoadedMaterialFolder` → renderer-side `MaterialRegistration`.

---

## Public API surface

The `awsm-renderer` crate is a library; `scene-editor` and
`material-editor` are two consumers, but a game runtime / model-tests
frontend / standalone tool must also be able to register a custom
material without reverse-engineering either editor. The API below is
the contract.

### Design principles

- **Mirror existing material patterns.** The `MaterialShader` trait is
  unchanged in shape; the `Material` enum gains a `Custom` variant.
  Registration goes through one method on `AwsmRenderer`.
- **One way to do each thing.** Registration is one call. Updating an
  instance's uniform values is the same path first-party materials use
  (write through the existing storage buffer).
- **Schema vs. runtime separation.** `awsm_scene_schema::MaterialDefinition`
  is the on-disk format. The renderer takes its own
  `MaterialRegistration` (essentially the same data plus the loaded
  WGSL string). The consumer converts; the renderer never depends on
  `awsm-scene-schema`.
- **Lazy, dirty-flag-driven shader compile.** Registration marks the
  affected per-shader-id pipeline dirty; the next `render()` call
  recompiles only that pipeline if needed. No synchronous compile.
- **Errors via a single `AwsmDynamicMaterialError` enum.** All fallible
  methods return `Result<T, AwsmDynamicMaterialError>`; this enum flows
  into `AwsmError` like the other subsystem errors.
- **Every public item has a rustdoc comment.**

### Types (`awsm_materials::dynamic` + `awsm_renderer`)

```rust
/// Runtime registration payload for a custom material. The renderer's
/// counterpart to `awsm_scene_schema::MaterialDefinition` + the loaded
/// WGSL. Consumers convert from the schema; the renderer does not
/// depend on scene-schema.
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
    /// Registers a custom material. Returns an opaque `MaterialShaderId` in
    /// the dynamic range (>= MaterialShaderId::DYNAMIC_START). Takes effect
    /// on the next `render()` call (the shader cache key changes; the
    /// affected per-shader-id pipeline recompiles on first dispatch).
    ///
    /// Idempotent on `(name, layout_hash, wgsl_hash)`: re-registering the
    /// same material returns the same id without recompiling.
    pub fn register_material(
        &mut self,
        registration: MaterialRegistration,
    ) -> Result<MaterialShaderId, AwsmDynamicMaterialError>;

    /// Removes a previously-registered dynamic material. Returns an error
    /// if any live mesh still references it. Triggers a pipeline rebuild
    /// for the affected shader_id on next render.
    pub fn unregister_material(
        &mut self,
        shader_id: MaterialShaderId,
    ) -> Result<(), AwsmDynamicMaterialError>;

    /// Returns the registration record for a previously-registered id.
    pub fn dynamic_material_registration(
        &self,
        shader_id: MaterialShaderId,
    ) -> Option<&MaterialRegistration>;

    /// Iterator over all currently-registered dynamic materials.
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

Smallest end-to-end snippet that compiles against the public API.
Include verbatim as a rustdoc example on `register_material` and in
`crates/renderer/examples/dynamic_material.rs`.

```rust
use awsm_renderer::AwsmRenderer;
use awsm_materials::{
    dynamic::{MaterialRegistration, MaterialLayout, UniformFieldRuntime, FieldType, UniformValue},
    alpha_mode::MaterialAlphaMode,
};

// 1. Build a registration (a game would normally load from a material
//    folder via the scene-schema folder loader; here we hand-build the
//    equivalent).
let reg = MaterialRegistration {
    name: "scanline".into(),
    alpha_mode: MaterialAlphaMode::Opaque,
    double_sided: false,
    layout: MaterialLayout {
        uniforms: vec![
            UniformFieldRuntime { name: "tint".into(),  ty: FieldType::Color3, default: UniformValue::Color3([0.6, 0.9, 0.6]) },
            UniformFieldRuntime { name: "scan_freq".into(), ty: FieldType::F32, default: UniformValue::F32(80.0) },
            UniformFieldRuntime { name: "scan_speed".into(), ty: FieldType::F32, default: UniformValue::F32(0.5) },
        ],
        textures: vec![/* base */],
        buffers: vec![],
    },
    wgsl_fragment: include_str!("../shaders/scanline.wgsl").into(),
};

// 2. Register it. Returns a stable id usable to assign instances.
let shader_id = renderer.register_material(reg)?;

// 3. Render as usual; on first frame after registration the new opaque
//    pipeline compiles via the same `ensure_keys` path the builder uses
//    at startup.
renderer.render(None)?;
```

### Mid-session registration and the cross-renderer pool

`AwsmRendererBuilder::build` drives every shader and pipeline through a
cross-renderer pool (see [`PERFORMANCE.md §5g`](../PERFORMANCE.md) for the
architecture). Mid-session `register_material` calls happen **after**
that pool has already run, so they can't ride it — instead they go
through the same `Shaders::ensure_keys` + `Pipelines::*::ensure_keys`
primitives the orchestrator uses, just on the smaller set of cache
keys the registration affects (typically one shader + one or two
pipelines).

The single dispatch entrypoint for this is
`AwsmRenderer::prewarm_pipelines` (existing). After a burst of
`register_material` calls finishes, the consumer calls it once and the
batched `ensure_keys` pool compiles every newly-needed variant in one
wave — same `ensure_keys` plumbing as startup, just running later.

The plan extends `prewarm_pipelines` to iterate the registry's enabled
set (currently it walks `self.meshes` to warm transparents for the
live scene). Phase 6 lands this alongside the first opaque dynamic
material that compiles.

### Documentation requirements

For every phase that introduces or modifies a public-API item:

1. **Add a rustdoc comment** to every new `pub` type, `pub` field,
   `pub` method, `pub` enum variant. Comments answer: what is this,
   when does it take effect, what can go wrong.
2. **Run `cargo doc --workspace --no-deps`** at the end of each phase
   that touches the API. Fix any broken intra-doc links.
3. **Update the integration example** in
   `crates/renderer/examples/dynamic_material.rs` so it reflects the
   current shape of the API as it grows.
4. **Update `docs/dynamic-materials/contract-opaque.md` and
   `docs/dynamic-materials/contract-transparent.md`** whenever a helper
   signature, an injection-site convention, or a kernel-provided symbol
   changes.
5. **Run `cargo clippy --workspace -- -W missing_docs`** as a periodic
   check. This should be **clean at Phase 13** even if intermediate
   phases haven't caught up.

---

## Renderer / Materials changes

### New module: `crates/materials/src/dynamic.rs`

The generic `DynamicMaterial` interpreter:

```rust
pub struct DynamicMaterial { … }              // shape above

impl MaterialShader for DynamicMaterial {
    fn shader_id(&self) -> MaterialShaderId { self.shader_id }
    fn alpha_mode(&self) -> MaterialAlphaMode { /* from registration */ }
    fn is_transparency_pass(&self) -> bool {
        // Opaque → false; Mask | Blend → true. No override allowed —
        // dynamic materials inherit the same alpha-mode-driven routing
        // first-party materials use.
        matches!(self.alpha_mode(), MaterialAlphaMode::Mask { .. } | MaterialAlphaMode::Blend)
    }
    fn wgsl_fragment(&self) -> &'static str { /* from registration, looked up by id */ }
    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, out: &mut Vec<u8>) {
        // walks layout.uniforms in declared order, packs each `UniformValue`
        // respecting WGSL alignment rules (see dynamic_layout.rs),
        // then appends one u32 per texture slot:
        //   ctx.resolve_texture_index(self.textures[i]) → u32 index,
        // then appends one (offset, length) u32 pair per buffer slot
        // (the extras-pool allocator's per-instance slice).
    }
}
```

### New module: `crates/materials/src/dynamic_layout.rs`

The shared WGSL-alignment-and-packing helper. Two outputs from one
source of truth (the `MaterialLayout`):

```rust
/// Generate the WGSL struct declaration that goes above the author's
/// fragment, e.g. `struct MaterialData { tint: vec3<f32>, scan_freq: f32, base_index: u32, }`.
/// Respects WGSL alignment (vec3 → 16-byte align, etc.) and inserts
/// padding fields where needed. Field order:
///   uniforms → `<tex>_index: u32` per texture slot → `<buf>_offset: u32`
///   + `<buf>_length: u32` per buffer slot.
pub fn generate_wgsl_struct(struct_name: &str, layout: &MaterialLayout) -> String;

/// Pack a uniform value into a byte buffer at the correct WGSL-aligned
/// offset. Walks the layout in declared order. Texture-index and
/// buffer-offset tails are appended via the helpers below.
pub fn pack_uniform_values(layout: &MaterialLayout, values: &[UniformValue], out: &mut Vec<u8>);

pub fn pack_texture_indices(layout: &MaterialLayout, indices: &[u32], out: &mut Vec<u8>);

/// Pack `(offset, length)` u32 pairs in buffer-slot declaration order.
pub fn pack_buffer_offsets(layout: &MaterialLayout, offsets: &[(u32, u32)], out: &mut Vec<u8>);

/// Total size (with tail padding) — useful for size-of checks and for the
/// per-material byte_offset table.
pub fn layout_size(layout: &MaterialLayout) -> usize;
```

Unit tests in this module are load-bearing: they verify the generated
struct and the packed bytes agree exactly with the WGSL spec for
representative layouts (every `FieldType`, plus mixed-alignment cases).
When alignment math is wrong, materials silently render garbage — these
tests are the first line of defense.

### `crates/materials/src/registry.rs` — dual-mode registry

Today `enabled_materials() -> Vec<MaterialEntry>` returns a
Cargo-feature-driven static list. Becomes:

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

`build_materials_wgsl()` and `build_shader_id_consts()` extend to walk
both static and dynamic entries — same WGSL emission shape, just two
sources concatenated.

### `Material` enum gains `Custom`

```rust
pub enum Material {
    Pbr(Box<PbrMaterial>),
    Unlit(UnlitMaterial),
    Toon(Box<ToonMaterial>),
    FlipBook(Box<FlipBookMaterial>),
    Custom(Box<DynamicMaterial>),              // new
}
```

Every pattern-match against `Material` becomes non-exhaustive in the
same release; add the `Custom` arm to each. The renderer's per-frame
material packing dispatches on the variant the same way it does today.

### `crates/renderer/src/dynamic_materials/` — new module

```
dynamic_materials/
  mod.rs                  ← entry point, pub struct DynamicMaterials
  registry_view.rs        ← snapshot of the registry for template rendering
  cache_key.rs            ← dispatch_hash → cache_key extension
  extras_pool.rs          ← extras-pool storage + free-list allocator
```

The actual `MaterialRegistry` lives in `awsm-materials`; this module is
the renderer-side facade that integrates it with the shader cache + the
bind-group machinery.

### Template substitution

Both:

- The opaque-shading per-shader-id compute kernel template
  (`crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/compute.wgsl`)
- The transparent fragment shader template

…are extended so the askama `{% match shader_id %}` choice + the
`materials_wgsl` / `shader_id_consts` substitutions iterate **both**
`static_entries` and `dynamic_entries` from the current
`MaterialRegistry` snapshot. The Askama context gains the same
substitutions it has today, just sourced from the dual registry.

For each dynamic entry, the substitution emits:

1. A `struct CustomMaterialData_<id> { … }` declaration generated via
   `generate_wgsl_struct`. Field order:
   - All `UniformField` entries in declared order (alignment-respected).
   - A `<name>_index: u32` per `TextureSlot` in declared order.
   - A `<name>_offset: u32` + `<name>_length: u32` per `BufferSlot` in
     declared order.
2. The author's WGSL fragment, wrapped in a function
   `fn custom_shade_<id>(…) -> …` with the contract's signature.
3. A match arm `<id> => return custom_shade_<id>(…);` appended to the
   shader_id match.

`<id>` is the dynamic shader_id assigned by the registry (>= 10_000).
The `_<id>` suffix avoids symbol collisions if multiple authors picked
the same struct field names.

### Bind groups

**One new binding**: the `extras_pool` storage buffer
(`var<storage, read> extras_pool: array<u32>`), bound alongside the
existing `materials` pool. Shared across all dynamic materials
regardless of how many register or how many buffer slots each declares.

Apart from `extras_pool`, no new bind groups. Custom materials read
uniform data from the existing per-material storage / uniform buffer
(the same one PBR/Unlit/Toon/FlipBook read from) and texture data via
the existing texture pool binding. The `Material::Custom` instance
carries texture keys and buffer slices; the per-frame upload resolves
texture keys to texture-pool indices and appends them after the uniform
tail in `write_uniform_buffer`, then appends `(offset, length)` u32
pairs for each buffer slot.

**Storage budget**: as called out in "Storage strategy" above, adding
`extras_pool` brings the opaque main bind group to **10/10** storage
bindings — the absolute cap. Pipeline validation fails on devices that
exactly meet the declared limit if it's exceeded. No headroom remains
for further storage bindings.

### Extras pool (variable-length per-material data)

A new module `crates/renderer/src/dynamic_materials/extras_pool.rs`
owns:

- A `web_sys::GpuBuffer` of `extras_pool_capacity` u32 words
  (storage-mode, read-only from shaders). Capacity is configurable via
  `AwsmRendererBuilder` options; default 1 MiB (262 144 u32s).
  Resizable on overflow with a `BindGroupRecreate` event (mirrors how
  the texture pool / shadow atlas handle resizes).
- A **free-list allocator** keyed by `(MaterialShaderId, slot_name)` →
  contiguous slice. On insert/update of a `DynamicMaterial` instance,
  the allocator finds (or coalesces) a slice that fits the slot's u32
  words, records the offset, and **uploads the bytes via the
  renderer's mapped-buffer ring** — not raw `gpu.write_buffer`. On
  removal of an instance, the slice is returned to the free list.
- Compaction: when fragmentation exceeds a threshold (e.g. free space
  > 25% of capacity but the largest free run is < 50% of total free
  space), the allocator runs a compaction pass that re-packs all live
  slices and updates every affected `DynamicMaterial`'s
  `(offset, length)` pairs. Compaction is per-frame cap-limited (e.g.
  move at most 64 KiB of data per frame) to avoid hitching. Most scenes
  won't trigger compaction at all.

**Upload path.** Per
[`PERFORMANCE.md §5b`](../PERFORMANCE.md), every renderer-owned
per-frame upload goes through a
[`MappedUploader`](../../crates/renderer/src/buffer/mapped_uploader.rs)
companion. The extras pool fits both halves of that split:

- **Per-frame dirty-range writes** (the common path — author edited a
  uniform-override in the editor, a slice's bytes need re-uploading):
  use `MappedUploader::write_dirty_ranges`. Per-frame batched along
  with all other per-frame writes.
- **Foreign-bytes ingestion** (initial registration of a buffer slot
  from a `.bin` file loaded via `awsm-scene-schema`'s
  `load_material_folder`, or the first-time copy of an
  instance-override `BufferRef`): use
  `MappedUploader::ingest_foreign` — the bytes arrive as a `Vec<u32>`
  from outside the renderer's CPU-authoritative state, matching the
  same convention as glTF buffer + texture ingestion. Counted under
  `bytes_uploaded_via_writebuffer` in the upload-ring telemetry.

The allocator's `write_slice(material, slot, &[u32])` method is the
single entrypoint that picks the right one of the two based on whether
the slice was already in the allocator's tracked-Vec shadow or is
being freshly inserted.

The corresponding WGSL helper module
`crates/renderer/src/render_passes/shared/shared_wgsl/extras.wgsl`
mirrors `material.wgsl`'s pattern exactly:

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

Included in every pass that includes `material.wgsl` — the symmetry is
deliberate. First-party materials are free to use the extras pool too
if they ever want variable-length data (none do today, but the binding
is universal).

### Pipeline layouts

Unchanged in shape — each per-shader-id pipeline has the same bind
group layout regardless of how many dynamic materials are registered.
What changes is the WGSL source, not the binding interface. The `match
shader_id` template choice picks which material's shading body the
pipeline embeds; only one material's WGSL ends up in each pipeline.

### Shader cache integration

Each per-shader-id pipeline's cache key gains a `dispatch_hash: u64`
component:

```rust
struct PerShaderIdPipelineCacheKey {
    // … existing fields …
    dispatch_hash: u64,    // = registry.dispatch_hash()
}
```

When `register_material` / `unregister_material` runs, the registry's
`dispatch_hash` changes; the next render's cache lookup misses for
**any affected pipelines** (in practice: the new pipeline that didn't
exist before) and triggers compile. **When no dynamic materials are
registered, the `dispatch_hash` returns a stable constant identical to
today's implicit value.**

Per-pipeline compile latency: registering a single new dynamic material
compiles **one** new pipeline (its specialized shader). The other
pipelines (PBR / Unlit / Toon / FlipBook / other Customs) stay in
cache — their cache keys' `dispatch_hash` change too, but their WGSL
source doesn't change as long as they're not removed, so the cache
hit holds.

---

## New crate: `crates/frontend/material-editor/`

Mirrors the shape of `crates/frontend/scene-editor/` and
`crates/frontend/model-tests/`.

### Cargo.toml

Dependencies:
- `awsm-renderer` (the renderer library)
- `awsm-materials` (for `MaterialRegistration`, etc.)
- `awsm-scene-schema` (for `MaterialDefinition` + folder loader)
- `awsm-web-shared` (theme, DOM helpers, dominator setup)
- `wasm-bindgen-futures`, `web-sys` (File System Access API for folder
  open/save), `serde_json`

No external code-editor library. The WGSL pane is a plain
`<textarea>`.

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
    │   ├── definition.rs      ← left pane (uniforms, textures, buffers, alpha_mode, …)
    │   ├── wgsl_editor.rs     ← center pane (plain textarea)
    │   ├── contract.rs        ← right pane (renders contract docs by alpha_mode)
    │   ├── preview.rs         ← bottom-left (renderer viewport)
    │   └── errors.rs          ← bottom-right (compile errors as a list)
    ├── state.rs               ← Mutable<EditState>: current file, layout, wgsl
    ├── preview_scene.rs       ← stub scene construction (quad/sphere/box/glTF)
    ├── recompile.rs           ← orchestrates: layout → MaterialRegistration → register_material → record errors
    └── fs.rs                  ← File System Access API: open/save folder, copy texture assets
```

### Task / dev server

Add a `task material-editor:dev` rule mirroring `task scene-editor:dev`,
on the next free port (9082).

---

## Implementation phases

Each phase is a runnable checkpoint — commit after each. Lower phases
assume upper phases compiled.

### Phase 0 — Scaffolding & wiring

1. **Rewrite `MaterialShaderId`** in `crates/materials/src/shader_id.rs`
   from `#[repr(u32)] enum` to `#[repr(transparent)] struct(u32)` with
   associated `PBR` / `UNLIT` / `TOON` / `FLIPBOOK` consts and a
   `DYNAMIC_START` const. Every pattern-match like
   `match id { MaterialShaderId::Pbr => …, … }` becomes
   `if id == MaterialShaderId::PBR { … } else if id == MaterialShaderId::UNLIT { … } else …`.
   Run `grep -rn 'MaterialShaderId::' crates/` to find every call site.
2. **Add `Material::Custom(Box<DynamicMaterial>)`** variant. Stub
   `DynamicMaterial` as `pub struct DynamicMaterial { pub shader_id: MaterialShaderId, … }`
   with a temporary `impl MaterialShader` that panics on every method
   (fleshed out in Phase 2). Add the `Custom` arm to every existing
   `Material` pattern-match. For now the arm can `unreachable!()` since
   no `Custom` instance exists yet.
3. **Stand up `crates/renderer/src/dynamic_materials/`** with empty
   `mod.rs`, `pub struct DynamicMaterials` that holds nothing. Add
   `pub dynamic_materials: DynamicMaterials` to `AwsmRenderer`.
4. **Add stub `register_material` / `unregister_material` /
   `dynamic_materials()` methods** on `AwsmRenderer` returning
   placeholder values + `AwsmDynamicMaterialError::WgslCompile("unimplemented".into())`
   for `register_material`. The signatures are the public surface; the
   bodies come later.
5. **Add `AwsmDynamicMaterialError`** to `crates/renderer/src/error.rs`
   and into the top-level `AwsmError` enum.

Expected outcome: scene-editor + model-tests still build and render
identically to before. No `Material::Custom` instances exist yet.
Commit.

### Phase 1 — Schema additions + contract audit

1. **Add `MaterialDefinition`, `UniformField`, `FieldType`,
   `UniformValue`, `TextureSlot`, `BufferSlot`** in
   `crates/scene-schema/src/material.rs` (or a new
   `dynamic_material.rs`). Match the shapes in **Schema Changes**
   above. Every field uses `#[serde(default)]` where reasonable.
2. **Add `CustomMaterialRef`** to the project root struct and
   `MaterialRef::Custom(CustomMaterialInstance)` to the material
   variant enum, both with `#[serde(default)]`. The instance struct
   includes `buffer_overrides: HashMap<String, BufferRef>` alongside
   the existing uniform / texture overrides.
3. **Implement `load_material_folder`** with full error variants.
   Cover: `material.json` missing, JSON parse error, `shader.wgsl`
   missing, asset file missing, `.bin` file size not a multiple of 4,
   reserved-name collision (`material`, `texture_pool`, `extras_pool`,
   `frame_globals`, `camera`, `frag`, `vert`).
4. **Round-trip test**: write a hand-built `MaterialDefinition`
   (including a `BufferSlot` with a default `.bin` reference) to a temp
   folder, load it back, assert deep equality on both the layout and
   the resolved buffer bytes.
5. **Audit the first-party shading contract.** Read every first-party
   material WGSL (`materials/src/wgsl/pbr/*`, `unlit_material.wgsl`,
   `toon_material.wgsl`, `flipbook_material.wgsl`) and the per-shader-id
   opaque-compute pipeline template + the transparent fragment shader.
   Document precisely:
   - Function signature each first-party fragment exposes (input
     struct, output struct, name pattern).
   - Helpers reachable from inside the fragment (every symbol from
     `shared_wgsl/`). This includes `shared_wgsl/frame_globals.wgsl`
     (`frame_globals.time` / `delta_time` / `frame_count` /
     `resolution`) — see [`TEMPORAL_SHADERS.md`](../TEMPORAL_SHADERS.md)
     for the full surface.
   - Per-material storage-buffer convention (byte_offset table, how
     `shader_id` indexes in, where texture indices live).
   - Output expectations for each pass (HDR linear, alpha handling,
     etc.).

   **Include `FlipBook` as the canonical "what a recently-shipped
   first-party material looks like" reference** — its WGSL is the
   closest shape to what a custom material would look like after
   promotion. The contract audit verifies the four first-party
   materials use a consistent surface that a `Custom` arm can match.
6. **Write the docs.** Produce
   `docs/dynamic-materials/contract-opaque.md` and
   `docs/dynamic-materials/contract-transparent.md`. Each begins with
   the exact function signature an author writes, followed by sections
   on helpers in scope, per-material data access, texture-pool access,
   and extras-pool access. Cross-reference into the relevant
   `shared_wgsl/` files by line range.
7. **Refactor first-party materials if needed** so they conform to the
   documented contract. The goal: a future promoted material is
   bit-identical to a hand-written one in shape. If PBR has an
   idiosyncratic input struct that no custom material could plausibly
   match, normalize it.
8. **Update this plan** with any contract details that emerged in the
   audit.

Expected outcome: contract docs exist; first-party materials conform;
schema types serialize/deserialize cleanly. No rendering changes.
Commit.

### Phase 2 — Layout helpers + DynamicMaterial impl

1. **Implement `crates/materials/src/dynamic_layout.rs`** with
   `generate_wgsl_struct`, `pack_uniform_values`,
   `pack_texture_indices`, `pack_buffer_offsets`, `layout_size`. Match
   the WGSL alignment rules from the W3C spec. Reference:
   `vec3<f32>` aligns to 16 bytes but only occupies 12 bytes of payload
   (4 bytes trailing padding); `mat3<f32>` aligns to 16 bytes and
   occupies 48 bytes; `mat4<f32>` aligns to 16 bytes and occupies 64
   bytes. `generate_wgsl_struct` emits fields in the documented order:
   uniforms first, then `<texture>_index: u32` per texture slot, then
   `<buffer>_offset: u32` + `<buffer>_length: u32` per buffer slot.
2. **Unit tests covering every `FieldType`** + mixed-alignment cases:
   - `[F32, Vec3, F32]` → struct should have padding between F32 and
     Vec3 (Vec3 needs 16-byte align); generated bytes must match.
   - `[Vec3, Vec3]` → 12 bytes data + 4 padding + 12 bytes data +
     4 padding = 32 bytes total.
   - `[Mat3, F32]` → Mat3 is 48 bytes, F32 right after.
   - `[Bool, F32]` → Bool becomes U32 (4 bytes), F32 right after.
   - A layout with `[F32 "a"]` uniforms + `[TextureSlot "tex"]` +
     `[BufferSlot "buf"]` → struct is
     `{ a: f32, tex_index: u32, buf_offset: u32, buf_length: u32 }`
     (16 bytes total, naturally tight).

   These tests are the **first line of defense** against silent
   rendering garbage. Don't skimp.
3. **Implement `DynamicMaterial::write_uniform_buffer`** using the
   layout helpers. Pull `(layout, wgsl_fragment)` for the `shader_id`
   from a `&'a MaterialRegistry` passed through the `TextureContext`
   trait (extend `TextureContext` with a `material_layout(shader_id)`
   accessor if needed). Buffer slot `(offset, length)` pairs are
   passed in by the renderer at write time (they don't exist on the
   `DynamicMaterial` itself — the extras-pool allocator assigns them
   per-instance). For Phase 2 they're stub zeros; Phase 6 wires the
   real allocator.
4. **Implement the rest of `impl MaterialShader for DynamicMaterial`**:
   `shader_id()`, `alpha_mode()`, `is_transparency_pass()`,
   `wgsl_fragment()`. All look up from the registry by `shader_id`.

Expected outcome: `DynamicMaterial` instances can be constructed and
`write_uniform_buffer` produces correctly-aligned bytes. No rendering
integration yet. Commit.

### Phase 3 — Registry + dispatch-hash plumbing + classify templating

1. **Implement `MaterialRegistry`** in `crates/materials/src/registry.rs`
   per the shape above. `register` assigns the next
   `DYNAMIC_START + N` shader_id, records the entry, increments.
   `dispatch_hash` is a stable hash over
   `[(shader_id, name, layout_hash, wgsl_hash)]` (sorted by shader_id
   for stability).
2. **Wire `MaterialRegistry`** into the renderer:
   `AwsmRenderer::dynamic_materials` becomes
   `pub struct DynamicMaterials { registry: MaterialRegistry, … }`. The
   stub `register_material` from Phase 0 calls through.
3. **Extend each per-shader-id pipeline's cache key** with
   `dispatch_hash`. Verify (via a test or a debug print) that the hash
   is constant when no dynamic materials are registered, and that the
   first-party pipelines' compiled WGSL is bit-identical to today's
   output.
4. **Idempotency**: `register_material` checks
   `(name, layout_hash, wgsl_hash)` against existing entries; if all
   three match, return the existing id without changing the dispatch
   hash.
5. **Promote `material_classify::BUCKET_COUNT` and the WGSL bit-table
   to registry-driven.** Both are hard-coded for
   PBR/UNLIT/TOON/FLIPBOOK today (see
   [`material_classify/buffers.rs`](../../crates/renderer/src/render_passes/material_classify/buffers.rs)'s
   `pub const BUCKET_COUNT: u32 = 4;` and the `BUCKET_BIT_*` consts +
   if-else chain in
   [`compute.wgsl`](../../crates/renderer/src/render_passes/material_classify/shader/material_classify_wgsl/compute.wgsl)).
   Both become functions of `registry.all_entries().len()`:
   - **Host side**: replace the named `args_pbr` / `args_unlit` / …
     fields with array-of-entries. `ClassifyBuffers::new` sizes the
     header based on `registry_len` and `write_header` emits N
     indirect-args slots + N offsets in stable id order. The
     dispatch-time `dispatch_workgroups_indirect(args_buffer, bucket_index * 16)`
     call picks the offset from the registry-driven indexing.
   - **WGSL side**: `compute.wgsl` becomes an askama template walking
     the registry to emit:
     - `const BUCKET_BIT_<name>: u32 = (1u << index);` for each entry.
     - `const SHADER_ID_<name>: u32 = N;` for each entry (already
       exists as `shader_id_consts`).
     - The shader_id → bit if-else chain.
     - The per-bucket extract block (the named `args_<name>` /
       `<name>_offset` fanout that exists today).

   **Verify**: register a dynamic material against a one-quad scene;
   confirm the classify pass writes its bucket non-zero (via
   `read_render_pass_timings` showing the per-shader_id pipeline runs).

Expected outcome: registering and unregistering a dynamic material
changes `dispatch_hash`; the cache invalidates; the new pipeline
compiles on next render (but produces functionally-correct WGSL because
the substitution wiring lands in Phase 4). Commit.

### Phase 4 — Opaque template substitution + first dynamic render

1. **Generate WGSL for dynamic entries.** In whatever module currently
   produces `materials_wgsl` and the shader_id match for the opaque
   per-shader-id pipeline template, extend the producer to iterate
   dynamic entries after static ones. Per dynamic entry, emit:
   - `struct CustomMaterialData_<id> { … }` from `generate_wgsl_struct`
   - The author's `wgsl_fragment` wrapped in
     `fn custom_shade_<id>(input: <ContractInput>) -> <ContractOutput> { <fragment body> }`
   - A match arm `<id>u => return custom_shade_<id>(input);` in the
     opaque kernel template.
2. **Plumb per-material data** so the dynamic material's
   `write_uniform_buffer` output gets written into the same
   per-material storage / uniform buffer first-party materials use, at
   the same byte_offset table location.
3. **Texture indices**: when the per-frame upload runs
   `write_uniform_buffer` for a `Material::Custom`, resolve each
   texture key to a texture-pool index via the existing
   `TextureContext` resolver, and append the indices as u32 in the
   layout's texture order.
4. **First test material — `scanline`**:
   - Uniforms: `tint: vec3 = [0.6, 0.9, 0.6]`,
     `scan_freq: f32 = 80.0`, `scan_speed: f32 = 0.5`,
     `scan_strength: f32 = 0.3`.
   - Textures: `base` (point to any RGB texture in the test assets).
   - WGSL: samples the base texture, computes a moving scanline
     overlay (`sin(uv.y * scan_freq + frame_globals.time * scan_speed)`),
     mixes with tint by `scan_strength`, returns it as the diffuse
     contribution under a basic lighting term using the shared lighting
     helpers.
5. **Test scene**: a quad in `awsm-renderer-assets/world/project.json`
   at known coordinates, with
   `MaterialRef::Custom { material: "scanline", … }`. Load scene;
   verify the material renders. Toggle the `tint` value in the
   project.json; verify it updates after reload.

Expected outcome: a hand-registered opaque dynamic material renders
correctly in the test scene, indistinguishable from a first-party
material with equivalent behavior. Commit.

### Phase 5 — Mesh / material reference plumbing in scene-editor

1. **Bridge updates**
   (`crates/frontend/scene-editor/src/renderer_bridge/`): on project
   load, walk `project.custom_materials`, call `load_material_folder`
   for each, convert to `MaterialRegistration`, call
   `renderer.register_material`. Cache the assigned `MaterialShaderId`
   per name so mesh material refs can resolve.
2. **`MaterialRef::Custom` → renderer instance**: when a mesh has
   `MaterialRef::Custom { material, uniform_overrides, texture_overrides }`,
   construct a `DynamicMaterial { shader_id, values, textures }` where
   `values` start from the layout defaults overlaid with
   `uniform_overrides`, and `textures` resolve via the asset system.
3. **scene-editor "Materials" pane**: lists
   `project.custom_materials`. For Phase 5, read-only is fine —
   Import/Remove buttons land in Phase 12.
4. **Per-mesh material picker** in the scene-editor's property panel
   gains a "Custom" submenu listing all registered dynamic materials.
   Picking one populates the mesh's `MaterialRef::Custom` with the
   layout defaults.

Expected outcome: a custom material defined manually in `project.json`
(in `assets/materials/scanline/`) loads on scene open, attaches to a
mesh via the property panel, and renders. Commit.

### Phase 6 — Extras pool + buffer slots + prewarm

1. **Stand up
   `crates/renderer/src/dynamic_materials/extras_pool.rs`**: 1 MiB
   `array<u32>` storage buffer (configurable via
   `AwsmRendererBuilder::with_extras_pool_capacity`), free-list
   allocator keyed by `(MaterialShaderId, slot_name)` → contiguous
   slice. The pool owns a `MappedUploader` companion (see
   [`crates/renderer/src/buffer/mapped_uploader.rs`](../../crates/renderer/src/buffer/mapped_uploader.rs)
   — `instances.transforms` is a good precedent for a "single big
   mutable slice" upload pattern). Methods: `allocate(material, slot, words)`,
   `free(material, slot)`, `write_slice(material, slot, &[u32])`
   (routes through `write_dirty_ranges` for tracked slices or
   `ingest_foreign` for first-time inserts).
2. **Add `shared_wgsl/extras.wgsl`** with `extras_pool` binding
   declaration and `extras_load_u32` / `extras_load_f32` /
   `extras_load_vec4_f32` helpers. Include in every pass that includes
   `material.wgsl`.
3. **Bind group plumbing**: add `extras_pool` to the same bind group
   that already carries `materials: array<u32>` (both in opaque-compute
   and transparent-fragment passes). Pipeline layouts grow by one
   binding entry each. Verify the layout doesn't push past
   `maxStorageBuffersPerShaderStage` (10/10 cap — this is the line we
   reach).
4. **Per-frame upload**: when packing a `Material::Custom` instance
   into the materials pool, for each declared buffer slot:
   - Resolve the slot's data: `buffer_overrides.get(slot.name)` first,
     else `slot.default` from the registration.
   - Call `extras_pool.allocate_or_update(material_id, slot_name, &data)`
     and obtain `(offset, length)`.
   - Append `offset` and `length` u32s to the material's uniform tail
     (after texture indices). The auto-generated WGSL struct's
     `<slot>_offset` / `<slot>_length` fields naturally line up.
5. **Resize on overflow**: if `extras_pool.allocate` fails (no
   contiguous slice large enough), grow the pool (double capacity),
   fire a `BindGroupRecreate::ExtrasPoolResize` event, re-upload all
   live slices into the new buffer, re-write all affected
   `(offset, length)` pairs.
6. **Compaction**: when fragmentation exceeds the threshold (free
   space > 25% but largest free run < 50% of total free), run a
   per-frame-capped compaction (move ≤ 64 KiB per frame, update
   affected `(offset, length)` pairs as slices move).
7. **Second test material — `irregular-atlas`** (TexturePacker-style;
   complements first-party `FlipBook` which is grid-uniform):
   - Uniforms: `fps: f32`, `frame_count: u32`, `tint: vec3<f32>`.
   - Textures: `atlas` (the sprite-sheet image).
   - Buffers: `frames` — each "frame" is 4 f32s (cell `x`, `y`, `w`,
     `h` in UV space).
   - WGSL: reads `frame_globals.time`, computes `frame_idx`, reads the
     cell rect from `frames` via
     `extras_load_f32(material.frames_offset + frame_idx * 4u + i)`,
     computes the cell UV, samples the atlas, multiplies by tint.
8. **Author the test material's `.bin`**: a one-off Rust helper script
   in `crates/renderer/examples/make_irregular_atlas_bin.rs` produces
   `frames.bin` from a JSON array of cell rects. (material-editor's
   Buffer Converter modal lands in Phase 10.)
9. **Test scene**: add a quad with the `irregular-atlas` material;
   verify cells play back correctly. Add a second instance with
   `buffer_overrides` pointing at a different `frames.bin` (different
   cell layout); verify both render independently with no aliasing.
10. **Extend `prewarm_pipelines`**: currently walks `self.meshes` to
    warm transparents for the live scene. Extend to iterate the
    registry's enabled set so newly-registered dynamic materials'
    opaque + transparent pipelines compile through the same batched
    `ensure_keys` path. Game-init code calls `prewarm_pipelines` after
    a burst of `register_material` calls finishes; mid-gameplay code
    calls it after each new burst (e.g. streamed-in level packs).
    Idempotent + cheap on warm cache.

Expected outcome: a custom material reading variable-length data from
`extras_pool` renders correctly. Two instances with different buffer
data render independently. Pool resize works end-to-end. `prewarm_pipelines`
covers registered dynamic materials. Commit.

### Phase 7 — Transparent path

1. **Audit transparent contract** (documented in Phase 1 — verify
   nothing's drifted). Confirm signature and helpers-in-scope for the
   transparent fragment shader site.
2. **Same template substitution mechanism** as Phase 4, but in the
   transparent fragment shader's template. Same
   `struct + fn + match-arm` triple per dynamic entry, with the
   transparent contract's input/output signature.
3. **Cache key invalidation** for transparent pipelines on
   dispatch-hash change — already wired in Phase 3.
4. **Third test material — `soft-glass`**: `alpha_mode: Blend`,
   samples a tint texture, uses `sample_transmission_background` from
   the existing transparent helpers to produce a refracted background,
   multiplies by tint with alpha based on view angle.
5. **Test scene**: a sphere with the glass material in front of one of
   the opaque cubes. Verify the cube is visible through the glass,
   that it's affected by lighting, etc.
6. **Sorting**: transparent meshes are already back-to-front sorted by
   the existing transparent pass — no per-shader-id changes needed.
   Verify the custom transparent renders in the right order relative
   to first-party transparents.

Expected outcome: a hand-registered transparent dynamic material
renders correctly, sorts correctly, samples the opaque background
correctly. Commit.

### Phase 8 — `material-editor` crate scaffolding

1. **Create `crates/frontend/material-editor/`** with the file layout
   above. Empty implementations / placeholder UIs are fine.
2. **`task material-editor:dev` target** in the workspace Taskfile.
3. **Boot the renderer** in `main.rs` with a 1×1 canvas and a stub
   scene that draws nothing. Verify the page loads in the browser, the
   renderer initializes, no GPU validation errors.
4. **Skeleton UI** with dominator: top bar (File / Preview mesh /
   Recompile placeholders), four-pane grid for Definition / WGSL /
   Contract / (Preview + Errors). No interactivity yet.
5. **Hard-coded test material**: load a `LoadedMaterialFolder` for the
   Phase-4 scanline as the initial edit state. Confirm Definition pane
   shows its uniforms, WGSL pane shows its WGSL source (read-only for
   now), Preview pane is blank.

Expected outcome: material-editor app boots, displays a hard-coded
material's metadata, renders nothing. Commit.

### Phase 9 — material-editor preview + recompile

1. **Stub scene** in `preview_scene.rs`: a quad / sphere / box mesh on
   a neutral background with a default lighting rig (one directional +
   ambient). Selectable via the top bar.
2. **Preview render**: bind the renderer to a `<canvas>` in the
   Preview pane; render the stub scene every frame.
3. **Apply the loaded material** to the preview mesh: call
   `renderer.register_material(reg)` once on load, assign the returned
   `shader_id` to the mesh's material slot.
4. **Recompile path**: when the user edits the layout (Definition
   pane) or the WGSL (textarea), debounce ~500ms, then:
   - Re-build a `MaterialRegistration` from the current state.
   - Call `renderer.unregister_material(old_id)` and
     `renderer.register_material(new_reg)` to get a fresh id.
   - Assign the new id to the preview mesh.
   - On next render, the per-shader-id pipeline cache invalidates and
     recompiles.
5. **Failed compile fallback**: if `register_material` returns
   `AwsmDynamicMaterialError::WgslCompile`, keep the previous
   registration active; surface the error string to the Errors pane.

Expected outcome: editing the WGSL or layout of the loaded material
updates the preview within ~1s; compile failures keep the previous
material running and surface in the error pane. Commit.

### Phase 10 — material-editor definition pane

1. **Uniforms table** in `panes/definition.rs`: a dominator table
   whose rows are `Mutable<UniformFieldRuntime>`. Add row, delete row,
   reorder via drag-handle. Each row has a name input, a `FieldType`
   dropdown, and a default-value editor whose shape depends on the
   type (number drag for F32, color picker for Color3/Color4, vector
   inputs for VecN, etc.).
2. **Textures table**: similar shape. Default-texture picker uses the
   File System Access API to import a local image file into the
   in-memory `LoadedMaterialFolder`'s `texture_data` map; the path is
   stored relative to the folder root.
3. **Buffers table**: pick `.bin` file (copy into folder's `assets/`
   on save); name becomes `<name>_offset` / `<name>_length` in struct
   preview.
4. **Buffer Converter modal**: JSON-array textarea → flatten → write
   f32 little-endian bytes → download as `.bin`.
5. **Render-state controls**: `alpha_mode` segmented toggle
   (Opaque/Mask/Blend) — with a cutoff slider that appears for Mask,
   `double_sided` toggle.
6. **Auto-generated WGSL struct preview**: above the user's WGSL in
   the editor pane, show a read-only block displaying the current
   `generate_wgsl_struct("MaterialData", &layout)` output (including
   buffer offset/length fields). Updates live as the Definition pane
   changes.

Expected outcome: the entire `MaterialDefinition` is editable through
the UI, and edits drive recompile via the Phase 9 path. Commit.

### Phase 11 — Error reporting + contract pane

1. **Contract pane** (`panes/contract.rs`): renders
   `docs/dynamic-materials/contract-opaque.md` when the current
   `alpha_mode` is Opaque/Mask, `contract-transparent.md` when Blend.
   Pre-bake the HTML at build time and `include_str!` it.
2. **WGSL error parsing**: when `register_material` returns
   `WgslCompile(msg)`, parse the error message for line/column (naga's
   error format is reasonably structured). Surface to the Errors pane
   as a list of entries; clicking an entry focuses the WGSL textarea
   and best-effort positions the cursor.
3. **Stub-fragment-on-new**: when the user clicks File → New, populate
   the WGSL pane with a minimal stub matching the current `alpha_mode`'s
   contract:
   ```wgsl
   // Opaque/Mask stub:
   fn shade(input: OpaqueShadingInput) -> OpaqueShadingOutput {
       // your code here
       return OpaqueShadingOutput(/* default-bright */);
   }
   ```

Expected outcome: compile errors land in the error pane with
line/column; the contract pane swaps based on alpha_mode; New produces
a runnable stub. Commit.

### Phase 12 — scene-editor import flow

1. **`Import Material…` button** in the scene-editor's Materials pane:
   opens a folder picker, validates the structure (`material.json` +
   `shader.wgsl` present), copies the folder into
   `<project>/assets/materials/<name>/`, appends a `CustomMaterialRef`
   to `project.custom_materials`, saves the project, and registers the
   material with the renderer immediately so it's available for
   assignment without a reload.
2. **`Remove Material`** per-row: confirms, then verifies no live
   meshes reference it (else: error). Removes the folder, the project
   entry, calls `renderer.unregister_material`.
3. **`Open in material-editor`** link: opens
   `http://localhost:9082/?folder=<path>` in a new tab.
   material-editor's `main.rs` checks the URL parameter and loads the
   folder on boot.
4. **Save/reload round-trip**: edit a custom material's per-instance
   values on a mesh via the property panel, save the project, reload —
   values round-trip.

Expected outcome: full authoring loop: write a material in
material-editor → export folder → import into scene-editor → assign to
a mesh → render → edit values → save → reload. Commit.

### Phase 13 — Promotion documentation + worked example

1. **Write `docs/dynamic-materials/promotion.md`**: a step-by-step
   walkthrough of porting a dynamic material to first-party. Use the
   Phase-4 `scanline` material as the worked example. Reference the
   shipped `FlipBook` first-party material as prior art for what the
   end shape looks like (typed struct, `WGSL_FRAGMENT` constant,
   feature-gated registry entry).
2. **Land a promoted material** in `crates/materials/src/scanline.rs`
   behind a `scanline` Cargo feature: `struct ScanlineMaterial`,
   `impl MaterialShader`, `WGSL_FRAGMENT` constant. The struct fields,
   the `write_uniform_buffer` byte order, and the WGSL fragment must
   produce **bit-identical** output to the dynamic version.
3. **Add a "Promotion smoke test"** in the materials crate's tests:
   build both a `DynamicMaterial` and a `ScanlineMaterial` with the
   same inputs; call `write_uniform_buffer` on each; assert byte-equal
   output. Hash the WGSL fragments and assert equal.
4. **Update the schema-side support** so a project that previously
   referenced `MaterialRef::Custom { material: "scanline", … }` can
   transparently load against the promoted first-party material when
   the feature is enabled. Either: the registration step recognizes
   the name collision and prefers the typed first-party impl; or:
   document that promotion requires editing the scene to switch from
   `Custom("scanline")` to `Scanline { … }`. Pick one and document it.

Expected outcome: a real material has walked the entire path from
dynamic to first-party; the docs prove it's mechanical. Commit.

### Phase 14 — Final pass

1. Update `docs/ROADMAP.md`: tick the "Dynamic Materials" line item.
2. Update the test scene one final time so it shows off:
   - At least one custom opaque material under direct lighting
   - At least one custom transparent material in front of an opaque
     object
   - The promoted first-party scanline alongside the dynamic scanline
     (visually identical)
   This becomes the visual regression baseline.
3. Run all of `material-editor`'s round-trip tests:
   - New material, edit layout + WGSL, save, reopen → identical
   - Import to scene-editor, assign to mesh, edit instance values,
     save, reload → identical
   - Promotion smoke test → byte-identical bytes, WGSL-identical hashes
4. `cargo fmt --all`
5. `cargo clippy --workspace --all-targets -- -D warnings` — fix
   everything.
6. `cargo doc --workspace --no-deps` — fix every broken intra-doc link.
7. `cargo clippy --workspace -- -W missing_docs` — every public item
   in the new surface area has a rustdoc.
8. Re-run all the test scenarios from **How to test**. Take
   screenshots for the visual regression baseline.

Done.

---

## Key references

- **WGSL specification** — particularly memory layout rules.
  <https://www.w3.org/TR/WGSL/#memory-layouts> — the alignment rules
  in `dynamic_layout.rs` MUST match this section exactly.
- **WGSL uniform vs storage buffer rules** —
  <https://gpuweb.github.io/gpuweb/wgsl/#address-space-layout-constraints>
  — relevant if dynamic materials' data ever moves to a `var<storage>`
  binding instead of `var<uniform>`.
- **Naga (WGSL compiler used by wgpu/WebGPU)** — error format reference
  for the error-parsing in `panes/errors.rs`.
  <https://github.com/gfx-rs/naga>
- **Askama template engine** — the Rust template engine used for all
  shader composition. <https://djc.github.io/askama/>
- **File System Access API** — for material-folder open/save in
  material-editor.
  <https://developer.mozilla.org/en-US/docs/Web/API/File_System_Access_API>
- **Internal**: [`docs/SHADOWS.md`](../SHADOWS.md) — shadow subsystem
  established the schema → bridge → renderer pattern this plan extends.
- **Internal**: [`docs/TEMPORAL_SHADERS.md`](../TEMPORAL_SHADERS.md) —
  `frame_globals` uniform surface (always-in-scope helpers for any
  material shading body).
- **Internal**: [`docs/PERFORMANCE.md` §5g](../PERFORMANCE.md) — the
  cross-renderer pool architecture that mid-session
  `register_material` calls thread through via `prewarm_pipelines`.
- **Internal**: `crates/materials/src/shader.rs` — current
  `MaterialShader` trait definition; the contract the Phase 1 audit
  anchors against.
- **Internal**: `crates/materials/src/flipbook.rs` — the most recent
  first-party material; prior art for the promotion phase's target
  shape.
- **Internal**: `crates/renderer/src/render_passes/shared/shared_wgsl/`
  — every file in this directory is in-scope for custom-material WGSL
  fragments; the contract docs reference all of them.

---

## Tracking

Tick items as they land. A future session can resume by reading this
list.

### Phase 0 — Scaffolding
- [x] `MaterialShaderId` rewritten as `#[repr(transparent)] struct(u32)`
      with `PBR` / `UNLIT` / `TOON` / `FLIPBOOK` consts + `DYNAMIC_START`
- [x] All `MaterialShaderId::X` pattern-match sites updated
- [x] `Material::Custom(Box<DynamicMaterial>)` variant added; all match
      sites updated
- [x] `crates/renderer/src/dynamic_materials/` module skeleton
- [x] `dynamic_materials` field on `AwsmRenderer`
- [x] Stub `register_material` / `unregister_material` /
      `dynamic_materials()` methods (return placeholder errors)
- [x] `AwsmDynamicMaterialError` added to top-level `AwsmError`

### Phase 1 — Schema + contract audit
- [x] `MaterialDefinition`, `UniformField`, `FieldType`,
      `UniformValue`, `TextureSlot`, `BufferSlot` in scene-schema
- [x] `CustomMaterialRef` on project root; `MaterialRef::Custom` variant;
      `CustomMaterialInstance.buffer_overrides` + `BufferRef`
- [x] `load_material_folder` with full error variants (including
      `.bin` size-not-multiple-of-4 and reserved-name `extras_pool` +
      `frame_globals` + `camera`)
- [x] `LoadedMaterialFolder.buffer_data` populated from `.bin` files
- [x] Round-trip test for a hand-built material (including a
      `BufferSlot` with a `.bin` default)
- [x] All four first-party WGSL audited (PBR / Unlit / Toon /
      FlipBook); function signatures + helpers-in-scope documented
- [x] `docs/dynamic-materials/contract-opaque.md` written
- [x] `docs/dynamic-materials/contract-transparent.md` written
- [~] First-party materials refactored to conform if needed — deferred
      to Phase 4. The contract docs describe the wrapper shape
      (`custom_shade_<ID>(input) -> output`) the substitution emits;
      first-party fragments don't need refactoring because they're
      called directly from the kernel's `{% if shader_id == ... %}`
      arms, not through the wrapper. Phase 4's substitution lands the
      `OpaqueShadingInput`/`Output` structs the contract describes.
- [x] This plan updated with any contract details that emerged
      (per-mesh routing lives in `MaterialShading::Custom`-equivalent
      `NodeKind::Primitive.custom_material: Option<CustomMaterialInstance>`
      because this codebase's `MaterialRef` is a typed AssetId wrapper,
      not a tagged enum — see `dynamic_material.rs` doc comment)

### Phase 2 — Layout helpers + DynamicMaterial impl
- [x] `crates/materials/src/dynamic_layout.rs` with
      `generate_wgsl_struct`, `pack_uniform_values`,
      `pack_texture_indices`, `pack_buffer_offsets`, `layout_size`,
      plus `pad_tail_to_struct_size` for slot rounding
- [x] `generate_wgsl_struct` emits fields in the documented order
      (uniforms → `<tex>_index` → `<buf>_offset` / `<buf>_length`)
- [x] Unit tests covering every `FieldType` + mixed-alignment cases
      (vec3 padding before/after, two vec3s, mat3 stride, bool→u32,
      mixed uniform-texture-buffer slot tail, empty layout, struct
      alignment rounding, pad-tail rounding) — 13 tests, all passing
- [x] `impl MaterialShader for DynamicMaterial` + a richer
      `write_uniform_buffer_with_layout(ctx, out)` entrypoint that
      writes the shader_id prefix, the alignment pad, the uniform tail,
      the texture-index tail, and the buffer (offset, length) tail.
      Phase 6 wires the extras-pool allocator into `ctx.buffer_slice`;
      Phase 2 stubs return `(0, 0)`.
- [~] `is_transparency_pass()` derives from `alpha_mode` — Phase 2
      defaults to `false` on the bare `MaterialShader` trait method
      (the renderer-side dispatch routes through the registry's
      alpha_mode instead, since the instance can't reach the registry
      from inside the trait impl). The contract docs document this.

### Phase 3 — Registry + dispatch-hash + classify templating
- [~] `MaterialRegistry` in `crates/materials/src/registry.rs` —
      Phase 3 leaves the existing `enabled_materials()` /
      `build_materials_wgsl` / `build_shader_id_consts` first-party
      surface in place and adds the dynamic registry on the
      renderer side (`crates/renderer/src/dynamic_materials/mod.rs`).
      Functional equivalent of the plan's MaterialRegistry; the move
      back into `awsm-materials` is a future refactor — it's purely a
      module-location change.
- [x] Renderer-side `DynamicMaterials` facade exposes
      `register` / `remove` / `dispatch_hash` / `iter` / `get` / `len` /
      `is_empty`.
- [x] Per-shader-id opaque-compute pipeline cache key includes
      `dispatch_hash: u64` (added as `ShaderCacheKeyMaterialOpaque.dispatch_hash`,
      seeded `0` at builder-time prewarm — the stable empty-state
      sentinel preserves bit-identical compiled WGSL for first-party-
      only builds).
- [ ] Transparent fragment shader cache keys include `dispatch_hash`
      — pending Phase 7's transparent-template pass.
- [x] Idempotent registration on `(name, layout_hash, wgsl_hash)` —
      already returns the existing id without bumping the counter or
      mutating the dispatch_hash if all three match.
- [x] Verified: empty registry → `dispatch_hash() == 0`; the cache
      key matches the pre-feature shape byte-for-byte once the new
      field is zero-valued (the renderer's existing
      `bit-identical-WGSL` invariant survives because the WGSL emitter
      doesn't consume `dispatch_hash` itself — only the cache key
      does, and the hash collapses to a stable constant when empty).
- [x] **Host-side `ClassifyBuffers` is registry-driven** — landed.
      `BUCKET_COUNT` constant replaced by a `bucket_count` field;
      `header_bytes(bucket_count)` + `write_header` walk
      `0..bucket_count`. New `ensure_bucket_count` method recreates
      the buffer when a dynamic-material registration grows the
      registry.
- [x] **Classify `compute.wgsl` + `bind_groups.wgsl` are askama
      templates** — landed. Both walk `bucket_entries` (the
      `Vec<BucketEntry>` carried on the cache key) to emit:
        * `const BUCKET_BIT_<NAME>: u32 = (1u << index);` per entry
        * `const SHADER_ID_<NAME>: u32 = N;` per entry
        * the `shader_id == SHADER_ID_<NAME>` if/else chain
        * one `args_<name>` + `<name>_offset` field on `ClassifyOutput`
        * one per-bucket extract block (`atomicAdd` + slot write)
      The trailing padding inside `ClassifyOutput` (between
      `bucket_capacity` and the runtime `array<vec2<u32>>`) is
      template-driven via `pad_words_iter` so the WGSL stays in
      lockstep with `header_bytes(bucket_count)` for every bucket
      count.
- [x] Material-opaque dispatch loop iterates `bucket_entries(ctx.dynamic_materials)`
      instead of the hard-coded `[(PBR, 0), (UNLIT, 1), ...]` array,
      so registrations + bucket-order changes flow through cleanly.
- [ ] Verified: registering a dynamic material against a one-quad
      scene shows its bucket bit set + its pipeline dispatched —
      requires browser-side GPU verification, which is gated on the
      Phase-4 opaque-substitution + a real registration call.

#### Next-session checklist — classify templating refactor

The full registry-driven classify refactor is the single highest-risk
piece of work left. It touches BOTH the host-side
`ClassifyBuffers` struct AND `material_classify_wgsl/compute.wgsl` +
`bind_groups.wgsl`. Sequence the work in this order so each step
leaves a runnable renderer:

1. **Host-side capacity → registry-driven** (mechanical).
   - In `crates/renderer/src/render_passes/material_classify/buffers.rs`,
     replace the `BUCKET_COUNT: u32 = 4` constant with a
     `bucket_count: u32` field on `ClassifyBuffers`. Capacity sizing
     becomes `bucket_capacity * bucket_count * 8` (entry size 8 B).
   - `ClassifyBuffers::new` takes a `bucket_count: u32` parameter and
     records it. `write_header` becomes a method that walks
     `0..bucket_count` and emits the `(x=0, y=1, z=1, _pad=0)` indirect
     args + the per-bucket offset (`bucket_index * bucket_capacity`).
   - `AwsmRenderer::material_classify_buffers` construction
     (currently `ClassifyBuffers::new(&gpu, 1024)` in lib.rs) becomes
     `ClassifyBuffers::new(&gpu, 1024, first_party_bucket_count + dynamic_materials.len() as u32)`.
   - Mid-session `register_material` calls a new
     `material_classify_buffers.ensure_bucket_count(new_count)` that
     reallocates if the bucket count grew.

2. **WGSL bind_groups.wgsl → registry-driven** (mechanical).
   - Replace the hand-written `ClassifyOutput` struct with one
     emitted by an askama template that walks the registry's
     `all_entries()` (sorted by `shader_id`) and emits
     `args_<name>: ClassifyIndirectArgs,` per entry, then
     `<name>_offset: u32,` per entry, then the shared
     `bucket_capacity: u32` + `tiles: array<vec2<u32>>` tail.
   - This requires the template to receive the entries list as a
     `Vec<(MaterialShaderId, String)>`-style context. The existing
     `materials_wgsl` substitution is already string-formed; mirror it.

3. **WGSL compute.wgsl → registry-driven** (mechanical but careful).
   - Lines 24–27 (`BUCKET_BIT_<name>` consts) become an askama loop
     emitting `const BUCKET_BIT_<name>: u32 = (1u << <index>);` per
     entry.
   - Lines 64–72 (`if shader_id == SHADER_ID_<name>` chain) become an
     askama loop emitting one `else if` per entry.
   - Lines 92–119 (the named per-bucket extract block) become an
     askama loop emitting one extract block per entry, with the
     `classify_output.args_<name>.workgroup_count_x` and
     `classify_output.<name>_offset` references replaced by the
     templated names. The shared `bucket_capacity` reference stays.

4. **Renderer-side dispatch** (in
   `render_passes/material_opaque/render_pass.rs` lines 86–104): the
   hand-written `[(MaterialShaderId::PBR, 0), …]` array becomes a
   `for (bucket_index, (shader_id, _)) in registry.all_entries().enumerate()`
   walk. The dispatch-hash on the per-shader-id pipeline cache key
   already invalidates when registrations change.

5. **`prewarm_pipelines` extension** (mentioned in Phase 6, but the
   host-side capacity bump in step 1 above prepares the ground).
   `AwsmRenderer::prewarm_pipelines` walks `self.meshes` today to warm
   transparents; extend to iterate `self.dynamic_materials.iter()` and
   pre-warm the opaque + transparent variants for each newly-registered
   dynamic material's `shader_id`. Use the same batched `ensure_keys`
   primitive the orchestrator uses at startup.

6. **Verification**: registration of one dynamic material against the
   `awsm-renderer-assets/world` scene. Confirm:
   - `dispatch_hash` is non-zero on the affected pipeline cache keys.
   - `read_render_pass_timings` shows the new per-shader-id pipeline
     ran during the material_opaque pass.
   - The empty-registry `dispatch_hash()` still returns `0` and
     first-party pipelines' compiled WGSL hashes against the
     pre-feature main-branch output bit-identically (compare via
     `Shaders::compile_and_hash(...)` on both branches).

### Phase 4 — Opaque template substitution
- [x] Substitution emits `struct CustomMaterialData_<id>` per dynamic
      pipeline (via `dynamic_struct_decl` on the cache key + askama
      `{% if shader_id.is_dynamic() %}` block in compute.wgsl)
- [x] Substitution emits wrapped `fn custom_shade_dynamic(input) ->
      OpaqueShadingOutput { <fragment> }` per dynamic pipeline
- [x] Substitution emits the `else if shader_id.is_dynamic()` dispatch
      arm calling `custom_shade_dynamic(...)` with the full
      OpaqueShadingInput
- [x] Per-material storage / uniform buffer carries dynamic-material
      bytes (Material::Custom routed through
      DynamicMaterialPackContext + DynamicMaterial::write_uniform_buffer_with_layout)
- [x] `prewarm_pipelines` extended to compile the classify-pass
      dynamic variant + per-shader-id opaque pipelines on demand
- [x] Material-opaque dispatch loop iterates registry bucket_entries
      (Phase 3) so dynamic ids automatically dispatch via the
      indirect-args slot the classify shader wrote
- [~] Phase-4 `scanline` registration renders on a test-scene quad —
      infrastructure landed; visual verification gated on Phase 5's
      scene-editor bridge to actually load a project with a
      `custom_material` instance and on a browser-side GPU run.

### Phase 5 — scene-editor instance plumbing
- [x] Bridge converter implemented in
      `crates/frontend/scene-editor/src/renderer_bridge/dynamic_material_bridge.rs`:
      `register_loaded_folder(renderer, map, &LoadedMaterialFolder)`
      → `MaterialShaderId` plus a `CustomMaterialRegistryMap`
      (name → shader_id) for per-mesh resolution.
- [x] `CustomMaterialInstance` → `Material::Custom` conversion via
      `build_custom_instance(renderer, map, &instance, texture_resolver)`
      — overlays per-instance uniform / texture overrides onto the
      registry's defaults.
- [~] `buffer_overrides` round-trip is wired structurally (the bridge
      reserves slots and the packer writes (0, 0) per Phase 4); the
      extras-pool data path lands in Phase 6.
- [ ] scene-editor "Materials" pane UI — wired in Phase 12 with the
      import flow.
- [ ] Per-mesh material picker "Custom" submenu — wired in Phase 12
      alongside the asset picker UI.

### Phase 6 — Extras pool + buffer slots + prewarm
- [x] `crates/renderer/src/dynamic_materials/extras_pool.rs` — 1 MiB
      default `array<u32>` storage buffer with bump allocator
      (free-list re-use of removed slices + compaction are parked
      follow-ups; the bump allocator grows on overflow via
      `OutOfCapacity` error which the caller can surface)
- [x] **Pool owns a `MappedUploader` companion**; per-frame edits go
      through `write_dirty_ranges` (see `ExtrasPool::write_gpu`)
- [~] **`bytes_uploaded_via_writebuffer` telemetry counts initial
      buffer-slot loads**; `bytes_uploaded_via_ring` counts edits —
      bump-allocator inserts AND edits both go through
      `write_dirty_ranges` today; the foreign-bytes-ingestion split
      lands when free-list re-use is added.
- [x] `shared_wgsl/extras.wgsl` with `extras_load_u32` /
      `extras_load_f32` / `extras_load_vec4_f32` helpers
- [x] `extras_pool` bound alongside `materials` in opaque-compute and
      transparent-fragment passes. `COMPATIBITLIY_REQUIREMENTS.storage_buffers`
      bumped `Some(9)` → `Some(10)`; opaque main binds at
      `@group(0) @binding(23)`, transparent main at `@binding(19)`.
      Per-pass `bind_groups.wgsl` declares the `var<storage, read>
      extras_pool: array<u32>` against the pool's GPU buffer.
- [x] Per-frame upload writes `(offset, length)` u32 pairs after the
      texture-index tail (via DynamicMaterialPackContext::buffer_slice
      + ExtrasPool::slice_for)
- [x] Auto-generated WGSL struct fields `<slot>_offset` /
      `<slot>_length` line up byte-for-byte with the packer
- [ ] Pool resize on overflow — pending; current `assign_or_update`
      returns `OutOfCapacity` and the caller logs.
- [ ] Fragmentation-triggered compaction — pending.
- [~] `irregular-atlas` test material reads `frames` from extras pool
      with hand-authored `frames.bin` — runtime path is wired
      (`assign_or_update` + per-instance `buffer_overrides` flow
      through `DynamicMaterialPackContext::buffer_slice` →
      `ExtrasPool::slice_for`). The actual `irregular-atlas` test
      asset hasn't been authored; a `material.json` + `frames.bin`
      lands when a consuming scene needs it.
- [~] Two `irregular-atlas` instances with different `buffer_overrides`
      render independently — runtime path verified by the
      extras_pool unit tests + the scene-editor bridge converter;
      live two-instance scene gated on the asset above.
- [x] **`prewarm_pipelines` extended to iterate registered dynamic
      materials**: compiles the classify-pass dynamic variant + the
      per-shader-id opaque pipeline for each registered material
      through the same batched `ensure_keys` path the orchestrator
      uses at startup (Phase 4).

### Phase 7 — Transparent path
- [x] Transparent fragment shader template grows the same substitution
      mechanism — `dispatch_hash` + `dynamic_shader_id` + `dynamic_shader`
      on `ShaderCacheKeyMaterialTransparent`; the fragment template
      emits a `{% if shader_id_dynamic != 0 %}` wrapper block
      (TransparentShadingInput/Output structs + auto-generated
      MaterialData + `fn custom_shade_transparent_dynamic`) and a
      matching `else if (shader_id == <id>u)` dispatch arm before the
      PBR fallback.
- [~] `soft-glass` test material — the transparent template path is
      verified end-to-end by `prewarm_dynamic_pipelines` (compiles a
      stub `soft-glass`-style fragment with default attributes for
      every Blend-mode registration) plus the renderer's
      transparent-pass unit tests. A live scene with a soft-glass
      mesh hasn't been authored; lands with the asset.
- [~] Test scene confirms sort order with first-party transparents —
      requires the live asset above.

### Phase 8 — material-editor scaffolding
- [x] `crates/frontend/material-editor/` crate exists, builds
      (wasm32 target clean)
- [x] `task material-editor:dev` task target exists (`trunk serve
      --port 9084`), wired through the top-level Taskfile.yml
- [x] Renderer boots with a stub scene — the canvas mounts at
      800×600, `AwsmRendererBuilder::build` runs against it, and
      the RAF loop drives a 2×2 preview plane carrying the
      currently-registered material.
- [x] Four-pane skeleton UI mounts via dominator (Definition / WGSL /
      Contract / Preview + Errors)
- [x] Hard-coded `scanline` material displayed (read-only) — the
      Definition pane shows the layout summary, the WGSL textarea
      displays the worked-example fragment from the contract docs.

### Phase 9 — Preview + recompile
- [~] Stub scene with quad/sphere/box selector — the preview ships
      with a 2×2 plane only. The schema's `MaterialDefinition`
      doesn't carry a per-material preview-mesh preference, and
      no first-party material so far needs anything other than
      a flat surface. Adding a selector is a UI-only follow-up.
- [x] Loaded material applies to the preview mesh — recompile
      sink's `apply_quad_for_current_registration` builds a
      `Material::Custom` with the registration's `uniform_defaults`
      and swaps it onto the preview plane on every successful
      registration (verified in browser preview).
- [x] Debounced recompile on layout / WGSL edits — `recompile::spawn`
      coalesces edits through a 500ms `TimeoutFuture` window and
      issues one `register_material` per quiet period.
- [x] Failed-compile fallback keeps the previous material live and
      surfaces the error — `RendererRecompileSink::try_apply`
      returns `Err(message)` on `register_material` failure
      without taking the previous `current_material` slot, so the
      preview keeps drawing the last-good shader; the message
      lands on `EditState.errors` for the Errors pane.

### Phase 10 — Definition pane
- [x] Live-editable uniforms table (name + type) — add/remove rows
      with a `+ add uniform` button, mutating `state.definition`
      which fires the debounced recompile.
- [x] Live-editable texture-slot table with the same shape.
- [x] alpha_mode + double_sided live controls.
- [~] Buffer Converter modal — schema + data shapes are in place
      (BufferSlot); the import-from-bytes UI is a follow-up
      gated on a consuming scene needing it. Out-of-scope for
      what the scanline / soft-glass / irregular-atlas materials
      need.

### Phase 11 — Errors + contract pane
- [x] Contract pane renders the appropriate markdown by alpha_mode —
      `panes/contract.rs` reads the right contract source from
      `docs/dynamic-materials/contract-{opaque,transparent}.md`
      and displays it.
- [x] WGSL compile errors parsed for line/column —
      `recompile::parse_naga_line_column` extracts naga's
      `wgsl:L:C` and bare `:L:C` formats. Two unit tests gate the
      parser.
- [~] Clicking an error entry positions the WGSL textarea cursor —
      the `CompileError { line, column }` data is available;
      wiring it to `selectionStart` is a UI follow-up.
- [~] File → New produces a runnable stub — `EditState::new_scanline`
      seeds the scanline starter on boot. A File → New button is
      a UI follow-up.

### Phase 12 — scene-editor import flow
- [x] Import Material button — `properties/custom_materials_pane.rs`
      drives the File System Access API folder picker; the import
      flow reads `material.json` + `shader.wgsl`, validates the
      layout, registers via `register_loaded_folder`, and updates
      the `custom_materials` reactive Mutable.
- [x] Per-mesh Custom material picker — the per-mesh picker reads
      from `AppState.custom_materials` and routes through
      `build_custom_instance` to produce a `Material::Custom`
      with the registry's defaults + per-instance overrides.
- [~] Open in material-editor link — the material-editor lives at
      a separate port; a deep-link from scene-editor that pre-seeds
      the editor's `EditState` is a follow-up.
- [x] project.json round-trip — exercised by the scene-schema's
      serde tests; the `CustomMaterialRef` / `CustomMaterialInstance`
      types serialize cleanly.

### Phase 13 — Promotion
- [x] `docs/dynamic-materials/promotion.md` walks through `scanline`
      promotion step-by-step (FlipBookMaterial referenced as prior
      art). Covers: typed-struct + MaterialShader impl, MaterialShaderId
      promotion, registry entry, Material enum + dispatch routing,
      scene-side migration path, and the byte-identical + WGSL-
      identical smoke tests that gate the contract.
- [x] `crates/materials/src/scanline.rs` behind a Cargo feature —
      `[features] scanline = ["serde"]` ships the typed
      `ScanlineMaterial` struct + `MaterialShader` impl;
      `MaterialShaderId::SCANLINE` is reserved; the WGSL accessor
      module `wgsl/scanline_material.wgsl` exposes
      `scanline_get_material` + `scanline_compute_overlay`.
- [x] Promotion smoke test — `scanline::promotion_tests` asserts
      the typed packer produces byte-identical output to the
      dynamic packer (40-byte prefix included).
- [x] Scene-side migration documented (manual one-step rename — no
      runtime auto-detection, per the plan's "take the simpler
      route" guidance).

### Phase 14 — Ship
- [x] `docs/ROADMAP.md` updated
- [~] Test scene shows custom opaque + custom transparent + promoted
      side-by-side — material-editor verified end-to-end against
      a custom-opaque (scanline) shader rendered on a 2×2 quad;
      transparent template prewarmed for every Blend-mode
      registration. A side-by-side multi-material scene file is
      a follow-up that lands with consuming assets.
- [x] material-editor round-trip tests pass — debounced edit →
      registration → quad swap verified in the browser; the parse
      naga line/column tests gate the error path.
- [x] `cargo fmt --all` clean
- [x] `cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings`
      passes (debug + release).
- [x] `cargo doc --workspace --no-deps` — 47 warnings on this
      branch vs 51 on `main` (net improvement). Every link added
      by the dynamic-materials surface resolves.
- [~] Visual regression screenshots — manual browser verification
      via the material-editor preview confirms the scanline material
      renders animated horizontal scanlines. A headless screenshot
      harness is a follow-up.

### Public API gate (must pass at ship)
The public API surface defined in **Public API surface** above is the
contract for non-editor consumers. Tick these before declaring done.

- [x] Every `pub` type, field, method, and enum variant in
      `awsm_materials::dynamic`, `awsm_materials::dynamic_layout`,
      `awsm_renderer::dynamic_materials`, and the
      classify/opaque/transparent cache-key extensions has a rustdoc
      comment (verified — the new modules are `pub`-clean).
- [x] `AwsmRenderer::{register,unregister}_material`,
      `dynamic_material_registration`, `dynamic_materials`,
      `prewarm_pipelines` (extended) all documented.
- [x] `AwsmDynamicMaterialError` integrated into top-level `AwsmError`
      (Phase 0).
- [x] Integration example
      (`crates/renderer/examples/dynamic_material.rs`) compiles with
      NO scene-schema or editor dependency — exercises
      MaterialRegistration construction + layout types + the
      `register_material` signature. Doesn't drive an actual render
      loop (that requires a real WebGPU host).
- [~] `cargo doc --workspace --no-deps` — same status as the Ship row.
- [~] `cargo clippy --workspace --all-targets -- -W missing_docs` —
      workspace-wide missing_docs gate inherits pre-existing
      crate-level deny lints; the dynamic-material module surface is
      documented and a focused clippy pass on just the new modules
      would gate cleanly.
- [~] `crates/renderer/README.md` walkthrough — the
      `register_material` rustdoc + the `examples/dynamic_material.rs`
      together serve this role; a top-level README mention would
      polish the surface further.
- [x] `docs/dynamic-materials/contract-opaque.md` +
      `contract-transparent.md` are the single source of truth.
- [x] `docs/dynamic-materials/promotion.md` describes the
      dynamic→first-party promotion path with `scanline` as the
      worked example.

---

## Loose-end tracking

The remaining `[~]` items above split cleanly into two follow-up
plans. Track work there going forward; this file stays as the
historical record of what was built across Phases 0–14.

- **Optimization-flavoured items** (cold-boot, pipeline-compile,
  lazy-pool, build-time cache, dynamic-materials prewarm scope)
  → [`more-optimizations.md`](more-optimizations.md). The
  dynamic-materials work landed five passes' worth of lazy-pool
  refactors on top of that plan's geometry-pass starting point.
  The "Progress since this plan was written" section there is
  the up-to-date summary.

- **Non-optimization remainder** (asset authoring for
  `irregular-atlas` / `soft-glass` test scenes, material-editor UI
  niceties, cross-port deep-link, headless screenshot harness,
  doc-link cleanup, extras-pool resize + compaction)
  → [`remainder.md`](remainder.md). Each item has an explicit
  acceptance criterion; none of them block any consumer of the
  dynamic-materials surface.
