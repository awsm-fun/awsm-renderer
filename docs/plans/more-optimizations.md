# Deferred & Future Optimizations

Living TODO list for performance + user-experience improvements that
were scoped out of the recent optimization sprints. Each entry tries
to be concrete enough that someone (human or LLM) can pick it up cold.

Status legend:
- 🚀 — clear measurable win
- 🧊 — speculative or low-impact
- ⚠️ — has a known correctness caveat

---

## ✅ Recently landed

These were on this list previously; they've shipped and are noted
here so the doc stays an accurate "still TODO" list rather than a
mixed pile.

- ✅ **`apply_visibility_to_node` identity guard** — bridge entry
  skips renderer round-trip when effective visibility didn't change.
- ✅ **`apply_visibility_subtree` batches into one
  `with_renderer_mut`** — per-mesh hide/show ops for a whole subtree
  flip now collect into one renderer-lock acquisition instead of N.
- ✅ **`MeshLightIndicesGpu::write_gpu` fast path on empty scenes** —
  when neither this frame nor the prior frame had any per-mesh light
  slices, the per-mesh-meta zero walk is skipped. Saves O(meshes)
  writes per frame for directional-only / no-light scenes.
- ✅ **`Materials::write_gpu` uses dirty-range tracking** — was
  already done; doc entry was stale.
- ✅ **Coverage readback `mapAsync` skips when inflight** — was
  already done; doc entry was stale.
