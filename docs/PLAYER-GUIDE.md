# Building a Player on awsm-renderer

This guide is for building a **player** — a game or runtime that consumes
`awsm-renderer` as a library, owns its own canvas and render loop, and loads
content the editor produced. It is written against a real, working reference
harness (a standalone wasm app that path-depends on these crates and renders
glTF models, our own scene-bundle format, skinned animation, and instantiated
prefabs). Every snippet below is adapted from that harness.

`awsm-renderer` targets the browser: it talks to WebGPU through `web-sys`, so a
player is a **`wasm32-unknown-unknown`** app, not a native binary. There is no
native rendering path.

---

## 1. Project setup

### Crate dependencies

A player typically pulls four crates:

| Crate | What it gives you |
|-------|-------------------|
| `awsm-renderer` | the renderer, camera, render loop entry points |
| `awsm-renderer-gltf` | load raw/foreign glTF (`.glb`) into the renderer |
| `awsm-scene` | our scene/bundle types (`Scene`, nodes, assets) + `scene_from_toml` |
| `awsm-scene-loader` | load a `Scene` into the renderer (`populate_awsm_scene`) |

If you depend on them by path (developing against a checkout), pin the **shared**
types you touch directly — `glam` and `wasm-bindgen` — to the *same* versions the
renderer workspace uses, or the types won't line up across the crate boundary:

```toml
[dependencies]
awsm-renderer       = { path = "../renderer/packages/crates/renderer" }
awsm-renderer-gltf  = { path = "../renderer/packages/crates/renderer-gltf" }
awsm-scene          = { path = "../renderer/packages/crates/scene" }
awsm-scene-loader   = { path = "../renderer/packages/crates/scene-loader" }

wasm-bindgen         = "0.2.118"   # match the workspace
wasm-bindgen-futures = "0.4.68"
js-sys               = "0.3.95"
glam                 = "0.32.1"    # match the workspace
anyhow               = "1"
tracing              = "0.1"
gloo-render          = "0.2.0"     # requestAnimationFrame wrapper
gloo-net             = { version = "0.6.0", features = ["http"] }  # fetch assets

[dependencies.web-sys]
version  = "0.3.95"
features = ["Window", "Document", "Element", "HtmlElement",
            "HtmlCanvasElement", "Navigator", "Gpu", "console"]
```

You only need the `web-sys` features your *own* code uses (window, canvas, gpu
handle). The renderer pulls the WebGPU `web-sys` surface itself.

### Required `.cargo/config.toml`

WebGPU lives behind `web-sys`'s unstable cfg, and `getrandom` needs a wasm
backend. **A player must set both flags** (mirror the renderer workspace):

```toml
[target.wasm32-unknown-unknown]
rustflags = ["--cfg=web_sys_unstable_apis", "--cfg=getrandom_backend=\"wasm_js\""]

[target.'cfg(not(target_arch = "wasm32"))']
rustflags = ["--cfg=web_sys_unstable_apis"]

[build]
target = "wasm32-unknown-unknown"
```

Without `web_sys_unstable_apis` the WebGPU types don't exist; without the
`getrandom` backend the build fails to link.

### Trunk

