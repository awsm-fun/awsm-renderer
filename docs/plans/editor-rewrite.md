# Editor Rewrite v2 — prototype-first, from a blank slate

> **Status:** Planning, ready to execute via a fresh `/goal`. This **replaces**
> the v1 plan. v1 seeded the new editor from the old `scene-editor` and reskinned
> it — which baked the old information architecture in and could never reach the
> prototype. This plan builds the UI **from scratch** against the reference,
> using the old code only as a how-to reference for Rust/WASM/dominator + the
> renderer wiring.

---

## 0. Execution contract (read first)

Run **start-to-finish autonomously** from a single `/goal`.

- **Branch:** `editor` (continue on it; the v1 work is archived in-tree, see §3).

- **RUN + DEBUG + VERIFY ONLY IN REAL CHROME, VIA THE CLAUDE-IN-CHROME MCP.**
  Never use the IDE/internal "Launch preview" panel for verification — it is not
  the real WebGPU runtime. The workflow is: `mcp__Claude_in_Chrome__tabs_context_mcp`
  → `…__navigate` → `…__computer`(screenshot) / `…__read_console_messages`. The
  build is served by `trunk` and loaded as a URL in a real Chrome tab.
  - Reference: `cd ~/Downloads/editor-reference && http-server --cors --index -p 9090`
  - Build: `task editor-dev`  → `http://localhost:9085`
  - Keep **two real-Chrome MCP tabs** open — reference `:9090` and build `:9085`.
    For every panel: screenshot both, diff layout/spacing/color/type/per-state +
    interactions, iterate until they match. Check `read_console_messages`
    (onlyErrors) after each load — zero panics / GPU validation errors.

- **SANITY-CHECK THE MCP WORKFLOW BEFORE BUILDING (part of M0, hard gate).**
  Before writing UI code: serve the reference, `navigate` a real Chrome MCP tab
  to `:9090`, and confirm you can **screenshot it** and **read its console**. If
  the Claude-in-Chrome MCP is unavailable or can't drive a real tab, **STOP and
  surface that** — do not proceed building blind against the internal preview.
  (During planning this was validated: both `:9090` and `:9085` drove cleanly in
  real Chrome — screenshots + console reads worked.)

- **Order:** milestones **M0 → M12** in order. Keep the branch compiling and
  **`task lint` green** (rustfmt + clippy `-D warnings`, all crates) at the end
  of every milestone.

- **COMPLETION SIGNAL.** When the entire plan is done and the §14 Definition of
  Done holds (all milestones, lint green, verified in real Chrome, GPU
  reconciled), end with the literal line on its own:
  **`FINISHED!!! WOOHOOO!!!`** — and only then. Do not write it early.
- **Gate at M1.** The multi-renderer-instance refactor is **audited first** (§6).
  If per-instance device-scoping regresses performance, STOP and switch the
  Material preview to the single-renderer fallback (§6.4) — note it inline and
  continue. This is the one place to pause-and-decide; everything else has a
  decided default (§13).
- **Don't stop to ask.** Defaults are decided here. If a genuinely new ambiguity
  appears, pick the most reversible option, note it inline, continue.
- **Fidelity bar:** the reference is the source of truth for **UX, UI, layout,
  icons, and interactions** — 100%. Where the real engine and the mock data
  model diverge, prefer the prototype's UX and adapt the real model.

---

## 1. Why v1 failed + the corrected method