- ✅ **`indirect-first-instance` dual-path architecture** — see
  [`docs/PERFORMANCE.md`](../PERFORMANCE.md) for the two paths
  (storage-array meta when the device exposes the feature, portable
  uniform-with-dynamic-offset when it doesn't). `FeatureToggle`
  with `Auto` / `On` / `Off` controls which path is taken.

---

## 🚀 Renderer init (time-to-first-frame)

- **Parallelize `RenderTextures::new` with the late-stage render-pass
  pipeline construction in `AwsmRendererBuilder::build`.** Currently
  serialised through `&mut` borrow of various `pipeline_layouts /
  pipelines / shaders / bind_group_layouts`. Could be refactored to
  hand out independent sub-borrows, or just split `RenderTextures` to
  not need the shared mut state (it only consumes the texture formats
  + features). Likely 50–100 ms saved on cold init depending on adapter.

- **Defer `OpaqueMipgen::new` until first transmissive mesh.** Only
  used when a material has `transmission > 0`. Most scenes never
  trigger it. Could be wrapped in `OnceCell` + lazy-init on first
  transmission path.

- **Lazy-allocate the GPU-driven feature buffers when no meshes are
  present.** `OcclusionBuffers`, `CompactionBuffers`, `CoverageBuffers`
  all reserve 1024-slot capacity at init even for empty scenes. With
  the feature gating already in place, these can stay `None` until
  the first mesh insert dirties them.

- **Pre-warm `LineRenderer` shaders in parallel with `Picker`.** Both
  go through the same shared `&mut shaders` so they're currently
  serial. `LineRenderer::load` and `Picker::new` each compile 1–2
  shaders; can be parallelized by collecting shader cache keys
  up-front and issuing one `ensure_keys` batch.

## 🚀 Editor per-frame loop

- **🧊 Add `mesh_node_ids` index** mirroring the
  `light_node_ids` / `decal_node_ids` / `collider_node_ids` /
  `camera_node_ids` pattern, for any future per-frame mesh walks.
  Currently mesh nodes are reached through the `model_meshes` Vec
  on each bridge entry, so no per-frame walk exists — file under
  "if it becomes hot."

- **🧊 Camera-list cache for the header dropdown.** `list_authored_cameras`
  does a full tree DFS on every open. Add a cached `Vec<(NodeId,
  display_name)>` invalidated on `camera_node_ids` change.

## 🚀 Model insert (UX time)

- **Web Worker for glTF JSON+buffer parse.** `GltfBuffers::new`
  takes ~900 ms for the 27 MB robot purely on Wasm-side CPU. Could
  be moved to a worker if we accept a structured-clone transfer of
  the parsed result. Caveat: transfer cost for the 27 MB blob may
  eat the win unless we move the whole pipeline (fetch + parse +
  texture decode) to the worker.

- **Pre-decode raster bitmaps during `extract_gltf_materials_into`,
  not on first `instance_template`.** Currently the raster prefetch
  is hoisted into `load_and_populate` (runs once per glb via
  `Shared`), but it could be kicked off *synchronously* the moment
  the bytes land in `pending_assets`. Saves the ~1 s of
  `createImageBitmap` time during the loading-modal window.

- **🚀 Deduplicate the two GPU texture-pool uploads per glb texture.**
  The renderer-gltf path uploads each image via
  `Textures::add_image(ImageData::Bitmap)` for the baked materials,
  AND the editor's `texture_cache::get_or_upload` uploads the same
  image (separate `ImageBitmap` decode + separate pool slot) for the
  editor's editable material override. That's 2× GPU storage + 2×
  decode work per texture. Plumb a mapping `AssetId → existing
  TextureKey` from `renderer-gltf` into the editor's `texture_cache`
  so the override path reuses the pool slot.

- **🧊 Lazy-compile material shaders only for material types actually
  present in the scene.** `MaterialOpaquePipelines::new` builds 12
  variants (3 shader_ids × 2 msaa × 2 mipmaps) up-front. If the
  scene only has PBR materials, the Unlit + Toon variants are dead
  weight. Track which shader_ids the live material set uses; build
  others on first material insert.

## 🚀 Renderer per-frame

- **⚠️ Cache `transpose_per_mesh` output across frames when neither
  lights nor mesh AABBs moved.** `MeshLightIndicesGpu::write_gpu` now
  fast-paths the no-light-overlap case (see "Recently landed" above),
  but the *with*-lights case still rebuilds the packed `u32` index
  array every frame and re-patches every per-mesh slice. A dirty-flag
  on `LightMeshBuckets` (set whenever a light is added/removed/moved
  or a relevant mesh AABB changes) would let the GPU upload reuse
  last frame's bytes byte-for-byte on static scenes.

- **`scene_spatial::rebuild_if_needed` cadence tuning.** The current
  defaults are `rebuild_period_frames = 600` and
  `rebuild_dirty_threshold = 200`. Both could be data-driven —
  larger scenes benefit from less-frequent rebuilds (rebuild cost
  scales with mesh count); smaller scenes can rebuild more eagerly
  for tighter query quality.

## 🚀 Renderer memory / allocation

- **Hot-path `Vec` allocations** in `renderable.rs::collect`,
  particle simulator, line strip vertex packing. Could use
  thread-local pooling for these. The renderable Vecs are reset
  every frame; keep them on `AwsmRenderer` and clear-in-place.

## 🚀 Editor reactive system

- **Coalesce reactive signal cascades that fan-out across many
  observers.** `bump_nodes_revision` fires when any bridge entry
  changes; consumers (selection observer, gizmo, point-handle,
  inspector) re-derive their own state. For a multi-mesh model
  insert this can spike to dozens of cascades per frame.

- **Debounce `material_cache::cascade_after_delete`-style cascades.**
  Currently each MaterialAsset deletion walks the scene; multi-asset
  cleanup pays N×scene-walk where 1 would do.

- **Pre-warm the gltf loader during editor startup.** Currently the
  first glb insert pays both `GltfLoader::load` parse cost +
  `populate_gltf` cost as foreground work. Could fetch + parse the
  gizmo + a "warmup nullary glb" during init.

## 🧊 Speculative / micro-optimizations

- **Replace `Mutex<HashMap>` with `Mutex<IndexMap>` for the bridge
  node table** to get deterministic iteration order — would help
  some observer cascades collapse identical re-evaluations.

- **`Arc<Mutex<...>>` → `Rc<RefCell<...>>` wherever we don't actually
  cross threads** (wasm is single-threaded; the `Send` / `Sync`
  bounds add lock-acquire cost for no real safety win). Big refactor;
  only worth it after profiling shows it's hot.

- **Skip per-frame `transforms.get_world(tk)` for transforms whose
  parent chain hasn't moved.** Requires a "dirty since last query"
  flag on each transform node.

- **Buffer compaction for the per-mesh material meta SSBO** — when a
  mesh is removed, its slot becomes a hole; over many edits the
  buffer becomes sparse. Periodic compaction could improve cache
  locality for the opaque shader's meta lookups.

---

## Notes for future sessions

When you pick up an item from above:
- The `optimizations` branch should test green
  (`cargo test --workspace`, `cargo clippy --workspace`) and visually
  render correctly in the editor under all three
  `?ifi=on / off / auto` modes. Smoke-test both paths before adding
  more work — silent failure on one path is the easiest mistake.
- Repro setup: launch editor (`task scene-editor:dev`), insert a
  Primitive Box and confirm it renders. Toggle
  `?ifi=off` and confirm it still renders. Toggle `?ifi=on` likewise.
- A 27 MB skinned `robot-001.glb` loads end-to-end in ~1.5 s on the
  current branch (down from a 15 s materialize-timeout error before
  the optimization sprint).
- The `indirect-first-instance` WebGPU feature has narrow real-world
  support (Firefox: none; Chrome desktop: Linux-Intel only as of
  mid-2026), so the portable `ifi=off` path is the one most player
  devices will hit in shipped games. Both paths are first-class —
  benchmarks should cover both before any "optimization" claim.
