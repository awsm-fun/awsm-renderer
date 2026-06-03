# Editor Rewrite — unified `frontend/editor`

> **Status:** Planning, ready to execute. This doc folds the external design
> handoff (`~/Downloads/editor-reference/`, hereafter "the prototype") into the
> real codebase and the locked decisions. It supersedes the prototype's
> `HANDOFF.md` and `_src/INVENTORY.md` wherever they conflict.

## 0. Execution contract (read first)

This plan is meant to be run **start-to-finish autonomously** from a single
prompt. An executing agent should:

- Work on branch **`editor`** (already checked out).
- Follow milestones **M1 → M10 (§12) in order**. Keep the branch compiling and
  **`task lint` green** (rustfmt + `clippy --all --all-features --tests -D
  warnings`) at the end of every milestone.
- **Do not stop to ask questions** — every fork has a decided default here
  (§2, §13). If a genuinely new ambiguity appears, pick the most reversible
  option, note it inline in this doc, and continue.
- **Verify visually via Chrome MCP** using the prototype-vs-build tab workflow
  (§11). The static prototype is served at `http://localhost:9090`.
- After the **backend** change (M7), run the **GPU verification loop** and
  reconcile materials baselines (memory: "Materials overhaul verification").
- **Definition of done (§13).** The old crates are gone, `task lint` is green,
  `task editor-dev` serves a unified editor matching the prototype across all 5
  screenshots, and custom materials carry author-declared pass dependencies.

## 1. Goal

Replace the two separate frontends — `packages/frontend/scene-editor`
(`awsm-scene-editor`) and `packages/frontend/material-editor`
(`awsm-material-editor`) — with **one** new crate
`packages/frontend/editor` (`awsm-editor`). It exposes both capabilities behind
a top-bar **Scene ⇄ Material** segmented switch and is linked from `site-index`
as a single entry.

The renderer, the scene-graph/`actions::*` logic, and the FS/history/worker
plumbing are **reused**. What changes is (a) the surrounding chrome/UX, rebuilt
to the prototype, and (b) the **design system itself** — `web-shared`'s atoms +
theme are **rewritten** to embody the new look (§4, §6). The prototype is a
*visual* reference only (React + inline Babel); **do not port JSX**.

Two structural changes beyond a straight merge:
- **Eliminate `packages/crates/editor`** (`awsm-renderer-editor`) by folding its
  gizmo/grid code into `web-shared` (§5).
- Add a **Material Pass Dependencies** section (`ShaderIncludes` /
  `FragmentInputs`) for custom materials — UI **and** backend (§7). This concept
  post-dates both frontends and appears nowhere in the prototype.

## 2. Locked decisions

