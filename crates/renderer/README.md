# awsm-renderer

WebGPU visibility-buffer deferred renderer for the AWSM engine. Library
crate — applications drive it through the `AwsmRenderer` struct and its
subsystems (`lights`, `meshes`, `materials`, `shadows`, etc).

## Shadows quick start

Shadows are off-by-default per light. To turn them on, register
`LightShadowParams` against an inserted `LightKey`. The descriptor
buffer + cascade fit + sampling all light up automatically.

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

// 2. Enable shadows on it. `cast: false` keeps the light but skips
//    its shadow pass.
renderer.set_light_shadow_params(sun, LightShadowParams {
    cast: true,
    hardness: LightShadowHardness::Soft,
    cascade_count: 4,
    resolution: 2048,
    ..LightShadowParams::default()
})?;

// 3. (Optional) Override per-mesh defaults. Opaque meshes
//    automatically cast + receive; transparent / sprite / particle
//    meshes default to neither.
renderer.set_mesh_shadow_flags(some_mesh_key, MeshShadowFlags {
    cast: false,
    receive: true,
})?;

// 4. Render. The render graph short-circuits shadow generation when
//    `renderer.shadows.any_active()` is `false`.
renderer.render(None)?;
```

### Filter modes

`LightShadowHardness` chooses the sample kernel:

- `Hard` — 1-tap `textureSampleCompareLevel`. Crispest, cheapest.
- `Soft` — fixed 3×3 PCF.
- `Pcss` — Percentage-Closer Soft Shadows (blocker search + variable-
  kernel PCF). Contact-hardening; reserve for hero lights.

### Cascaded directional shadows

Directional lights use up to 4 cascades, split via PSSM with a tunable
`cascade_split_lambda`. Each cascade halves the previous cascade's
resolution (per-cascade shadow LOD). The far cascade can be temporally
throttled via `FarCascadeUpdateRate` to skip its render pass every
2/4/8 frames.

### Point + spot light shadows

`Light::Point` uses a `texture_depth_cube_array` slot pool; capacity
defaults to 8 cube slots (configurable via `ShadowsConfig::max_point_shadows`).
`Light::Spot` packs a perspective shadow map into the same 2D atlas
as directional cascades.

### Screen-space contact shadows

A short screen-space ray-march refines the directional shadow term to
catch micro-occlusion the cascade resolution misses (gaps under feet,
hair, etc). Global toggle: `ShadowsConfig::sscs_enabled`.

### Schema → runtime conversion

`scene_schema::LightShadowConfig` and `MeshShadowConfig` are the
on-disk shapes; the scene-editor's `renderer_bridge::node_sync` is the
only place in the codebase that converts them to the renderer's
runtime `LightShadowParams` / `MeshShadowFlags`. A non-editor consumer
(game runtime, model-tests frontend, standalone tool) skips the
schema crate entirely and constructs `LightShadowParams` directly.
