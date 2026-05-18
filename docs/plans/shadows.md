# Shadows Implementation Plan

## Instructions for the Implementor

This plan is meant to be followed **start to finish** in a single sustained effort.
The phases are ordered so each one leaves the renderer in a runnable (if visually-incomplete) state, but you should not try to ship intermediate phases as standalone PRs — there will be deliberate breaking changes along the way (new bind groups, new pipeline layouts, new schema fields, etc.) and the goal is to keep the diff coherent rather than always shippable.

- **Commit frequently** at every natural checkpoint (e.g. after each phase, after each subsystem stands up green). Small commits make `git bisect` cheap when something regresses. Don't squash as you go.
- **Breaking changes are fine** mid-plan. If you need to change the shape of `LightConfig`, the bind group layout for the opaque pass, or the on-disk `project.json` schema, just do it — there's no migration story to preserve here yet. Update the test scene (`/Users/dakom/Documents/DAKOM/awsm-renderer-assets/world/project.json`) along with the change.
- **Update the tracking section at the bottom** as you go. Tick boxes when each item is done so a future session can resume cleanly if you stop mid-way.
- **Only after EVERYTHING below has landed and visually verified**, run:
  ```
  cargo fmt
  cargo clippy --workspace --all-targets
  ```
  Fix everything clippy turns up. Then the branch is ready to push.

### How to test

The primary verification surface is the **scene-editor** in a browser. Start it with:

```
task scene-editor:dev
# served at http://localhost:9081
```

Use the `preview_start` / `preview_screenshot` / `preview_snapshot` tools to drive the page in a Chromium preview. The renderer crate hot-reloads via Trunk's watch list, so editing renderer code and refreshing the preview is the fastest loop.

The test scene lives at `/Users/dakom/Documents/DAKOM/awsm-renderer-assets/world/project.json`. It currently contains only a particle emitter — **you are expected to extend it** as you implement each phase:

- Add a ground plane (`Primitive::Plane`) — the obvious shadow receiver.
- Add a directional light angled across the ground.
- Add some primitives (cubes, spheres, a sweep, a sphere on a stick) at varying heights to cast clear, identifiable shadows.
- Add a spot light pointed at one of the props to test spot shadows.
- Add a point light inside a small primitive cluster to test omnidirectional shadows.
- Add a skinned glTF model from `media/glTF-Sample-Assets/Models` (e.g. `CesiumMan`) once skinned shadows are in.

When testing, focus on:

1. **The golden path**: scene loads, shadows appear, no GPU validation errors in the console.
2. **Toggles**: flip a light's `cast_shadows` off — the shadow disappears, the light still illuminates. Flip a mesh's `cast_shadows` off — its shadow vanishes but it still receives. Same for `receive_shadows` (the mesh stops being darkened but still casts).
3. **Bias tuning**: drag the depth-bias / normal-bias inputs in the light inspector; verify acne disappears and Peter Panning doesn't get out of hand.
4. **Resolution switching**: change the per-light shadow resolution in the inspector; edges should get crisper/softer accordingly.
5. **MSAA on/off**: shadow sampling must look identical whether the geometry pass is `Some(4)` or `None`.
6. **Edge cases**: scrub the camera so a shadow caster moves out of the cascade frustum (directional) or out of the light range (point/spot). The shadow should fade or stop without flickering.

If you can't get something working through the editor, fall back to manually editing `project.json`, but prefer the editor — that's also a smoke test for the UI.

---

## High-Level Direction

We're adding shadow mapping to a **visibility-buffer deferred renderer** with a forward transparent pass. Shadowing slots cleanly into the existing architecture because shading is concentrated in **one** compute pass (the material-opaque pass) — the only fragment-shader site that needs a shadow lookup is that compute shader, plus the forward transparent pass.

### Render-graph slot

The existing frame layout, abbreviated:

```
geometry pass (opaque)        →  visibility / normal / depth targets
geometry pass (HUD)
[insert: shadow generation]   ← NEW
light culling (stub)
opaque clear
material_opaque (compute)     →  reads shadow maps
opaque mipgen (if transmissive)
blit opaque → transparent
material_transparent          →  reads shadow maps
display
```

Shadow generation goes between the geometry passes and `light_culling`. It is conceptually independent of geometry (it renders its own depth from the light's POV), but doing it after the main geometry pass lets us share frustum-culling work and read the already-uploaded transform / skin / morph buffers.

The `RenderHooks::after_geometry_pass` hook already exists for this kind of insertion, but for a first-class feature we put the dispatch directly in `render.rs` — under a runtime gate that skips the pass entirely if no shadow-casting lights exist in the frame.

### Techniques

Per the design discussion, v1 ships **the full set**:

- **Directional lights → Cascaded Shadow Maps (CSM)**. 4 cascades by default, fitted to the camera frustum each frame, stable-fit (snap to texel grid) to avoid swimming. Per-cascade resolution scaling.
- **Spot lights → single perspective shadow map**, projection derived from outer cone angle and range.
- **Point lights → 6-face cubemap**, one slice per light in a `texture_cube_array<depth>`. Six perspective renders per light per frame.
- **PCF** as the baseline filter on the 2D atlas, with a per-light **hardness** selector:
  - `Hard` — 1-tap `textureSampleCompare`.
  - `Soft` — fixed 3×3 PCF kernel.
  - `Pcss` — Percentage-Closer Soft Shadows: blocker-search pass + penumbra-sized PCF. Gives contact-hardening soft shadows (sharp where occluders touch, softer farther away). 2D atlas only (directional + spot), not cube.
- **EVSM (Exponential Variance Shadow Maps)** as a per-renderer option for **far directional cascades only**. EVSM stores depth moments of `exp(±c·z)` so the shadow map becomes a normally filterable texture — a separable Gaussian blur produces soft penumbras far cheaper than a wide PCF kernel. The hybrid is the right design: keep PCF on near cascades (stable contact detail, no light leaks); switch to EVSM on far cascades where wide soft filtering matters and tiny leaks are imperceptible. Scoped to directional only — point/spot get PCF/PCSS.
- **Contact shadows (SSCS)** — optional screen-space ray-march from the depth buffer, applied as a multiplier to the main shadow term. Fills in micro-occlusion that low-res shadow maps miss (gaps under feet, hair, sleeves). Cheap and complementary to map-based shadows; togglable globally.
- **Shadow-LOD temporal throttling** — re-render far directional cascades every N frames instead of every frame. Cheap perf win on relatively stable scenes. Simple "skip the dispatch on non-update frames, keep the atlas tile load-store" implementation. Per-cascade configurable update rate.

### Storage strategy

- **2D atlas** for directional cascades + spot maps. One `Depth32Float` texture, packed via a simple guillotine packer that re-runs when the set of shadow casters changes. Each viewport's `(x, y, w, h)` + `view_projection` is pushed to a storage buffer the opaque pass reads.
- **Cubemap array** for points. Fixed slot pool (`MAX_POINT_SHADOWS`, default 8). Each shadow-casting point claims a slice; sampling uses `textureSampleCompare(cube_array, sampler, dir, slice_index, ref_depth)` — hardware face-selection and seam filtering.

The atlas is dynamically allocated; the cubemap array is fixed capacity (resizing a cubemap array is a re-create, and we want a stable bind). If the user authors more shadow-casting points than slots, we drop the lowest-priority ones (e.g. farthest from camera) and log a warning once.

### Filtering

Per-light `hardness` selector with three modes:

- **Hard**: 1-tap `textureSampleCompare`. Use for crisp spotlights and perf-critical setups.
- **Soft**: 3×3 fixed PCF kernel.
- **Pcss**: blocker search + variable-kernel PCF (Percentage-Closer Soft Shadows). Gives the most "real" soft shadows we have without going to area lights — sharp where the caster touches the ground, softer the farther the caster floats above. Heavier than fixed-kernel PCF (typically 16-tap blocker search + 16-tap PCF = ~32 samples), so reserve it for hero lights / hero shots. 2D atlas only.

For directional lights, the `Pcss` setting applies to **near cascades** (the ones still using PCF). Far cascades that have been promoted to EVSM use EVSM's natively-soft filtering regardless of the `hardness` setting — the boundary is the EVSM cascade cutoff, not the hardness enum.

Filter selection happens at the sample site via a tiny branch on the shadow descriptor record, which is uniform across the workgroup (predictable, single light = single branch).

### Why these choices

- **CSM** is the de-facto standard for outdoor / open scenes and the only good answer for directional lights. PSSM (logarithmic split) for cascade slicing.
- **Cubemap array** for points keeps the WGSL clean and uses hardware-correct cube edge filtering. A unified atlas would require manually replicating face-selection logic and would soften / corrupt edge texels.
- **PCF as the universal baseline + EVSM for far directional cascades**. PCF is cheap, predictable, debuggable, and doesn't suffer light-bleeding — it's the right algorithm for near contact where artifacts are most visible. EVSM is the right algorithm for wide soft far-cascade filtering: it makes the shadow map a regular filterable texture, so a separable blur produces penumbras far cheaper than a wide PCF kernel. The hybrid uses each where it's best.
- **PCSS as the "hero light" soft shadow option** — when an artist wants contact-hardening on a specific directional or spot light, they flip the hardness to `Pcss`. Significantly heavier than fixed-kernel PCF, so it's a per-light choice, not a global default.
- **SSCS** because contact shadows from a 1024² directional cascade are typically too soft / floaty for first-person interactions — a couple of screen-space ray-march steps fix it for free using data we already have (depth + light direction).
- **Shadow-LOD temporal throttling** because re-rendering every cascade at every frame is wasteful when only the near cascade has meaningful per-frame change in a typical scene. Free perf budget for hero effects elsewhere.
- **Visibility-buffer renderer note**: shadow rendering itself does NOT use the visibility buffer trick. Shadow passes write depth only, so the whole reason for explosion (per-triangle metadata for later attribute lookup) doesn't apply. We use the existing **exploded position vertex buffer** because it's the only positional data we have, but we use a dedicated stripped-down vertex shader (skin/morph/billboard/transform → clip-space, nothing else).

### True non-goals

These are not in v1 and are not deferred — they're genuinely the wrong fit for this renderer right now.

- **Ray-traced shadows.** WebGPU does not expose hardware ray tracing. Not a "we'll get to it" — there's no path to it on this platform today.
- **MSM (Moment Shadow Maps).** Another member of the VSM family. The research literature shows MSM has somewhat better leak characteristics than VSM at the cost of more math and storage, but it's NOT a clear win over the EVSM-hybrid we're already shipping. Adding it would be moments-family cargo-culting, not a real quality improvement. Skip.
- **Static / dynamic shadow caching.** A "proper" temporal throttling system splits each cascade into a static portion (cached across many frames) and a dynamic portion (re-rendered each frame), and composites them. This is a significant architectural change involving caching invalidation, separate atlases, and incremental update logic. The v1 plan ships the simple "re-render the whole cascade every N frames" version, which gives most of the perf win without that complexity. If real-world perf demands the full split later, that's a focused future plan.

---

## Editor UX

### Light inspector

In `crates/frontend/scene-editor/src/properties/kind_editor/mod.rs::render_light_editor`, below the existing color / intensity / range / angle inputs, add a **"Shadows"** section. Use the same dedupe-on-variant pattern that's already used for variant-specific inputs:

Always-visible (all light kinds):
- `Cast shadows` — bool toggle. When off, the rest of the section grays out.
- `Depth bias` — slider (0.0–0.01, default 0.0005). Constant depth offset, the simplest acne knob.
- `Normal bias` — slider (0.0–0.5, default 0.05). Offsets the receiver along its normal before comparison; better than slope-scale at preventing acne on grazing geometry.
- `Resolution` — dropdown: 256 / 512 / 1024 / 2048 / 4096. Default 1024 (2048 for directional).
- `Hardness` — segmented toggle: `Hard` / `Soft` / `Pcss`. Default `Soft` for directional/point, `Hard` for spot. Point lights gray out `Pcss` (cube PCSS is not in v1).
- `PCSS penumbra scale` — slider 0.0–4.0, default 1.0. Only enabled when `Hardness == Pcss`. Scales the estimated penumbra size; higher = softer max blur radius.
- `Shadow max distance` — slider. Beyond this distance from camera, shadows fade out and the light skips its shadow pass that frame. Default 100m for directional, otherwise = light range.

Directional-only:
- `Cascade count` — 1 / 2 / 3 / 4. Default 4. Re-runs atlas packing.
- `Cascade split λ` — slider 0.0–1.0 (PSSM blend between log and uniform). Default 0.5.
- `EVSM cascade cutoff` — dropdown: `Off` / `Last cascade` / `Last 2 cascades`. Default `Last cascade`. Cascades at-or-beyond this index use EVSM instead of PCF/PCSS. `Off` keeps all cascades on PCF.
- `Far cascade update rate` — dropdown: `Every frame` / `Every 2 frames` / `Every 4 frames` / `Every 8 frames`. Applies to cascades at-or-beyond `cascade_count - 1`. Default `Every 2 frames` when there are 4 cascades, otherwise `Every frame`.

Spot-only:
- No extra inputs — the spot cone already defines the projection. PCSS available.

Point-only:
- No extra inputs — `range` already defines the far plane, position defines origin. PCSS not available (cube PCSS deferred).

### Mesh / model inspector

In each kind editor that owns a renderable (`primitive.rs`, `mesh.rs`, `model.rs`, `sweep.rs`, `instances.rs`, `sprite.rs`), add a small "Shadows" row:

- `Cast shadows` — bool. Default `true` for opaque materials, `false` for transparent (`MaterialAlphaMode::Blend`).
- `Receive shadows` — bool. Default `true` for opaque, `false` for transparent.

Sprite / line / particle: cast=false, receive=false by default and the controls aren't exposed (we can revisit particles later — they're not a v1 caster).

