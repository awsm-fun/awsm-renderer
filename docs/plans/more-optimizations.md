# Deferred & Future Optimizations

Living TODO list for performance + user-experience improvements that
were scoped out of the recent optimization sprints. Each entry tries
to be concrete enough that someone (human or LLM) can pick it up cold.

Status legend:
- 🔥 — blocking a real user-facing problem
- 🚀 — clear measurable win
- 🧊 — speculative or low-impact
- ⚠️ — has a known correctness caveat

---

## 🔥 Pre-existing renderer bug (out of scope for the recent sprint)

**Problem:** Inserting any user mesh on the optimizations branch
(Primitive Box, Sprite, glTF Model, etc.) succeeds at the bookkeeping
level — bridge entry created, `model_meshes` populated,
`asset_status = Ready` — but the geometry never appears in the
viewport. Only the editor's transform-controller gizmo + grid render.

**Evidence:**
- `main` renders Primitive Box correctly.
- Every commit on the `optimizations` branch tested
  (`cc491fc` parent of recent work, `510b391`, current HEAD) is
  broken. The bisect midpoint `a17a1a7` is *more* broken (full black
  canvas, not even the grid renders).
- The regression is somewhere in the GPU-driven rendering pipeline
  introduced between `main` and `cc491fc`: classify pass, indirect
  dispatch, HZB build, occlusion cull, coverage, material_classify
  bucketing.
- Optimization commits added on top (decal layout fix, IBL parallelize,
  shader pre-warm, was_dirty gate, createImageBitmap, batched
  materializer, indexed light/decal sync) do not introduce or fix this
  bug — Box renders / doesn't-render identically across them.

**Where to start digging:**
1. Verify the geometry pass actually writes per-pixel mesh IDs into
   `visibility_data` for a newly-inserted Primitive Box (capture a
   WebGPU command buffer; check the texture).
2. Verify the material_classify compute reads those mesh IDs and
   produces a non-empty bucket for the right `shader_id` (PBR).
3. Verify the per-shader_id opaque indirect dispatch actually runs
   (workgroup count != 0).
4. Check whether the `MaterialMeshMeta` upload includes the new
   mesh's slot (`material_dirty` + buffer write in the per-frame
   `render()` flow).
5. Confirm `add_raw_mesh` doesn't need an extra `mark_create`
   (BindGroupCreate event) that the gltf path implicitly gets from
   `finalize_gpu_textures`.

A separate session is set up to focus on this.

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

- **🚀 Skip `apply_visibility_to_node`'s renderer round-trip when
  effective visibility didn't change.** Currently the bridge entry's
  `effective_visible` is overwritten unconditionally, and a
  `spawn_local` + `with_renderer_mut` fires even when the value
  is the same. Add a `if old == new { return; }` guard.

- **🚀 `apply_visibility_subtree` already runs in O(N + root-depth)
  after the recent fix, but each `apply_visibility_to_node` *still*
  spawns one `spawn_local` task per descendant.** Batch all
  hide/show ops for a subtree flip into a single `with_renderer_mut`
  closure. Saves N × renderer-lock-acquire on every Group hide/show.

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

- **Skip `material_classify_buffers::reset_header` when the prior
  frame's bucket counts are known to be zero.** The atomic-counter
  zero only matters if classify is going to dispatch and the buckets
  could carry over. With no opaque renderables (empty scene or
  fully-culled), the reset is wasted.

- **Coverage readback `mapAsync` could be skipped on frames where
  `coverage_readback_state.inflight` is true.** Right now the
  per-frame `coverage::dispatch` runs even when the prior `mapAsync`
  hasn't resolved; the readback just queues up. Gating dispatch on
  `!inflight` keeps GPU pressure lower while the consumer (skin-skip
  / material LOD) is still using last-frame counts.

- **`scene_spatial::rebuild_if_needed` cadence tuning.** The current
  defaults are `rebuild_period_frames = 600` and
  `rebuild_dirty_threshold = 200`. Both could be data-driven —
  larger scenes benefit from less-frequent rebuilds (rebuild cost
  scales with mesh count); smaller scenes can rebuild more eagerly
  for tighter query quality.

## 🚀 Renderer memory / allocation

- **`MeshLightIndicesGpu::write_gpu` rebuilds a packed `u32` index
  array every frame.** Could maintain a dirty bit per light bucket
  and only re-pack the affected ones.

- **Hot-path `Vec` allocations** in `renderable.rs::collect`,
  particle simulator, line strip vertex packing. Could use
  thread-local pooling for these.

- **`Materials::write_gpu` pessimistically uploads on every
  material_dirty event.** Could track per-material dirty ranges via
  `DynamicUniformBuffer::dirty_ranges` (already implemented for the
  meta buffer).

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

## Notes for the bug-fix session

When you pick up the pre-existing rendering bug at the top:
- The `optimizations` branch on origin has all 7 optimization
  commits from the most recent sprint. They're individually correct
  and tested green (`cargo test --workspace`, `cargo clippy --workspace`).
- The repro is: launch editor (`task scene-editor-dev`), click Insert
  → Primitive… → Box. Nothing visible in viewport (only grid).
  Compare to `git checkout main` where the same action shows a gray cube.
- Insert Model with a small glb (e.g. `crates/frontend/scene-editor/assets/gizmo.glb`)
  is fast and reaches `asset_status = Ready` but the meshes don't
  render — same root cause as the Primitive Box bug.
- A 27 MB skinned `robot-001.glb` loads end-to-end in ~1.5 s after
  these optimizations (down from a 15 s materialize-timeout error
  before the sprint).