**v1's mistake:** it `cp`'d `scene-editor/src` into the new crate and reskinned
it. Result — under the graphite paint it's still the old editor: the old
full-width tab header (not the prototype's compact top bar + ribbon), the old
`kind_editor` inspector that leaks material internals (shading, vertex colors)
onto meshes, and the old `material-editor` 4-pane folded in wholesale instead of
the prototype's Studio layout with a live preview ball. Reskinning can't reach
the prototype because the prototype's **information architecture** is different,
not just its colors.

**The method this time:**
1. **Build the DOM to match the reference**, panel by panel, verified tab-to-tab
   in real Chrome. UI is written from a blank slate.
2. **Pull the engine in deliberately, as each feature needs it** — the renderer
   bootstrap, `renderer_bridge` (GPU↔scene sync), scene-graph model, `actions`,
   FS, gizmo, picking, worker pool. These are UI-agnostic and correct; they are
   *referenced/adapted* from the archived old crate, not inherited as a skeleton.
3. **Keep the three correct non-UI wins from v1** (§3).

---

## 2. Locked decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Method | **Blank-slate rebuild** of `packages/frontend/editor`; old editor archived as `packages/frontend/bad-editor-whoopsie`, used only as a code reference. |
| 2 | Scope | **Full functional parity** with the old editor (all node kinds, glTF import, env/skybox/IBL, shadows, save/load), rewired behind the prototype-faithful UI. Nothing regresses. |
| 3 | Material model | **Two kinds.** (a) **Custom** = reusable registered dynamic-WGSL material *assets* — the only thing editable in Material mode (the Studio), shared across meshes. (b) **Built-in** = a **fixed palette of the four first-party families** (PBR/Unlit/Toon/Flipbook) shown in the Content Browser; assigning one sets an **inline** first-party material on the mesh with per-mesh params (edited in the mesh **Material block**, never opened in the Studio). Built-ins are NOT shared assets and NOT files. The **Material-mode Library is custom-only**; the **Scene Content Browser shows both** (built-ins marked with a distinct outline + family glyph/"built-in" chip). |
| 4 | Persistence | **A project *directory* of TOML + separate source/binary files** (FS Access *directory* picker), replacing both `.awsm`-single-file and the old per-material FS folders. Layout (flat at root): `project.toml` (scene graph · per-mesh inline built-in material params · refs to custom materials by id · env · camera · settings); `material-<id>.toml` + `material-<id>.wgsl` per **custom** material (alpha/double-sided/base-color/slots/declared pass-deps/debug values + file refs); `assets/textures/<hash>.<ext>`, `assets/buffers/<id>.bin`, `assets/models/<hash>.glb`. TOML via `toml`+serde (wasm-OK). Independent of `scene-schema`'s `project.json` (model-tests/tuning-scenes untouched). |
| 5 | Renderer instances | **First-class N independent `AwsmRenderer` instances** via per-device-scoped renderer-core caches (§6) — the Material preview is the first second-instance; future multi-renderer games are supported by construction. **Gated on an audit (M1): if it regresses perf, fall back** to single-renderer preview (§6.4). |
| 6 | Old crate in workspace | **Removed from workspace members + taskfiles** (reference-only; never compiled). |
| 7 | Inspector | **100% prototype fidelity** — IA, icons, UX, per-kind editors (`kind-editors.jsx`). No material internals on the mesh. |
| 8 | Controller architecture | **Every editor mutation is a serializable `EditorCommand` dispatched through one `EditorController` singleton; the UI is just one driver.** Commands are **invertible and form the undo/redo log** (command-sourcing — replaces snapshot-based undo). The controller also exposes a serializable **query/snapshot** read API (scene tree · selection · materials · compile errors · mode · project state) so external agents can inspect state. UI event handlers build + dispatch commands; they never mutate editor state directly. A future MCP/websocket transport is a **thin adapter** over the same `dispatch`/`snapshot` — **designed for now, NOT built now** (only the clean seam). Reuse the old `actions::*` as *command implementations*; the old snapshot-history is replaced by the command log. |

---

## 3. Keep / archive / build-fresh

**Keep as-is (correct, non-UI — do NOT redo):**
- **M7 pass-deps backend** in `awsm-materials` + `awsm-renderer`: `ShaderIncludes`/
  `FragmentInputs` on `MaterialRegistration` → `DynamicShaderInfo` → the 3
  shading-host templates' `inc` gating + `dispatch_hash` (+ tests). Behaviour-
  preserving; the new editor reuses it.
- **M1 viewport3d fold** in `web-shared::viewport3d` (grid + transform_controller
  + point_handle); the old `crates/editor` is already gone.
- **M2 design tokens** in `web-shared/src/theme` (the `tokens.css` port). The
  palette is right; it's the *components built on it* that need rebuilding (§5.2).

**Archive (reference only):** the current `packages/frontend/editor` → move to
`packages/frontend/bad-editor-whoopsie`, drop from `Cargo.toml` members + the
`editor` taskfile, remove from CI/site-index. It stays in-tree as a grep target
for engine wiring (bridge, actions, scene model, FS, canvas, context, gizmo).

**Build fresh (the whole UI):** top bar, scene ribbon, outliner, viewport overlay
chrome, inspector, content browser, command palette, settings drawer + modals,
toasts, **and the entire Material mode** (Studio layout, library, definition rail
incl. pass-deps, live preview, code pane, contract drawer, register/assign/
breadcrumb).

---

## 4. Reference map + component inventory

`~/Downloads/editor-reference/` (served at `:9090`). **Do not port JSX** — rebuild
in Rust/dominator. Each file → what it drives:

| File | Drives |
|---|---|
| `app.jsx` | App shell, top bar, mode switch, ribbon host, `MaterialRibbon` (layout switch · bucket meter · **Assign** split-button · **Register**), `Toasts`, the Scene→Material→Register→Assign flow, ⌘K wiring. The clearest behavior map. |
| `tokens.css` | The design tokens (already ported to `web-shared` theme). |
| `ui.jsx` | **Atoms:** `Icon`, `IconBtn`, `Btn`, `Section`, `Row`, `NumField`, `Vec3`, `TextInput`, `Select`, `Toggle`, `Check`, `Segmented`, `Swatch`, `Badge` + the `ICONS` SVG map. |
| `ui-extra.jsx` | `Popup`, `DropButton`, `MenuItem`, `MenuSep`, `Modal`, `Slider`, `RightDrawer`, `ContextMenu`, `DrawerSection`. |
| `scene-mode.jsx` | Outliner (tree · multi-select · collapse · drag-reparent · drag-assign · context menu · empty state) + Inspector host + batch panel + Content Browser host. |
| `kind-editors.jsx` | Per-kind inspector: Light/Camera/Model/Group/Empty + mesh `Geometry` (shape switcher + params) + `MaterialBlock` (asset card + Edit link + Assigned select + base color/metallic/roughness) + `Shadows`. |
| `viewport.jsx` | Viewport overlay chrome: `MaterialBall` (CSS mock of the rendered ball), `Gizmo`, transform-tool palette (Select/Move/Rotate/Scale · Q/W/E/R), `ViewAxis` nav cube, shading modes (Solid/Material/Wire), object·tris + camera readout chips. |
| `ribbon-rows.jsx` | `SceneRibbon` tab strip (Insert/Object/Environment/Camera) + `InsertRow`/`ObjectRow`/`EnvironmentRow`/`CameraRow`/`DropFileRow`. |
| `content-browser.jsx` | `ContentBrowser` (bottom drawer: category tabs+counts · search · add · card grid · drag-assign · dbl-click→Material) + `AssetInspector` (right panel asset editor). |
| `material-mode.jsx` | `MaterialLibrary` (asset list, ready/draft/error badges, "on {obj}") + `DefinitionPanel` (alpha/double-sided/base-color · **DEBUG** value editors · Uniforms/Textures/Buffers; **add Pass Dependencies here**). |
| `material-shell.jsx` | `MaterialMode` layout composer (Library 222 · Definition 244 · main; studio/code/split) + `CodePane` (wgsl + problems strip) + `HelpDrawer` (Material Contract). |
| `material-preview.jsx` | `PreviewPane` — real shaded mesh + mesh switcher (Sphere/Cube/Plane/Cylinder/**Selected object**) + env presets (Studio/Sky/Void) + compile-error dim overlay. |
| `code-editor.jsx` | `CodeEditor` — WGSL editor (syntax highlight + gutter + error lines). |
| `extras.jsx` | `CommandPalette` (⌘K, fuzzy, ↑↓/↵/Esc). |
| `settings-overflow.jsx` | `SettingsDrawer` (was the Editor tab) + `AboutModal`/`ClearAllModal`/`MissingAssetsModal`/`StatsBar`. |
| `data.jsx` | The mock data model — mirror its *shape* onto the real schema, not 1:1. |
| `handoff_screenshots/01–05` | Scene · Material · ⌘K · Content Browser · Multiselect-batch. |
| **Ignore** | `tweaks-panel.jsx` (the prototype's own dev harness), `_src/` (= the old source; the archived crate is canonical). |

---

## 5. Architecture

### 5.1 Crate layout
Fresh `packages/frontend/editor` (`awsm-editor`). Suggested module shape — built
DOM-first; engine modules adapted from the archived crate as features need them:

```
editor/src/
  main.rs            # bootstrap: theme, panic hook, worker reg, scene renderer, mount app
  app.rs             # top bar + mode router + ribbon host + global overlays (toasts/modals/cmdk)
  theme bridge       # (uses web-shared design system)
  controller/        # EditorController singleton (§5.5) — the command/query authority
    command.rs       #   EditorCommand (serde, invertible) — every mutation
    query.rs         #   EditorQuery / snapshot() (serde) — readable editor state
    dispatch.rs      #   dispatch(cmd) + the command-based undo/redo log
  engine/            # adapted from bad-editor-whoopsie (UI-agnostic); the controller
                     #   calls into these (they're command IMPLEMENTATIONS, not called by UI):
    context.rs       #   renderer handle(s), camera handles, worker pool
    bridge/          #   GPU↔scene-graph sync (node_sync, asset_cache, gizmo, env, shadows, …)
    scene/           #   in-memory scene model + mutate (snapshot history REPLACED by command log)
    actions/         #   insert/object/camera/view/project ops (invoked by dispatch)
    fs.rs, keys.rs, content_hash.rs
    project/         #   TOML project dir serialize/deserialize (§10)
  scene_mode/        # FRESH UI: ribbon, outliner, viewport-chrome, inspector (kind editors)
  material_mode/     # FRESH UI: studio layout, library, definition(+pass-deps), preview, code, contract
    preview/         #   2nd-renderer preview (mesh switcher + env presets)  [or single-renderer fallback]
  content_browser.rs # FRESH UI
  command_palette.rs # FRESH UI
  settings/          # FRESH UI: settings drawer + about/clear/missing modals + stats bar
  toasts.rs
```

### 5.2 web-shared design system (rebuild the component library)
Keep the M2 **tokens**. Build the prototype's component set as proper `web-shared`
atoms (salvage/expand the existing ones): `Icon`(+ the full `ICONS` SVG map),
`IconBtn`, `Button` (ghost/quiet/primary/solid + sm/md), `Section` (collapsible,
`right` slot, `dense`), `Row`, `NumField` (axis-tinted, step/min/max/suffix),
`Vec3`, `TextInput`, `Select`, `Toggle`, `Check`, `Segmented`, `Swatch`, `Badge`
(neutral/accent/ok/warn/danger), `Popup`, `DropButton`, `MenuItem`/`MenuSep`,
`Modal`, `Slider`, `RightDrawer`, `ContextMenu`, `DrawerSection`. These are
generic and shared; editor-specific composites live in the editor crate.

### 5.3 Project format = a TOML directory (§10)
The project is a **directory** (picked via the FS Access *directory* picker),
serialized as **TOML** with sources/binaries as separate on-disk files — no
single self-contained file, no base64 embedding. The editor's own model
(`engine::project`) round-trips to/from it. Replaces the old per-material FS
folders. See §10 for the exact layout.

### 5.4 Multiple renderer instances (§6)
Make N independent `AwsmRenderer` instances coexist by per-device-scoping the
renderer-core GPU-resource caches. The Material preview is a second instance with
its own canvas, camera, env, and preview mesh. **Gated on the M1 audit.**

### 5.5 EditorController — the command/query authority (decision 8)
All editor/project state is governed by one `EditorController` (a thread_local
singleton like `AppState`). **The UI is just one driver of it.**

- **`EditorCommand` (serde).** One enum covering *every* mutation: insert/delete/
  duplicate/reparent/rename nodes · select/deselect/set-selection · set transform
  (per-axis TRS) · prefab toggle · per-kind params (geometry shape+params, light,
  camera, model, collider, curve, particle, decal, line, sprite, …) · assign
  material (built-in family **or** custom id) · set built-in material params ·
  create/edit/delete a custom material (alpha · double-sided · base color ·
  uniforms/textures/buffers slots · declared pass-deps · WGSL) · register/update
  material · env (skybox/IBL/shadows) · scene-affecting camera ops · settings ·
  project new/load/save · switch mode. Commands are **data** (no closures), so
  they serialize.
- **Invertible — the command log IS undo/redo.** Applying a command records its
  inverse (captured from current state at apply-time — e.g. `SetTransform` records
  the prior transform; `DeleteNode` captures the removed subtree; `RegisterMaterial`
  inverts to unregister / re-register-previous). Undo pops + applies the inverse;
  redo re-applies. This **replaces** the old snapshot-based history. **Transient**
  commands (selection, mode switch, camera orbit, panel toggles) are dispatched
  but NOT recorded in the undo log (or live in a separate lightweight ring).
- **`EditorController::dispatch(EditorCommand) -> Result<…>`** is the single entry
  point. Async (some commands await the renderer/FS). UI event handlers translate
  gestures → commands → `dispatch`; **never mutate editor state directly.**
- **`snapshot()` / `EditorQuery` (serde):** a serializable read of editor state —
  scene tree (id/name/kind/transform/parent), selection, material list (custom +
  the built-in palette), per-custom-material compile status/errors, mode, project
  dirty/name. For external inspection + headless tests.
- **URL-driven load + import (gesture-free — build now).** FS file-pickers need a
  user gesture, which an external transport (and headless tests) can't supply. So
  the loading/import commands are **source-abstracted** and include URL variants
  that `fetch` over HTTP — no gesture:
  - `LoadProjectFromUrl(base_url)` — fetch `<base_url>/project.toml` + the
    referenced `material-<id>.{toml,wgsl}` + `assets/*` and build the project.
    (`LoadProjectFromDirectory(handle)` is the FS-picker variant.)
  - `ImportModelFromUrl(url)` / `ImportTextureFromUrl(url)` — fetch bytes → create
    the asset. (`…FromFile(file)` are the picker variants.)
  The project (de)serializer is written over a `ProjectSource` = `Url(base)` |
  `Directory(handle)`; asset import over `AssetSource` = `Url(url)` | `File(file)`.
  **Saving** stays a directory handle for now (gesture); the serializer is
  sink-abstracted so a future server/HTTP-PUT sink (for the MCP path) is a thin
  add. This URL path is exactly what the future MCP agent uses to drive remote
  asset imports + project loading.
- **Dev/testing use:** during the build, the agent can serve a known test project
  + assets from a second `http-server` and `LoadProjectFromUrl` it — scriptable,
  gesture-free scene setup for verifying panels (then verify via `snapshot()` +
  real-Chrome screenshots). Round-trip persistence can be checked by serializing
  the project to an in-memory TOML tree and reading it back (no disk write needed).
- **Transport seam (NOT built now).** A future MCP/websocket adapter would
  `serde`-decode a command → `dispatch` and encode `snapshot()` back. Build only
  the seam (+ the URL load/import variants above); not the server itself.
- Pure-UI ephemeral state (hover, which panel is open, the Studio layout toggle)
  may stay local to components — the controller governs *editor/project* state,
  not view chrome.

---

## 6. M1 — Renderer-instance audit + device-scoping (DO THIS FIRST, GATED)

The prototype's live preview needs a second renderer; v1 proved two same-thread
`AwsmRenderer`s throw cross-device `GPUValidationError`s because renderer-core
caches device-bound GPU objects in process-global `thread_local!`s.

### 6.1 The offending surface (audit these)
Device-bound GPU resources cached in `thread_local!` (re-used across devices →
the bug):
- `renderer-core/src/texture/blit.rs` — `BlitPipeline` HashMap cache
- `renderer-core/src/brdf_lut/generate.rs` — `BRDF_LUT_PIPELINE` + sampler
- `renderer-core/src/texture/mipmap.rs` — `MipmapPipeline` HashMap cache
- `renderer-core/src/texture/mega_texture/pipeline.rs` — `AtlasPipeline` + shader
- `renderer-core/src/texture/mega_texture/mipmap.rs` — `MipmapPipeline` cache
- `renderer-core/src/texture/mega_texture/writer.rs` — staging `GpuBuffer`
- `renderer-core/src/texture/convert_srgb.rs` — `ConvertSrgbPipeline` cache

**Not GPU resources (device-agnostic constants — leave):** the many
`LazyLock<TextureUsage>` / `LazyLock<BufferUsage>` (bitflags), `debug.rs` label
maps, `workers/entry.rs` dispatch registry, meta `LazyLock<BufferUsage>`,
`renderer.rs` `CompatibilityRequirements`. Audit `scheduler.rs`'s
`Option<HashSet>` + `edge_buffers.rs` `Mutex<bool>` init-once flags (likely
log-once guards — confirm they don't gate per-renderer GPU state).

### 6.2 The refactor
Re-scope each device-bound cache from a single global to **per-device** (key the
cache by a device identity, or move the cache onto the renderer/device wrapper so
each `AwsmRenderer` owns its own). GPU resources are inherently device-bound, so
the resource must be per-device; only its *creation cost* matters.

### 6.3 The perf gate (the user's explicit condition)
**Audit + measure before committing.** BRDF-LUT integration is the one
expensive-to-create resource (a compute pass); per-device means one extra
generation per renderer instance — a creation-time cost, not per-frame. Confirm:
(a) no per-frame hot-path cost is introduced; (b) per-instance startup cost is
acceptable for a handful of renderers; (c) the existing single-renderer path's
GPU output is **bit-identical** (run the GPU verification loop + materials
baselines — memory "materials-overhaul-verification"). If any of these regress,
**do not ship the refactor** — take §6.4 instead, and record the finding.

### 6.4 Fallback (if the audit says no)
Single renderer: in Material mode, render a dedicated preview scene (the chosen
preview mesh + edited material + env) to a preview canvas driven by the one scene
renderer (swap what it draws by mode), instead of a second instance. Less
general (no multi-renderer games yet) but no renderer-core churn.

### 6.5 M1 RESULT (decided — SHIP the refactor; fallback NOT taken)
Audited all 7 device-bound `thread_local!` GPU caches (§6.1) + the
`AwsmRendererWebGpu` device wrapper. Finding: no device-identity mechanism
existed; all caches keyed on semantic props only (format/MSAA) → a 2nd renderer
with a different device reused device-A objects → cross-device validation errors.

**Refactor shipped:** added a `DeviceId` newtype (monotonic counter assigned once
in `AwsmRendererWebGpuBuilder::build()`, stored on `AwsmRendererWebGpu`, exposed
via `device_id()`). All 7 caches now key by `DeviceId`: blit / mipmap / mega-
texture-mipmap / convert-srgb gained a `device` field in their `HashMap` key;
the singleton BRDF-LUT pipeline+sampler, atlas pipeline+shader-module, and
mega-texture staging buffer went `RefCell<Option<T>>` → `RefCell<HashMap<DeviceId,T>>`.
The `edge_buffers` `Mutex<bool>` are log-once guards (left as-is); `scheduler.rs`
has no offending statics.

**Perf gate (§6.3) — PASS.** The refactor changes only cache *keys*, never any
GPU-object creation path, so for a single device the behaviour is bit-identical
by construction (one id, same objects, same O(1) lookups — no per-frame cost; the
one extra per-device BRDF-LUT generation is the intended creation-time cost).
Empirically confirmed: `model-tests` renders the Fox in a full IBL environment
(reflective metal = BRDF-LUT, mipmapped surfaces, HDR skybox = blit) with **zero
console / GPU-validation errors** in real Chrome after the change. `task lint`
green. (No automated golden-image/checksum harness exists in-repo — the audit
confirmed the tuning scenes are manual-inspection fixtures — so the "bit-identical"
claim rests on the keys-only nature of the change + the clean render, not a
numeric diff.) Multi-instance is now supported by construction; the M10 Material
preview will be the first second-instance.

---

## 7. Scene mode (100% prototype fidelity)

### 7.1 Top bar (`app.jsx`)
Brand mark · **Scene/Material** `Segmented` · ⚙ Settings · ⌘K search button ·
spacer · project label (dirty dot · name · `unsaved`) · New/Save/Load/Undo/Redo
icon buttons · ⋯ overflow (red dot when missing assets). Compact, NOT the old
full-width tabs.

### 7.2 Ribbon (`ribbon-rows.jsx`) — 2 rows
Tab strip **Insert | Object | Environment | Camera** + Assets toggle, then the
active tab's action row:
- **Insert:** Empty · Model… (.glb/.gltf) · Light… (Dir/Point/Spot) · Collision…
  (Box/Sphere/Capsule/Cylinder/Cone/Ellipsoid) · Camera · Primitive…
  (Plane/Box/Sphere/Cylinder/Cone/Torus) · Curve… (Curve/Sweep/Instances) ·
  Visual… (Line/Sprite/Particle/Decal/Shared Mesh) · + Material Asset. **All
  wired to real `actions::insert::*` (full parity).**
- **Object:** Duplicate · Split (model >1 prim) · Deselect · Delete — disabled
  w/o selection.
- **Environment:** Skybox… · IBL… · Shadows… (modals; real env wiring).
- **Camera:** Reset View · Projection · View (Free-Fly + authored cameras).

### 7.3 Outliner (`scene-mode.jsx`)
Tree (kind icon · name · eye · lock) · group collapse · single+multi-select
(ctrl/cmd, shift-range, primary=last) · drag-reparent · drag-assign (material
card → row) · right-click context menu (Rename/Duplicate/Lock/Hide/Mark
prefab/Delete) · empty state · missing-asset red glyph · prefab PF tag · locked
rows refuse select/drag but accept drop + context menu. Header: kicker title +
add(+). Filter input.

### 7.4 Viewport + overlay chrome (`viewport.jsx`)
The **real WebGPU canvas** + overlays: transform-tool palette (Select/Move/
Rotate/Scale · Q/W/E/R · tooltips), `ViewAxis` nav cube (top-right), shading-mode
toggles (Solid/Material/Wire → real renderer debug modes where available),
readout chips (object · tris; projection · focal length). Grid + gizmo gated by
settings; accepts material drops ("Assign to …"). Amber selection bounds.

### 7.5 Inspector (`kind-editors.jsx`) — priority: asset > node
- 0 → "Nothing selected" hint.
- 1 node → name · prefab toggle · **Transform** (TRS, Euler/Quat segmented,
  reset) · **kind editor** (Light/Camera/Model/Group/Empty / mesh = Geometry
  shape-switcher+params, **Material block**, Shadows). The Material block is the
  single place to edit the assigned material's params (decision 3) and is
  **kind-aware**: a **built-in** material shows its first-party params (PBR →
  base color/metallic/roughness/extras; Unlit → base color; Toon/Flipbook → their
  params), stored inline per-mesh; a **custom** material shows its declared
  uniforms as per-instance overrides + an **Edit→Material** link (WGSL + definition
  edited in the Studio, not here). The `Assigned` select switches between the
  built-in families and the custom material assets.
- 2+ → batch panel (N selected · Duplicate/Deselect/Delete).
- asset selected (content browser) → `AssetInspector` with a "‹ Properties"
  return.

---

## 8. Material mode (100% fidelity — the Studio)

Layout composer (`material-shell.jsx`): grid **Library (222) · Definition (244) ·
main**, with a Material ribbon (layout switch Studio/Code/Split · Format · bucket
meter `N/MAX_BUCKET_ENTRIES` · **Assign** split-button · **Register/Update**).
Breadcrumb `‹ Scene ▸ {obj} ▸ {mat}` when entered from a node.

- **Library** (`MaterialLibrary`): custom-material list · ready/draft/error
  badges · "on {selected obj}" marker · New material. (Custom-WGSL-only, decision 3.)
- **Definition** (`DefinitionPanel`): alpha Opaque/Mask/Blend (+Cutoff) ·
  Double-sided · Base color · the "DEBUG values drive preview only" banner ·
  **Pass Dependencies** (the M7-backed `ShaderIncludes`/`FragmentInputs` chips,
  closure-aware via `resolve()`) · Uniforms/Textures/Buffers slot editors w/
  DEBUG value editors.
- **Preview** (`material-preview.jsx`): the **real rendered material** on a mesh
  (Sphere/Cube/Plane/Cylinder/**Selected object**) in an env preset
  (Studio/Sky/Void) · compiled/error badge · dim overlay on compile failure ·
  floating mesh switcher. (Second renderer instance, or §6.4 fallback.)
- **Code** (`CodePane` + `CodeEditor`): `shader.wgsl` w/ highlight + gutter +
  error lines · Problems strip · Format · help button → **Material Contract**
  slide-out (`HelpDrawer`).
- **Flows:** transactional **Register** (blocked on compile errors → "Can't
  register"); bucket count = `MAX_BUCKET_ENTRIES`; post-register toast "Assign to
  {obj}"; drag-to-assign from the content browser; the Assign split-button's
  chevron opens a filterable mesh list.

---

## 9. Cross-cutting UI
Content Browser (`content-browser.jsx`) — Materials tab lists **custom material
asset cards + the 4 fixed built-in family palette entries** (PBR/Unlit/Toon/
Flipbook, marked with a distinct outline + family glyph/"built-in" chip; no user
count); both are drag-/Assign-able to meshes; double-click a *custom* card →
Material mode (built-ins don't open the Studio). Textures + Meshes tabs as in the
prototype. · Command Palette (`extras.jsx`, full
command set — switch mode, settings, toggle browser, insert any kind, select any
object, open any material) · Settings drawer (`settings-overflow.jsx`: Viewport
grid/gizmo/MSAA/heatmap + Display units/accent/density) · About/ClearAll/Missing-
assets modals · Stats bar · Toasts.

---

## 10. Persistence (TOML project directory) + import
The project is a **directory** the user picks via the FS Access *directory*
picker. All manifests are **TOML**; sources + binaries are separate files. Flat
layout at the project root:

```
my-project/                  ← picked directory = the project
  project.toml               ← scene graph (nodes · transform · kind cfg · per-mesh INLINE built-in
                                material params · refs to custom materials by id) · env · camera · settings
  material-<id>.toml          ← per CUSTOM material only: kind · alpha/double-sided/base-color ·
                                uniform/texture/buffer slots · declared pass-deps · debug values · file refs
  material-<id>.wgsl          ← per CUSTOM material: the WGSL source
  assets/
    textures/<hash>.<ext>    ← png/jpg/ktx, content-hashed (dedup)
    buffers/<id>.bin         ← buffer-slot data
    models/<hash>.glb        ← imported glTF
```

- **Built-in** materials are NOT files — their per-mesh params live inline in the
  mesh's node in `project.toml`. Only **custom** (dynamic-WGSL) materials get a
  `material-<id>.toml` + `material-<id>.wgsl` (they're the reusable shared assets).
- **Load is source-abstracted** (`ProjectSource`, §5.5): from a picked **directory
  handle** (FS Access, gesture) OR from a **base URL** (`fetch` `project.toml` +
  referenced files, gesture-free — for the MCP/external path + headless tests).
  Both build the same in-memory project. Save = write the directory tree (handle).
  `toml` + serde (wasm-OK). Replaces FS-Access material folders; no single-file `.awsm`.
- **glTF import** (full parity), also source-abstracted (`AssetSource`): Insert
  Model… from a **file** (picker) OR from a **URL** (`fetch`, gesture-free). Real
  `actions::insert::model` + the gltf bridge; the .glb is content-hashed into
  `assets/models/`. (Imported glTF materials map to inline built-ins, §13.)
- The renderer/model-tests `scene-schema` `project.json` path is **untouched**
  (the editor's TOML project is its own format; reuse `scene-schema`/`awsm-materials`
  *types* where they already model the renderer contract — e.g. dynamic-material
  definitions — serialized to TOML).

---

## 11. Tooling / cutover
- **M0:** `git mv packages/frontend/editor packages/frontend/bad-editor-whoopsie`;
  remove from `Cargo.toml` members + delete its taskfile include + remove from
  site-index/CI; create a fresh `packages/frontend/editor` (`awsm-editor`,
  `publish=false`) with the new `index.html` (JetBrains Mono + graphite boot) +
  `editor.yml` (dev `:9085`). `bad-editor-whoopsie` is NOT in the workspace (won't
  compile) — pure reference.
- Editor dev port stays **9085**; reference on **9090**. site-index + pages.yml
  already point at `editor/` (kept from v1 cutover).

---

## 12. Milestones (each ends `task lint`-green + verified tab-to-tab in real Chrome)

- **M0 — MCP sanity-check + archive + scaffold.** FIRST: serve the reference
  (`http-server --cors --index -p 9090`) and confirm the Claude-in-Chrome MCP can
  navigate a real tab to `:9090`, screenshot it, and read its console (the §0 hard gate;
  stop if it can't). Then move old editor → `bad-editor-whoopsie` (out of
  workspace); create a fresh empty `editor` crate that boots, mounts the
  design-system stylesheet, and shows an empty app shell on `:9085` — verified in
  a second real-Chrome MCP tab.
- **M1 — Renderer-instance audit + device-scoping (GATED, §6).** Audit, refactor
  renderer-core caches per-device, measure perf + GPU baselines. **Decide:**
  ship multi-instance, or fall back (§6.4). Record the result.
- **M2 — web-shared design system.** Build the full atom/molecule set (§5.2) from
  `ui.jsx`/`ui-extra.jsx`, verified against the prototype.
- **M3 — App shell + EditorController foundation + top bar + mode router.**
  Establish the `EditorController` singleton + `EditorCommand` (invertible) +
  `EditorQuery`/`snapshot()` + the command-based undo/redo log **before any
  panel**, so every later panel follows the dispatch pattern. Brand · Scene/
  Material segmented · settings/⌘K buttons · project label · actions · overflow ·
  toasts host — each wired as a dispatched command. Wire the real scene renderer +
  canvas. **Cross-cutting rule for M4–M12: every mutation is an `EditorCommand`
  through the controller; UI never mutates editor state directly; undo/redo comes
  for free from the command log.**
- **M4 — Scene ribbon + Insert (full parity).** All Insert/Object/Environment/
  Camera rows wired to real `actions::*`.
- **M5 — Outliner.** Tree + multi-select + drag-reparent + context menu + empty
  state, bound to the real scene model.
- **M6 — Viewport + overlay chrome.** Real canvas + transform tools + nav cube +
  shading toggles + readout chips + gizmo + picking + drag-assign.
- **M7 — Inspector (kind editors, full parity).** Transform + every kind editor +
  Material block + Shadows, prototype-faithful, no material internals on meshes.
- **M8 — Content Browser + Asset Inspector.**
- **M9 — Material mode: Studio layout + Library + Definition + Code + Contract.**
  Custom-WGSL authoring + recompile/register into a renderer (single renderer,
  no preview yet).
- **M10 — Material preview** (second renderer instance from M1, or §6.4 fallback):
  mesh switcher + env presets + error overlay. Plus Pass Dependencies chips (M7
  backend reused) + Register/Assign/breadcrumb flow.
- **M11 — TOML-project persistence + glTF import + Settings/modals/cmd-palette/stats.**
  Save/Load (directory handle) **and gesture-free `LoadProjectFromUrl` +
  `ImportModel/TextureFromUrl`** (source-abstracted, §5.5) — exercise the URL path
  by serving a test project from a second `http-server` and loading it. New, Clear
  All, missing-assets, About, full ⌘K.
- **M12 — Polish + parity sweep + DONE.** Pixel-match all 5 screenshots
  tab-to-tab in real Chrome; finish the deeper Insert kinds (curves/sweep/
  instances/particles/decals/colliders/line/sprite) to **real runtime parity**;
  final GPU verification; `task lint` green; confirm §14 Definition of Done in
  full. **Then, and only then, output the literal final line `FINISHED!!! WOOHOOO!!!`.**

---

## 13. Decided defaults + open items

Decided (no need to ask): live-preview = 2nd renderer pending the M1 audit, else
§6.4 fallback; inspector follows the prototype IA with live primitive
regeneration; viewport overlay chrome built in full (wire real, stub the purely-
visual like the nav cube's live orientation + exact tri counts); env presets are
real lit environments (gradient/IBL + a key light); "Selected object" preview
renders the material on a copy of the current selection; web-shared hosts the
shared atoms, editor hosts composites; the project is a TOML directory (§10) replacing material folders.

**Driver's calls (decided — implement these, don't ask):**
- **TOML schema:** a fresh `editor::engine::project` model serialized with
  `toml`+serde; reuse `awsm-materials`/`scene-schema` *types* where they already
  model the renderer contract (dynamic-material definition, declared pass-deps).
- **Deeper Insert kinds = real parity, not appear-only.** Decision #2 is full
  functional parity, so curves/sweep/instances/particles/decals/colliders/line/
  sprite must actually work (wired to the real engine), finished in M12. They may
  land incrementally *within* M12, but M12 isn't done until they're functional.
- **glTF → built-in mapping:** an imported glTF material becomes an **inline
  built-in** material on its mesh — glTF metallic-roughness PBR → PBR built-in
  (base-color factor/texture, metallic, roughness, normal/emissive/occlusion);
  `KHR_materials_unlit` → Unlit built-in. (Imported models never auto-create
  custom-WGSL assets.)
- **Env presets (real lighting via the renderer):** Studio = neutral
  studio IBL/gradient + a key light; Sky = sky-gradient skybox + a sun light;
  Void = near-black + a subtle rim light. Reuse the real env/IBL/skybox path.
- **Built-in card treatment:** the family's-accent outline + a small mono
  `BUILT-IN` chip + the family glyph (instead of a custom-material swatch); no
  user-count; double-click is a no-op (doesn't open the Studio).

---

## 14. Definition of done
- `packages/frontend/editor` is a **from-scratch** crate matching the prototype
  across screenshots 01–05 (verified tab-to-tab in real Chrome); `bad-editor-
  whoopsie` is archived out of the workspace.
- Scene mode at **full parity** with the old editor's functionality; Material mode
  is the **Studio** layout with a **live preview**, custom-WGSL authoring, and the
  closure-aware **Pass Dependencies** UI (M7 backend).
- Multiple renderer instances work (or the documented single-renderer fallback is
  in place, with the audit result recorded).
- Projects save/load as a **TOML directory** (`project.toml` + `material-<id>.{toml,wgsl}`
  + on-disk `assets/`); load + asset/glTF import work **both** from an FS directory
  handle (picker) **and gesture-free from a URL** (`LoadProjectFromUrl` /
  `ImportModel/TextureFromUrl`, §5.5) — verified by loading a served test project.
  Built-in materials (PBR/Unlit/Toon/Flipbook) are assignable from the Content
  Browser; only custom materials open in the Studio.
- Every editor mutation is a serializable `EditorCommand` dispatched through the
  `EditorController`; undo/redo is the command log; a serializable `snapshot()`/
  query surface exists. (No MCP/websocket transport yet — only the documented
  seam.) UI never mutates editor state directly.
- Every Scene-mode panel + Material-mode Studio + content browser + ⌘K + settings
  was **verified in real Chrome via the MCP** (not the internal preview), diffed
  against the reference until matching.
- `task lint` green; GPU verification reconciled; `model-tests` still builds.
- **When all of the above holds, output the literal line `FINISHED!!! WOOHOOO!!!`**
  as the final message (and not before).
```