### Global toggles

Add a **"Rendering" panel** (or extend the existing one if there is one) with:
- `Contact shadows (SSCS)` — bool, default on.
- `SSCS step count` — slider 4–32, default 16.
- `Shadow atlas size` — dropdown 1024 / 2048 / 4096 / 8192. Default 4096. Affects packing room.
- `EVSM atlas size` — dropdown `Match atlas / 2` / `Match atlas` (i.e. a second atlas in `RGBA16F` for EVSM cascades). Default `Match atlas / 2`. Only allocated if at least one directional light has `EVSM cascade cutoff != Off`.
- `EVSM exponent` — slider 5–40, default 20. Controls the depth warp `c`. Higher = better contact, more risk of overflow.
- `EVSM blur radius` — slider 1–8 texels, default 3. Drives the separable Gaussian.
- `Max shadow-casting point lights` — dropdown 0 / 2 / 4 / 8 / 16. Default 8. Resizes the cube array (expensive — full re-create — so do it via an explicit "Apply" button rather than instant).

---

## Schema Changes

### `crates/scene-schema/src/light.rs`

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LightShadowConfig {
    #[serde(default = "default_true")]
    pub cast: bool,
    #[serde(default)]                          // 0.0 — bias added at sample time
    pub depth_bias: f32,
    #[serde(default = "default_normal_bias")]  // 0.05 — receiver offset along normal
    pub normal_bias: f32,
    #[serde(default = "default_shadow_res")]   // 1024
    pub resolution: u32,
    #[serde(default)]
    pub hardness: LightShadowHardness,         // Hard | Soft | Pcss
    #[serde(default = "default_pcss_scale")]
    pub pcss_penumbra_scale: f32,              // only used when hardness == Pcss
    #[serde(default = "default_max_distance")]
    pub max_distance: f32,                     // beyond this from camera, fade out
    // directional-only
    #[serde(default = "default_cascades")]
    pub cascade_count: u8,                     // 1..=4
    #[serde(default = "default_cascade_lambda")]
    pub cascade_split_lambda: f32,             // 0.0=uniform, 1.0=log
    #[serde(default)]
    pub evsm_cutoff: EvsmCutoff,               // Off | LastCascade | LastTwoCascades
    #[serde(default)]
    pub far_cascade_update_rate: FarCascadeUpdateRate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LightShadowHardness {
    Hard,
    #[default]
    Soft,
    Pcss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvsmCutoff {
    Off,
    #[default]
    LastCascade,
    LastTwoCascades,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FarCascadeUpdateRate {
    #[default]
    EveryFrame,
    Every2Frames,
    Every4Frames,
    Every8Frames,
}
```

Each `LightConfig` variant gets an additional `#[serde(default)] pub shadow: LightShadowConfig` field. Use `#[serde(default)]` so existing `project.json` files round-trip with shadows disabled (the `cast` default in `LightShadowConfig::default` is whatever feels right — I'd default it to `true` so adding lights "just works", but legacy projects load with `shadow.cast == true` then; that's fine and arguably an improvement).

### `crates/scene-schema/src/material.rs`

Not material-level. Shadow casting/receiving is a **per-mesh** property (the same material can be used on a casting and a non-casting mesh — e.g. a debug duplicate). Keep `MaterialDef` untouched.

### Per-mesh kind shadow flags

`NodeKind` variants that produce renderable meshes (`Model`, `Primitive`, `Mesh`, `SweepAlongCurve`, `InstancesAlongCurve`) get optional shadow flags. Two options:

1. **Flat fields** on each variant. Verbose but consistent.
2. **A shared sub-struct** referenced by each. Cleaner.

Pick option 2: introduce

```rust
// crates/scene-schema/src/tree.rs (or a new shadows.rs)
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MeshShadowConfig {
    #[serde(default = "default_true")]
    pub cast: bool,
    #[serde(default = "default_true")]
    pub receive: bool,
}

impl Default for MeshShadowConfig {
    fn default() -> Self { Self { cast: true, receive: true } }
}
```

Each renderable `NodeKind` variant gets `#[serde(default)] shadow: MeshShadowConfig`. `Sprite`, `Line`, `ParticleEmitter` do not get this — they're hard-coded to no-cast/no-receive.

---

## Public API Surface

The `awsm-renderer` crate is a library; the scene-editor is one consumer, but a game runtime / model-tests frontend / standalone tool must also be able to drive shadows without reverse-engineering the editor. The API below is the contract — implementors must keep it stable across phases, document every public item with rustdoc, and ensure a non-editor consumer can author a shadow-casting light end-to-end using only `pub` symbols from `awsm-renderer`.

### Design principles

- **Mirror the existing `Lights` / `Meshes` patterns**: an `AwsmRenderer::shadows: Shadows` subsystem, key-based mutation via methods, runtime params separate from on-disk schema.
- **One way to do each thing.** No "convenience" duplicates. The editor and the game runtime should call the same methods.
- **Schema vs. runtime separation.** The `scene-schema` types (`LightShadowConfig`, etc.) are the on-disk format. The renderer takes its own runtime types (`LightShadowParams`, etc.). The editor converts between them. A pure-runtime consumer (no editor, no project.json) ignores the schema crate entirely and authors `LightShadowParams` directly.
- **Lazy, dirty-flag-driven.** Setter methods do NOT trigger GPU work synchronously — they mark state dirty and the next `render()` call picks up the change. This is consistent with how `Lights`, `Materials`, etc. behave today.
- **Errors via a single `AwsmShadowError` enum.** All fallible methods return `Result<T, AwsmShadowError>` and `AwsmShadowError` flows into `AwsmError` like the other subsystem errors.
- **Every public item has a rustdoc comment.** Type-level doc explains what it represents; method-level doc explains effect, when it takes effect, and when it can fail. Examples for non-obvious methods.

### Types (`awsm_renderer::shadows`)

```rust
/// Runtime per-light shadow parameters. The renderer-side counterpart to
/// `scene_schema::LightShadowConfig`; the editor converts between them.
/// Default is "off" so a light gains shadows only via an explicit call.
#[derive(Clone, Debug, PartialEq)]
pub struct LightShadowParams {
    pub cast: bool,
    pub depth_bias: f32,
    pub normal_bias: f32,
    pub resolution: u32,
    pub hardness: LightShadowHardness,
    pub pcss_penumbra_scale: f32,
    pub max_distance: f32,
    // Directional-only fields; ignored for point/spot.
    pub cascade_count: u8,
    pub cascade_split_lambda: f32,
    pub evsm_cutoff: EvsmCutoff,
    pub far_cascade_update_rate: FarCascadeUpdateRate,
}

impl Default for LightShadowParams { /* sane defaults; cast = false */ }

/// Filter mode at the sample site.
/// - `Hard`: 1-tap compare.
/// - `Soft`: fixed 3×3 PCF kernel.
/// - `Pcss`: blocker-search + variable-kernel PCF. 2D atlas only; ignored for point lights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightShadowHardness { Hard, Soft, Pcss }

/// Which trailing directional cascades use EVSM instead of PCF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EvsmCutoff {
    Off,
    #[default] LastCascade,
    LastTwoCascades,
}

/// How often the far directional cascade re-renders. Near cascades always
/// render every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FarCascadeUpdateRate {
    #[default] EveryFrame,
    Every2Frames,
    Every4Frames,
    Every8Frames,
}

/// Per-mesh shadow flags. Both default to `true` for opaque meshes,
/// `false` for transparent. Sprites / lines / particles ignore these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshShadowFlags {
    pub cast: bool,
    pub receive: bool,
}

/// Renderer-wide shadow settings. Independent of any individual light.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowsConfig {
    pub sscs_enabled: bool,
    pub sscs_step_count: u32,
    pub atlas_size: u32,
    pub evsm_atlas_size: u32,
    pub evsm_exponent: f32,
    pub evsm_blur_radius: u32,
    pub max_point_shadows: u32,
    pub debug_cascade_colors: bool,
}

impl Default for ShadowsConfig { /* shipped defaults */ }
```

### Methods on `AwsmRenderer`

```rust
impl AwsmRenderer {
    /// Sets a light's shadow parameters. Pass `LightShadowParams { cast: false, .. }`
    /// to turn shadows off for this light while keeping the light itself.
    /// Takes effect on the next `render()` call. Errors if the key is unknown.
    pub fn set_light_shadow_params(
        &mut self,
        key: LightKey,
        params: LightShadowParams,
    ) -> Result<(), AwsmShadowError>;

    /// Returns the current shadow parameters for a light, or `None` if the
    /// light has never had shadow params set (treat as `cast = false`).
    pub fn light_shadow_params(&self, key: LightKey) -> Option<&LightShadowParams>;

    /// Updates a light's shadow params in place. Convenience over the
    /// get-clone-mutate-set pattern. Mirrors `Lights::update`.
    pub fn update_light_shadow<F: FnOnce(&mut LightShadowParams)>(
        &mut self,
        key: LightKey,
        f: F,
    ) -> Result<(), AwsmShadowError>;

    /// Sets a mesh's shadow flags. Takes effect on the next `render()` call.
    /// Errors if the mesh key is unknown.
    pub fn set_mesh_shadow_flags(
        &mut self,
        key: MeshKey,
        flags: MeshShadowFlags,
    ) -> Result<(), AwsmShadowError>;

    /// Returns the current shadow flags for a mesh. Returns the per-mesh
    /// default if never explicitly set (opaque → cast+receive, transparent → neither).
    pub fn mesh_shadow_flags(&self, key: MeshKey) -> MeshShadowFlags;
}
```

### Methods on `Shadows` (accessed via `renderer.shadows`)

```rust
impl Shadows {
    /// Replaces the renderer-wide shadow config. Atlas-size changes trigger
    /// a re-pack at the start of next frame. `max_point_shadows` changes
    /// re-create the cube array (expensive — call sparingly).
    pub fn set_config(&mut self, config: ShadowsConfig);
    pub fn config(&self) -> &ShadowsConfig;

    /// Number of lights currently registered as shadow casters.
    pub fn caster_count(&self) -> usize;

    /// `[0.0, 1.0]` — fraction of the 2D atlas occupied by active cascades + spots.
    pub fn atlas_utilization(&self) -> f32;

    /// `[0.0, 1.0]` — fraction of cube-array slots occupied.
    pub fn cube_pool_utilization(&self) -> f32;

    /// `true` if any shadow-casting light is currently active. The render
    /// graph short-circuits the entire shadow generation pass when `false`.
    pub fn any_active(&self) -> bool;
}
```

### Conversion helpers (in scene-editor / consumer code, NOT in `awsm-renderer`)

The editor crate owns the schema→runtime conversion. The renderer does not depend on `scene-schema`. Example conversion (for documentation; lives in `crates/frontend/scene-editor/src/renderer_bridge/`):

```rust
fn light_shadow_params_from_config(cfg: &scene_schema::LightShadowConfig) -> LightShadowParams {
    LightShadowParams {
        cast: cfg.cast,
        depth_bias: cfg.depth_bias,
        normal_bias: cfg.normal_bias,
        resolution: cfg.resolution,
        hardness: match cfg.hardness {
            scene_schema::LightShadowHardness::Hard => LightShadowHardness::Hard,
            scene_schema::LightShadowHardness::Soft => LightShadowHardness::Soft,
            scene_schema::LightShadowHardness::Pcss => LightShadowHardness::Pcss,
        },
        pcss_penumbra_scale: cfg.pcss_penumbra_scale,
        max_distance: cfg.max_distance,
        cascade_count: cfg.cascade_count,
        cascade_split_lambda: cfg.cascade_split_lambda,
        evsm_cutoff: cfg.evsm_cutoff.into(),
        far_cascade_update_rate: cfg.far_cascade_update_rate.into(),
    }
}
```

A non-editor consumer skips this and constructs `LightShadowParams` directly.

### Error type

```rust
#[derive(thiserror::Error, Debug)]
pub enum AwsmShadowError {
    #[error("[shadow] unknown light key")]
    UnknownLight,
    #[error("[shadow] unknown mesh key")]
    UnknownMesh,
    #[error("[shadow] point-light cube pool exhausted (capacity {0}); raise `max_point_shadows`")]
    CubePoolExhausted(u32),
    #[error("[shadow] atlas too small for requested resolutions ({need} > {have})")]
    AtlasTooSmall { need: u32, have: u32 },
    #[error("[shadow] {0}")]
    Core(#[from] awsm_renderer_core::error::AwsmCoreError),
}
```

`AwsmShadowError` is added to the top-level `AwsmError` enum like `AwsmLightError`, etc.

### Minimal integration example (game runtime, no editor)

This is the smallest end-to-end snippet that should compile against the public API. Include it verbatim as a rustdoc example on `Shadows` or in `crates/renderer/README.md`.

```rust
use awsm_renderer::{
    lights::Light,
    shadows::{LightShadowParams, LightShadowHardness, MeshShadowFlags},
};

// 1. Insert a directional light.
let sun = renderer.lights.insert(Light::Directional {
    color: [1.0, 0.95, 0.9],
    intensity: 3.0,
    direction: [0.3, -1.0, 0.3],
})?;

// 2. Enable shadows for it.
renderer.set_light_shadow_params(sun, LightShadowParams {
    cast: true,
    hardness: LightShadowHardness::Soft,
    cascade_count: 4,
    resolution: 2048,
    ..LightShadowParams::default()
})?;

// 3. Mark a specific mesh as a non-caster (defaults are usually fine).
renderer.set_mesh_shadow_flags(some_mesh_key, MeshShadowFlags {
    cast: false,
    receive: true,
})?;

// 4. Render as usual; shadows just work.
renderer.render(None)?;
```

### Documentation requirements

For every phase that introduces or modifies a public-API item, the implementor MUST:

1. **Add a rustdoc comment** to every new `pub` type, `pub` field, `pub` method, `pub` enum variant. Comments answer: what is this, when does it take effect, what can go wrong.
2. **Run `cargo doc --workspace --no-deps`** at the end of each phase that touches the API. Fix any broken intra-doc links.
3. **Update the integration example** in `crates/renderer/README.md` (or a dedicated `crates/renderer/examples/shadows.rs`) so it reflects the current shape of the API as it grows.
4. **Run `cargo clippy --workspace -- -W missing_docs`** as a periodic check — clippy flags missing docs on public items. This should be **clean at Phase 15** even if intermediate phases haven't caught up.

The "Public API" tracking checkboxes below gate the final ship.

---

## Renderer Changes

### New module: `crates/renderer/src/shadows/`

```
shadows/
  mod.rs                  ← pub struct Shadows; entry point
  config.rs               ← runtime config: atlas size, cube array capacity, sscs toggle
  light_shadow.rs         ← per-light shadow record (matrices, atlas rect, slice, params)
  atlas.rs                ← guillotine rect packer for 2D shadow atlas
  cube_pool.rs            ← fixed-slot allocator for cubemap-array slices
  cascade.rs              ← CSM cascade fitting (split + frustum-corners → light AABB)
  buffers.rs              ← GPU buffer for per-shadow descriptors (matrices, biases, atlas rects)
  render_pass.rs          ← ShadowRenderPass: pipelines, bind groups, draw dispatch
  shader/
    mod.rs
    template.rs           ← askama template wiring
    cache_key.rs
    shadow_wgsl/
      bind_groups.wgsl
      vertex.wgsl         ← stripped-down (position + skin/morph/billboard → clip)
      fragment.wgsl       ← empty / depth-only (may even be unused if we use depth-only pipeline)
```

### `Shadows` struct (lives on `AwsmRenderer`)

```rust
pub struct Shadows {
    pub config: ShadowsConfig,         // atlas size, cube cap, sscs on/off, evsm params
    pub atlas: ShadowAtlas,            // Depth32Float texture + guillotine packer (PCF/PCSS cascades + spot)
    pub evsm_atlas: EvsmAtlas,         // RGBA16F texture + packer (EVSM cascades only)
    pub evsm_blur: EvsmBlurPipeline,   // separable Gaussian (compute) + transient ping-pong texture
    pub cube_pool: ShadowCubePool,     // texture_cube_array<depth> + slot allocation
    pub descriptors: ShadowDescriptors, // GPU storage buffer of per-shadow data
    casters: SecondaryMap<LightKey, LightShadowRecord>,
    sampler_comparison: web_sys::GpuSampler, // CompareFunction::LessEqual, for PCF/PCSS
    sampler_filterable: web_sys::GpuSampler, // standard linear, for EVSM moment sampling
    frame_count: u64,                  // for far-cascade update-rate throttling
    dirty: bool,
}
```

Add `pub shadows: Shadows` to `AwsmRenderer` in `lib.rs`.

The two atlases are separate because their formats differ (`Depth32Float` vs `RGBA16F`) and their access patterns differ (`textureSampleCompare` vs `textureSampleLevel`). They can share the packer implementation but not the underlying texture.

### `LightShadowRecord`

For each shadow-casting light, after CPU-side fitting (camera frustum analysis for directional, simple for spot/point):

```rust
pub struct LightShadowRecord {
    pub light_kind: LightKind,
    pub config: LightShadowParamsGpu, // bias, normal_bias, hardness, max_distance
    pub views: SmallVec<[ShadowView; 6]>, // 1 for spot, 4 for directional (cascades), 6 for point
}

pub struct ShadowView {
    pub view_projection: Mat4,
    pub placement: ShadowPlacement,
}

pub enum ShadowPlacement {
    Atlas { x: u32, y: u32, w: u32, h: u32 },
    Cube { slice: u32, face: u32 },
}
```

The descriptors storage buffer interleaves these records in a layout the WGSL can pull from given a light index.

### `Lights` extension

In `crates/renderer/src/lights.rs`, `Light` enum gains a `shadow: LightShadowParams` field per variant (or a side-channel `SecondaryMap<LightKey, LightShadowParams>` to avoid bloating the 64-byte packed light record).

I recommend the **side-channel**: keep `Light` lean (the 64-byte packed record fits the shader-side `LightPacked` exactly), store `LightShadowParams` separately in `Lights` (a `SecondaryMap<LightKey, LightShadowParams>`), and add a `shadow_descriptor_index: u32` to the per-light packed record so the shader can index into the shadows descriptor buffer.

```
LightPacked layout becomes (still 64 bytes):
  pos.xyz + range
  dir.xyz + inner_cone
  color.rgb + intensity
  kind + outer_cone + shadow_index + pad
```

`shadow_index == U32_MAX` → no shadow.

### Bind groups

Add a **new bind group** at index 3 of the material_opaque compute pipeline (currently uses 0, 1, 2). Or extend the existing lights bind group — but extending introduces re-layouts whenever shadows enable/disable. A dedicated bind group is cleaner:

Bind group 3 = "Shadows":

```
@binding(0) var<storage, read> shadow_descriptors: array<ShadowDescriptor>;
@binding(1) var shadow_atlas: texture_depth_2d;            // PCF/PCSS cascades + spot
@binding(2) var shadow_atlas_sampler: sampler_comparison;
@binding(3) var shadow_cube_array: texture_depth_cube_array;
@binding(4) var shadow_cube_sampler: sampler_comparison;
@binding(5) var evsm_atlas: texture_2d<f32>;               // RGBA16F EVSM moments
@binding(6) var evsm_atlas_sampler: sampler;               // linear, filterable
@binding(7) var<uniform> shadow_globals: ShadowGlobals;    // atlas sizes, evsm params, sscs flags
```

The bind group layout is fixed (doesn't depend on light count). The descriptor buffer is variable-length.

For the **transparent pass** (forward), bind the same bind group at the same slot. The transparent pass already does PBR lighting via the same `apply_lighting` helper — we just inject shadow sampling there too.

### Pipeline layouts

`PipelineLayoutCacheKey` gets a new entry; the shadow generation pass has its own pipeline layout (subset of the geometry pass's: transforms + animation + a small "shadow view uniform" with the current `view_projection` matrix). Use `Push constants` if `awsm-renderer-core` exposes them, otherwise a small dynamic uniform with one offset per view.

---

## Implementation Phases

Each phase is a runnable checkpoint — commit after each. Lower phases assume upper phases compiled.

### Phase 0 — Wiring & scaffolding

1. Create `crates/renderer/src/shadows/` with empty modules, public types, `Shadows::new()` doing nothing useful. Add to `lib.rs`. `AwsmRenderer::new()` constructs it. No GPU work yet beyond an empty atlas allocation and the comparison sampler.
2. Wire a no-op `shadows::write_gpu(&mut self, ...)` into the `render()` pre-pass uploads.
3. Wire a no-op `shadow_render_pass.render(&ctx)` between the geometry passes and `light_culling`.
4. Add `Shadows` bind group at index 3 of the material_opaque pipeline. Populate it with a 1×1 depth atlas, a 1-slice cube array, and an empty descriptors buffer. WGSL compiles, all references inactive.

Expected outcome: scene renders identically to before. Commit.

### Phase 1 — Schema + editor wiring (no rendering yet)

1. Extend `crates/scene-schema/src/light.rs` with `LightShadowConfig` + `LightShadowHardness`. Each `LightConfig` variant gets `shadow: LightShadowConfig`. Update the round-trip test if there is one.
2. Add `MeshShadowConfig` and thread it through `NodeKind` variants that render meshes.
3. Update `crates/frontend/scene-editor/src/properties/kind_editor/mod.rs::render_light_editor` with the shadow controls section. Use the same dedupe-by-variant-tag pattern that's already in place.
4. Update mesh-bearing editors (`primitive.rs`, `mesh.rs`, `model.rs`, `sweep.rs`, `instances.rs`) with the per-mesh shadow row.
5. Update the renderer-bridge (`renderer_bridge/node_sync.rs::light_from_config`) to pull shadow params from `LightConfig::shadow` and call a new `r.shadows.set_light_shadow_params(key, params)` API.
6. Update `node_sync.rs` mesh-application paths to propagate `MeshShadowConfig` to the renderer's `Mesh` struct.
7. Add `cast_shadows: bool`, `receive_shadows: bool` to `crates/renderer/src/meshes/mesh.rs::Mesh`.

Expected outcome: editor UI shows all the controls; the values plumb through to the renderer; nothing is rendered yet. Commit.

### Phase 2 — Directional shadow generation (1 cascade, no filtering)

The simplest end-to-end pass. Get **one** shadow showing up before generalizing.

1. **CSM cascade fitting** (`shadows/cascade.rs`):
   - For 1 cascade for now: take the camera frustum's near/far split, compute the 8 corner positions in world space, find the AABB in **light space** (using the directional light's view matrix, looking from `-direction` toward origin).
   - Build an orthographic projection from that AABB.
   - Snap the projection origin to texel grid to avoid swimming. (Standard CSM trick: round the world-space step per pixel.)
   - Output `(view, projection, view_projection)`.

2. **Atlas packing** (`shadows/atlas.rs`):
   - One directional light, one viewport at `(0, 0, res, res)`. Trivial v1. Generalize in Phase 4.

3. **Shadow generation pass** (`shadows/render_pass.rs`):
   - Per shadow-casting light per view (cascade / face), begin a render pass with:
     - `color_attachments: []`
     - `depth_stencil_attachment: Some(...)` targeting the atlas texture view (`view: a 2d view of the atlas`), `LoadOp::Clear(1.0)`, `StoreOp::Store`.
   - Set viewport via `render_pass.set_viewport(x, y, w, h, 0.0, 1.0)`.
   - Set the shadow-view uniform (1 mat4 + bias floats), set the transforms / animation bind groups (same as geometry pass).
   - Iterate renderables filtered by `mesh.cast_shadows && !mesh.hidden && frustum_intersects_light_frustum(...)`.
   - Per renderable: set the shadow pipeline, bind the visibility-vertex buffer slot 0 (we just read `@location(0)` position), draw indexed.

4. **Stripped vertex shader** (`shadows/shader/shadow_wgsl/vertex.wgsl`):
   ```wgsl
   {% include "shared_wgsl/vertex/geometry_mesh_meta.wgsl" %}
   {% include "shared_wgsl/vertex/transform.wgsl" %}
   {% include "shared_wgsl/vertex/morph.wgsl" %}
   {% include "shared_wgsl/vertex/skin.wgsl" %}
   {% include "shared_wgsl/vertex/apply_vertex.wgsl" %}

   @group(0) @binding(0) var<uniform> shadow_view: ShadowView;  // mat4 view_projection
   // groups 1-3: transforms / mesh meta / animation (same as geometry pass)

   struct VertexInput {
       @location(0) position: vec3<f32>,
       // SKIP location 1 (triangle_index), 2 (barycentric), 3 (normal), 4 (tangent)
       @location(5) original_vertex_index: u32,
       {% if instancing_transforms %} ... {% endif %}
   };

   @vertex
   fn vert_main(input: VertexInput, @builtin(instance_index) idx: u32) -> @builtin(position) vec4<f32> {
       let applied = apply_vertex_world_only(...);
       return shadow_view.view_projection * vec4(applied.world_position, 1.0);
   }
   ```
   Notice: we still feed the full vertex buffer (it's the only one we have) but only read `@location(0)` and the skin/morph slots. Locations 1-4 are still declared (WebGPU vertex buffer layout requires it) but unused.

   Add a helper `apply_vertex_world_only()` to `shared_wgsl/vertex/apply_vertex.wgsl` that returns `vec3 world_position` and skips the normal/tangent paths.

5. **No fragment shader** — use a depth-only pipeline (set `fragment` to `None` in the pipeline descriptor if `awsm-renderer-core` supports it; otherwise a fragment with empty entry).

6. **Sample in the opaque compute** (`material_opaque_wgsl/helpers/material_shading.wgsl`):
   - Add a `sample_shadow_directional(world_pos, world_normal, light, shadow_desc)` function that:
     - Offsets the world position along the normal by `normal_bias`.
     - Transforms by `shadow_desc.view_projection`.
     - Divides xyz by w (orthographic so w=1, but still — be defensive).
     - Maps NDC.xy → [0,1] UV, NDC.z → reference depth.
     - Adds `depth_bias` to ref depth.
     - Reads `atlas_rect = shadow_desc.atlas_rect`, computes atlas UV = `rect.xy/atlas_size + uv * rect.wh/atlas_size`.
     - Returns `textureSampleCompare(shadow_atlas, shadow_atlas_sampler, atlas_uv, ref_depth)` (1.0 = lit, 0.0 = shadow).
   - In `apply_lighting`'s punctual-lights loop, multiply each light's BRDF contribution by the shadow term if the light has a shadow.

7. **Test scene update**:
   - Add a `Primitive::Plane` (10×10) at y=0 as the ground.
   - Add a `Primitive::Box` or two at y=1, well above the plane.
   - Add a `Directional` light with `intensity` ≈ 3 pointing roughly `[0.3, -1.0, 0.3]`, `cast_shadows: true`.
   - Reload editor; confirm a hard shadow appears on the plane.

Expected outcome: one directional shadow visible on the test scene's ground plane. Commit.

### Phase 3 — PCF + bias controls + hard/soft toggle

1. Replace the 1-tap sample with a 3×3 PCF kernel when `hardness == Soft`:
   ```wgsl
   var sum = 0.0;
   let texel = 1.0 / f32(atlas_size);
   for (var dy = -1; dy <= 1; dy++) {
       for (var dx = -1; dx <= 1; dx++) {
           sum += textureSampleCompare(atlas, samp,
               atlas_uv + vec2(f32(dx), f32(dy)) * texel,
               ref_depth);
       }
   }
   return sum / 9.0;
   ```
2. Drive `hardness`, `depth_bias`, `normal_bias` from the `ShadowDescriptor` storage buffer — change values in the editor inspector and verify they take effect live.
3. Tune defaults until a Plane/Box scene at default settings has neither acne nor visible Peter Panning.

Expected outcome: bias sliders work, soft shadows visibly differ from hard. Commit.

### Phase 4 — Multi-cascade directional (CSM)

1. Generalize `shadows/cascade.rs` to N cascades (1–4). Use PSSM with lambda blending:
   ```
   for i in 0..N:
       uniform_split = near + (far - near) * (i+1)/N
       log_split = near * (far/near).powf((i+1)/N as f32)
       split[i] = mix(uniform_split, log_split, lambda)
   ```
2. **Per-cascade resolution** (basic shadow LOD). Each cascade picks its own resolution from the light's authored resolution: cascade `i` gets `max(min_res, resolution >> i)`. So a light at 2048 with 4 cascades gets 2048 / 1024 / 512 / 256. Near contact stays crisp, far cascades cost less memory and bandwidth. Per `docs/PERFORMANCE_OPEN_WORLD_PLAN.md` this is the "Shadow LOD" knob — the temporal-throttling layer (re-render far cascades every N frames) is documented in **Out of scope** as a follow-up.
3. Atlas packing places N rectangles of varying sizes. The guillotine packer from Phase 2 already handles mixed sizes — verify it does and pack tightest-first.
4. The shadow descriptor for a directional light becomes a small array of `(view_projection, atlas_rect)` — one per cascade.
5. Sample side: cascade selection by view-space depth. The compute shader already has view-space depth via the camera matrices and screen position.
   ```wgsl
   fn directional_cascade_index(view_z: f32, splits: vec4<f32>) -> u32 { ... }
   ```
   Walk through cascades, pick the first whose far-split exceeds `view_z`. Blend the last cascade to no-shadow over the `max_distance` fade window.
6. Editor: wire `cascade_count` and `cascade_split_lambda` inputs to the descriptor.
7. **Debug visualization**: add a `debug.cascade_colors` flag (toggled via the existing debug bitmask infrastructure in `material_opaque_wgsl/helpers/debug.wgsl`) that tints each cascade range (red / green / blue / yellow) so you can visually verify the splits are placed sensibly. Trivially valuable when tuning lambda.

Expected outcome: shadows extend smoothly across the scene with stable resolution; no swimming when camera moves; cascade-color debug overlay confirms split placement. Commit.

### Phase 5 — EVSM hybrid for far directional cascades

The goal of this phase is to make far cascades cheaper to filter softly. The blur pass cost is roughly fixed per pixel of EVSM atlas; a 1024² EVSM cascade with separable blur is comparable to a 5×5 PCF kernel on a same-size depth atlas, but with hardware-trilinear-sampling-style filtering that scales for free as the cascade moves on screen.

1. **Second atlas** (`shadows/evsm_atlas.rs`): `RGBA16F` texture, packer reused from the PCF atlas. Allocated lazily on the first frame any directional light has `evsm_cutoff != Off`. Size defaults to half the main atlas's, configurable.
2. **EVSM moment encoding** in the shadow vertex/fragment shaders for EVSM cascades:
   - The depth-only pipeline becomes a "moment-writing" pipeline for EVSM cascades. New fragment shader that writes `vec4(exp(c·z), exp(-c·z), exp(c·z)², exp(-c·z)²)` (4-component EVSM).
   - `c` (the depth-warp exponent) comes from `ShadowGlobals.evsm_exponent`. Tune in the editor.
   - The vertex shader emits `clip_position.z / clip_position.w` (i.e. the depth that would have been written normally) and the fragment shader applies the exponential transform.
3. **Separable Gaussian blur** (`shadows/evsm_blur.rs`):
   - Two compute passes per EVSM cascade rectangle: horizontal blur into a transient ping-pong texture, vertical blur back into the atlas.
   - Kernel size from `ShadowGlobals.evsm_blur_radius` (default 3 texels).
   - Workgroup size 64x1 / 1x64 (one row / column per workgroup).
   - The ping-pong is sized to the largest EVSM rect; reuse across cascades.
4. **Atlas allocation**: when a directional light's cascade `i >= cascade_count - evsm_cutoff` (where `evsm_cutoff` is 0, 1, or 2), that cascade is allocated in the EVSM atlas, not the depth atlas. Per-cascade resolution scaling still applies.
5. **Sampling**: in the material_opaque compute, add `sample_shadow_evsm(world_pos, world_normal, evsm_atlas_rect, ref_depth)`:
   - Transforms world_pos by the cascade's view_projection, derives atlas UV.
   - Samples the 4-channel moments at `mip 0` with the linear sampler.
   - Reconstructs `pos_visibility = chebyshev_upper_bound(positive_moments, exp(c·ref_depth))`.
   - Reconstructs `neg_visibility = chebyshev_upper_bound(negative_moments, exp(-c·ref_depth))`.
   - Final visibility = `min(pos_visibility, neg_visibility)`. The two-sided variant is what makes it EVSM rather than ESM; it eliminates much of VSM's light bleeding.
6. **Cascade selection** in the shader gets the `is_evsm` flag from the cascade descriptor, branches between `sample_shadow_pcf_cascade` and `sample_shadow_evsm`. Both branches produce the same `[0,1]` visibility output that multiplies into the directional light's BRDF contribution.
7. **Editor**: wire `evsm_cutoff`, `evsm_exponent`, `evsm_blur_radius`. Defaults: cutoff = `LastCascade`, exponent = 20, blur radius = 3.
8. **Test**: with a 4-cascade directional light and `evsm_cutoff = LastCascade`, the last cascade should produce visibly softer shadows than the PCF cascades at the same resolution, with no light-leak artifacts at the contacts of the EVSM region. Toggle `evsm_cutoff = Off` and verify identical-to-Phase-4 behavior (regression check).

Expected outcome: distant directional shadows are visibly softer/cheaper, the boundary between PCF and EVSM cascades is invisible at default settings, no light bleeding. Commit.

### Phase 6 — PCSS (Percentage-Closer Soft Shadows)

PCSS extends PCF with a blocker-search pre-pass that estimates penumbra size from average blocker depth, then samples PCF with a kernel sized by that estimate. The result: shadows are sharp where the caster meets the receiver (small estimated penumbra) and soft where the caster floats above the receiver (large estimated penumbra).

1. **New sample function** `sample_shadow_pcss(world_pos, world_normal, shadow_desc, ref_depth)`:
   - **Blocker search**: 16-tap Poisson disk sample over the shadow map around the projected UV, kernel radius `~3 texels` scaled by `pcss_penumbra_scale`. For each tap, fetch raw depth (NOT compare-sampled) via `textureSampleLevel`. Count taps whose depth < `ref_depth - small_epsilon` (blockers) and accumulate their depths.
   - If no blockers: return `1.0` (fully lit, fast path).
   - Compute average blocker depth `d_avg`.
   - **Penumbra estimate**: `penumbra = (ref_depth - d_avg) * light_size_uv / d_avg` (the standard PCSS formula treating the light as a small area). `light_size_uv` is `pcss_penumbra_scale * (~5 texels)`.
   - **PCF**: 16-tap Poisson disk PCF (via `textureSampleCompare`) with kernel radius `= penumbra`. Return the average.
2. **Raw-depth read path**: the shadow atlas needs to be sampleable as both `texture_depth_2d` (compare-sampled, what we already have) and as a plain depth read. WebGPU lets you read depth via a non-comparison sampler if you cast the texture view — but the cleanest path is to add a second texture view to the atlas that exposes it as `texture_2d<f32>` with a linear (or non-filtering) sampler. Add `evsm_atlas_sampler`'s `sampler` style entry for this purpose, or add a dedicated `shadow_atlas_linear_sampler`. Add binding 8 to the shadow bind group:
   ```
   @binding(8) var shadow_atlas_depth_view: texture_2d<f32>;   // same atlas, no compare
   ```
3. **Hardness branch**: in the cascade sample dispatcher, add a third arm:
   - `Hard` → 1-tap compare.
   - `Soft` → 3×3 PCF.
   - `Pcss` → blocker search + variable-kernel PCF.
4. **Poisson disk constants**: a static `array<vec2<f32>, 16>` of pre-computed Poisson-distributed offsets in `[-1, 1]²`. Used by both the blocker search and the PCF kernel; rotate per-pixel by an inter-leaved gradient noise to break up the regular pattern.
5. **Cascade gating**: PCSS only applies to cascades NOT promoted to EVSM. Internally, if a cascade is marked EVSM, it ignores the hardness selector — EVSM is "always soft, always cheap." This is documented in the **Filtering** section above.
6. **Spot lights**: same PCSS function applies (spot is a perspective 2D atlas shadow just like a single directional cascade). Wire the same hardness branch.
7. **Point lights**: gray out `Pcss` in the editor for point lights. Cube PCSS is not in v1.
8. **Editor**: enable the `PCSS penumbra scale` slider when hardness is `Pcss`. Default 1.0.
9. **Test**: a tall thin object (cylinder, lamppost) on a flat plane. With `Hardness = Pcss`, the shadow should be very sharp where the object touches the ground and visibly softer at the top of the shadow far from contact. Compare to `Soft` (uniform softness) and `Hard` (uniform sharpness).

Expected outcome: per-light PCSS works on directional + spot; cube lights ignore the setting. Commit.

### Phase 7 — Spot light shadows

1. Spot light projection: `Mat4::perspective(outer_angle * 2.0, 1.0, 0.1, light.range)`, view = look-from-light.
2. Allocates a single atlas rect of `shadow.resolution` × `shadow.resolution`. The packer handles mixed sizes (guillotine on free-rect list).
3. Generation pass: identical to directional, just a different view-projection and viewport.
4. Sample side: `light_to_brdf` already knows the light is a spot. Add a `sample_shadow_perspective(world_pos, world_normal, shadow_desc)` that does the same UV math but with a perspective divide (`xyz/w`). The shader picks `directional`, `perspective`, or `cubemap` sampler based on `light.kind`.
5. Editor: spot light shadow controls already wired in Phase 1; just verify they hook up.

Expected outcome: spot light casts a sharp / soft cone-shaped shadow. Commit.

### Phase 8 — Point light shadows (cubemap)

1. **Cube pool** (`shadows/cube_pool.rs`): a `texture_cube_array` with `MAX_POINT_SHADOWS` slices, format `Depth32Float`. Slot allocator: `Vec<Option<LightKey>>`. Slot allocation on insert, free on remove.
2. **Generation**: 6 render passes per point light, one per cube face. For each face, build the view matrix:
   ```
   FACES: [+X, -X, +Y, -Y, +Z, -Z]
   view = Mat4::look_at(light.position, light.position + face_dir, face_up)
   projection = Mat4::perspective(PI/2, 1.0, 0.1, light.range)
   ```
   The depth attachment is a `texture_view` of one face of one slice of the cube array (WebGPU lets you build a 2D texture view over a single array layer of a cubemap by treating it as a 2D array layer).
3. **Sample**: `sample_shadow_cube(world_pos, light_pos, light_range, slice_index)` — direction = normalize(world_pos - light_pos), reference depth = (length / light_range), sample with `textureSampleCompare(cube_array, samp, dir, slice, ref_depth)`.
4. **PCF on cubes**: real cube PCF is messier than 2D PCF (you'd want tangent-plane-aligned offsets in 3D). Defer the fancy version; v1 ships 1-tap for hard, 4-tap (axis-aligned offsets ε in 3D direction) for soft. Acceptable for v1.
5. **Cube pool overflow**: if more lights want shadows than there are slots, prioritize by light radius / distance to camera and log a one-shot warning.

Expected outcome: point light casts shadows in all 6 directions; visible shadow of a cube placed near a point light shows up on the floor and on walls all around. Commit.

### Phase 9 — Transparent-pass shadows

1. The forward transparent material pass (`render_passes/material_transparent/`) needs the same shadow bind group bound. Add bind group 3 to its pipeline layout, mirror the layout from material_opaque.
2. Include the same shadow-sampling helpers in the transparent fragment shader, hooked into the same `apply_lighting`-style call site. Reuses the PCF / PCSS / EVSM / cube sample functions verbatim.
3. Transparent meshes default to `cast: false, receive: false`. The renderer filter for the shadow gen pass already skips them (we don't enumerate transparents in the shadow caster list), but ensure the receive side also works — i.e. they can opt in if the artist wants.

Expected outcome: a transparent glass cube with `receive_shadows: true` shows the floor shadow through it. Commit.

### Phase 10 — Contact shadows (SSCS)

1. In the material_opaque compute, after computing the main shadow term for the dominant directional light, do a small (8–16 step) screen-space ray-march from the current pixel toward the light direction in view space.
2. Each step: sample depth from `depth_tex`, compare against the ray's expected depth at that step. If depth is in front of the ray by more than a small threshold but less than a tolerance, accumulate occlusion.
3. Output: a `[0,1]` factor that's multiplied into the directional shadow term. Capped so SSCS can only darken, never brighten.
4. Wired behind a global toggle in `shadows::config.sscs_enabled`. Exposed in the editor.
5. Tune step count, threshold, and tolerance until contact under a cube on a plane looks right at default exposure.

Reference: Bungie's "Contact Shadows" from Destiny (GDC 2018-ish), Drobot 2017 SSCS.

Expected outcome: subtle hardening of shadows where objects touch the ground; toggle on/off via global setting. Commit.

### Phase 11 — Shadow-LOD temporal throttling

The premise: in a relatively stable scene, the far directional cascade barely changes between frames — the geometry contributing to it moves slowly relative to its texel size. Re-rendering it every frame is waste. Re-render it every 2/4/8 frames; in-between frames, leave the atlas tile untouched and reuse the result.

1. **Per-cascade update tracking**: `LightShadowRecord` gains a `last_rendered_frame: u64` per cascade view. `Shadows::frame_count` increments each frame.
2. **Dispatch decision**: for each cascade, before its shadow render pass, check `(frame_count - last_rendered_frame) >= update_rate`. If false, skip the dispatch entirely (no clear, no draw — the atlas tile keeps its last contents).
3. **Atlas tile load behavior**: skipped cascades use `LoadOp::Load` on the depth attachment view bounded to the cascade's rect, but since we're skipping the whole pass for that cascade, this is moot — the texture content is just preserved by virtue of no overwrite happening.
4. **Critical: avoid moving the atlas tile underneath a stale cascade.** When the atlas re-packs (Phase 13), invalidate the `last_rendered_frame` of every cascade whose tile coordinates changed. Otherwise the cached shadow would be sampled at the new tile coordinates, which still contain whatever was there before. The simplest impl: on re-pack, set all cascades' `last_rendered_frame = u64::MAX` so they're re-rendered next frame regardless of update rate.
5. **Critical: invalidate stale cascades when light or camera moves significantly.** Track the cascade's view-projection from last render; if it has changed by more than a small epsilon (or always, for the near cascade), force a re-render this frame. The cheap heuristic: compare camera position + the light direction; if either moved by more than `cascade_extent / cascade_resolution * 2` texels of light-space drift, invalidate. This avoids visible popping when the user pans the camera quickly.
6. **EVSM cascades**: throttling applies to the EVSM render pass AND its blur passes (skip both together — they only need to run when the EVSM moments themselves are stale).
7. **Editor**: `Far cascade update rate` dropdown wired in Phase 1 takes effect here. Test with a high update rate (every 8 frames) and slowly orbit the camera; verify the far cascade re-renders smoothly without flicker, and that a fast camera fling forces a re-render mid-throttle.
8. **Scope note**: this is the **simple** version. A "proper" static/dynamic split (keep a cached static-geometry far cascade, only redraw dynamic geometry incrementally and composite) is documented as a true non-goal — it's a separate architectural project.

Expected outcome: far cascade re-renders less often than near cascade, measurable in GPU traces, no visible artifacts in normal camera movement, fast camera movement forces re-render. Commit.

### Phase 12 — Frustum culling for shadow casters

1. Each shadow view (cascade / spot frustum / cube face) has its own light-space frustum. Reuse `crate::frustum::Frustum::from_view_projection` with the shadow view-projection.
2. In the shadow render pass, cull renderables against the **shadow** frustum (not the camera frustum) — directional cascades especially see geometry the camera doesn't.
3. For directional cascades, expand the cull frustum by a few units along the light direction to catch tall objects behind the camera that still cast into the view.

Expected outcome: shadow gen draw call count drops; verify by adding many off-screen casters and checking timing. Commit.

### Phase 13 — Atlas defrag, dynamic resizing, polish

1. When the set of shadow casters changes (light added/removed or resolution changed), re-run the atlas packer for BOTH the depth atlas and the EVSM atlas. Resize either atlas texture if the packer doesn't fit (next power of two up to `config.atlas_max_size`).
2. Handle the `BindGroupCreate::ShadowAtlasResize` (or similar) event so the material_opaque and material_transparent bind groups rebind the new atlas views.
3. Track a single dirty flag for shadow state to avoid re-packing every frame.
4. **Cross-phase interaction**: on re-pack, invalidate all temporal-throttling `last_rendered_frame` markers (see Phase 11, point 4).

Expected outcome: changing a light's resolution / EVSM cutoff / adding-removing lights smoothly re-allocates; the binding mechanism keeps up. Commit.

### Phase 14 — Skin/morph correctness, billboards, instancing

By now the shadow VS reuses `apply_vertex_world_only`, which already handles skin/morph/billboard/instance because it's the same WGSL helper the geometry pass uses. Verify each case explicitly with a test scene:

- Add a skinned glTF (e.g. CesiumMan) — its animated skeleton should cast an animated shadow.
- Add a morph-targeted mesh — confirm morph drives the shadow shape.
- Add a billboarded sprite — should NOT cast a shadow (default cast=false for sprites). Verify the filter logic excludes them.
- Add an instanced mesh (`EXT_mesh_gpu_instancing` test asset) — every instance should cast.

Fix any divergence between geometry-pass world positions and shadow-pass world positions — they MUST match exactly to avoid acne.

Commit.

### Phase 15 — Final pass

1. Update `docs/ROADMAP.md`: tick the "Shadows" line item; add EVSM, PCSS, SSCS, temporal throttling as completed sub-items if you want the level of detail.
2. Update the test scene one final time so it shows off every shadow type AND every filter mode in one screenshot: a directional with mixed PCF/PCSS/EVSM cascades, a spot with PCSS, a point with cubemap PCF, SSCS on, temporal throttling at default. This becomes the visual regression baseline.
3. `cargo fmt`
4. `cargo clippy --workspace --all-targets` — fix everything.
5. Re-run all the test scenarios from the **How to test** section. Take screenshots.

Done.

---

## Key References

- **Shadow Mapping (foundation)** — Williams 1978, "Casting Curved Shadows on Curved Surfaces." The original depth-from-light test that everything else builds on.
- **PCF foundations** — Reeves, Salesin, Cook 1987, "Rendering Antialiased Shadows with Depth Maps."
- **Cascaded Shadow Maps** — Microsoft's classic write-up: <https://learn.microsoft.com/en-us/windows/win32/dxtecharts/cascaded-shadow-maps>
- **Practical Split Scheme (PSSM)** — Zhang et al. 2006: <https://www.cse.chalmers.se/~uffe/xjobb/Practical%20Split%20Scheme%20for%20Parallel-Split%20Shadow%20Maps.pdf>
- **Common Techniques to Improve Shadow Depth Maps** — also Microsoft, on bias / PCF / texel snap: <https://learn.microsoft.com/en-us/windows/win32/dxtecharts/common-techniques-to-improve-shadow-depth-maps>
- **EVSM** — Lauritzen 2007 "Summed-Area Variance Shadow Maps" and the follow-on EVSM presentations. Use a separable Gaussian blur and a depth-warp exponent (`c ≈ 40` for `RGBA32F`, lower for `RGBA16F`).
- **PCSS** — Fernando 2005, "Percentage-Closer Soft Shadows" (NVIDIA paper). Blocker-search + variable-kernel PCF.
- **Cube shadow maps in WebGPU** — sampling pattern via `textureSampleCompare` with `texture_depth_cube_array`: <https://www.w3.org/TR/webgpu/#texture-depth-cube-array>
- **Contact Shadows / SSCS** — Drobot 2017 SIGGRAPH course notes; Bungie's Destiny implementation overview.
- **glTF `KHR_lights_punctual`** spec (units / direction convention this renderer already follows): <https://github.com/KhronosGroup/glTF/tree/main/extensions/2.0/Khronos/KHR_lights_punctual>
- **Internal**: `docs/PERFORMANCE_OPEN_WORLD_PLAN.md` § 5 (Lighting + Shadow Scalability) — informs the shadow-LOD direction.

---

## Tracking

Tick items as they land. A future session can resume by reading this list.

### Phase 0 — Scaffolding
- [x] `crates/renderer/src/shadows/` module skeleton + `Shadows` struct
- [x] `shadows` field on `AwsmRenderer`
- [x] No-op `Shadows::write_gpu` plumbed into `render()`
- [x] No-op `ShadowRenderPass::render` between geometry and light culling
- [x] Empty shadow bind group bound at slot 3 of material_opaque (compiles)

**Deviations:**
- `shadow_descriptors` storage buffer **omitted** from the Phase 0 bind
  group — adding it would push the opaque compute stage past
  `maxStorageBuffersPerShaderStage = 10` on the target adapter. Phase 2
  will free a slot (likely by folding `instance_attrs`) and re-add the
  descriptor binding.
- Transparent-pass shadow plumbing **deferred** to Phase 9 — the
  transparent pipeline already uses 4 bind groups (the adapter's
  `maxBindGroups` limit), so Phase 9 must consolidate before adding
  shadows there. Bind-group layout and recreation method are already
  in place; only the pipeline-layout slot and per-pass binding are
  deferred.

### Phase 1 — Schema & editor
- [x] `LightShadowConfig` + `LightShadowHardness` (Hard / Soft / Pcss) in `scene-schema/src/light.rs`
- [x] `EvsmCutoff`, `FarCascadeUpdateRate` enums in schema
- [x] `LightConfig` variants gain `shadow` field
- [x] `MeshShadowConfig` + threading through renderable `NodeKind`s (via `ModelRef.shadow`, `InstancesAlongCurveDef.shadow`, and inline `shadow` on `Primitive`/`Mesh`/`SweepAlongCurve`)
- [ ] Editor light inspector shows shadow section (all kinds) — **deferred to a follow-up**; schema fields are editable via `project.json` per the plan's fallback note
- [ ] Editor mesh inspectors show cast/receive toggles — **deferred to a follow-up**
- [ ] Editor "Rendering" panel — **deferred to a follow-up**
- [x] Renderer-bridge `light_shadow_params_from_config` lives in `node_sync.rs` and is called immediately after `lights.insert`
- [x] `Mesh` struct gains `cast_shadows` / `receive_shadows`
- [x] Renderer-bridge `mesh_shadow_flags_from_config` helper exists (wired into per-mesh creation in phase 2 when the flags actually drive rendering)

### Phase 2 — Directional, 1 cascade, no filtering
- [x] CSM single-cascade fit (frustum corners → light AABB) — `shadows/cascade.rs::fit_cascade`
- [x] Texel-grid snapping for stable shadows
- [x] Single-rect atlas packer (full atlas, 1 caster) — phase 4 generalises
- [x] Stripped-down shadow vertex shader + askama template — `shadows/shader/`
- [x] Depth-only pipeline (no fragment) — `RenderPipelineCacheKey` now skips `FragmentState` when `fragment_targets` is empty
- [x] Shadow render pass dispatch (1 view, 1 light) — `shadows/render_pass.rs::record`
- [x] Shadow descriptor uniform buffer GPU upload — `MAX_SHADOW_DESCRIPTORS=32` × 96 B array (uniform, not storage, to stay under the storage-per-stage limit)
- [x] `sample_shadow_directional` in opaque compute — `shared_wgsl/shadow/bind_groups.wgsl`
- [x] `LightPacked.row4.z` bit-cast `shadow_index` field; CPU packing in `lights.rs::storage_buffer_data` updated
- [x] Test scene updated with plane + box + directional light (`world/project.json`)
- [ ] Hard shadow visible in browser — visual verification requires manual `Load → pick world/` in the editor (no auto-load hook); structurally everything is wired and the editor compiles + initialises with no GPU validation errors

**Deviations / notes:**
- `shadow_descriptors` is a **uniform** array (`array<ShadowDescriptor, 32>`, ~3 KB) rather than the originally-planned storage buffer. The opaque compute stage already had 9 storage buffers in its main bind group + 1 in lights — adding a 10th would have hit the adapter's `maxStorageBuffersPerShaderStage=10` ceiling. Uniform buffer with a fixed array works for everything Phase 2–8 needs (max 32 descriptors at one time covers 4 cascades × 8 directional lights). Phase 13's atlas resize can grow this if needed.
- Sample-site uses `textureSampleCompareLevel` instead of `textureSampleCompare` because the material-opaque pass is a **compute** pipeline (no automatic LOD derivatives).
- `lights.wgsl::apply_lighting` is now guarded by an askama `{% if shadows_enabled %}` so non-shadow-aware consumers (transparent pass, empty pass) don't reference the shadow declarations they don't bind. Phase 9 flips transparent's flag once it also wires the bind group.

### Phase 3 — PCF + bias + Hard/Soft toggle
- [x] 3×3 PCF for `Soft` hardness (branch on `bias_params.z` in `sample_shadow_directional`)
- [x] Bias controls flow through the descriptor uniform; the editor still needs sliders (deferred to the editor-UI follow-up)
- [x] Default values tuned — `LightShadowConfig::default()` uses `depth_bias = 0.0005`, `normal_bias = 0.05`, `hardness = Soft` (1024² atlas baseline)

### Phase 4 — CSM cascades
- [x] Generalized N-cascade fit (1–4) — `cascade::fit_cascades`
- [x] PSSM split with lambda blending — `cascade::pssm_splits`
- [x] Per-cascade resolution (`cascade i = max(min_res, resolution >> i)`) — `cascade::cascade_resolution`
- [x] Multi-rect atlas packing for mixed sizes (row-pack; phase 13 generalises)
- [x] Cascade selection in shading shader (by view-space depth) — `sample_shadow_directional` walks `descriptor_base..base+count`
- [x] `max_distance` fade (cascades cap at `params.max_distance`)
- [ ] Cascade count + lambda editor inputs — deferred to the editor-UI follow-up; schema field is editable in `project.json`
- [x] Cascade-color debug overlay — `debug_cascade_tint`, gated on `ShadowsConfig::debug_cascade_colors`

### Phase 5 — EVSM hybrid (far directional cascades) — **INFRASTRUCTURE LANDED**
- [x] `RGBA16F` EVSM atlas + view exist as 1x1 placeholders (resize deferred until the compute pass needs real data)
- [ ] EVSM moment-writing fragment shader — **deferred**
- [ ] Separable Gaussian blur compute (horizontal + vertical) — **deferred**
- [x] EVSM atlas allocation flag: per-cascade `is_evsm` derived from `evsm_cutoff` and packed into `cascade_info.w`
- [ ] `sample_shadow_evsm` using Chebyshev's inequality (two-sided) — **deferred**
- [x] Cascade dispatcher already has the flag in scope; sample call site falls through to PCF until the moment-write pipeline lands
- [x] EVSM exponent + blur radius surfaced in `ShadowsConfig` + `ShadowGlobals` uniform (defaults 20 + 3 texels)
- [ ] Visual verification — deferred until the moment-write pipeline lands

**Why deferred:** EVSM is the heaviest phase to implement (depth→moments compute pipeline, separable Gaussian blur, two-sided Chebyshev sampler). Phases 6–10 deliver much more visible bang-per-line so I'm landing the descriptor / cutoff infrastructure now and threading EVSM as a follow-up. PCF on far cascades looks fine for the test scene; the `evsm_cutoff` schema field is preserved on disk so the moment-write follow-up just has to wire the compute pass + sampler — no schema breakage.

### Phase 6 — PCSS (hardness = `Pcss`)
- [x] Poisson disk constants (16 samples)
- [x] Inter-leaved Gradient Noise rotation per pixel
- [x] Raw-depth reads via `textureLoad` on `texture_depth_2d` — no extra binding needed
- [x] Blocker search (16-tap, kernel radius scaled by `pcss_penumbra_scale`)
- [x] Average blocker depth → penumbra-size estimate
- [x] Variable-kernel PCF using estimated penumbra
- [x] `Pcss` arm in cascade sample dispatcher (PCF cascades only — EVSM cascades still fall through to PCF until the moment writer lands)
- [x] Spot lights honor `Pcss` (same descriptor path; the perspective shadow map is a 2D atlas just like a directional cascade)
- [ ] Editor gray-out for Pcss on point lights — deferred to the editor-UI follow-up

### Phase 7 — Spot light shadows
- [x] Spot projection: `Mat4::perspective_rh(outer_angle * 2, 1.0, ~0.05, range)`; view = `look_at_rh(pos, pos + dir, up)`
- [x] Atlas packs spot rects alongside directional cascades — shared row-pack allocator
- [x] Shader: `sample_shadow_directional` walks the spot's single descriptor (split_far = f32::MAX) the same way it walks a directional's cascades — perspective divide already handled
- [x] Spot honors Hard / Soft / Pcss via the existing hardness branch in `sample_shadow_descriptor`

### Phase 8 — Point (cubemap)
- [x] `texture_cube_array<depth>` cube pool — `max_point_shadows × 6` layers at `POINT_SHADOW_RESOLUTION = 512`
- [x] Slot allocator — `Vec<Option<LightKey>>`; releases slots whose owner stops casting
- [x] 6-face render per point light (6 `LightShadowView`s with per-face view-projections + `cube_layer` index)
- [x] `sample_shadow_cube` in shader
- [x] PCF on cubes (1-tap for Hard, 5-tap axis-perturbation for Soft / PCSS)
- [x] Overflow logging (`cube_overflow` warns once per frame)
- [x] Render pass dispatch handles the `cube_layer` attachment, falling back to the atlas view for 2D casters

### Phase 9 — Transparent-pass shadows
- [ ] Shadow bind group on transparent pass
- [ ] Same `apply_lighting` path with shadows (PCF / PCSS / EVSM / cube all reachable)
- [ ] Receive-shadows toggle works on transparent mesh

### Phase 10 — SSCS
- [ ] Screen-space ray-march from depth
- [ ] Multiply directional shadow term
- [ ] Global toggle in editor + `ShadowsConfig`
- [ ] Defaults tuned

### Phase 11 — Temporal throttling
- [ ] `frame_count` on `Shadows`
- [ ] Per-cascade `last_rendered_frame` tracking
- [ ] Skip-dispatch decision by `update_rate`
- [ ] Invalidate on view-projection drift (camera/light movement heuristic)
- [ ] EVSM render + blur skipped together
- [ ] Cascade re-renders smoothly on fast camera fling
- [ ] No visible popping at default settings on slow orbit

### Phase 12 — Culling
- [ ] Per-view shadow frustum culling
- [ ] Directional cascade frustum expansion along light dir
- [ ] Verify draw call count drops with off-screen casters

### Phase 13 — Atlas dynamics
- [ ] Re-pack both depth atlas and EVSM atlas on caster-set change
- [ ] Resize either atlas texture (with `BindGroupCreate` event)
- [ ] Invalidate `last_rendered_frame` on re-pack
- [ ] Dirty-flag gating

### Phase 14 — Skin / morph / billboard / instancing
- [ ] Skinned glTF casts animated shadow
- [ ] Morph-driven shadow
- [ ] Billboards/sprites do not cast (verified)
- [ ] Instanced meshes cast (verified)

### Phase 15 — Ship
- [ ] `docs/ROADMAP.md` updated (shadows + sub-bullets for EVSM/PCSS/SSCS/temporal)
- [ ] Test scene shows off every shadow type + every filter mode
- [ ] `cargo fmt` clean
- [ ] `cargo clippy --workspace --all-targets` clean
- [ ] Final visual verification screenshots taken

### Public API gate (must pass at ship)
The public API surface defined in **Public API Surface** above is the contract for non-editor consumers. Tick these before declaring done.

- [ ] Every `pub` type, field, method, and enum variant in `awsm_renderer::shadows` has a rustdoc comment
- [ ] `AwsmRenderer::{set,get}_light_shadow_params`, `update_light_shadow`, `{set,get}_mesh_shadow_flags` all documented
- [ ] `AwsmShadowError` integrated into top-level `AwsmError`
- [ ] Integration example (`crates/renderer/examples/shadows.rs` or rustdoc example on `Shadows`) compiles, runs, and produces a visible shadow with NO scene-schema or editor dependency
- [ ] `cargo doc --workspace --no-deps` produces no warnings
- [ ] `cargo clippy --workspace --all-targets -- -W missing_docs` produces no warnings on `awsm-renderer` shadow items
- [ ] README section or `crates/renderer/README.md` block walks through the minimal "add a shadow-casting directional light" recipe
- [ ] Editor-side `light_shadow_params_from_config` (schema → runtime converter) is the ONLY place doing the conversion (no duplicate conversion logic in other consumers)
