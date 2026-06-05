# Animation Editor — implementation plan (single-`/goal` executable)

> **Status:** Planning complete, ready to execute via one `/goal`. This plan adds
> a third top-level workspace — **Animation** — to `packages/frontend/editor`
> (`awsm-editor`), alongside **Scene** and **Material**, and implements the
> renderer-core + crate changes the feature needs end-to-end. It is the animation
> analog of [editor-rewrite.md](editor-rewrite.md), and reuses that editor's
> architecture wholesale (EditorController command/query, the bridge, TOML
> persistence, web-shared design system).
>
> It is inspired by the React design prototype at
> `~/Downloads/animation-reference/` (and its `HANDOFF.md`), but is **much more
> thorough**, maps everything onto the **real** editor + renderer code, and fills
> the gaps the HANDOFF left open (serving the reference, the renderer-core
> blending redesign, light/camera/material-param targets, cross-tab sync,
> verification methodology).

---

## 0. Execution contract (READ FIRST)

Run **start-to-finish autonomously** from a single `/goal`. Work milestone by
milestone (§12), keeping the branch compiling and **`task lint` green** (rustfmt
+ clippy `-D warnings`, all crates) at the end of every milestone.

### 0.1 Branch
Continue on the **`animation`** branch (already checked out). Do not branch off.

### 0.2 THE LOAD-BEARING RULE — *everything* goes through `EditorController`
This is the single most important architectural requirement of this plan, and it
is **non-negotiable**:

> **Every animation mutation — without exception — is a serializable
> `EditorCommand` dispatched through the one `EditorController` singleton. The UI
> (and the renderer bridge) never mutate animation state directly.**

