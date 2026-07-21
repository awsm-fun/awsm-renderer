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
| `awsm-renderer-scene` | our scene/bundle types (`Scene`, nodes, assets) + `scene_from_toml` |
| `awsm-renderer-scene-loader` | load a `Scene` into the renderer (`populate_awsm_scene`) |

If you depend on them by path (developing against a checkout), pin the **shared**
types you touch directly — `glam` and `wasm-bindgen` — to the *same* versions the
renderer workspace uses, or the types won't line up across the crate boundary:

```toml
[dependencies]
awsm-renderer       = { path = "../renderer/packages/crates/renderer" }
awsm-renderer-gltf  = { path = "../renderer/packages/crates/renderer-gltf" }
awsm-renderer-scene          = { path = "../renderer/packages/crates/scene" }
awsm-renderer-scene-loader   = { path = "../renderer/packages/crates/scene-loader" }

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
renderer.update_animations(dt_ms)?;       // advance animation clips (ms delta)
renderer.set_camera(view, camera_params)?; // your camera (see §4)
renderer.update_transforms();              // flush the transform graph
renderer.render(None)?;                     // draw (Some(&RenderHooks) to customize)
```

Drive it from `requestAnimationFrame`. With `gloo-render`, **keep the returned
`AnimationFrame` handle alive** or the loop stops after one frame:

```rust
fn schedule_frame(/* renderer, last_ts, cam, cell: Rc<RefCell<Option<AnimationFrame>>> */) {
    let handle = gloo_render::request_animation_frame(move |ts| {
        let dt = /* ts - last_ts, clamped >= 0 */;
        let mut r = renderer.borrow_mut();
        r.update_animations(dt).ok();
        r.set_camera(cam_view, cam_params).ok();
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

The renderer has no built-in camera controller — a camera is two halves you
supply each frame to the ONE entry point, `set_camera`:

- a **view matrix** (`glam::Mat4`) — yours to build however you like: a
  `Mat4::look_at_rh`, your own controller, physics, a replay, VR. Owning your
  view matrix is first-class, not a fallback.
- **`CameraParams`** — projection kind + clip planes (+ optional depth of
  field): `CameraParams::perspective(fov_y_rad, near, far)` or
  `CameraParams::orthographic(half_height, near, far)`.

```rust
use awsm_renderer::camera::CameraParams;
use glam::{Mat4, Vec3};

renderer.set_camera(
    Mat4::look_at_rh(
        Vec3::new(2.0, 3.0, 12.0), // eye
        Vec3::ZERO,                // target (the point being looked AT)
        Vec3::Y,                   // up
    ),
    CameraParams::perspective(45f32.to_radians(), 0.1, 100.0),
)?;
```

The renderer supplies everything a caller could get wrong: the **depth
convention** from its own features (reverse-Z by default — the projection and
the depth tests can never disagree), the **aspect ratio** from the live surface
(correct across resizes with no plumbing), and the camera **position**, derived
from the view matrix. Note there is no aspect argument anywhere — that is by
design.

`params.aperture` / `params.focus_distance` drive depth-of-field (defaults
f/5.6, 10 m). For a camera driven by a scene node's transform, build the view
with `camera::view_from_world(world)` (glTF convention: -Z forward, +Y up).
Read back what the renderer is using via `renderer.camera_matrices()` — the
`CameraMatrices` snapshot (view/projection/derived data) for pick rays and
screen-space math.

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
use awsm_renderer_scene_loader::populate_awsm_scene;
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
let scene: awsm_renderer_scene::Scene = awsm_renderer_scene::scene_from_toml(&toml_text)?;
```

…or build one programmatically (handy for tests/procedural content). The
`awsm_renderer_scene` types are re-exported at the crate root:

