# awsm-renderer

WebGPU visibility-buffer deferred renderer for the AWSM engine. Library
crate — applications drive it through the `AwsmRenderer` struct and its
subsystems (`lights`, `meshes`, `materials`, `shadows`, etc).

## Dynamic materials quick start

Register your own WGSL fragment at runtime, get back a
`MaterialShaderId`, and reference it from a `Material::Custom`. The
renderer compiles the per-pass pipelines asynchronously through its
[pipeline-readiness scheduler]; meshes that reference a not-yet-ready
material are silently skipped for the frames in which the compile is
still in flight, then "pop in" on the frame after `Ready`. No
synchronous wait is required for steady-state use; tests / cold-boot
flows can opt into [`wait_for_pipelines_ready`] to drain the scheduler.

```rust
use awsm_renderer::{
    AwsmRenderer,
    dynamic_materials::registration::MaterialRegistration,
    materials::Material,
};
use awsm_scene_schema::dynamic_material::MaterialDefinition;
use awsm_scene_schema::material::MaterialAlphaMode;

// 1. Build the registration. `definition` carries the public param
//    surface (uniforms / textures / buffers) per the schema crate;
//    `wgsl_fragment` is the author's shading-stage body, with the
//    `input.material.<field>` accessors generated from `definition`.
let definition = MaterialDefinition {
    name: "scanline".into(),
    version: 1,
    alpha_mode: MaterialAlphaMode::Opaque,
    double_sided: false,
    uniforms: vec![/* … see docs/dynamic-materials/contract-opaque.md */],
    textures: vec![],
    buffers: vec![],
};
let wgsl_fragment = std::fs::read_to_string("assets/materials/scanline/shader.wgsl")?;
let registration = MaterialRegistration::new(definition, wgsl_fragment);

// 2. Register. Returns immediately — compile is queued and will
//    transition Pending → Ready on a later render-frame preamble.
let shader_id = renderer.register_material(registration)?;

// 3. Build a Material::Custom that points at the registered shader.
//    `per_instance` carries the author-defined uniforms (color tints,
//    floats, etc.) keyed by the `definition.uniforms[*].name` declared
//    above.
let material = Material::Custom(/* see crates/renderer/examples/dynamic_material.rs */);
let material_key = renderer.add_material(material)?;

// 4. Add a mesh referencing that material. The mesh enters the
//    scene immediately; the first 1–N frames render without it
//    (until the pipeline-readiness scheduler resolves), then it
//    appears on the frame after Ready.
let mesh_key = renderer.add_mesh(/* … */)?;

// 5. Render normally. No `prewarm` await needed.
renderer.render(None)?;

// Optional: for cold-boot / test flows where you want the scene to
// be paint-complete before the first render, drain the scheduler:
renderer.wait_for_pipelines_ready().await?;
```

The full author-facing WGSL contract — what symbols are in scope, what
`OpaqueShadingInput` / `OpaqueShadingOutput` look like, how to read
`input.material.<field>`, how to sample the texture pool, and how to
use buffer slots via the extras pool — lives in
[`docs/dynamic-materials/contract-opaque.md`] and
[`docs/dynamic-materials/contract-transparent.md`]. The
"promote-to-first-party" walkthrough (taking a stable dynamic material
and folding it into the renderer's built-in shader set) is in
[`docs/dynamic-materials/promotion.md`].

A fully worked end-to-end example, including buffer slots and a
per-instance override, is at
[`crates/renderer/examples/dynamic_material.rs`].

[pipeline-readiness scheduler]: src/pipeline_scheduler/mod.rs
[`wait_for_pipelines_ready`]: src/pipeline_scheduler/mod.rs
[`docs/dynamic-materials/contract-opaque.md`]: ../../docs/dynamic-materials/contract-opaque.md
[`docs/dynamic-materials/contract-transparent.md`]: ../../docs/dynamic-materials/contract-transparent.md
[`docs/dynamic-materials/promotion.md`]: ../../docs/dynamic-materials/promotion.md
[`crates/renderer/examples/dynamic_material.rs`]: examples/dynamic_material.rs

## Shadows quick start

Shadows are off-by-default per light. To turn them on, register
`LightShadowParams` against an inserted `LightKey`. The descriptor
buffer + cascade fit + sampling all light up automatically.

```rust
use awsm_renderer::{
    lights::Light,
    shadows::{LightShadowParams, LightShadowHardness, MeshShadowFlags},
};

// 1. Insert a directional light. Pass `None` for shadow params (no
//    shadow); pass `Some(LightShadowParams { cast: true, .. })` to
//    enable shadows in the same call.
let sun = renderer.insert_light(
    Light::Directional {
        color: [1.0, 0.95, 0.9],
        intensity: 3.0,
        direction: [0.3, -1.0, 0.3],
    },
    None,
)?;

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