The reference harness uses [trunk](https://trunkrs.dev). A minimal `index.html`:

```html
<!DOCTYPE html>
<html>
  <head><meta charset="utf-8" /><title>player</title>
    <style>html,body{margin:0;height:100%;background:#222} canvas{display:block}</style>
  </head>
  <body>
    <link data-trunk rel="rust" data-wasm-opt="0" />
  </body>
</html>
```

Run with `trunk serve --port 9090`. (WebGPU requires a secure context;
`localhost` counts.)

---

## 2. Bootstrapping the renderer

Create a canvas, grab the WebGPU handle, and build the renderer through its two
builders:

```rust
use awsm_renderer::{
    core::{
        command::color::Color,
        configuration::{CanvasAlphaMode, CanvasConfiguration, CanvasToneMappingMode},
        renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits},
    },
    profile::RendererProfile,
    AwsmRendererBuilder,
};

let gpu = window.navigator().gpu();
let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas.clone())
    .with_configuration(
        CanvasConfiguration::default()
            .with_alpha_mode(CanvasAlphaMode::Opaque)
            .with_tone_mapping(CanvasToneMappingMode::Standard),
    )
    .with_device_request_limits(DeviceRequestLimits::max_all());

let mut renderer = AwsmRendererBuilder::new(gpu_builder)
    .with_profile(RendererProfile::Desktop)
    .with_clear_color(Color::MID_GREY)
    .build()
    .await?;
```

`build()` is async (it requests a GPU device). Pick `RendererProfile::Desktop`
or another profile to set quality/feature defaults.

Wire up `console_error_panic_hook::set_once()` and a tracing subscriber
(`tracing_wasm`) first so panics and logs reach the dev console.

---

## 3. The render loop

The renderer does not own a loop — you drive it. Each frame, in order:

```rust
renderer.update_animations(dt_ms)?;      // advance animation clips (ms delta)
renderer.update_camera(camera_matrices)?; // your camera (see §4)
renderer.update_transforms();             // flush the transform graph
renderer.render(None)?;                    // draw (Some(&RenderHooks) to customize)
```

Drive it from `requestAnimationFrame`. With `gloo-render`, **keep the returned
`AnimationFrame` handle alive** or the loop stops after one frame:

```rust
fn schedule_frame(/* renderer, last_ts, cam, cell: Rc<RefCell<Option<AnimationFrame>>> */) {
    let handle = gloo_render::request_animation_frame(move |ts| {
        let dt = /* ts - last_ts, clamped >= 0 */;
        let mut r = renderer.borrow_mut();
        r.update_animations(dt).ok();
        r.update_camera(cam.clone()).ok();
        r.update_transforms();
        r.render(None).ok();
        schedule_frame(/* re-arm with cloned handles */);
    });
    *cell.borrow_mut() = Some(handle);
}
```

A `Rc<RefCell<AwsmRenderer>>` is the simplest way to share the renderer between
your setup code and the RAF closure.

---

## 4. Camera

The renderer is **matrices-only**: it has no built-in camera or controller. You
supply a `CameraMatrices` every frame. For the common right-handed perspective
case, use the constructor (no need to hand-roll glam):

```rust
use awsm_renderer::camera::CameraMatrices;
use glam::Vec3;

let cam = CameraMatrices::perspective(
    Vec3::new(2.0, 3.0, 12.0), // eye
    Vec3::ZERO,                // target
    Vec3::Y,                   // up
    45f32.to_radians(),        // fov_y
    width as f32 / height as f32,
    0.1, 100.0,                // near, far
);
renderer.update_camera(cam)?;
```

`CameraMatrices` is `{ view, projection, position_world, focus_distance,
aperture }`. `focus_distance`/`aperture` drive depth-of-field; the `perspective`
helper defaults focus to `target` at f/16 — set the fields directly if you want
manual control. Build your own `view`/`projection` (orthographic, custom rigs)
and fill the struct yourself when the helper doesn't fit.

---

## 5. Loading content

There are two ingestion paths. Both are **transactions**: stage content, then
commit once (compiling pipelines a single time).

### (a) Foreign glTF — `populate_gltf`

For loading a raw `.glb` directly (an asset you didn't author in the editor):

```rust
use awsm_renderer_gltf::{AwsmRendererGltfExt, GltfLoader};

let bytes = gloo_net::http::Request::get(url).send().await?.binary().await?;
let data  = GltfLoader::from_glb_bytes(&bytes).await?.into_data(None)?;
renderer.populate_gltf(data, None).await?;   // None = default scene
renderer.commit_load(|_| {}).await?;          // <-- THE finalize/compile point
```

`populate_gltf` (the `AwsmRendererGltfExt` ext-trait method) only *stages*;
`commit_load` is the one finalize point that uploads and compiles. You may
`populate_gltf` several glbs before a single `commit_load`.

### (b) Our scene format — `populate_awsm_scene`

This is the path a shipped player uses: load a `Scene` the editor exported.

```rust
use awsm_scene_loader::populate_awsm_scene;
use std::collections::HashMap;

let assets: HashMap<String, Vec<u8>> = /* bundle-relative path -> pre-fetched bytes */;
let loaded = populate_awsm_scene(&mut renderer, &scene, &assets, |_phase| {}).await?;
```

Two things to know:

- **`assets` is a `HashMap<String, Vec<u8>>`** of bundle-relative path →
  *already-fetched* bytes. The loader does not fetch for you — pre-fetch every
  asset the scene references (its glbs, textures) into the map first. A
  primitive-only scene needs an **empty map**.
- **`populate_awsm_scene` commits internally.** It runs the whole batched
  transaction (build materials → upload → compile) and commits — do **not** call
  `commit_load` after a plain scene load.

It returns a `LoadedScene { meshes, nodes, prefabs, .. }`; `nodes` maps each
`NodeId` to its renderer handles, and `prefabs` is your prefab table (§7).

#### The import-only / fast-path story

The editor *imports* arbitrary glTF and refactors it, in memory, into our own
format (geometry-only glbs + sidecar metadata + a `scene.toml`). A shipped game
ships **our format** and loads it through `populate_awsm_scene`, which is the
fast, predictable path. `populate_gltf` is for the foreign-glTF case (tools,
debugging, runtime-fetched third-party models), not the primary content path.

### Getting a `Scene`

Either parse a bundle the editor produced:

```rust
let scene: awsm_scene::Scene = awsm_scene::scene_from_toml(&toml_text)?;
```

…or build one programmatically (handy for tests/procedural content). The
`awsm_scene` types are re-exported at the crate root:

```rust
use awsm_scene::{
    AssetEntry, AssetId, AssetSource, EditorNode, LightConfig, LightKind, MeshRef,
    MeshShadowConfig, NodeId, NodeKind, PrimitiveShape, RuntimeMesh, Scene, Trs,
};

let mut scene = Scene { name: "demo".into(), ..Default::default() };

// A primitive box mesh asset...
let box_mesh = AssetId::new();
scene.assets.entries.insert(
    box_mesh,
    AssetEntry::new(AssetSource::Mesh(RuntimeMesh::Primitive(
        PrimitiveShape::Box { dims: [1.0, 1.0, 1.0] },
    ))),
);
// ...placed by a mesh node...
scene.nodes.push(EditorNode {
    id: NodeId::new(),
    name: "Box".into(),
    transform: Trs::default(),
    kind: NodeKind::Mesh {
        mesh: MeshRef(box_mesh),
        material: None,                       // None -> magenta unlit fallback (see Gotchas)
        shadow: MeshShadowConfig::default(),
    },
    locked: false, visible: true, prefab: false, children: vec![],
});
// ...lit by a directional light (its direction = the node rotation's -Z).
scene.nodes.push(EditorNode {
    id: NodeId::new(),
    name: "Sun".into(),
    transform: Trs::default(),
    kind: NodeKind::Light(LightConfig::default_for(LightKind::Directional)),
    locked: false, visible: true, prefab: false, children: vec![],
});
```

`PrimitiveShape::Primitive` meshes regenerate from their params at load, so they
carry no external asset bytes.

---

## 6. Animation

glTF clips (and our format's stored clips) **auto-play** once loaded. Drive them
by calling `update_animations(dt_ms)` with the per-frame millisecond delta in
your loop (§3). That's all skinned/morph/TRS animation needs — the reference
harness's Fox skins and animates with nothing more than this call.

---

## 7. Prefabs

A node marked `prefab: true` is a **prefab root**. At load, the scene-loader
captures that subtree as a *hidden template* in `LoadedScene.prefabs`
(`HashMap<NodeId, PrefabTemplate>`) instead of placing it — your game spawns
copies on demand:

```rust
let mut instances = Vec::new();
for (id, template) in loaded.prefabs.iter() {
    for pos in spawn_points {
        let trs = Trs { translation: pos, ..Default::default() };
        match template.instantiate(&mut renderer, trs) {
            Ok(inst) => instances.push(inst),
            Err(e)   => tracing::error!("instantiate {id:?}: {e:?}"),
        }
    }
}
renderer.commit_load(|_| {}).await.ok();  // finalize instancing pipelines, etc.
// keep `instances` alive — see below
```

Notes:

- `instantiate(&mut renderer, world_trs)` anchors a fresh copy at `world_trs`
  (the root's authored local is replaced; descendants keep their locals).
  Duplicated meshes **share** the template's GPU buffers, so a copy is cheap.
- **Call `commit_load` once after a spawn batch** — instantiation can stage GPU
  work (e.g. instancing pipelines), and `commit_load` finalizes it.
- **Retain the returned `PrefabInstance`s.** They own the per-instance mesh and
  transform keys; teardown is *explicit* (`PrefabInstance::teardown`) — there is
  no `Drop`. A demo that never despawns can `std::mem::forget` them; a real game
  holds them and calls `teardown` when despawning.
- A prefab may contain an **`InstancesAlongCurve`** (a curve + a source mesh):
  each instantiated copy gets its own row of meshes placed along the curve. This
  works through `instantiate` — verified live in the reference harness.

---

## 8. Gotchas

- **`material: None` renders magenta.** That's the unlit fallback. To give a
  primitive a real built-in material, set
  `NodeKind::Mesh { material: Some(MaterialInstance { asset, inline, .. }), .. }`
  where `asset` resolves to a registered material and `inline: MaterialDef`
  carries the built-in PBR params (base color / metallic / roughness). glTF
  models bring their own materials, so they render lit/textured out of the box.
- **`populate_awsm_scene` commits internally; `populate_gltf` does not.** Call
  `commit_load` yourself only on the glTF path (and after a prefab spawn batch).
- **`assets` for `populate_awsm_scene` is pre-fetched bytes**, not a fetcher —
  load every referenced asset path into the map before calling.
- **Pin `glam` and `wasm-bindgen`** to the workspace versions, or types won't
  unify across the crate boundary.
- **Set both `.cargo/config.toml` cfgs** (`web_sys_unstable_apis`,
  `getrandom_backend="wasm_js"`) or the build won't compile/link.

### API changes made while writing this guide

Two small additive ergonomics fixes landed on the renderer so the player path is
clean:

- `06d0f64e` — `awsm-renderer-gltf` now re-exports `GltfLoader`, `GltfData`, and
  `populate_gltf` at the crate root (they previously lived only in submodules, so
  a consumer's imports didn't resolve).
- `c98eb8d4` — `CameraMatrices::perspective(eye, target, up, fov_y, aspect,
  near, far)` constructor, removing the glam boilerplate every consumer was
  hand-rolling.