```rust
use awsm_renderer_scene::{
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
- `c98eb8d4` — a `CameraMatrices::perspective(..)` constructor (since
  superseded by the consolidated `set_camera(view, CameraParams)` API shown in
  §4).

---

## 9. Multithreading (opt-in)

Single-threaded is the default and stays first-class — the editor and
model-viewer run it daily. A **game** can opt into running the renderer in a
Web Worker (off the main thread) and, on top of that, into a **physics/sim
worker** that shares the renderer's sim state through shared linear memory.
The complete, runnable reference is **`examples/multithreaded/`** (`task
mt:dev` → <http://127.0.0.1:9090>); each milestone is a `?demo=` mode:

| `?demo=` | Shows |
|---|---|
| `smoke` | two workers share one `WebAssembly.Memory` (an `AtomicU32` crossing the boundary) |
| `arena` | the seqlock arena under a high-rate foreign writer (torn-read safety) |
| `render` | the renderer hosted in a worker over the shared **transform arena** |
| `motion` | a physics worker moving node transforms via shared memory (bounds/culling track the moved positions) |
| `crowd` | instanced transforms **and** attributes driven by the physics worker |
| `churn` | live spawn/despawn topology as a bind→ack→free transaction (slot reuse, invariant-checked) |
| `lights` | a physics worker animating a **light** via its bound transform (the lit spot sweeps a static surface) |
| `skin` | a physics worker flexing a real rigged glTF (CesiumMan) by driving its **skin joints** through the arena |
| `scene` | the **player path** — `load_scene_for_player` runs *in the worker* (the editor-export loader), then a runtime light is added + driven by physics through the arena |
| `remote` | the Layer 1 `RenderCommand`/`RenderEvent` protocol — DOM driver loads a real **glTF** (DamagedHelmet), streams a progress bar, picks, queries bounds, recolours the material |
| `input` | full input forwarding + a main-thread responsiveness meter (Long Tasks API) |

### 9.1 The threaded build profile

A normal wasm build has a private, non-shared linear memory. Three things,
together, produce a bundle that imports one **shared** memory every thread
attaches to (see `examples/multithreaded/rust-toolchain.toml`,
`taskfiles/examples/multithreaded.yml`, `Trunk.toml`):

1. **Nightly + `rust-src`** (`rust-toolchain.toml`), for `-Z build-std`.
2. **`RUSTFLAGS` + build-std** — keep the repo's existing cfgs and add:
   ```
   -C target-feature=+atomics,+bulk-memory,+mutable-globals
   -C link-arg=--shared-memory -C link-arg=--import-memory
   -C link-arg=--max-memory=<bytes>
   -C link-arg=--export=__heap_base
   -C link-arg=--export=__tls_base  -C link-arg=--export=__tls_size
   -C link-arg=--export=__tls_align -C link-arg=--export=__wasm_init_tls
   ```
   plus `-Z build-std=std,panic_abort` (recompiles `std` with atomics).
   The `--shared-memory --import-memory` pair is what makes the memory shared
   AND imported; the `__heap_base`/`__tls_*`/`__wasm_init_tls` exports are what
   `wasm-bindgen`'s thread transform needs to emit the `init(module, memory)`
   glue. Without them you get a private memory and `init` that ignores `memory`.
3. **COOP/COEP headers** on serve (`Trunk.toml [serve] headers`):
   `Cross-Origin-Opener-Policy: same-origin` +
   `Cross-Origin-Embedder-Policy: require-corp`. Without them
   `crossOriginIsolated` is `false` and `SharedArrayBuffer` is unavailable.
   **Production hosts must send the same two headers.**

> Disable `wasm-opt` for the threaded bundle (`data-wasm-opt="0"`) unless your
> `wasm-opt` is invoked with `--enable-threads --enable-bulk-memory`; otherwise
> it can strip the atomics/shared-memory.

### 9.2 Spawning workers on one shared memory

Build a worker from an inline blob and post it `wasm_bindgen::module()` +
`wasm_bindgen::memory()` (the shared module + shared memory); the worker calls
`init({ module_or_path: wasm_module, memory })` and then a role entry point.
See `examples/multithreaded/src/bootstrap.rs` — `spawn_shared_worker` /
`WORKER_BOOTSTRAP_JS`. The render worker can itself spawn the physics worker
the same way (the glue URL is stashed on the worker global). Hand the
`OffscreenCanvas` to the render worker with a transfer list
(`spawn_shared_worker_transfer`).

### 9.3 Layer 1 — the remote-renderer protocol

A main-thread driver that doesn't run game logic inside the worker controls it
over a typed channel (`examples/multithreaded/src/protocol.rs`):

- **`RenderCommand`** (main → worker): `LoadGltf { url }` (the worker fetches a
  **same-origin** `.glb` and runs the load transaction), `Load { models }`
  (geometry as Transferable `ArrayBuffer`s, referenced by index — never
  serialize `web_sys` GPU handles), `UpdateCamera`, `Bounds`,
  `SetMeshMaterial { emissive }`, `Screenshot`, `Pick { x, y }`.
- **`RenderEvent`** (worker → main): `Loading(LoadingStats)` per commit phase
  (drive a progress bar — the DOM paints each phase off-main, the
  responsiveness win), `Ready`, `PickResult`, `BoundsResult`, `MaterialChanged`,
  `ScreenshotBytes`, `Error`.

Serialize request→reply commands that take an async renderer borrow (`Pick`,
`LoadGltf`) one at a time — chain the next on the previous reply — so two
`&mut`-borrowing futures never overlap (the render loop already yields via
`try_borrow_mut`).

Two real constraints surfaced here:
- **Same-origin assets.** COEP `require-corp` blocks cross-origin fetches, and a
  worker resolves relative URLs against its `blob:` base — so pass an **absolute**
  same-origin URL (bundle the `.glb` with Trunk `copy-file`).
- **`Screenshot` is platform-bounded.** `OffscreenCanvas.convertToBlob` is
  rejected by Chrome (`NotReadableError`) on a WebGPU-configured canvas — the
  swapchain image isn't host-readable after present. A robust capture needs an
  in-renderer `copyTextureToBuffer`+`mapAsync` path; the command surfaces the
  real `Error` rather than faking one.

`SlotMap` handles (`MeshKey`/`TransformKey`/…) cross by value; `LoadingStats`
/`PickResult` are already `Copy`.

### 9.4 Layer 2 — shared-memory sim state

The bridge is **`awsm_renderer::buffer::shared_arena`**: a chunked,
stable-address arena with a per-slot `AtomicU32` seqlock + a coarse chunk
dirty bitmap. It is the **single** foreign-writable primitive — everything
else (materials, pipeline state, GPU handles) stays render-worker-private.
Topology (`allocate`/`free`/grow) is owner-only; foreign threads only write
values to already-bound slots.

**Node transforms.** Switch the renderer into shared mode once at setup:

```rust
renderer.transforms.enable_shared_arena();
// Per body, at spawn (one round-trip), hand the physics worker:
let binding   = renderer.transforms.arena_slot_binding(transform_key).unwrap();
let dirty_addr = renderer.transforms.arena_dirty_words_addr().unwrap();
```

The physics worker then writes a world `Mat4` (64 semantic bytes) every frame
with **zero `postMessage`**:

```rust
use awsm_renderer::buffer::shared_arena::foreign_write;
unsafe { foreign_write(binding, dirty_addr, &world_mat4_bytes); }
```

The render worker's `update_transforms()` descends the arena, packs each
changed slot 64 B → 112 B (model + inverse-transpose normal), and uploads —
work proportional to the number of movers, not the scene size.

**Instances.** For instanced meshes, the physics worker writes per-instance
world `Mat4`s (64 B) and `InstanceAttr`s (16 B) into two arenas; the render
worker hands the contiguous mirrors straight to
`renderer.instances.transform_write_all_bytes(key, bytes)` /
`attribute_write_all_bytes(key, bytes)` (GPU-ready bytes, no `Transform`
round-trip). Instance **count** is topology (owner-side); per-instance values
are foreign.

**Lights and skins ride the transform arena for free.** A punctual light
derives its world pose from a **bound transform** (`lights.bind_transform` +
`update_from_transforms`), and skin **joints are themselves `TransformKey`s**
(`meshes::update_world` → `skins.update_transforms` recomputes joint matrices
from the same per-frame dirty set). So a physics worker animates a light, or
flexes a skinned mesh, with **no new foreign-writable buffer** — it just moves
the light's bound transform / the skin's joint transforms in the arena
(`?demo=lights`, `?demo=skin`). Read a glTF's joint keys from
`GltfPopulateContext.transform_is_joint` after `populate_gltf`.

**Live spawn/despawn is a transaction, not a per-node poke.** Topology
(`allocate`/`free`) is owner-only and must be quiescent while foreign threads
write. The owner-side flow is bind → (foreign writes) → on retire, *unbind* the
slot and wait for the sim worker's ack before *freeing* it (so the freed slot
can be safely reused). `?demo=churn` exercises this with slot-reuse + an
invariant check.

**The player path runs in the worker.** A shipped game doesn't load glTF
directly (that's the editor / model-viewer route) — it loads a `Scene` the
editor exported, via `awsm_renderer_scene_loader::load_scene_for_player(renderer, &scene,
&assets, on_phase)`. That call works unchanged inside the render worker:
materials, primitive + baked meshes, lights, environment and the commit
transaction all run off-main. After it returns, the static world is driven by
`LoadedScene.nodes` (each node's `transform` is an arena-backed `TransformKey` —
hook it to physics), prefabs by `LoadedScene.prefabs[..].instantiate(...)`, and
new lights by `insert_light` + a bound transform. `?demo=scene` loads a scene,
adds a light at runtime, and sweeps it from the physics worker. `assets` is any
`SceneAssets` — an in-memory map, or an **async same-origin fetcher** (COEP
blocks cross-origin asset fetches).

### 9.5 `queue.writeBuffer` from shared memory

**Measured (Chrome, crossOriginIsolated):** `queue.writeBuffer` *accepts* a
`SharedArrayBuffer`-backed `TypedArray`, and a mapped range can be written from
one — corroborated by the fact that the threaded renderer already uploads from
the (shared) wasm heap every frame and renders correctly. So the upload is **not
forced off shared memory** by the platform.

Given that, the render-side descent's per-frame "copy at the pack step" is not a
removable redundant copy — it is **necessary computation**, already proportional
to the dirty count. For each *moved* transform it (1) snapshots the 64 B model
matrix out of shared memory torn-read-safe (seqlock), (2) **packs** to the 112 B
GPU layout — which computes the inverse-transpose **normal matrix**, genuine
derived data — into the renderer's regular `Vec<u8>` mirror, (3) uploads via the
mapped ring. Each step earns its keep; none is a redundant memcpy. The mirror is
the single source the single-threaded and threaded paths share.

### 9.6 Gotchas (threaded)

- The renderer is `!Send` and stays on the render worker; only the
  shared-arena boundary (`SlotBinding` + raw addresses) crosses threads.
- Build the worker renderer with `DeviceRequestLimits::max_all()` (the
  renderer's advanced passes want >8 storage buffers / >16 sampled textures
  per stage), exactly like the editor / model-viewer.
- A foreign write is `unsafe`: the addresses must point into live shared
  memory from the owning arena, and the owner must not be reallocating that
  slot's topology concurrently (it never is — topology is quiescent during the
  write loop).
- **The arena's value region is `UnsafeCell<u8>`, not `u8`.** A foreign thread
  writing a `&[u8]` you reached via `&mut` would be UB under Stacked Borrows
  (`SharedReadOnly` provenance); backing the bytes with `UnsafeCell` gives the
  `SharedReadWrite` provenance the cross-thread write needs. Verified with miri.
- **Image decode needs a non-shared copy.** The browser's `Blob` constructor
  rejects a `SharedArrayBuffer`-backed `ArrayBufferView` (*"must not be
  shared"*), so decoding an embedded glTF image in a worker fails if you pass a
  *view* over the (shared) wasm heap. `renderer-core`'s `image::bitmap::load_u8`
  copies into a fresh non-shared `Uint8Array` first — correct on both builds.
- **Resize / DPR.** The render worker owns an `OffscreenCanvas`; size it to the
  display backing store (`devicePixelRatio`) and forward main-thread
  `ResizeObserver` + `matchMedia` DPR changes to the worker as messages (see
  `examples/multithreaded/src/viewport.rs`).
- **Measure responsiveness honestly** with the Long Tasks API
  (`PerformanceObserver('longtask')`), not rAF gaps — the main thread should log
  *zero* long tasks while the worker compiles/loads (`?demo=input`).