| # | Decision | Choice |
|---|----------|--------|
| 1 | New crate | `packages/frontend/editor`, package `awsm-editor`, `publish = false`. |
| 2 | Cutover | **Big-bang replace.** Build `editor` to parity, then delete both old crates + taskfiles in the same branch (final milestone). Old crates stay buildable *during* dev for side-by-side comparison; removed last, not incrementally. |
| 3 | Material-mode scope | **Custom WGSL materials only** (today's `material-editor` scope). First-party PBR/glTF materials stay edited in **Scene mode's** inspector material block, as `scene-editor` does today. |
| 4 | Pass-dependencies feature | **UI + backend plumbing.** Thread author-declared `ShaderIncludes`/`FragmentInputs` through the dynamic-material registration API + shader cache key, replacing the hardcoded `ShaderIncludes::all()` in `dynamic.rs`. |
| 5 | Material persistence | **Keep File System Access folders** (`material.json` + `shader.wgsl`). The cross-app `?folder=` deep-link collapses into in-app navigation. |
| 6 | **`crates/editor`** | **Eliminate it.** Fold `grid` + `transform_controller` + `point_handle` into **`web-shared`** (not the editor binary crate — see §5.2 rationale: `model-tests` also consumes them). Remove `awsm-renderer-editor` from the workspace + publish set. |
| 7 | **Design system** | **Rewrite `web-shared`'s atoms + theme to the new design.** The prototype's `tokens.css` is the source of truth, re-expressed in the codebase's `class!`/`ColorBackground`/`ChromeFill` idiom. This **supersedes** the HANDOFF's "map onto existing palette, don't fork" guidance. `model-tests` is unaffected (it has its own local atoms/theme; only uses `web-shared::perf`). |

## 3. Terminology map (prototype → real codebase)

| Prototype term | Real codebase | Notes |
|---|---|---|
| "bucket" / `MAX_BUCKETS = 32` | `MAX_BUCKET_ENTRIES = 32` (`renderer/src/dynamic_materials/registry.rs:163`) | Real cap; driven by the classify shader's `tile_mask: atomic<u32>`. Exceeding = hard error (`resolve_first_party_variant_or_cap_err`, registry.rs:1230). |
| "Register material" (transactional) | `AwsmRenderer::register_material` + dynamic registry reconcile | Mirror `AwsmDynamicMaterialError`. `material-editor` `recompile.rs` + `host.rs` (`RendererRecompileSink`) already implement the debounced compile→register loop. |
| "DEBUG values drive preview only" | Preview renderer's per-instance uniform defaults | A mesh overrides them when the material is assigned in a scene. |
| Uniforms / Textures / Buffers slots | `MaterialLayout` (`awsm-materials` `dynamic_layout`): `UniformFieldRuntime`, `TextureSlotRuntime`, `BufferSlotRuntime` | Buffers = extras-pool slices. |
| **"material pass dependencies"** (your term; absent from prototype) | **`ShaderIncludes`** + **`FragmentInputs`** (`materials/src/shader_includes.rs`) | "Heart of skinny materials." See §7. |
| Material `swatch` gradients | n/a — use real preview-render thumbnails or a solid debug-color swatch | Prototype thumbnails are CSS gradients we don't have. |

## 4. What is reused vs. rewritten

**Reused as-is (logic, no redesign):**
- The renderer + `actions::*` + scene graph + `renderer_bridge` + FS + history +
  worker pool from `scene-editor`.
- `material-editor`'s `EditState` + `recompile` + `host` + FS material load/save.
- `web-shared` **non-UI** utilities: `perf`, `util/*` (signal/window/storage/
  mixins/async_loader/config), `logger`, `error`, `free_camera`.

**Rewritten (the new design system, in `web-shared`):**
- `web-shared/src/theme/*` — port `tokens.css` (graphite/slate OKLCH surfaces,
  azure accent `#5b8dd6` user-tweakable, amber reserved for viewport selection,
  radii 4/6/9/13px, JetBrains Mono for code/numerics, system-ui for chrome) into
  `ColorRaw`/`ColorBackground`/`ColorText`/`ColorBorder`/`ChromeColor`/
  `ChromeFill`/`ChromeShadow`/`FontSize`/`FontWeight`/`ZIndex`.
- `web-shared/src/atoms/*` — rewrite the visual layer of existing atoms (`Button`,
  `Checkbox`, `Modal`, `TextInput`, `TextArea`, `Dropdown`, `FilePicker`,
  `Label`, `ProgressBar`, `Toast`, `icons`/`dynamic_svg`). **Keep public builder
  APIs stable where practical** so M1-seeded code keeps compiling; change a
  call-site only when the new design needs it.
- **Add the new atoms the prototype needs** (these don't exist yet): `Segmented`
  (mode switch / alpha mode / layout switch), `Badge` (ready/draft/error tones),
  `Popup`/`MenuItem`/`MenuSep` (overflow + assign chevron + context menus),
  `Section`/`Row`/`PanelHeader`/`kicker` (rail sections), `Swatch` (color),
  `Toggle`, `NumField`, `Segmented`, `IconBtn`, `Tooltip`. Names are guidance;
  match the prototype's components in `ui.jsx`/`ui-extra.jsx`.

**Newly homed in `web-shared` (folded from `crates/editor`, §5):** `grid`,
`transform_controller`, `point_handle` (3D viewport helpers — not UI atoms).

> `model-tests` keeps working through all of this: it has its own local atoms +
> theme and only imports `web-shared::perf::resolve_renderer_profile` (stable).
> Its only required edit is re-pointing its `awsm_renderer_editor::{grid,
> transform_controller}` imports at `web-shared` (§5.3).

## 5. Eliminating `crates/editor` (`awsm-renderer-editor`)

### 5.1 What it is
`packages/crates/editor/src/`: `grid/` (`pipelines.rs` `EditorPipelines` +
`render.rs` `render_grid`), `transform_controller.rs` (TRS gizmo:
`TransformController`, `TransformObject`, `TransformTarget`, `GizmoKind`,
`GizmoSpace`, `ray_plane_intersection`), `point_handle.rs` (`PointHandleSet`
control-point gizmo). Depends on `awsm-renderer`, `awsm-renderer-gltf`,
`awsm-meshgen`, `glam`, `web-sys`. **No published library depends on it.** It is
currently published to crates.io only because it lacks a `publish = false` flag.

### 5.2 Fold target = `web-shared` (rationale / deviation note)
You asked to fold it "into the new frontend editor crate." Doing so literally
breaks `model-tests`, which **also** imports `transform_controller` + `grid`
(`model-tests/src/pages/app/scene.rs`, `.../scene/editor.rs`,
`.../sidebar/editor.rs`). A frontend **binary** crate can't be a clean dependency
of another frontend, and duplicating gizmo math is worse. **`web-shared` is the
shared frontend lib both already depend on**, already depends on `awsm-renderer`,
and is being heavily edited anyway (§4). So fold there. → If you'd rather
duplicate the gizmo/grid into `model-tests` and put the originals in the editor
crate, that's the only alternative; say so and I'll flip it.

### 5.3 Mechanics
1. Move `grid/`, `transform_controller.rs`, `point_handle.rs` into
   `web-shared/src/` (e.g. under a `viewport3d` module, or top-level `grid` +
   `gizmo`). Add `awsm-renderer-gltf` + `awsm-meshgen` to `web-shared`'s deps;
   move the `shaders/grid.wgsl` asset too.
2. Re-point imports: the new `editor` crate and `model-tests` import
   `awsm_web_shared::{viewport3d::grid::…, viewport3d::transform_controller::…}`
   (or chosen path) instead of `awsm_renderer_editor::…`.
3. Delete `packages/crates/editor/`. Remove it from `[workspace] members` and
   remove the `awsm-renderer-editor = { path = …, version = "0.2.0" }` entry in
   root `Cargo.toml` (`[workspace.dependencies]`).
4. Remove `PATH_CRATE_EDITOR` from `taskfiles/config.yml` and drop the
   `--watch "{{.PATH_CRATE_EDITOR}}"` lines from `model-tests.yml` (and the new
   `editor.yml`), adding a `--watch` on `web-shared` instead (already present for
   the editor; add to `model-tests.yml`).
5. `cargo publish --workspace` (the `publish`/`_publish` task) now has one fewer
   member; nothing references it.

## 6. Target architecture

### 6.1 Crate skeleton
Seed `packages/frontend/editor` from `scene-editor` (the superset), then graft
`material-editor`'s panes as a mode:

```
packages/frontend/editor/
  Cargo.toml          # package = "awsm-editor"; publish = false; union of both dep lists
  index.html          # boot-loader + fonts + gizmo asset copy-dir (from scene-editor)
  src/
    main.rs           # bootstrap, worker registration, RAF loop, mode router
    app.rs            # top bar + mode switch + ribbon host + overlays (the App shell)
    mode.rs           # Mode { Scene, Material } as a Mutable; drives ribbon + workspace
    context.rs        # RendererHandle(s), camera, compile/error handles, WorkerPool
    common/           # bootstrap, config, fs, keys, error, content_hash, command_palette,
                      #   settings_drawer, content_browser, overflow_menu, toasts host
    scene/            # ← scene-editor: state, scene graph, renderer_bridge, canvas,
                      #   tree (outliner), properties (inspector), ribbon rows, actions
    material/         # ← material-editor: EditState, recompile, host, panes
                      #   (library, definition incl. NEW dependencies section, preview,
                      #    code/wgsl, contract drawer)
    prelude.rs
```

`common/` vs `scene/` vs `material/` organization is the implementer's call; the
constraints are no path/symbol collisions and a clean Scene/Material seam.

### 6.2 Modes, renderer instances, canvas
- A single `Mutable<Mode>` (`Scene` | `Material`) drives the ribbon (§9.1) and
  the workspace body, matching the prototype's `mode` state (`app.jsx:128`).
- **Two renderer contexts, lazily created** (decided default — lowest risk):
  Scene mode = the full `AwsmRenderer` (pipelines, picking, gizmo, shadows, env);
  Material mode = the lightweight preview renderer (preview ball + mesh switcher +
  env presets). Separate canvases; shared worker pool + `web-shared`.
- Reuse `scene-editor/canvas.rs` (pointer→pick/gizmo/camera) for the Scene
  viewport; `material-editor/panes/preview.rs` for the Material preview.

### 6.3 Scene ↔ Material hand-off
Mirror `app.jsx` (`materialFrom`, `onEditMaterial`, breadcrumb, post-register
"Assign to {object}" toast) with real state: Scene inspector "material block" →
**Edit** sets `material_from: Option<NodeId>` and flips `Mode::Material`, loading
that material's `EditState`; Material mode shows breadcrumb `‹ Scene ▸ {object} ▸
{material}` (`material-shell.jsx:145`); **Register** (transactional, blocked on
compile errors) succeeds → toast **Assign to {object}** flips back and assigns.

## 7. Material Pass Dependencies (net-new) — decision #4: UI + backend

### 7.1 What it is
`materials/src/shader_includes.rs`: a material declares the optional shared
shader modules it uses; the renderer compiles the **transitive closure**
(`ShaderIncludes::resolve`) and emits only those `{% include %}`s. Bindings stay
full/pass-owned; only WGSL *code* is gated.
- **`ShaderIncludes`** (u32 bitset, append-only): `MATH, CAMERA, COLOR_SPACE,
  TEXTURES, VERTEX_COLOR, LIGHT_ACCESS, APPLY_LIGHTING, BRDF,
  MATERIAL_COLOR_CALC, SHADOWS, SKYBOX, EXTRAS` (bit 9 retired). One-hop deps in
  `direct_deps()` (e.g. `APPLY_LIGHTING → BRDF, LIGHT_ACCESS, MATH, CAMERA`).
- **`FragmentInputs`** (u32 bitset): `NORMALS, TANGENTS, UV, LIGHTS, VIEW_DIR,
  VERTEX_COLOR`.
- First-party materials each declare a precise constant (`pbr.rs:450`,
  `unlit.rs:68`, `toon.rs:92`, `flipbook.rs:173`).

### 7.2 The gap to close
Custom/dynamic materials opt into everything: `dynamic.rs:175`
`shader_includes() -> ShaderIncludes::all()` (and `fragment_inputs() -> all()`),
with the comment *"until the dynamic-registration API carries a per-material
declaration, dynamic materials opt into the full optional surface."* Every custom
material is "fat." This feature lets authors declare a tighter set and has the
renderer honor it.

### 7.3 Backend (renderer + materials)
1. **Carry it.** Extend the dynamic registration record
   (`renderer/src/dynamic_materials/registry.rs`) and the `register_material`
   surface with `shader_includes: ShaderIncludes` + `fragment_inputs:
   FragmentInputs`.
2. **Honor it.** `DynamicMaterial::shader_includes()`/`fragment_inputs()` read the
   registered set (via the registry, as the existing TODO anticipates) instead of
   `all()`. The renderer already resolves the closure + emits only needed
   includes — feed it the declared set.
3. **Cache key.** Fold the declared set into the dynamic shader cache key so same
   WGSL + different deps don't collide and a change re-specializes. Additive to
   the `(ShadingBase, features)` first-party variant map (custom = dynamic id
   range).
4. **Default-safe.** Absent/empty declaration ⇒ default to `all()` (back-compat).
   Author narrowing is opt-in. When a narrowed material references an ungated
   symbol, surface a clear "referenced a module you didn't declare" compile
   diagnostic.
5. **Persist.** Add the declared sets to `material.json` (§10), versioned;
   default `all()` when absent.

> ⚠️ Steps 1–3 touch `awsm-materials` + `awsm-renderer` and move GPU material
> baselines (memory "Materials overhaul verification": tuning-50-materials
> checksum). Run the GPU verification loop after M7 and re-baseline intentionally
> if outputs shift — do not let the loop silently absorb a regression.

### 7.4 UI (Material-mode Definition rail)
Add a **"Pass Dependencies"** collapsible `Section` to `DefinitionPanel`
(`material-mode.jsx:133`), with Surface / Uniforms / Textures / Buffers. Two chip
groups: **Shader includes** (one chip per `ShaderIncludes` flag) and **Fragment
inputs** (one per `FragmentInputs` flag).
- Toggling a chip updates the declared set in `EditState`; recompile re-registers
  (debounced, like WGSL edits).
- Render the **resolved closure** distinctly: author-picked flags active; flags
  pulled in by `resolve()` shown derived/locked with a "required by X" tooltip.
  Drive this from the real `ShaderIncludes` API (call `resolve`/`direct_deps`) —
  do not reimplement the closure.
- Short banner explaining skinny materials ("Declare only what your WGSL uses;
  declaring less = a leaner pipeline").
- "Auto-detect from WGSL" is **out of scope** — declaration is authoritative.

## 8. Scene mode (faithful to prototype + INVENTORY)

Three columns + collapsible bottom drawer; a 2-row ribbon above.

### 8.1 Ribbon: `Insert | Object | Environment | Camera` + Assets toggle
(`ribbon-rows.jsx`, `header/*`). "Editor" tab removed (→ Settings drawer §9).
- **Insert:** Empty · Model… (.glb/.gltf) · Light… (Directional/Point/Spot) ·
  Collision… (Box/Sphere/Capsule/Cylinder/Cone/Ellipsoid) · Camera · Primitive…
  (Plane/Box/Sphere/Cylinder/Cone/Torus) · Curve… (Curve/Sweep/Instances) ·
  Visual… (Line/Sprite/Particle Emitter/Decal/Shared Mesh) · + Material Asset.
- **Object:** Duplicate · Split (model >1 prim) · Deselect · Delete — disabled
  w/o selection.
- **Environment:** Skybox… · IBL… · Shadows… (modals).
- **Camera:** Reset View · Projection · View (Free Fly + authored cameras).
- **Assets** toggle opens the Content Browser (§9). Wire to real `actions::*`.

### 8.2 Left — Outliner (`tree/`, `scene-mode.jsx`)
Kind icon · name · visibility eye · lock; group collapse; single + **multi-select**
(ctrl/cmd toggle, shift extends, primary = last); **drag-to-reparent**;
**drag-to-assign** (drop material card on a row); right-click context menu
(Rename/Duplicate/Lock/Hide/Mark prefab/Delete); **empty state**; missing-asset
red glyph; prefab "PF" tag; locked rows refuse select/drag but accept drop +
context menu.

### 8.3 Center — Viewport (`canvas.rs`, `viewport.jsx`)
Real WebGPU canvas + overlay chrome: transform-tool palette
(Select/Move/Rotate/Scale + tooltips/shortcuts), nav cube, shading toggles,
readout chips (object · tris; projection · focal length). Accepts material drops.

### 8.4 Right — Inspector (`properties/`, `kind-editors.jsx`)
Priority: selected asset > node selection. 0 → hint; 1 node → name, prefab
toggle, Transform (TRS, Euler/Quat, reset) + kind editor + material block +
shadows; **2+ → batch panel**. Content-browser asset selected → Asset inspector
with "‹ Properties" return. **Material block** is the entry to Material mode
(assigned material + quick params + **Edit**); first-party PBR params edited here
(decision #3).

## 9. Cross-cutting chrome

- **Top bar** (`app.jsx`): Brand · `Scene/Material` segmented · ⚙ Settings · ⌘K
  search · project label (dirty dot/name/unsaved) · New/Save/Undo/Redo · ⋯
  overflow (red dot when missing assets). Wire to `actions::project`/`history`.
- **Command palette ⌘K** (`extras.jsx`): fuzzy — switch mode, open Settings,
  toggle Content Browser, insert any kind, select any object, open any material.
  ↑↓/↵, Esc.
- **Settings drawer** (`settings-overflow.jsx`): replaces "Editor" tab. Viewport
  (Grid/Gizmo/MSAA/Heatmap) + Display (units/accent/density); Grid/Gizmo also as
  viewport overlay toggles.
- **Content Browser** (`content-browser.jsx`): collapsible bottom drawer. Tabs
  (All/Materials/Textures/Meshes + counts), search, add buttons, thumbnail grid.
  Cards draggable (assign) + double-click material → Material mode.
- **Overflow ⋯:** Scene stats · Missing assets (N) · Clean unused (N) · About ·
  Clear All. **Toasts** for action feedback.

## 10. Material mode (custom WGSL only — decision #3)

Library · Definition rail · main (preview + code), Studio/Code/Split layout
switch (`material-mode.jsx`, `material-shell.jsx`). Backed by `material-editor`'s
`EditState` + `recompile` + `host`.
- **Library** (`MaterialLibrary`): custom-material list, ready/draft/error badges,
  "on {selected object}" marker, New material.
- **Definition rail** (`DefinitionPanel`): Surface (alpha Opaque/Mask/Blend +
  Cutoff, Double-sided, Base color) · **Pass Dependencies (NEW §7.4)** ·
  Uniforms · Textures · Buffers, each slot with its DEBUG preview-value editor +
  the "debug values drive preview only" banner.
- **Preview** (`preview.rs`): shaded ball + mesh switcher + env presets; error
  overlay on WGSL failure.
- **Code** (`wgsl_editor.rs` / `code-editor.jsx`): `shader.wgsl` editor +
  highlight + Problems strip + slide-out **Material Contract** drawer.
- **Material ribbon**: layout switch · Format · **bucket meter**
  (`bucketsUsed / MAX_BUCKET_ENTRIES`) · **Assign** split-button (quick + chevron
  → filterable mesh list) · **Register/Update** (transactional; "Can't register"
  on errors).

**Persistence (decision #5):** FS-Access folders — `material.json` (alpha,
double-sided, base color, layout, **declared ShaderIncludes/FragmentInputs**) +
`shader.wgsl`. Reuse FS load/save + `buffer_converter.rs`. `?folder=`/`?material=`
kept only as a convenience entry param into the unified app.

## 11. Tooling / build / deploy (big-bang)

1. **New** `taskfiles/frontend/editor.yml` (model on `scene-editor.yml`): `dev`
   = `trunk serve --port {{.PORT_EDITOR_DEV}}` watching `editor`, `renderer`,
   `renderer-core`, `scene-schema`, `web-shared`, `materials` (no more
   `PATH_CRATE_EDITOR`); `build` mirrors scene-editor's prod build (drop
   `assets/world`, write `404.html`).
2. **config.yml:** add `PATH_CRATE_MATERIALS`, `PATH_CRATE_EDITOR_FRONTEND`
   (`frontend/editor`), `PORT_EDITOR_DEV: 9085` (fresh port — avoids clashing
   with still-present scene-editor `9081`/material-editor `9084` during dev),
   `URL_DEV_EDITOR`, `URL_PROD_EDITOR` (`…/editor/`). **Remove** `PATH_CRATE_EDITOR`
   and the scene/material-editor URL+port entries in the final cleanup (M10).
3. **Taskfile.yml:** add the `editor` include + `editor-dev` aggregate; in M10
   replace `material-editor`/`scene-editor` includes and aggregate tasks; keep
   `model-tests`, `media-*`, `lint`, `publish`.
4. **site-index:** `index.template.html` + `site-index.yml` — replace
   `URL_SCENE_EDITOR` + `URL_MATERIAL_EDITOR` with one `URL_EDITOR` (dev
   `http://localhost:9085`, prod `…/editor/`). `site-index` stays the landing
   page; `editor` + `model-tests` are its entries.
5. **Cargo.toml workspace:** add `packages/frontend/editor`; in M10 remove
   `packages/frontend/material-editor`, `packages/frontend/scene-editor`, and
   `packages/crates/editor` (the last in M1, §5).
6. **Delete (M10):** `scene-editor`, `material-editor` crates + their taskfiles;
   grep the repo for `scene-editor`/`material-editor`/`URL_MATERIAL_EDITOR`/
   `awsm-scene-editor`/`awsm-material-editor`/`awsm-renderer-editor`/
   `awsm_renderer_editor` and clean stragglers (CI `.github/workflows`, README,
   docs, deploy scripts).

## 12. Milestones (one branch `editor`, ordered, each ends `task lint`-green)

1. **M1 — Scaffold + eliminate `crates/editor`.** Create
   `packages/frontend/editor` seeded from `scene-editor`; rename package to
   `awsm-editor` (`publish = false`); add to workspace + a working `editor.yml`
   (`PORT_EDITOR_DEV: 9085`). Fold `grid`/`transform_controller`/`point_handle`
   into `web-shared` (§5); re-point `editor` **and** `model-tests` imports; delete
   `crates/editor`; update workspace/config/taskfile watch lists.
   **Verify:** `task editor-dev` serves Scene mode at scene-editor parity (old
   look) on :9085; `task model-tests-dev` still builds/serves; `task lint` green.
2. **M2 — New design system** in `web-shared` (§4, §7-decision): port
   `tokens.css` → theme; rewrite atom visuals (stable APIs); add the new atoms
   (`Segmented`, `Badge`, `Popup`/`MenuItem`, `Section`/`Row`/`PanelHeader`,
   `Swatch`, `Toggle`, `NumField`, `IconBtn`, …).
   **Verify:** editor compiles + reflects the new look where atoms are used;
   screenshot vs prototype.
3. **M3 — Shell + mode switch.** Top bar, `Scene/Material` segmented, mode router,
   toasts host, overflow menu, ⌘K skeleton (Scene commands first).
4. **M4 — Restyle Scene chrome** to the prototype (`01-scene-mode.png`): ribbon
   (Insert/Object/Env/Camera), outliner (context menu, group collapse,
   multi-select, drag), inspector layout, Settings drawer.
5. **M5 — Content Browser** (`04-content-browser.png`): bottom drawer replacing
   Assets dropdowns; asset inspector + clean return; drag-to-assign;
   multi-select/batch (`05-multiselect-batch.png`).
6. **M6 — Fold in Material mode** (`02-material-mode.png`): bring `material-editor`
   panes in as the Material workspace (Library/Definition/Preview/Code + layout
   switch + contract drawer + material ribbon + bucket meter); wire the
   Scene↔Material hand-off (§6.3). FS-Access persistence reused.
7. **M7 — Pass Dependencies backend** (§7.3): thread declared sets through
   registration + emit + cache key; default-safe; persist to `material.json`.
   **Run the GPU verification loop**; reconcile baselines.
8. **M8 — Pass Dependencies UI** (§7.4): the Definition-rail section, closure-aware
   chips driven by the real `ShaderIncludes` API.
9. **M9 — Command palette completeness** (`03-command-palette.png`: open
   materials, select objects, insert kinds), shortcuts beyond ⌘K, per-state
   polish across all 5 screenshots.
10. **M10 — Cutover/cleanup.** Delete `scene-editor` + `material-editor` crates +
    taskfiles; update `site-index`, `Taskfile.yml`, `config.yml`, workspace
    members, CI, README; grep stragglers; `task lint` green; final
    prototype-vs-build diff across all screenshots.

## 13. Decided defaults + definition of done

Resolved (no need to ask): two lazy renderer contexts (§6.2); pass-deps default
to `all()` with opt-in narrowing (§7.3.4); editor dev port `9085` (§11.2);
`material.json` versioned, default `all()` when the declared sets are absent;
gizmo/grid fold target = `web-shared` (§5.2); design system rewritten in
`web-shared`, `model-tests` untouched (§4).

**Out of scope for v1** (flesh out later from existing `_src`/`kind_editor`):
deep particle/sweep/decal editors, "Format", full per-action undo/redo,
auto-detect of pass-deps from WGSL.

**Definition of done:**
- `packages/crates/editor`, `packages/frontend/scene-editor`,
  `packages/frontend/material-editor` are deleted; no workspace/taskfile/CI/
  README reference survives.
- `task editor-dev` serves a unified editor on :9085 with both modes; it matches
  the prototype across `01`–`05` (verified via Chrome MCP tab diff).
- Custom materials carry author-declared `ShaderIncludes`/`FragmentInputs`,
  honored by the renderer (not `all()`), persisted in `material.json`; GPU
  verification reconciled.
- `task lint` green; `model-tests` still builds + serves.

## 14. Reference index

`~/Downloads/editor-reference/` — **visual reference only, do not port JSX:**
- `app.jsx` — shell/behavior map (state, flows, toasts, register/assign).
- `scene-mode.jsx`, `kind-editors.jsx`, `ribbon-rows.jsx` — Scene mode.
- `material-mode.jsx` (DefinitionPanel — home of the NEW deps section),
  `material-shell.jsx` (code pane + contract drawer + layout composer),
  `material-preview.jsx`.
- `content-browser.jsx`, `settings-overflow.jsx`, `extras.jsx` (⌘K),
  `code-editor.jsx`, `viewport.jsx`.
- `data.jsx` — mock data model (mirror shape onto the real schema, not 1:1).
- **`tokens.css` — the source of truth for the new design system (§4, §7); port
  it into `web-shared` theme.**
- `ui.jsx` / `ui-extra.jsx` — the atom set to recreate in `web-shared` (§4).
- `handoff_screenshots/01–05` — Scene, Material, ⌘K, Content Browser,
  Multiselect/batch.
- **Ignore** `tweaks-panel.jsx` (the prototype's own dev-tweak harness) and the
  `_src/` bundle (our *old* source; this repo is canonical).
```