That includes the things that *feel* like ephemeral view state: the **playhead
time**, **play/pause**, **active clip**, **solo/mute**, **selection**, **mixer
layer weights/masks**, and obviously every clip/track/channel/keyframe edit. They
are all controller state, set via commands (transient ones for playhead/transport
so they don't pollute undo, invertible ones for data edits).

Why this rule is load-bearing — it is what makes **all** of the following fall
out *for free*, with no extra feature work:

- **Cross-tab sync.** The browser lets you open the same project in several tabs,
  each framing a *different camera* on the *same* synced animation (the user's
  explicit goal — this replaces any in-app "split view"). Because every mutation
  is a `dispatch(EditorCommand)` and the controller already exposes
  `snapshot()`/`dispatch_json` seams (see `main.rs`), a thin **`BroadcastChannel`
  relay** (§9) re-dispatches commands across tabs and they scrub/edit in
  lock-step. If any panel mutates state directly, that tab silently desyncs — so
  direct mutation is a *correctness* bug, not a style nit.
- **Undo/redo** comes from the command log (invertible commands).
- **Headless tests + the future MCP/websocket transport** drive the exact same
  `dispatch`/`snapshot` surface.
- **Gesture-free URL load** (`LoadProjectFromUrl`) for automated verification.

Only **pure view chrome** may stay local to a component: timeline zoom
(`pxPerSec`), which dock tab is open (Dope/Curves/Mixer), dock split fraction,
frames-vs-seconds unit, hover. Anything that another tab would need to agree on
is controller state.

**Restate this rule at the top of every animation milestone you start.**

### 0.3 Run + debug + verify ONLY in real Chrome, via the Claude-in-Chrome MCP
Follow [docs/DEBUGGING-PREVIEW.md](../DEBUGGING-PREVIEW.md) **completely** — read
it before taking a single screenshot. The non-negotiables from it:

- **Use real Chrome through `mcp__Claude_in_Chrome__*`**, with the dev server run
  as a plain background shell process (`trunk serve …`). **Do NOT use
  `mcp__Claude_Preview__*`** — the in-app webview crashes on heavy WebGPU scenes
  (documented root-cause in that doc).
- **The tab must be the foreground, active tab of a non-minimized window**, or
  `requestAnimationFrame` pauses and the canvas freezes at 300×150 — *this will
  bite hard for an animation feature whose whole point is a running rAF clock.*
  If a capture wrapped in rAF times out (~45 s) but a trivial `Date.now()` eval
  returns instantly, the tab is hidden — **ask the user to foreground it**, don't
  chase a phantom GPU hang.
- **Don't trust the screenshot for fine detail.** It's JPEG-compressed/resampled.
  For "did the mesh actually move / did the uniform actually drive the GPU",
  use **`getImageData` pixel reads** or **transform/value readback**, or ask the
  user a **specific yes/no** question. Subtle motion will look "fine" in a
  screenshot even when it's wrong.
- **Animation pin for deterministic captures.** When diffing frames, freeze the
  clock (drive `update_animations(0.0)` / hold the playhead) on both captures —
  morph/transform drift otherwise swamps the real signal (see the doc's
  "Animation pin").
- **Trunk rebuild signal:** wait for `applying new distribution` in the dev log,
  then **hard reload** (`window.location.reload(true)`). Per-shader pipeline
  compiles can take ~60 s on the test machine — wait for the compile log lines
  before sampling pixels.
- **Keep tool outputs small** (filter `read_console_messages` with a `pattern`;
  compute pixel stats in-page).
- **Scratch hygiene:** confine scratch artifacts to `/tmp/scratch/`; never commit
  diagnostics.

### 0.4 Dev servers + ports (start these at M-A0)
| What | Command | Port |
|---|---|---|
| **Editor (build under test)** | `task editor-dev` → `trunk serve --port 9085 …` | **9085** |
| **Animation React reference** | `cd /Users/dakom/Downloads/animation-reference && http-server --index --cors --port 9091` | **9091** |
| media-local (glTF samples) | `cd <media-local> && http-server --cors -p 9082` | 9082 |
| media-additional-assets | `http-server --cors -p 9083` | 9083 |

- **The HANDOFF never says how to run the reference — this is that step.** The
  reference is a React + inline-Babel app that pulls React/ReactDOM/Babel from the
  **unpkg CDN** and loads `anim-*.jsx` + `assets/*` over XHR, so it **must** be
  served over HTTP with CORS (not `file://`). `http-server` is already the
  repo-standard dev server (see [docs/SERVER_GUIDELINES.md](../SERVER_GUIDELINES.md)
  and the media tasks). Port **9091** is free (9090 is the *editor*-reference from
  the editor-rewrite plan). Serving it needs network access for the CDN; if unpkg
  is unreachable, surface that to the user rather than building blind.
- **Keep two real-Chrome MCP tabs open** — reference `:9091` and build `:9085`.
  For every panel: screenshot both, diff layout/spacing/color/type/per-state +
  interactions, iterate until they match. `read_console_messages` (errors only,
  with a `pattern`) after each load — zero panics / GPU validation errors.

### 0.5 MCP sanity-check before building (hard gate, part of M-A0)
Before writing UI: serve the reference, `navigate` a real Chrome MCP tab to
`:9091`, confirm you can **screenshot it** and **read its console**. Also confirm
`:9085` (the editor) drives cleanly. If the Claude-in-Chrome MCP can't drive a
real tab, **STOP and surface that** — do not fall back to the internal preview.

### 0.6 Fidelity bar
The reference is the source of truth for **UX, UI, layout, icons, interactions**
— match it. Where the real engine and the mock data model diverge, prefer the
prototype's UX and adapt the real model. The prototype's viewport is a **CSS mock
robot labeled "mock playback · not GPU"** — **we replace it with the real WebGPU
scene** (§7.3); that's the one place we deliberately exceed the prototype.

### 0.7 Don't stop to ask
Defaults are decided here (§2, §11). If a genuinely new ambiguity appears, pick
the most reversible option, note it inline, continue. The user already made the
big calls (§2).

### 0.8 Completion signal
When the entire plan is done and the §13 Definition of Done holds (all
milestones, `task lint` green, verified in real Chrome, GPU playback + blending
reconciled), end with this literal line on its own and **only** then:

**`ANIMATION SHIPPED — ROLL THE CREDITS!!!`**

Do not write it early.

### 0.9 Autonomy protocol (this is a big unsupervised run — self-regulate)
The user is **not watching**. Optimize for *not getting stuck* and *not silently
shipping something broken*.

- **Never block on the absent user.** The verification gate is the **§6.8 query
  surface** (numeric, deterministic), not human eyes. The only time you `STOP`
  is a hard infra/decision wall (below) — and then you stop *clearly*, not wait.
  Visual-subtle items the queries can't decide go on a **`VERIFY-LATER` list** in
  your final report; keep going, don't stall.
- **STOP-and-surface (do not guess past these):**
  1. The Claude-in-Chrome MCP can't drive a real tab (§0.5).
  2. unpkg/CDN unreachable so the reference won't load (§0.4).
  3. **M-R2 blending gate:** if accumulate-then-write **cannot** be made
     bit-identical for the single-clip path (invariant **I4** fails — the
     model-tests Fox regresses), STOP. A broken core playback path must not be
     built on. Report the finding; do not ship the redesign over a red I4.
  4. A renderer-core change forces a wide breaking API churn you can't keep
     `task lint`-green within the milestone.
- **Run straight through.** Execute **M-A0 → M-A9 in one pass** — no intermediate
  "stop for review" checkpoint. The per-milestone commits (below) are the review
  trail; the user reviews from those, not from a paused run.
- **Continue-with-default (pick the reversible option, note inline, move on):**
  every other ambiguity.
- **Checkpoint protocol.** At the **end of every green milestone**: `task lint`
  clean, then **`git commit`** on `animation` with a clear message (e.g.
  `feat(anim): M-R2 weighted/additive blending engine`). Do **not** push (unless
  asked). Commits are the resume points + the review trail.
- **Resumability.** On (re)start, infer the current milestone from `git log` +
  which modules already exist; re-run that milestone's verification before
  proceeding. The plan is ordered so each milestone is independently green.
- **Order discipline.** Renderer first (M-R0→M-R3), then editor (M-A1→M-A9). Don't
  start a UI panel whose backing command/query/lowering isn't landed — the panel
  has nothing correct to drive.
- **Restate §0.2 (everything via EditorController) at the top of each milestone.**

---

## 1. What we're building

A professional **clip-authoring workspace** as a third editor mode, faithful to
the prototype:

- A **Clip Library** (left rail) — named clips, each with duration / loop / speed
  / direction, plus a per-clip color. **+ New clip** creates an empty clip;
  importing a `.glb` **extracts its animation clips** into the library (exactly
  like importing a `.glb` extracts materials).
- A **viewport** that is the **real WebGPU scene**, posed by the sampled active
  clip at the playhead — play/scrub drives the actual renderer (no mock rig),
  with a **Solo-subtree** isolation control (§7.3).
- A **resizable timeline dock** with three switchable editors:
  - **Dope Sheet** — tracks → channels, draggable keyframe diamonds, transport.
  - **Curve/Graph editor** — real sampled curves with editable cubic tangents.
  - **Mixer (NLA)** — clip strips on weighted layers (Replace / Additive), trim
    handles, repeat fill, per-layer weight + **bone mask**.
- A **Key/Track inspector** (left, under the library) — edit the selected
  keyframe (time/value/interp/tangents) or track (sampler / target / lowering).
- An **Add-Track picker** surfacing every animatable target: node transforms,
  morph weights, **material uniforms**, **built-in material params**, **light
  params**, and **camera params**.

### 1.1 The authoring model (above the renderer)
The editor introduces an authoring layer the renderer runtime does not have:

```
Clip  ─┬─ Track (one object × one property; ONE shared times[]; e.g. Bone.005 · Rotation)
       │     ├─ times[]  (the keyframe times — shared by the whole track, glTF-style)
       │     └─ Keyframe { value, interp(step|linear|cubic), in/out tangents }
       │            value = vec3 (T/S) | quat (R) | scalar (uniform/light/camera/morph)
       └─ duration · loop(loop|pingpong|once) · speed · direction · color
       (X/Y/Z "channels" are an edit/display decomposition of the keyframe value,
        NOT independent per-axis time streams — see §10)
```

On every edit (and on load) the clip **auto-compiles ("lowers")** — WYSIWYG, like
material hot-reload, **no manual bake button** — into the renderer's runtime
animation system (§4). Lowering is now near-**identity**: a track's shared
`times[]` + typed keyframe values map straight onto the matching renderer
`AnimationSampler` (vec3 for T/S, **`Quat` for Rotation** — quaternion-native, the
single source of truth, no Euler→Quat merge step), so the authored curve and the
GPU result are the same sampler (decision in §10; invariant §4.7-I5). The ribbon's
**"Live · N players"** chip reflects this auto-compile.

### 1.2 Why this is *easier* than Material mode in some ways
No built-in/uber split, no WGSL, no second renderer instance, no shader
compilation. The hard part is concentrated in **one place**: the renderer-core
animation redesign (§4) — a real Clip/clock abstraction, four new target kinds,
and weighted/additive blending. Get §4 right and the UI is a faithful port.

---

## 2. Locked decisions (the user's calls — implement, don't re-litigate)

| # | Decision | Choice |
|---|---|---|
| 1 | **Blending depth** | **Full blending now.** Weighted **Replace** + crossfade, **Additive** (with reference/rest pose), and **per-layer bone masks** — all GPU-driven in renderer-core. The Mixer drives the real scene. |
| 2 | **Clip runtime** | **A first-class named Clip/Group type in renderer-core** — a group of channel-samplers sharing one clock (duration/loop/speed/direction), advancing & wrapping in sync. The editor's clip maps 1:1. Formalizes the shared-clock semantics the HANDOFF flagged as missing. |
| 3 | **Animatable targets** | **All of:** node **Transforms** (T/R/S), **Morph weights** (geometry + material), **Custom-material uniforms** (named slots), **Built-in material params** (PBR base-color/metallic/roughness, Unlit color, Toon knobs), **Light params** (intensity/color, +range/cone for point/spot), and **Camera params** (FOV/focal/near-far). |
| 4 | **Persistence** | **Editor TOML side-files.** Clips persist as `animation-<id>.toml` in the editor's TOML project directory, mirroring `material-<id>.toml/.wgsl`. `scene-schema`'s `project.json` path (model-tests / tuning scenes) is **untouched**. |
| 5 | **Playback model** | **Drive the real scene.** Animation mode reuses the real WebGPU scene canvas (reparented, like Scene mode); the transport/scrub drives `update_animations` on the actual scene; edits auto-compile live; no mock rig, no 2nd renderer instance. |
| 6 | **Preview isolation** | **No split view.** Multi-camera viewing = **multiple browser tabs** synced via EditorController (§0.2, §9). In-page isolation is a **Solo-subtree** control: pick a node → only tracks under that subtree advance (others hold at rest), built on the prototype's per-track solo/mute. Camera stays the normal orbit + a "frame selection" button, independent per tab. |
| 7 | **Bone masks** | **Per-layer node set** — each Mixer layer carries a mask = a multi-selected set of scene nodes (with an "include descendants" toggle). No named-preset assets (yet). |
| 8 | **Additive rest pose** | **Optional base clip, default to rest.** Each additive layer may name a **base clip** as its reference pose; if unset, the reference is the target's **scene default / bind pose** (its authored local value when no clip drives it). |
| 9 | **Everything via EditorController** | §0.2 — load-bearing. Cross-tab sync is the payoff; build the `BroadcastChannel` relay seam (§9). |

---

## 3. Reference map (`~/Downloads/animation-reference/`, served at `:9091`)

**Do not port JSX — rebuild in Rust/dominator.** The mock data model
(`anim-data.jsx`) is grounded in the *real* renderer types (its header comments
cite `crates/renderer/src/animation`); mirror its **shape** onto the real model,
not 1:1. Each file → what it drives:

| File | Drives | Real-code analog |
|---|---|---|
| `index.html` | Loads React/Babel + the modules. **Serve at `:9091` (§0.4).** | — |
| `anim-app.jsx` | Editor shell: top bar (Scene/Material/**Animation** segmented), **animation ribbon** (clip color · name · Duration · FPS · `N tracks → N players` · Add Track · **Live · N players** chip), workspace = rail + viewport over a **resizable timeline dock**, toasts. The clearest behavior map. | `app.rs` mode router + a new `animation_mode/` |
| `anim-data.jsx` | The authoring model + a **JS port of the renderer's sampler** (Step/Linear/CubicSpline hermite). Targets tree (armature/bone→Transform; mesh→Transform+morph; material→uniforms), `PROP_META` (Translation/Rotation/Scale → dtype/axes/unit/euler), clip/track/channel/keyframe factories, `sampleChannel`/`sampleClip`/`trackKeyTimes`. | `controller/animation.rs` model + lowering; renderer `AnimationSampler` |
| `anim-rail.jsx` | **ClipLibrary** (rows: color dot · name · duration · LOOP/PP/ONCE badge · track count · ×speed), **KeyInspector** (keyframe: Time/Value/Interp/in-out tangents · track: Target/Property/**Lowers to**/Sampler/Channels), **AddTrackMenu** (search · group-by-target · UNIFORM/MORPH badges · "lowers to" hint · dims already-added). | `animation_mode/{library,inspector,add_track}.rs` |
| `anim-timeline.jsx` | **TimelineDock** shell: **Transport** (to-start/prev-key/play-pause/next-key/to-end · time readout w/ frames⇄seconds · direction · loop · speed slider), **Ruler** (nice-step ticks · scrub · playhead), **Dope Sheet** (freeze-pane: sticky names column + lanes · track/channel rows · expand chevron · mute eye · draggable `Diamond` glyphs · collapsed-track key union), zoom controls, empty state. | `animation_mode/timeline/` (dock, ruler, dope) |
| `anim-curves.jsx` | **CurvesView** — real curves sampled via the shared sampler, pinned value axis + gridlines, draggable keyframe dots, editable **cubic tangent handles**, per-channel show/hide, curve fill. | `animation_mode/timeline/curves.rs` |
| `anim-mixer.jsx` | **MixerView** — layers (Replace/Additive badge · weight slider), clip **strips** (drag-move · trim handles `l`/`r` · repeat hatch), `mixerDuration`. **Extend with the bone-mask editor (§2/#7).** | `animation_mode/timeline/mixer.rs` + renderer Mixer |
| `anim-rig.jsx` | **AnimViewport + Robot** — CSS mock posed from the sampled clip (`bobY`, neck/arm/leg rotations, eye emissive, smile morph), overlay chrome (ViewAxis, "mock playback · not GPU" chip). **We REPLACE the robot with the real WebGPU canvas** (§7.3); keep the overlay-chrome layout. | `animation_mode/viewport.rs` over the real canvas |
| `assets/colors_and_type.css`, `assets/ui-primitives.jsx`, `assets/ui-overlays.jsx` | Design tokens + atoms (`Icon`/`IconBtn`/`Btn`/`Section`/`Row`/`NumField`/`Vec3`/`TextInput`/`Select`/`Toggle`/`Segmented`/`Badge`/`Popup`/`Slider`/…). | **Already ported** to `web-shared` by the editor-rewrite (M2). Reuse; add any missing atom (e.g. timeline ruler needs none new — built from primitives). |
| `HANDOFF.md` | The renderer-fidelity matrix + intent. This plan supersedes it (more thorough, real-code-mapped). | — |
| `uploads/` | Screenshots. Use as the pixel-match target alongside the live `:9091`. | — |

---

## 4. Renderer-core changes (`packages/crates/renderer`, `renderer-gltf`, `materials`)

This is the heart of the work. Today's runtime (verified):

- `animation/player.rs` — `AnimationPlayer { speed, loop_style, play_direction,
  clip(priv), state(priv), local_time(priv) }`; `update(global_time_delta)`
  advances `local_time += dt*speed` with Loop/PingPong/None; `sample()` →
  `AnimationData`. **`clip`/`state`/`local_time` are private — no scrub/seek API.**
  Default `speed = 1.0/1000.0` (so `global_time_delta` is in **milliseconds**).
- `animation/data.rs` — `AnimationData = Transform(TransformAnimation{T?,R?,S?}) |
  Vertex(VertexAnimation{weights}) | Vec3 | Quat | F32 | F64`. `Vec3/Quat/F32/F64`
  variants exist but are **never produced or consumed** today. `TransformAnimation::apply`
  is per-field **assignment** (no weight).
- `animation/sampler.rs` — `AnimationSampler = Linear|Step|CubicSpline{in/out
  tangents}` over `times[]`/`values[]`; `sample(time)` binary-search + interp.
- `animation/clip.rs` — `AnimationClip { name, duration, sampler }` — **one
  channel per clip**, no grouping.
- `animation/animations.rs` — `Animations { players: DenseSlotMap<AnimationKey,_>,
  transforms: SecondaryMap<AnimationKey,TransformKey>, morphs:
  SecondaryMap<AnimationKey,AnimationMorphKey> }`; `insert_transform` /
  `insert_morph`; `AwsmRenderer::update_animations(dt)` is **last-write-wins**,
  handles only transforms + morphs.
- `renderer-gltf/src/populate/animation.rs` + `populate.rs` — glTF animation
  import **already works**: builds per-node samplers and `insert_transform` /
  `insert_morph` players (auto-plays on load). No named-clip grouping survives —
  the glTF animation `name` lands on `AnimationClip.name` but isn't indexed.

### 4.0 Strategy
Keep the existing loose-player path *as a primitive* but build the new
clip/mixer system **on top of `Animations`** so glTF auto-import and the editor's
authored clips share one runtime. Stage the renderer work so each sub-step is
independently lint-green and verifiable (§12 M-R0…M-R3).

### 4.1 Player scrub/seek API (small, do first)
Add to `AnimationPlayer` (player.rs): `local_time()`, `set_local_time(f64)`
(clamped to `[0,duration]`), `duration()`, `state()`, `set_state(AnimationState)`,
`reset()`, and `sample_at(f64)` (sample without mutating `local_time`, for the
editor scrubbing a paused clip). The editor's transport sets `state` to
`Paused` and drives `set_local_time` while scrubbing; `Playing` while playing.

### 4.2 Named Clip / Group + shared clock (decision #2)
Introduce a runtime grouping so a clip's channels advance & wrap **together**:

```rust
// animation/clip_group.rs (new)
new_key_type! { pub struct AnimationClipKey; }

pub struct AnimationChannel {
    pub target: AnimationTarget,         // §4.3
    pub sampler: AnimationSampler,       // already exists
}

pub struct AnimationClipGroup {
    pub name: String,
    pub duration: f64,
    pub loop_style: Option<AnimationLoopStyle>,
    pub speed: f64,
    pub play_direction: AnimationPlayDirection,
    pub channels: Vec<AnimationChannel>,
    local_time: f64,                     // ONE shared clock for the whole group
    state: AnimationState,
}
```

`AnimationClipGroup` gets the same advance/seek API as the player (shared
`local_time`), plus `sample_all(&self) -> Vec<(AnimationTarget, AnimationData)>`
(samples every channel at the shared `local_time`). Store groups in
`Animations` as `clips: SlotMap<AnimationClipKey, AnimationClipGroup>`. The
editor lowers one authored Clip → one `AnimationClipGroup`.

### 4.3 `AnimationTarget` — the four new target kinds (decision #3)
The current container hard-codes transform + morph maps. Replace target dispatch
with one enum so channels can address anything:

```rust
pub enum AnimationTarget {
    Transform(TransformKey),                                   // exists
    Morph(AnimationMorphKey),                                  // exists (geo|material)
    Uniform { material: MaterialKey, slot: usize },            // NEW — custom-material named slot
    BuiltinParam { material: MaterialKey, param: BuiltinMaterialParam }, // NEW
    Light { light: LightKey, param: LightParam },              // NEW
    Camera { camera: CameraKey, param: CameraParam },          // NEW
}
```

Each kind needs (a) a way to **write** a sampled `AnimationData` value into the
GPU-bound state, using existing update paths, and (b) a way to **read** its
current/rest value (for blending + additive reference):

- **Uniform** — `material: MaterialKey`, `slot: usize` (index into
  `MaterialLayout.uniforms`, declaration order). Write via
  `AwsmRenderer::update_material(key, |m| m.<Custom>.values[slot] = UniformValue::from(sample))`
  (`materials.rs:66`, `materials/src/dynamic.rs` `values: Vec<UniformValue>`,
  `dynamic_layout.rs` `UniformValue`/`UniformFieldRuntime{name,ty}`). `AnimationData`
  → `UniformValue`: `F32→F32`, `Vec3→Vec3/Color3`, `Quat→Vec4`, etc. — match the
  slot's `FieldType`. The materials crate **already exposes named animatable
  uniform slots** (the HANDOFF's prerequisite item #1 is *partly already done* —
  the slot model exists; we add the animation→buffer write branch + the
  (material,name)→slot resolution).
- **BuiltinParam** — `param: BuiltinMaterialParam` enum (`BaseColor`, `Metallic`,
  `Roughness`, `Emissive`, `UnlitColor`, `ToonSteps`, …). Write via
  `update_material(key, |m| /* set the matching field */)`. Read the field for
  rest/blend.
- **Light** — `light: LightKey`, `param: LightParam` (`Intensity`, `Color`,
  `Range`, `InnerAngle`, `OuterAngle`). Write via `lights.update(key, |l| …)`
  (`lights.rs:326`). **Never flip the `Light` variant** (Dir↔Point↔Spot) — those
  params are inapplicable to the wrong variant; the editor's Add-Track picker
  only offers params valid for the light's kind.
- **Camera** — `camera: CameraKey`, `param: CameraParam` (`FovY` /
  `OrthoHalfHeight`, `Near`, `Far`, and the DoF pair `Aperture` / `FocusDistance`
  already on `CameraMatrices`). **Mirror the lights pattern exactly** (this was the
  "unknown" — now resolved):
  - Today there is **no per-camera GPU key**: camera params live on the scene node
    as `NodeKind::Camera(CameraConfig)` (`scene-schema/src/camera.rs`:
    `CameraProjection::Perspective { fov_y_rad }` / `Orthographic { half_height }`,
    `near`, `far`), and the **editor render loop rebuilds the projection each
    frame** from the active camera node and calls `renderer.update_camera(matrices)`
    (`editor/src/engine/render_loop.rs:88-139` `scene_camera_matrices`;
    `renderer/src/camera.rs:15-28` `update_camera`). Camera nodes are passive in
    `node_sync` (no `camera_key`).
  - **Add the minimal lights-shaped machinery:** a `CameraKey` + a `Cameras` store
    in renderer holding the *animatable source params* (`fov_y`/`half_height`,
    `near`, `far`, `aperture`, `focus_distance`) with `insert` / `remove` /
    `update(key, |p| …)` + a dirty flag — mirror `lights.rs:265-336`. `node_sync`
    materializes a `NodeKind::Camera` → a `CameraKey` stored as `camera_key` on
    `RendererNode` (like `light_key`), seeded from the node's `CameraConfig` and
    kept in sync on `SetKind`. `update_animations` writes the sampled value via
    `cameras.update(key, |p| p.fov_y = …)`. The render loop's
    `scene_camera_matrices` reads the active camera's params **from the `Cameras`
    store** (which animation may have just mutated) instead of straight off the
    node, so an animated FOV flows into the per-frame projection rebuild. **Read**
    for blend/rest = the store's current param. Now camera is a first-class target,
    fully inside the blending engine + the §6.8 query, no special-casing.

### 4.4 Weighted / additive blending engine (decision #1) — the big one
Today `update_animations` writes each target directly in iteration order
(last-write-wins). Replace it with **accumulate-then-write**:

```rust
// animation/mixer.rs (new)
pub enum LayerMode { Replace, Additive { base_clip: Option<AnimationClipKey> } }

pub struct AnimationStrip {        // one clip placed on a layer's timeline
    pub clip: AnimationClipKey,
    pub start: f64, pub len: f64, pub scale: f64, pub repeat: bool,
}
pub struct AnimationLayer {
    pub mode: LayerMode,
    pub weight: f64,
    pub mask: Option<TargetMask>,  // None = whole rig
    pub strips: Vec<AnimationStrip>,
}
pub struct TargetMask { /* set of TransformKeys (+ optionally other targets) */ }

pub struct AnimationMixer { pub layers: Vec<AnimationLayer>, time: f64 /* shared NLA clock */ }
```

New `update_animations(dt)`:
1. **Advance** the mixer clock (and/or each standalone clip group's clock — see
   §4.5) by `dt`.
2. **Resolve active contributions per target.** For each layer, for each strip
   active at the current time, find the underlying `AnimationClipGroup`, compute
   its local time (`(t - start)/scale`, wrapped if `repeat`), `sample_all`, and
   bucket samples by `AnimationTarget`.
3. **Composite per target** into a single value, in layer order:
   - Seed `acc` = the target's **rest value**. **⚠ rest = the stored
     bind/default value, NOT `transforms.get_local` (which holds *last frame's
     already-animated* value — reading that drifts/feeds-back and is the classic
     blend bug).** Maintain a **rest cache** (`SecondaryMap<…, RestValue>`)
     captured when a target is first bound to any clip (and refreshed when the
     authored default changes, e.g. the user edits the node's default transform in
     Scene mode). See §4.7-I1.
   - **Replace** layer (weight `w`, respecting `mask`): `acc = blend(acc,
     layerPose, w)` — `lerp` for Vec3/F32/weights, **`slerp` for Quat**.
     **Preserve per-field optionality:** a `TransformAnimation` field that is
     `None` (e.g. a T-only track) must leave `acc`'s R/S untouched — blend only the
     present fields (§4.7-I3).
   - **Additive** layer (weight `w`, respecting `mask`): `delta = layerPose −
     reference` where `reference` = the layer's `base_clip` pose if set, else the
     target's **rest** value (decision #8). Apply scaled: T/S/weights/scalars
     `acc += w·delta`; **rotation** `acc = slerp(IDENTITY, deltaQuat, w) · acc`
     (quaternion multiply).
   - A target **not** in a layer's `mask` is untouched by that layer.
4. **Write once** per target via §4.3's write path (`set_local` /
   `update_material` / `lights.update` / camera update). One write per target per
   frame → no intermediate GPU churn. A target with **no** active contribution this
   frame is written back to its **rest** value (so disabling a track restores the
   default, rather than freezing the last animated frame).

Provide a **simple path** too: a clip group with no mixer (single clip, weight 1,
Replace, whole rig) must reduce to today's behavior bit-for-bit — glTF
auto-import and "just play this clip" use it.

### 4.5 Wiring + back-compat
- glTF import (`renderer-gltf`) currently inserts loose players. Keep that for the
  **non-editor** consumers (model-tests), but add an extraction API the **editor**
  uses (§6.7) that returns parsed clips as *data* (named groups of
  channel-samplers keyed by glTF node index) **without** auto-inserting, so the
  editor owns playback through its clip model. Gate the existing auto-insert
  behind the populate path the editor doesn't use, or have the editor remove the
  auto-inserted players after capturing them. Prefer the clean extraction API.
- `update_animations` is called from the renderer's frame update
  (`transforms.rs` `update_transforms` runs right after; lights re-derive from
  animated transforms via `lights.update_from_transforms`). Confirm ordering:
  animation writes locals → `update_world` → lights follow.

### 4.6 Renderer verification (per §0.3)
- **Unit-test the sampler + blend math** in Rust where possible (pure functions:
  slerp/lerp/additive delta, shared-clock wrap, strip-local-time) — these need no
  browser and are the fastest, most reliable signal; lean on them heavily.
- **Prefer GPU-independent value readback.** The §6.8 `SampleClipTimeseries` query
  reads the **CPU-side renderer state** after `update_animations(0.0)` (the local
  transform, the `DynamicMaterial.values[slot]`, the `Light` field) — it does
  **not** require a rendered frame, so it works **even when the tab is
  backgrounded** (no rAF dependency). For an unsupervised run this is the primary
  gate; `getImageData`/`CanvasStats` (which DO need a visible, rendered canvas) are
  the secondary check, used only where the truth is a rendered pixel.
- **GPU truth** via real Chrome where needed: load a known glTF (Fox/CesiumMan);
  pin the playhead; assert the posed value equals the expected sample; for a
  2-layer Replace crossfade assert the midpoint is the slerp midpoint. **Never
  declare blending correct from a screenshot alone.**

### 4.7 Correctness invariants (the classic bugs — get these right)
These are the failure modes an unsupervised implementer is most likely to ship.
Each is also a unit/query assertion.

- **I1 — Rest pose is a stored snapshot, never the live local.** Blend/additive
  reference reads the **bind/default** value from the rest cache (§4.4 step 3), not
  `get_local`. Test: play an additive layer for 100 frames at a fixed playhead →
  the pose must be **constant** (no per-frame drift).
- **I2 — Fail hard on misconfiguration; defer only known-pending bindings.** A
  misconfigured animation is a **bug** — surface it loudly, do not mask it with
  silent per-frame skips.
  - **Renderer core stays strict.** `update_animations` keeps returning `Err`
    (`MissingKey`, `WrongKind`, dtype mismatch) and callers **propagate** it. After
    bind-time validation, a missing key in the hot loop is an invariant violation —
    surface it (loud `tracing::error!` / debug-panic), never silently continue.
  - **Bind/lower time validates hard.** When the editor lowers a clip → renderer
    clip group, a channel whose target is **genuinely invalid** — references a
    deleted/nonexistent node or material, a value dtype that doesn't match the
    slot's `FieldType`, a light param invalid for the light's kind — is a **hard
    error**: toast + `tracing::error!` + mark the clip/project invalid. In
    headless/unsupervised this **halts the run** (§0.9 STOP) so the bug gets fixed.
  - **The one carve-out is NOT tolerance — it is not *creating* an invalid
    binding.** A target that is *legitimately not-yet-resolvable* within the known
    async materialization window (a custom material awaiting its first `register`,
    a node mid-`node_sync` insert) is **deferred**: the channel isn't bound yet and
    re-lowers when the dependency materializes (the same observer that re-resolves
    on re-materialization). Distinguish **pending** (a known in-flight dependency)
    from **invalid** (a reference that can never resolve) and fail hard on the
    latter. Test: a clip referencing a nonexistent node id → **loud error at load**,
    not a silent no-op.
- **I3 — Per-field transform optionality preserved.** A T-only (or R-only) track
  blends only its present `Option` fields; absent fields fall through to rest
  unchanged. Test: a track animating only translation must leave rotation/scale at
  their authored defaults.
- **I4 — Single-clip path is bit-identical.** One clip, no mixer, weight 1,
  Replace, whole rig must equal today's last-write-wins output. Test: the
  model-tests **Fox** animates identically before/after (pinned-frame joint
  readback matches; clean console).
- **I5 — Rotation is quaternion-native and the curve is bit-WYSIWYG (§10).** A
  rotation track *is* a `Quat` sampler (single source of truth); there is **no**
  separate Euler representation to diverge from. The Curve editor draws rotation by
  sampling that quaternion and projecting to Euler with continuity unwrapping, so
  the **displayed curve equals the GPU path between keys, not just at keys**. Test:
  sample the rotation curve and the timeseries query at the same off-key times →
  they match (quaternion); the Euler projection is continuous (no ±360 jumps).
  glTF rotation imports 1:1 with no reinterpretation. **Tracks carry one shared
  `times[]`; no channel-time merging exists** (§10) — eliminating the old
  merge-then-resample step and its rounding.
- **I6 — `update_animations` callers stay green.** Enumerate every caller (the
  renderer frame loop; `model-tests`; any tuning harness) and keep the public
  signature/behavior working — or update each callsite in the same change. Don't
  leave a caller compiling against the old API.

---

## 5. `scene-schema` / `awsm-materials` reuse (no `project.json` change)

- **Do NOT change `scene-schema`'s `project.json`** (decision #4). Animations live
  in the editor's own TOML (§6.6).
- **Reuse types where they already model the contract**, serialized to the editor
  TOML: `awsm_materials::dynamic_layout::{UniformValue, FieldType}` for uniform
  channel values; the renderer's `AnimationSampler`/interp kinds as the lowering
  target. The editor's authored keyframe model (§6.4) is its own serde types
  (richer than the renderer's — it keeps per-keyframe interp + tangents + Euler).
- **Named animatable uniform slots** already exist (`UniformFieldRuntime.name`).
  The only addition is resolving `(custom-material asset, uniform name)` → the
  live `MaterialKey` + `slot` index at lowering time (the bridge knows the
  `MaterialKey` from materialization; the slot index is the layout's declaration
  order). No materials-crate schema change required for uniforms.

---

## 6. Editor changes (`packages/frontend/editor`)

Mirror **Material mode** — it is the exact template (library + per-item editor +
live auto-compile + persistence + content-browser surfacing). Concrete insertion
points (verified):

### 6.1 Mode plumbing
- `controller/command.rs:45` — add `Animation` to `EditorMode`.
- `app.rs` `top_bar` segmented — add `SegOption::new("animation","Animation").icon("curve")`
  (the prototype uses the `curve` icon); update `mode_to_str`/`str_to_mode`.
- `app.rs` `workspace` router — add a third display-toggled `<div>` rendering
  `crate::animation_mode::render()` (both other workspaces stay mounted; the
  WebGPU canvas is reparented into whichever viewport is visible — Scene's or
  Animation's — never torn out, so the render loop keeps ticking).
- `main.rs` — `mod animation_mode;`.

### 6.2 `EditorController` state additions (`controller/mod.rs`)
All **controller-owned** (§0.2), so they broadcast + snapshot:
```rust
pub custom_animations: MutableVec<Arc<CustomAnimation>>, // the clip library (mirrors custom_materials)
pub current_clip:      Mutable<Option<AssetId>>,         // active clip (mirrors current_material)
pub playhead:          Mutable<f64>,                      // seconds; shared across tabs
pub playing:           Mutable<bool>,
pub anim_fps:          Mutable<u32>,                      // display only (frames⇄seconds), but synced
pub anim_solo_root:    Mutable<Option<NodeId>>,          // Solo-subtree focus (decision #6)
pub anim_selection:    Mutable<Option<AnimSel>>,         // selected track/channel/keyframe in timeline
pub mixer:             Mutable<MixerDoc>,                 // NLA layers/strips/masks/weights (decision #1/#7)
pub anim_view:         Mutable<AnimView>,                 // Dope|Curves|Mixer — view chrome, MAY stay local? NO: keep in controller so tabs agree
```
Pure view chrome that may stay component-local: timeline `pxPerSec` zoom, dock
split fraction, frames-vs-seconds unit toggle, hover.

### 6.3 `EditorCommand` additions (`controller/command.rs`) — every mutation
Add (follow the existing doc-comment + invertibility conventions; transient ones
listed in `is_transient`). **Data only, no closures.**

- **Lifecycle:** `AddClip { }` (empty clip; selects it), `DeleteClip { id }`,
  `DuplicateClip { id }`, `SetCurrentClip { id: Option<AssetId> }` *(transient)*.
- **Clip props (invertible):** `RenameClip`, `SetClipDuration`, `SetClipLoop`,
  `SetClipSpeed`, `SetClipDirection`, `SetClipColor`.
- **Tracks (invertible):** `AddTrack { clip, target_spec }` (target_spec =
  serializable target descriptor: NodeId+property, or NodeId+morph-name, or
  material-asset+uniform-name, or NodeId(light/camera)+param), `DeleteTrack`,
  `SetTrackSampler`, `SetTrackMute`, `SetTrackSolo`, `ToggleTrackExpand`
  *(expand = transient view? keep invertible-light or transient — it's per-clip
  data, keep it on the clip but transient for undo)*.
- **Keyframes (invertible, coalescing like `SetTransform`):** `AddKeyframe {
  clip, track, channel, t, value }`, `DeleteKeyframe`, `SetKeyframe { …, t?, value?,
  interp?, in_tangent?, out_tangent? }` (one command, partial patch; consecutive
  edits on the same keyframe coalesce into one undo step — reuse the
  `coalesce_key` mechanism).
- **Transport (transient — broadcast but not undone):** `SetPlayhead { t }`,
  `SetPlaying { on }`, `StepPlayhead { kind: Home|Prev|Next|End }`,
  `SetAnimFps { fps }`, `SetSoloRoot { id: Option<NodeId> }`,
  `SetAnimSelection { sel }`, `SetAnimView { view }`.
- **Mixer (invertible):** `AddLayer`, `DeleteLayer`, `SetLayerMode {
  layer, mode, base_clip? }`, `SetLayerWeight` *(coalescing)*, `SetLayerMask {
  layer, nodes, include_descendants }`, `AddStrip { layer, clip, start, len }`,
  `DeleteStrip`, `MoveStrip`/`TrimStrip` *(coalescing)*, `SetStripRepeat`.

Implement each in `controller/mod.rs::apply` returning `Some(inverse)` (data
edits) or `Ok(None)` (lifecycle/transient), exactly like the material commands.
Add the transient ones to `is_transient()` and a `label()` arm.

### 6.4 `controller/animation.rs` (new) — the authored model
Mirror `controller/custom_material.rs` (`CustomMaterial`). `CustomAnimation` holds
the clip: `id`, `name: Mutable<String>`, `duration`, `loop`, `speed`, `direction`,
`color`, and `tracks: MutableVec<Arc<Track>>`. A `Track` has **one shared
`times: Vec<f64>`** (decision §10) + `keys: Vec<Keyframe>` aligned to it, where
`Keyframe { value: TrackValue, interp, in_tangent, out_tangent }` and
`TrackValue = Vec3 | Quat | Scalar` (the whole typed value per keyframe; vec3
tangents/quat tangents for cubic). The X/Y/Z "channels" the UI shows are an
edit/display **decomposition** of `value`, not separate key streams. `TrackTarget`
is the serializable descriptor binding a track to a real target (NodeId + property
| NodeId + morph index/name | material-asset-id + uniform name | NodeId(light) +
LightParam | NodeId(camera) + CameraParam). Include the **lowering** function here
— now near-**identity** (no channel-time merge): hand the track's `times[]` +
typed values straight to the matching renderer `AnimationSampler`
(vec3 / **`Quat`** / scalar), collapsing per-keyframe interp into the sampler kind.
**Rotation is quaternion-native**: keyframe `value` is a `Quat`; Euler is only the
edit projection (Euler°↔quat at the keyframe-edit boundary, §7.4 / §10), so no
Euler→Quat *interpolation* reinterpretation ever happens.

### 6.5 `engine/bridge/animation_sync.rs` (new) — lower + drive
Mirror `node_sync.rs` (observers materialize/teardown GPU state):
- Observe `custom_animations` + each clip's `tracks`/keys → **debounced
  auto-compile** (≈the material 400 ms debounce): lower the active clip (and the
  mixer doc) into renderer `AnimationClipGroup`s + an `AnimationMixer`, resolving
  each `TrackTarget` to its live renderer key via the bridge
  (`bridge().nodes[node_id].transform_key`, the node's `light_key`,
  `material_keys`, mesh morph keys, the camera key). Re-resolve on
  re-materialization (kind change / re-import). **Resolution policy (§4.7-I2):**
  distinguish **pending** from **invalid**. A target awaiting a known in-flight
  dependency (material not yet registered, node mid-materialization) is **deferred**
  — its channel isn't bound yet and re-lowers when the dependency appears (not a
  silent skip — there's simply nothing valid to bind yet). A target that can
  **never** resolve (deleted/nonexistent node or material, dtype mismatch, light
  param invalid for the kind) is a **hard error**: `tracing::error!` + a toast +
  mark the clip invalid (red "broken track"); in headless this halts the run so
  the bug is fixed (§0.9). Do not paper over an invalid reference.
- Observe `playing` + `playhead` → drive the renderer clock: when `playing`,
  the render loop calls `update_animations(dt_ms)` and pushes the resulting
  `playhead` back to the controller via `SetPlayhead` (so the ruler + other tabs
  follow); when paused/scrubbing, set each group's `local_time` to `playhead`
  and call `update_animations(0.0)` for a one-shot pose (the **Animation pin**
  pattern, now first-class).
- Observe `anim_solo_root` → mute (skip) tracks whose target node is **not**
  under the solo subtree (rest-hold), reusing the per-track mute path. Frame the
  camera on the subtree on solo-root change (reuse the Scene-mode "frame
  selection"/camera-fit used by `SnapCameraToAxis`/`ResetCamera`).

### 6.6 Persistence (`controller/persistence.rs`)
Mirror `material_files()` exactly. Add to `EditorProject`:
`custom_animations: Vec<CustomAnimationRef>` (name + `animation-<slug>.toml`
ref) and write one `animation-<id>.toml` side file per clip carrying the full
authored model (duration/loop/speed/direction/color + tracks → channels →
keyframes with interp/tangents + each track's `TrackTarget` descriptor). Persist
the **mixer doc** in `project.toml` (it references clips by id). Round-trip
(serialize → in-memory TOML tree → parse) is checkable without disk I/O, like the
material round-trip. `LoadProjectFromUrl` fetches the `animation-*.toml` files
alongside `material-*` (extend the URL loader's file list).

### 6.7 glTF import → clip extraction (`engine/bridge/gltf.rs`)
The import already deconstructs meshes/materials/skins/animations. **Extend it to
capture animations into the clip library** (the user's "import a glb → extract
animation clips into assets", symmetric with material extraction):
- Read the glTF doc's `animations()` (the data the existing
  `build_node_animation_sampler_lookup` already walks). Build one
  `CustomAnimation` per glTF animation, named from the glTF animation name
  (fallback `Animation N`). For each channel, create a Track whose `TrackTarget`
  binds to the **editor NodeId** the glTF node mapped to (use the
  node-index→NodeId map the `asset_template` import already builds) + the property;
  convert the glTF sampler keyframes (times/values/tangents, interp) into the
  authored Channel keyframes (Quat rotation kept as a single track that lowers
  back to a `Quat` sampler; morph weights → a morph track).
- Do **not** auto-instantiate the renderer loose players for editor imports
  (§4.5) — the editor's lowering owns playback. Dispatch `AddClip`-style
  population through the controller (an `ImportClips { … }` controller path) so it
  participates in the snapshot/undo/cross-tab model.
- Verify against a known animated glTF (e.g. `CesiumMan`, `Fox`, `BrainStem`,
  `RiggedFigure`) served from media-local `:9082`: import → the clip appears in
  the library, plays in the real viewport, matches the source motion.

### 6.8 Animation **query surface** (the read API — verification + MCP seam)

Commands are the *write* half of the controller; **queries are the read half**.
The editor-rewrite (§5.5) already established a serializable `EditorQuery` /
`snapshot()` read surface and a `query_json`/`snapshot_json` wasm export in
`main.rs`. **Extend that same query mechanism** with animation read/verification
queries — do **not** invent a separate channel. Queries are **read-only**: they
never mutate persisted state, never record undo, never broadcast (§9); any one
that has to pin the playhead **saves and restores** the transport state. The
future MCP/websocket transport `serde`-decodes a query → `query()` → encodes the
result, exactly like commands. This is also **how the §12.1 MVP tests run** — each
matrix cell becomes a numeric assertion over a query result, driven gesture-free
from the real-Chrome tab via `window.wasmBindings.query_json(...)`.

Two query families cover the "movie via playhead stepping" idea — *video as
numbers*, the format a debugging agent can actually reason about (raw video/MP4
can't be analyzed here; a numeric time-series can):

```rust
// controller/query.rs — extend the existing EditorQuery (serde, read-only)
pub enum EditorQuery {
    Snapshot,                                    // existing
    // ---- value readback (deterministic, GPU-independent) ----
    SampleClipTimeseries {
        clip: AssetId,
        times: Vec<f64>,                         // seconds; pins the playhead at each (Animation-pin)
        targets: Vec<ReadbackTarget>,            // what to read at every pinned time
    },
    // ---- pixel readback (the getImageData path, now a query) ----
    CanvasPixels { coords: Vec<(u32, u32)> },    // exact RGBA at points (drawImage→getImageData in-page)
    CanvasStats  { region: Option<Rect> },       // mean/min/max luma + per-channel over a region
    Filmstrip {                                  // OPTIONAL gross-glance montage (one PNG data-URL)
        clip: AssetId, times: Vec<f64>, cols: u32, scale: f32,
    },
}

pub enum ReadbackTarget {
    NodeWorldMatrix(NodeId),
    NodeLocalTrs(NodeId),
    MorphWeight { node: NodeId, name: String },
    Uniform { material: AssetId, name: String },
    BuiltinParam { node: NodeId, param: BuiltinMaterialParam },
    LightParam { node: NodeId, param: LightParam },
    CameraParam { node: NodeId, param: CameraParam },
}
```

- **`SampleClipTimeseries`** is the workhorse. Handler: snapshot the transport
  (`playing`/`playhead`), set `playing=false`; for each `t` set the clip group's
  `local_time=t` + `update_animations(0.0)` (the **Animation-pin**, now
  first-class), read every `ReadbackTarget` via the §4.3 *read* paths, collect;
  then restore the transport. Returns
  `{ targets:[…], frames:[ { t, values:{ "<target>": <number|array> } }, … ] }`.
  This is **GPU-independent and exact** — it reads the renderer's values, not
  pixels — so the time-series is the gold-standard assertion (detects wrong interp
  kind, dropped/duplicated frames, dead channels, a slerp midpoint that's off, a
  two-channel desync).
- **`CanvasPixels` / `CanvasStats`** are the **getImageData path the user asked to
  route through queries**: the handler reaches the WebGPU canvas via `web_sys`,
  `drawImage`→`getImageData` (respecting the `DEBUGGING-PREVIEW.md` caveats:
  `preserveDrawingBuffer` / same-frame rAF), and returns small JSON (exact RGBA or
  in-page-computed stats). Use these when the truth is a *rendered* result (light
  luma rise, material color blend) rather than a renderer value.
- **`Filmstrip`** (optional) returns a single montage PNG data-URL of the pinned
  frames for a quick gross-motion glance — useful but **subject to compression**;
  never the sole evidence for subtle correctness.

The implementation builds these as part of M-A1 (alongside the controller +
snapshot), so every later milestone can assert against them.

---

## 7. Animation-mode UI (`animation_mode/`, fresh, prototype-faithful)

Build DOM-first against `:9091`, panel by panel, diffing in real Chrome. Suggested
module shape (mirrors `material_mode/`):
```
animation_mode/
  mod.rs            # render(): ribbon + workspace (rail | viewport / dock), toasts
  ribbon.rs         # clip color/name/Duration/FPS · N tracks→N players · Add Track · Live·N chip
  library.rs        # ClipLibrary (rows, + New clip, dup/delete context)
  inspector.rs      # KeyInspector (keyframe vs track editors)
  add_track.rs      # AddTrackMenu popup (all target families, grouped, badges, lowers-to)
  viewport.rs       # real-canvas host + overlay chrome + Solo-subtree + frame-selection
  timeline/
    dock.rs         # TimelineDock shell: transport + ruler + freeze-pane scroller + zoom + empty state
    transport.rs    # play/scrub/step/loop/dir/speed/frames⇄seconds
    ruler.rs        # nice-step ticks, scrub, playhead
    dope.rs         # Dope Sheet (rows + lanes + Diamond glyphs + mute/expand)
    curves.rs       # Curve/Graph editor (sampled paths + tangent handles)
    mixer.rs        # NLA layers/strips + per-layer weight/mode + bone-mask editor
```

### 7.1 Ribbon (`anim-app.jsx` ribbon)
Clip color chip · clip name `TextInput` · **Duration** `NumField` (suffix `s`) ·
**FPS** `Select` (12/24/30/60) · spacer · `N tracks → N players` mono readout ·
divider · **Add Track** (`Btn ghost` → AddTrackMenu) · **Live · N players** chip
(green; tooltip "Edits compile to AnimationPlayers automatically — no manual bake.
Same as material hot-reload."). Every control dispatches a command (§6.3); the
readouts derive from controller signals.

### 7.2 Left rail = ClipLibrary (top) + KeyInspector (bottom)
- **ClipLibrary** (`anim-rail.jsx`): header "Animations" + count + `+`; rows =
  color dot · name · duration · LOOP/PP/ONCE `Badge` · track count · `×speed`;
  active row accented; click → `SetCurrentClip`. Rebuild via
  `custom_animations.signal_vec_cloned()`.
- **KeyInspector** (`anim-rail.jsx`): when a **keyframe** is selected → Channel
  (read-only colored) · Time · Value · Interp `Select` (Constant/Linear/Cubic) ·
  in/out tangent `NumField`s (cubic only). When a **track** is selected → Target ·
  Property · **Lowers to** (e.g. `Transform·R`, `F32 · uniform`, `Vertex ·
  weight`) · Sampler `Select` · Channels list. Edits → `SetKeyframe`/
  `SetTrackSampler`.

### 7.3 Viewport — the REAL scene (replaces the mock robot)
`viewport.rs` hosts the **reparented WebGPU canvas** (the same one Scene mode
uses; toggled visible by mode). Keep the prototype's overlay-chrome layout
(`anim-rig.jsx`): ViewAxis nav cube (top-right), bottom-left clip-name +
"▶ playing" chip — but **drop** the "mock playback · not GPU" chip (it's real
now). Add:
- **Solo-subtree** control (decision #6): a small picker (or "use current
  selection") setting `anim_solo_root`; when set, only tracks under that node
  advance (others rest-hold) and the camera frames that subtree. A "Whole scene"
  reset clears it.
- **Frame selection** button (reuse the camera-fit path).
The transport scrubs the **real** pose. This is the payoff of decision #5 — WYSIWYG
GPU playback.

### 7.4 Timeline dock (`anim-timeline.jsx`)
- **Transport** (`transport.rs`): to-start/prev-key/play-pause/next-key/to-end;
  time readout button toggling **frames⇄seconds**; direction toggle; loop cycle
  (LOOP→PP→ONCE); speed slider. Each dispatches (`SetPlayhead`/`StepPlayhead`/
  `SetPlaying`/`SetClipDirection`/`SetClipLoop`/`SetClipSpeed`). `prev/next` snap
  to the union of keyframe times.
- **Ruler** (`ruler.rs`): `niceStepSec`-style ticks, click/drag scrub →
  `SetPlayhead`, playhead handle + line.
- **Freeze-pane scroller** with a sticky left names column + sticky top ruler, a
  shared `geo { pxPerSec, dur, timeToX, xToTime }`; the three views render into
  the same grid. `pxPerSec` zoom + Fit are **local** view chrome.
- **Dope Sheet** (`dope.rs`): track rows (chevron expand · kind icon · target +
  `property·uniform/morph` · mute eye) and channel rows (color dot · name · key
  count); lanes with draggable `Diamond` glyphs (drag → `SetKeyframe{t}`),
  collapsed-track key union; empty-state "This clip has no tracks" + Add Track.
- **Curves** (`curves.rs`): draw every curve by sampling the **lowered renderer
  sampler itself** (not a parallel JS math) so the curve is **bit-WYSIWYG with the
  GPU** (decision §10, invariant §4.7-I5). For scalar / T / S tracks this is the
  per-component value curve directly. For **rotation** (quaternion-native), render
  three **Euler-projection curves (X/Y/Z°)**: sample the quaternion sampler at
  curve resolution and convert each sample to Euler with **continuity unwrapping**
  (pick the Euler triple nearest the previous sample — no ±360 jumps / gimbal
  flips). Pinned value axis + gridlines; draggable keyframe dots
  (`SetKeyframe{value}` — for rotation the dragged Euler value converts to the
  stored quat); editable **cubic tangent handles** (`SetKeyframe{in/out_tangent}`
  — quaternion tangents for rotation, edited via the projection); per-channel
  show/hide (local); curve fill. Because there's one shared `times[]` per track,
  dragging a keyframe in time moves the whole keyframe (all components together).
- **Mixer** (`mixer.rs`): layers (Replace/Additive `Badge` toggle →
  `SetLayerMode`; for Additive, a **base-clip** `Select` — decision #8; weight
  slider → `SetLayerWeight`); a **bone-mask editor** per layer (decision #7:
  multi-select scene nodes + "include descendants" → `SetLayerMask`); clip strips
  (drag-move → `MoveStrip`; trim handles → `TrimStrip`; repeat hatch); add
  layer/strip. The mixer's `mixerDuration` drives the timeline length in Mixer
  view. **This view drives the real renderer Mixer (§4.4).**

### 7.5 Add-Track picker (`add_track.rs`, from `anim-rail.jsx` AddTrackMenu)
A `Popup` with search + group-by-target. Surfaces **all** target families
(decision #3), each row showing its **lowers-to** hint and a badge
(`UNIFORM`/`MORPH`/`LIGHT`/`CAMERA`); already-added `target·prop` rows dimmed +
"added". The target list is derived from the **real scene** (controller snapshot):
every node's transform (T/R/S), each mesh's morph names, each assigned
custom-material's named uniform slots, each built-in material's params, each
light's valid params (by kind), each camera's params. Selecting a row dispatches
`AddTrack { target_spec }`.

---

## 8. Cross-cutting: Content Browser, Command Palette, Toasts
- **Content Browser** (`scene_mode/content_browser.rs`): add a `Cat::Animation`
  tab (clips as cards, count, search, `+` New clip, double-click → Animation
  mode; imported-glTF clips appear here too). Symmetric with the Materials tab.
- **Command Palette** (`command_palette.rs`): add commands — switch to Animation
  mode, new clip, play/pause, add track, select any clip.
- **Toasts:** "Added track …", "Created empty clip", "Imported N clips from
  <file>", reusing the existing toast host.

---

## 9. Cross-tab sync (the EditorController payoff — build the relay seam)
Because §0.2 holds, cross-tab sync is a **thin adapter**, not a feature rewrite:
- The controller already has `dispatch_json` / `snapshot_json` seams (`main.rs`).
  Add a **`BroadcastChannel("awsm-editor")` relay**: on local `dispatch`, after
  applying, post the serialized command to the channel **tagged with an origin
  id**; on receiving a command from another tab, apply it **without** re-broadcast
  (guard the echo) and **without** re-recording undo (replay path, like undo/redo
  goes straight to `apply`). Late-joining tabs request a `snapshot()` from an
  existing tab to seed state.
- Transport commands (`SetPlayhead`/`SetPlaying`) are transient but **do**
  broadcast — that's how two tabs scrub together while showing different cameras.
  The **viewport free-fly camera** (orbit/pan/zoom + which authored camera a tab is
  looking through) is **per-tab local view state — NOT broadcast** — that's the
  whole point. ⚠ Don't conflate that with an **authored Camera node's animated
  params** (FOV/near/far keyframes): those are *clip data* and broadcast like any
  other keyframe edit — every tab agrees on the animation, each tab independently
  chooses whether to *view through* that camera.
- **Scope for this pass:** build the relay so two tabs on `:9085` stay in sync
  (verify: open two MCP tabs, edit a keyframe / scrub in one, see the other
  update). Keep it minimal and behind the same seam the future MCP/websocket
  transport will reuse. If time-boxed, at minimum land the **seam** (origin-tagged
  dispatch + snapshot-seed) even if the relay is feature-flagged.

---

## 10. Decided defaults (no open items remain)
**Decided (don't ask):**
- Time units: the renderer clock is **milliseconds** (`speed` default `1/1000`);
  the editor works in **seconds** at the model level and converts at the
  `update_animations` boundary. Keep clip durations/keyframe times in seconds in
  the authored model + TOML.
- Auto-compile, no bake button (WYSIWYG, material-style debounce).
- Reference served at **9091**; editor at **9085**.
- The viewport is the **real** scene; no mock rig; no 2nd renderer instance.
- **Rotation is quaternion-native, truly WYSIWYG (the user's call).** A rotation
  track stores a single **`Quat` channel** — the *exact* sampler the renderer plays
  (times[], quat values[], quat in/out tangents for CubicSpline, matching glTF) —
  so it is the single source of truth and glTF rotation imports 1:1 with **no
  reinterpretation**. The Curve editor renders rotation as three **Euler-projection
  curves (X/Y/Z°)** by sampling that quaternion sampler at curve resolution and
  converting each sample to Euler with **continuity unwrapping** (pick the Euler
  triple nearest the previous sample → no ±180/360 jumps or gimbal flips). Because
  the displayed curve is sampled from the *same* quaternion the GPU plays, it is
  **bit-WYSIWYG** — there is **no** Euler-hermite-vs-slerp divergence anymore
  (§4.7-I5). Editing: a keyframe is edited in Euler° (inspector NumFields / dragging
  a projected dot) and converted to the stored quaternion; cubic handles edit the
  quaternion in/out tangents via the projection.
- **All tracks use ONE shared `times[]` per track (glTF-faithful).** A keyframe
  holds the whole typed value (vec3 for T/S, quat for R, scalar for uniform/light/
  camera/morph) + per-component tangents for cubic; the "X/Y/Z" channels are an
  edit/display **decomposition**, not independent per-axis time streams. This makes
  every curve bit-WYSIWYG with the runtime sampler (vec3 lerp == per-component;
  rotation == the quaternion projection) and **removes all channel-time merging**
  from lowering. (This deliberately diverges from the prototype mock, which kept
  per-axis keys independent for mock simplicity; per-axis-independent keys are a
  possible future enhancement, gated behind a resample-on-lower that would
  sacrifice exact cubic WYSIWYG — not now.)
- Solo-subtree uses node-subtree containment of each track's target node.
- **Camera params are animatable via a lights-shaped `CameraKey` store** (§4.3
  Camera) — resolved, no longer an open item.
- **Skinned models:** glTF skin **joints import as addressable editor nodes**
  (via `asset_template`'s node-index→NodeId map), and a transform track on a joint
  drives the renderer's skinning matrices through the joint's `TransformKey` (the
  same path that makes the Fox animate today). The Add-Track picker lists joints
  like any node. Verify with `CesiumMan`/`RiggedSimple` (§6.7) — if the editor
  ever *collapses* the joint hierarchy on import, that's a bug to fix here, since
  bone tracks depend on per-joint nodes.

**No open items remain** — every target family has a concrete, cited write/read
path (transforms `set_local`; morphs `update_morph_weights_with`; uniforms +
built-in params `update_material`; lights `lights.update`; camera a new
lights-shaped `cameras.update`). Run straight through.

---

## 11. Milestones (each ends `task lint`-green + verified in real Chrome)

Restate §0.2 (everything via EditorController) at the start of every milestone.

- **M-A0 — Sanity + scaffold.** Start the dev servers (§0.4). MCP hard-gate
  (§0.5): drive `:9091` (reference) and `:9085` (editor) in real Chrome —
  screenshot + console both. Add `EditorMode::Animation`, the segmented option,
  the workspace router branch, `mod animation_mode` with an empty shell. Verify
  the tab switches and the canvas reparents without tearing the render loop.
- **M-R0 — Player scrub API + named Clip/Group + shared clock (§4.1–4.2).**
  Renderer-only; unit tests for clock wrap + seek. Single-clip group reduces to
  today's behavior (glTF Fox still auto-plays identically — verify in
  model-tests).
- **M-R1 — `AnimationTarget` + the new target write/read paths (§4.3).**
  Transform/morph unchanged; add Uniform, BuiltinParam, Light, and **Camera
  (mirror the lights pattern: `CameraKey` + `Cameras` store + `cameras.update`;
  render loop reads the active camera's params from the store — §4.3 Camera)**.
  Drive each from a hand-built clip group in a dev harness; verify one of each
  actually moves the GPU value (pixel/readback). **Keep the core strict per I2:
  bad/missing targets return `Err` and propagate — no silent per-frame skip**
  (§4.7).
- **M-R2 — Accumulate-then-write blending engine (§4.4) — HIGHEST-RISK GATE
  (§0.9).** Honor the §4.7 invariants (rest = stored snapshot I1; per-field
  optionality I3; bit-identical single-clip I4). Replace + crossfade first (verify
  slerp midpoint), then Additive (rest + optional base clip; verify no per-frame
  drift), then per-layer bone masks. Unit-test the blend math; GPU-verify a
  2-layer crossfade and an additive upper-body-mask case. **If I4 cannot be made
  bit-identical (the Fox regresses), STOP and surface (§0.9) — do not ship the
  redesign over a red I4.** Full blending is the bar.
- **M-R3 — glTF clip extraction API (§4.5/§6.7 renderer side).** Editor-facing
  extraction returns clips as data (no auto-insert).
- **M-A1 — Clip model + controller + commands + persistence + query surface
  (§6.2–6.8).** The `CustomAnimation` model, all `EditorCommand`s, `apply`
  handlers + inverses, `animation_sync` lowering/driving, `animation-<id>.toml`
  round-trip, **and the §6.8 `EditorQuery` additions (`SampleClipTimeseries` +
  `CanvasPixels`/`CanvasStats`) wired into `query_json`** so every later milestone
  asserts numerically. No fancy UI yet — drive via `dispatch` + verify via
  `snapshot()`, the new queries, and the real viewport posing.
- **M-A2 — Ribbon + ClipLibrary + KeyInspector (§7.1–7.2).** Tab-to-tab vs `:9091`.
- **M-A3 — Viewport (real scene) + transport + ruler + Dope Sheet (§7.3–7.4).**
  Play/scrub drives the real pose. Solo-subtree + frame-selection. Verify GPU
  motion (not just the timeline) with a pinned-frame readback.
- **M-A4 — Curve/Graph editor (§7.4 curves).** Curves match the GPU sample;
  tangent handles edit real tangents.
- **M-A5 — Mixer/NLA UI + bone-mask editor (§7.4 mixer)** wired to the renderer
  Mixer (§4.4). Layers/strips/weights/masks/base-clip all drive the real scene.
- **M-A6 — Add-Track picker (all target families) + glTF clip import (§6.7,
  §7.5).** Import an animated glTF → clips appear + play; author a uniform/light/
  camera track → it drives the GPU.
- **M-A7 — Content Browser tab + Command Palette + Toasts (§8).**
- **M-A8 — Cross-tab sync relay (§9).** Two `:9085` tabs stay in lock-step.
- **M-A9 — Polish + parity sweep + DONE.** Pixel-match every prototype panel
  tab-to-tab in real Chrome; **run the entire §12.1 MVP test matrix end-to-end**
  (every target family × Dope/Curves/Mixer + the cross-cutting tests) and confirm
  each passes; final GPU verification of playback + full blending; `task lint`
  green; `model-tests` still builds + the Fox still animates identically. Confirm
  §13 in full. **Then, and only then, output
  `ANIMATION SHIPPED — ROLL THE CREDITS!!!`.**

---

## 12. Verification methodology (animation-specific, per DEBUGGING-PREVIEW.md)
- **Panels:** screenshot `:9085` vs `:9091`, diff layout/spacing/color/type/state
  + interactions; iterate to match. Console errors-only after each load (zero
  panics / GPU validation errors).
- **GPU motion truth:** never trust a screenshot for "did it move". Use the
  **Animation pin** — set `playing=false`, set `playhead` to a known time, drive
  `update_animations(0.0)`, then **`getImageData`** silhouette pixels or read back
  a transform/uniform/light value via a dev `#[wasm_bindgen]` export, and compare
  to the expected sample. For blending, verify the **midpoint** of a crossfade is
  the slerp midpoint, and an additive masked layer moves **only** the masked
  subtree.
- **rAF visibility:** the tab MUST be foreground/visible or the clock freezes —
  if a rAF-wrapped read times out but `Date.now()` returns, ask the user to
  foreground the tab.
- **Cross-tab:** open two MCP tabs on `:9085`; mutate in one; confirm the other
  reflects it (scrub + a keyframe edit).
- **Subtle-visual cases:** prefer a numeric query (§6.8) that decides it. If truly
  only human eyes can ("does the wave read naturally?"), **do not block the
  unsupervised run** — record it on the `VERIFY-LATER` list (§0.9) with the exact
  setup + a specific yes/no question for the user, and continue. Save interactive
  yes/no for when the user is actually present.

### 12.1 MVP test matrix (the acceptance gate)

These are the **minimum** acceptance tests — the cross-product of **every target
family × every authoring surface (Dope Sheet / Curves / Mixer)**, plus the
cross-cutting paths. Each cell is **one concrete, binary-verifiable test**, run
**through the §6.8 query surface** — `SampleClipTimeseries` for exact value
readback (preferred: GPU-independent, deterministic) and `CanvasPixels`/
`CanvasStats` for the rendered-pixel cases — invoked gesture-free from the
real-Chrome tab via `window.wasmBindings.query_json(...)` and asserted
numerically. Both use the **Animation-pin** (the query sets `playing=false`, pins
`playhead`, `update_animations(0.0)`, reads, restores). Fall back to a **specific
yes/no to the user** only for the genuinely subtle cases; never a raw screenshot.
A milestone is not "done" until its row/column of this matrix passes;
**all** of it must pass for §13. (Mapping: the per-target write paths land at
**M-R1**, blending at **M-R2**; the Dope column is exercised at **M-A3**, Curves
at **M-A4**, Mixer at **M-A5**, the target families across all three at **M-A6**;
the cross-cutting tests at **M-A6/M-A8** and swept at **M-A9**.)

**Fixtures** (served from media-local `:9082` unless noted):
- `glTF-Sample-Models`: **`Fox`** / **`CesiumMan`** / **`BrainStem`** (skinned
  transforms), **`AnimatedMorphCube`** / **`MorphStressTest`** (morph weights),
  **`RiggedSimple`** (clean bone hierarchy for masks).
- **`anim-test` project** (author once at M-A6, save under the editor's served
  assets): a primitive **Sphere** with (a) a **custom dynamic-WGSL material**
  exposing a named `emissive: f32` + `tint: vec3` uniform, (b) a **built-in PBR**
  cube, (c) a **Point light** + a **Directional light**, (d) a **Camera** node.
  This is the deterministic, self-contained rig for the non-glTF target families.

| Target family | **Dope Sheet** | **Curve editor** | **Mixer / NLA** |
|---|---|---|---|
| **Transform** (node/bone Rotation; quaternion-native) | Key Rot.Y `0°→90°→0°` over 1 s (one shared-time keyframe set); at `t=0.5` readback world matrix → node at 90° (±ε). Repeat with **Step** (holds 0° until t=0.5) and **Linear** (45° at t=0.25) to prove interp kind. | Drag the middle key's **out-tangent** to overshoot; **assert the Euler-projection curve equals the timeseries query at the SAME off-key times** (bit-WYSIWYG, §4.7-I5), and the projection is continuous (no ±360 jump). | Two clips `RotA(0→90)`, `RotB(0→−90)` as **Replace** strips, one layer, weight `0.5` → node at the **slerp midpoint** (~0°). Add an **Additive** layer (base = rest) → delta composes on top. |
| **Morph weight** (geometry) | `AnimatedMorphCube`/`MorphStressTest`: key weight `0→1→0`; at `t=0.5` readback the morph-weight buffer → `1.0`; silhouette pixels shift (getImageData). | Cubic tangent on the weight curve smooths the in/out; the pinned-frame weight matches the sampled value. | Two morph clips **Additive** → weights add then clamp `[0,1]`; a masked layer leaves other morph targets untouched. |
| **Custom-material uniform** (`emissive` f32 / `tint` vec3) | `anim-test` sphere: key `emissive 0.8→2.6→0.8`; at the peak the sphere region reads **brighter** (getImageData dominant-channel jump, allowing tonemap ±20/255). | Tangent-eased emissive pulse; the pinned value at the apex equals the curve sample. | Crossfade two uniform clips (`tint` red↔blue) at weight `0.5` → purple-ish (dominant-channel check). |
| **Built-in material param** (PBR base color / metallic) | Key base color `red→blue`; the midpoint pixel is purple-ish; key `metallic 0→1` changes the specular response (user yes/no if subtle). | Ease the base-color channel; the pinned-frame color matches the sample. | Crossfade two base-color clips; confirm the blended color, **not** last-write-wins. |
| **Light param** (intensity + color) | `anim-test`: key point-light `intensity 0→strong`; the lit scene **brightens** (getImageData mean-luma rises); key color red→white shifts the dominant channel. | Ease intensity (no clipping pop); the pinned-frame luma matches. | Crossfade two light clips; **Additive** intensity sums; never flips the light variant. |
| **Camera param** (FOV) | Key `fov 30°→60°` on an authored camera (`cameras.update` store, §4.3); readback the stored `fov_y` at `t=0.5`; viewed through that camera the object's on-canvas extent **widens** (silhouette bbox at the two pinned frames). | Ease FOV; the pinned-frame `fov_y` equals the sample. | Crossfade two FOV clips; verify the blended `fov_y`. |

**Cross-cutting MVP tests (must also pass):**
- **glTF clip extraction (§6.7):** import `CesiumMan`/`BrainStem` → its clip(s)
  appear in the library + Content Browser; play → matches the source motion (pin a
  frame, readback a joint, compare to the source sample); the model-tests Fox
  still auto-plays **identically** (no regression).
- **Persistence round-trip (§6.6):** author a clip + a mixer layer, save the TOML
  project, reload (and also via `LoadProjectFromUrl`) → `snapshot()` is identical
  and the pinned-frame pose is identical.
- **Cross-tab sync (§9):** two `:9085` tabs on the same project — scrub the
  playhead **and** edit a keyframe in tab A → tab B reflects both; each tab keeps
  its own camera.
- **Solo-subtree (§7.3):** solo a subtree (e.g. `RiggedSimple`'s arm) → only that
  subtree animates; the rest holds at rest; the camera frames the subtree.
- **Clock semantics:** `Loop` wraps, `PingPong` reverses at the ends, `Once` ends
  + clamps; `speed` and `direction` scale/reverse the shared clock; a clip's
  channels stay **in sync** (the shared-clock guarantee, decision #2).
- **Auto-compile (no bake):** editing a keyframe updates the GPU pose within the
  debounce window with no manual action (the "Live · N players" chip).

---

## 13. Definition of done
- A third **Animation** mode exists in `packages/frontend/editor`, matching the
  `~/Downloads/animation-reference` prototype across all panels (ribbon · clip
  library · key/track inspector · real-scene viewport with Solo-subtree · Dope
  Sheet · Curve editor · Mixer/NLA with bone masks · Add-Track picker), verified
  tab-to-tab in **real Chrome via the Claude-in-Chrome MCP**.
- **renderer-core** gained: player scrub/seek API; a first-class named
  **Clip/Group with a shared clock**; the **`AnimationTarget`** enum with
  **Transform / Morph / Uniform / BuiltinParam / Light / Camera** write+read
  paths; and an **accumulate-then-write blending engine** with weighted **Replace
  + crossfade**, **Additive** (rest pose + optional base clip), and **per-layer
  bone masks** — all GPU-driven. A single clip with no mixer is **bit-identical**
  to the old last-write-wins path (Fox in model-tests animates identically).
- Importing a `.glb` **extracts its animation clips** into the library (symmetric
  with material extraction); authored clips can target node transforms, morph
  weights, **custom-material uniforms**, **built-in material params**, **light
  params**, and **camera params**, and they **drive the GPU live** (no bake).
- Clips persist as **`animation-<id>.toml`** in the editor TOML project (mirroring
  materials); load + glTF import work from an FS handle **and** gesture-free from a
  URL; `scene-schema`'s `project.json` is untouched and `model-tests` still builds.
- **Every animation mutation — clip/track/keyframe/transport/mixer — is a
  serializable `EditorCommand` dispatched through the `EditorController`**; undo/
  redo is the command log; transient transport/selection commands broadcast but
  aren't recorded; a serializable `snapshot()` covers animation state; the UI/
  bridge never mutate animation state directly.
- **A read-only query surface (§6.8)** extends the same `EditorQuery`/`query_json`
  seam with `SampleClipTimeseries` (exact value time-series) + `CanvasPixels`/
  `CanvasStats` (the getImageData path), drivable **gesture-free** for headless
  tests + the future MCP; the §12.1 matrix runs through it.
- **Cross-tab sync** works: two browser tabs on the same project stay in lock-step
  (synced playhead/edits) while each frames its own camera — via the
  `BroadcastChannel` relay over the same `dispatch`/`snapshot` seam (the future
  MCP/websocket transport reuses it).
- **Every cell of the §12.1 MVP test matrix passes** (every target family ×
  Dope/Curves/Mixer) plus all the cross-cutting MVP tests — verified by
  pixel/readback or specific user yes/no, not raw screenshots.
- `task lint` green (all crates); GPU playback + full blending verified in real
  Chrome (pixel/readback, not just screenshots).
- **When all the above holds, output the literal line
  `ANIMATION SHIPPED — ROLL THE CREDITS!!!`** as the final message, and not
  before.
```
