# Design + implementation spec: geometry in the load transaction (register → assign → commit)

**Status: fully specced, ready to implement.** Standalone — an implementer can execute it
start-to-finish without re-deciding architecture. File:line anchors throughout reflect the code as
of this branch (`follow-ups`).

> **Prerequisite already landed (do NOT redo):** the load transaction (`begin_load` → deferred adds →
> `commit_load`, the render gate, `LoadingStats`, `RendererConfigSpec`, the consolidated/private
> compile surface) is implemented + verified — see git history `6f09f989..HEAD`. The mesh
> pass-routing flags (`has_visibility_geometry` / `has_transparency_geometry`) are ALSO already
> derived at the `Meshes::insert` choke point from the geometry buffers provided (commit
> `fix(renderer): derive mesh pass-routing flags at insert …`). **This spec builds the NEXT layer on
> top of that — geometry itself becomes part of the transaction.** Keep that flag-derivation; it's a
> building block here, not something to change.

---

## 0. The problem (what this deletes)

A mesh's **geometry kind** — visibility (opaque/geometry pass, 56 B/vertex *exploded* to
`triangle_count*3`) vs transparency (forward pass, 40 B/vertex at original `vertex_count`) — is a
pure function of its **material** (alpha mode + transmission, `is_transparency_pass`). Today that
decision is **baked eagerly, per-mesh, at decode/insert time, in three different places that can
disagree**:

- `mesh_buffer_geometry_kind` (`renderer-gltf/src/buffers/mesh.rs:34`) — reads the glTF material.
- the `add_raw_mesh` (visibility-only) vs `add_raw_mesh_transparent` (transparency-only) split
  (`renderer/src/raw_mesh.rs:169` / `:368`) — caller picks the builder.
- `Materials::is_transparency_pass` (`renderer/src/materials.rs:640`) — the renderer-side classifier.

This caused the frame-killing `VisibilityGeometryBufferNotFound` black-screen class (gltf populate
built transparency-only geometry but the routing flag said otherwise — patched at the insert choke
point, but the *root* is the three-way split + eager baking). It also means:

- **No dedup.** Every `insert` mints a fresh `MeshResource` + uploads geometry, even for identical
  source geometry under different materials. The only sharing is `duplicate_with_transform` (same
  geometry **and** material, new transform — `meshes.rs:1232`).
- **Source is thrown away.** `convert_to_mesh_buffer` (`renderer-gltf/src/buffers/mesh.rs:77`) and
  `add_raw_mesh*` pack the visibility/transparency byte streams and **discard the source attributes**
  — so a mesh's kind can never be re-derived if its material changes (editor material reassignment
  needs a full rebuild the renderer can't do).
- **Eager upload contradicts the transaction.** Geometry is staged into the pool at `insert` time
  (during the *append* phase), not at `commit_load`. So "we don't load until commit" is true for
  textures + pipelines but a lie for geometry.

## 1. The model (resolved)

**Geometry joins the load transaction.** Same bracket as today (`begin_load` → declare →
`commit_load`); the new "append" is **assign a material to geometry**, NOT "upload geometry". The
GPU geometry representations are *derived and uploaded once each, at commit*, from the **union of
materials** bound to each geometry — by the **single** `material → GeometryKind` function. Rendering
continues live throughout, exactly like the rest of the transaction.

```
renderer.begin_load();                                  // cold load: show loading screen (optional for live)
let geo = renderer.register_geometry(source);           // CPU-only: retain source attrs, mint GeometryKey. NO gpu upload.
let m1  = renderer.add_mesh(geo, opaque_mat,     t1);   // record binding, mint MeshKey. NO gpu upload.
let m2  = renderer.add_mesh(geo, transparent_mat, t2);  // same geometry, different material/kind.
let stats = renderer.commit_load(|s| {…}).await?;       // derive+upload exactly the kinds needed, ONCE each, then textures+pipelines
//   geo uploads visibility ONCE (for m1) + transparency ONCE (for m2) = twice total; m1/m2 share them.
```

### The three resolved invariants

- **① One source of truth for kind.** A single `fn geometry_kind(material, is_hud) -> GeometryKind`
  (`Visibility | Transparency | Both`) lives in the renderer. `is_transparency_pass` is the
  transparency half of it; HUD ⇒ `Both`. The glTF decoder and the raw path BOTH call it. Delete
  `mesh_buffer_geometry_kind`'s duplicated logic + the `add_raw_mesh`/`add_raw_mesh_transparent`
  split.
- **② Source consumed at commit, then FREED.** A `GeometryKey` holds the CPU source
  (positions/normals/tangents/uvs/colors/indices + morph/skin source) needed to pack EITHER
  representation via the existing `mesh_pack::pack_visibility_bytes` / `pack_transparency_bytes` —
  but ONLY between `register_geometry` and its first `commit_load`. `commit_load` packs+uploads each
  kind the geometry's current bindings need (union, once each) and then **drops the source** — no
  reason to keep per-mesh attribute bytes in RAM once they're GPU-resident. **Consequence
  (deliberate):** a geometry's set of kinds is frozen at its first commit; needing a kind it never
  built (a live edit that flips opaque↔blend, or a later binding of a different kind) means
  **re-registering** the geometry. The editor always has the authored source, so its material-edit
  path re-materializes affected meshes — it never needs the renderer to retain source.
- **③ Dedup by geometry.** One `GeometryKey` → at most one shared GPU resource holding
  `visibility_offset: Option` + `transparency_offset: Option`. Every `MeshKey` bound to that geometry
  shares it (refcount). Routing flags on each instance mirror which representations the resource
  actually has (the already-landed derive-at-insert rule, now derive-at-commit-from-resource) — and
  `route_renderable` (`renderable.rs:480`) already disambiguates a both-rep resource by the
  instance's `wants_transparency`, so opaque + transparent instances of one geometry Just Work.

> **HARD INVARIANT:** `commit_load` stays identical for cold-load, full-reload, and live add — the
> only app-level choices remain "call `begin_load`?" and "`await`?". Geometry resolution is just more
> work inside the same commit. **If anything forces a divergence — a per-frame geometry upload, a
> kind decided outside the single function, a representation built before commit — STOP and ASK.**

## 2. API (resolved)

On `AwsmRenderer` (geometry registry lives on `Meshes`):

```rust
/// CPU-only: retain the source geometry, mint a GeometryKey. No GPU upload — the
/// visibility/transparency representations are derived at commit_load per the
/// materials bound to it. Cheap; call once per distinct source mesh.
pub fn register_geometry(&mut self, source: GeometrySource) -> GeometryKey;

/// Bind a material + transform to a geometry → a drawable mesh. Records the binding
/// and mints the MeshKey SYNCHRONOUSLY (callers keep their immediate-key ergonomics);
/// the mesh draws nothing until the commit that uploads its geometry kind. Many
/// add_mesh calls may share one GeometryKey (dedup).
pub fn add_mesh(&mut self, geometry: GeometryKey, material: MaterialKey,
                transform: TransformKey, opts: MeshOptions) -> Result<MeshKey>;

/// Convenience one-shot for one-off geometry: register_geometry + add_mesh. Replaces
/// BOTH add_raw_mesh and add_raw_mesh_transparent (kind is now material-resolved at
/// commit, so there is no opaque-vs-transparent variant). Keeps today's signature
/// shape so most call sites change only the name / drop the `_transparent`.
pub fn add_raw_mesh(&mut self, data: RawMeshData, transform: TransformKey,
                    material: MaterialKey) -> Result<MeshKey>;
```

`commit_load` gains a **geometry-resolution phase**, FIRST (before texture finalize), reported as a
new `LoadPhase::UploadingGeometry` (extend `crate::loading::LoadPhase` + `LoadingStats` with
`geometry_total` / `geometry_uploaded`, mirroring the texture counters):

```
commit_load:
  0. (NEW) resolve_geometry():
       for each GeometryKey referenced by ≥1 live mesh binding:
         kinds = ⋃ geometry_kind(binding.material, binding.hud) over its bindings   // invariant ①
         for kind in kinds not yet uploaded for this geometry:
           bytes = pack_<kind>(source)                                              // invariant ②
           upload once into the shared resource; record the offset                  // invariant ③
       set every bound mesh instance's has_visibility/has_transparency from the
       resource's available reps (route_renderable handles the rest)
       drop the GeometrySource bytes for that geometry — consumed, now GPU-resident (§1 ②)
  1. finalize_gpu_textures()  (unchanged)
  2. reconcile_material_variants()  (unchanged)
  3. drain_commit_compiles()  (unchanged)
  4. scene_committed = true
```

A pending (registered-but-not-yet-committed) mesh has neither representation ⇒ `route_renderable`
returns `Skip` ⇒ it's silently not drawn until its commit. No new gate state needed.

## 3. Data model (resolved)

- **`GeometrySource`** (new, CPU): the source — positions, normals (or compute-on-register), tangents
  (or compute), the custom-attribute set (uvs/colors), indices, optional morph + skin source. Enough
  to pack EITHER representation. This is the data both `RawMeshData` and the glTF decoder already
  produce just before they pack-and-discard today. **Held only register→first-commit, then dropped**
  (§1 ②).
- **`GeometryKey`** → registry entry holding the shared GPU resource (`visibility_offset:
  Option<usize>`, `transparency_offset: Option<usize>`, the buffer_info layout, AABB, morph/skin keys,
  a refcount of bound meshes) **plus the `GeometrySource` only until its first commit consumes it**.
- **`MeshResource`** (`meshes.rs:552`) folds into / is replaced by the per-`GeometryKey` resource:
  it is no longer per-`insert`; it is per geometry and shared. `mesh_to_resource` becomes
  `mesh_to_geometry`. `duplicate_with_transform`'s refcount sharing generalizes to "every `add_mesh`
  on a `GeometryKey` shares its resource."
- **`Mesh`** (instance, `meshes/mesh.rs:52`): unchanged except it now references a `GeometryKey`
  instead of owning a per-insert resource; keep the insert-derived `has_visibility_geometry` /
  `has_transparency_geometry` (now set from the shared resource's reps at commit).

## 4. The single `geometry_kind` function (delete the three-way split)

```rust
pub enum GeometryKind { Visibility, Transparency, Both }
pub(crate) fn geometry_kind(material: &Material, is_hud: bool) -> GeometryKind {
    if is_hud { return GeometryKind::Both; }                       // HUD draws in both passes
    if material.is_transparency_pass() { GeometryKind::Transparency } else { GeometryKind::Visibility }
}
```

`Material::is_transparency_pass` already encodes Blend / Opaque+transmission ⇒ transparency, Mask /
Opaque ⇒ visibility (`materials.rs`, and the glTF `mesh_buffer_geometry_kind` is the same logic over
`gltf::Material`). In the new model the **renderer `Material` exists before commit** (materials are
registered first), so the glTF decoder no longer classifies from `gltf::Material` — it registers the
renderer material + the geometry source, and the **commit** classifies. Delete
`mesh_buffer_geometry_kind` + `GltfMeshBufferGeometryKind`; the `GltfGeometryOverride` (Opaque /
Transparent / Both / FromMaterial) hint maps to a forced-union override on the binding if still
needed (the bundle loader's materialless-glb case) — fold it into `add_mesh`'s `opts`, don't keep a
parallel classifier.

## 5. Consolidate the surface (a primary goal, not cleanup)

When this lands there is **one obvious way** to get geometry on screen: `register_geometry` +
`add_mesh` (+ the `add_raw_mesh` convenience), all resolved at `commit_load`. **Delete:**

- `add_raw_mesh_transparent` (`raw_mesh.rs:368`) entirely — its reason to exist (caller pre-picks the
  transparency builder) is gone. Callers move to `add_raw_mesh` / `add_mesh`.
- `mesh_buffer_geometry_kind` + `GltfMeshBufferGeometryKind` (`renderer-gltf/src/buffers/mesh.rs`).
- The eager geometry staging inside `insert_resource` (`meshes.rs:949`) — moved to commit's
  `resolve_geometry`.
- Any now-dead `insert` / `insert_public` parameters (the explicit per-kind `visibility_geometry_data`
  / `transparency_geometry_data` args become internal to `resolve_geometry`).

**Acceptance grep:** outside the renderer's own internals, nothing builds a per-kind geometry buffer
or picks visibility-vs-transparency; the only geometry surface is `register_geometry` / `add_mesh` /
`add_raw_mesh`. No caller references `add_raw_mesh_transparent` or a `GeometryKind`/`is_transparency`
decision.

## 6. Implementation sequence (ordered; keep `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader --lib` green + `task lint` clean per step)

1. ✅ **`geometry_kind` + `GeometryKind`.** Added the single function (§4) in
   `meshes/geometry.rs` + unit tests (opaque/mask→Visibility, blend→Transparency, hud→Both, bridges
   `is_transparency_pass`). No behavior change yet (nothing calls it).
> **Refinement (recorded, upholds default-equals-today):** the spec said "compute normals/tangents
> on register". Tangents are gated on the bound material (`material_wants_tangents` — only when a
> normal map is sampled), which isn't known at `register_geometry`; computing them unconditionally
> there would regress meshes that don't need them. So **normals are computed at register** (material-
> independent) and **tangents at commit** in `resolve_geometry` (when the bound materials are known),
> from the retained positions/normals/UV0/indices. This preserves today's behavior exactly.

2. ✅ **`GeometrySource` + `GeometryKey` registry + retain source.** Add the registry on `Meshes`;
   `register_geometry` stores the source CPU-side (compute normals/tangents on register, as the
   builders do today). No GPU upload. Existing `insert` keeps working (parallel path) so the build
   stays green.
> **Step-3 split (resolved during build — the resolve body is coupled to the bindings):**
> `resolve_geometry`'s body needs the `geometry → meshes` bindings that `add_mesh` (step 4) creates,
> so it can't be meaningfully written before them. **Step 3a (done):** `LoadPhase::UploadingGeometry`
> + `LoadingStats.geometry_total/geometry_uploaded` + the `resolve_geometry` phase hook wired as
> `commit_load`'s phase 0 (reports the phase; empty registry today). **Step 3b:** the resolution body
> (pack/upload per (geometry,kind), share one resource across bound meshes, set flags, free source) +
> the binding maps + the granular viewer UI land WITH step 4 (`add_mesh`), since they all interlock.
>
> **Validated design for the resolve body (executes next; prep `wire_instance` refactor already
> landed):**
> - **Deferred-resource is render-safe.** Between `add_mesh` and `resolve_geometry` a mesh has no
>   resource/flags; `route_renderable` returns `Skip` for a no-buffer mesh (renderable.rs already
>   designs for "mid-upload" meshes — see its line ~259), and the non-render resource accessors are
>   only hit on the draw path, which Skip'd meshes never reach. `sync_spatial_for_mesh` only needs
>   `world_aabb` — set from the source at `add_mesh` — and skin info; call it at *resolve* (after the
>   resource exists) so skinned meshes flag correctly.
> - **Maps on `Meshes`:** `mesh_to_geometry: SecondaryMap<MeshKey, GeometryKey>`,
>   `geometry_to_meshes: SecondaryMap<GeometryKey, Vec<MeshKey>>`.
> - **`add_mesh(geometry, material, transform, opts)`** (AwsmRenderer): build `Mesh` (world_aabb from
>   the source's aabb, double_sided from the material), `meshes.list.insert` → MeshKey (sync), record
>   both maps. NO resource / meta / sync_spatial / upload.
> - **`Meshes::resolve_geometry(materials, transforms) -> Result<Vec<MeshKey>>`:** snapshot
>   `geometries.keys()`; per key, `geometries.remove(key)` (frees source — owned, sidesteps the borrow
>   conflict with the per-key uploads) + `geometry_to_meshes.remove(key)`; union
>   `geometry_kind(material, mesh.hud)` over the bound meshes; compute tangents once iff any bound
>   material `material_wants_tangents` (`awsm_tangents::generate_tangents` over source
>   pos/normals/uv0/indices); `pack_visibility_bytes` and/or `pack_transparency_bytes`; rebuild
>   `MeshBufferInfo` (vis/transp `Some/None` per the union, triangles from the source) →
>   `buffer_infos.insert`; call the existing `insert_resource` ONCE → shared `resource_key`; set
>   `resource.refcount = bound.len()`; `wire_instance` each bound mesh; return the wired keys.
>   `AwsmRenderer::resolve_geometry` then `sync_spatial_for_mesh` per wired key + reports
>   `geometry_uploaded`. (`material_wants_tangents` → `pub(crate)`.)
> - **`add_raw_mesh` = `register_geometry` + `add_mesh`** (delete `add_raw_mesh_transparent`) in the
>   step-6 call-site migration; keep the legacy `insert` until step 8 so the tree stays green.

3. ✅ **`resolve_geometry` in `commit_load`** (granular UI deferred to step 6, where it's runtime-
   verifiable once geometry flows). Body landed: union kinds → pack reps once → one shared resource →
   `wire_instance` each bound mesh → free source; `LoadingStats` geometry counters; commit phase 0.
   Implement the commit phase (§2 step
   0): per `GeometryKey`, union the kinds from bound materials, pack+upload missing reps once via
   `mesh_pack::pack_*`, set instance flags from the resource, then free the source. The pool-write
   plumbing already exists in `insert_resource` — move it here, keyed per (geometry, kind), idempotent.
   Add `LoadPhase::UploadingGeometry` + the `LoadingStats` `geometry_total` / `geometry_uploaded`
   counters (mirroring the texture counters). **Then wire BOTH viewers' loading UI to render every
   phase + counter granularly** — model-tests overlay (`context.rs` `LoadingStatus` /
   `canvas.rs::commit`) and the editor activity/boot indicator (`engine/activity.rs`, `main.rs` boot
   `on_progress`, `web-shared` boot loader): show distinct, live "Uploading geometry X/Y" / "Uploading
   textures X/Y" / "Compiling pipelines (N)" lines driven off `LoadingStats`, replacing the coarse
   `shader_prewarm` bool / `compile_pending` count / single boot message. One mapping
   (`LoadingStats → label`) shared by both viewers if practical.
4. ✅ **`add_mesh` + `register_geometry` wired to deferral.** Landed on `AwsmRenderer` (+ `AddMeshOpts`,
   `Meshes::bind_mesh`); mints the MeshKey sync, records the binding, uploads nothing. (`add_raw_mesh`
   = register + add_mesh happens in the step-6 call-site migration.) Original text:
   `add_mesh` records the binding + mints the
   MeshKey synchronously, references the GeometryKey, NO upload. Make `add_raw_mesh` = register +
   add_mesh. At this step the geometry is uploaded by `commit_load` (step 3), so a normal model still
   renders after its commit.
> **Step-5 split (resolved during build):** **5a (done):** `GeometrySource` extended with the
> morph/skin buffer-layout (`geometry_morph_info` / `material_morph_info` / `skin_info` + the
> `material_morph_key`); `resolve_geometry` reattaches them to the rebuilt `buffer_info` + passes the
> keys to `insert_resource` (raw path = all `None`, behavior unchanged). Confirmed morph/skin travel
> cleanly with the retained source — deltas are kind-independent, no design divergence. **5b (next,
> glTF crate):** `convert_to_mesh_buffer` (`renderer-gltf/src/buffers/mesh.rs`) STOPS calling
> `create_visibility_vertices`/`create_transparency_vertices` + drops the `geometry_kind` param;
> instead it RETAINS the source (positions/normals/uv0 as `Vec<[f32;_]>` from `attribute_data_by_kind`,
> + `triangle_indices`, front_face) and keeps `pack_vertex_attributes` (custom attrs) + triangle data
> + `convert_morph_targets`/`convert_skin`. The decode output (drop the vis/transp shared buffers from
> `GltfData` + `MeshBufferInfoWithOffset`) carries the retained source + custom bytes + layout +
> morph/skin (keys + the new layout infos). `populate_gltf_primitive` builds a
> `GeometrySource` from it → `register_geometry` + `add_mesh(geometry, material_key, transform_key,
> AddMeshOpts{instanced,hud,hidden})` instead of `meshes.insert`. Delete `mesh_buffer_geometry_kind`
> + `GltfMeshBufferGeometryKind`. Runtime-verify Fox + DamagedHelmet on :9080.

5. 🟡 **Migrate the glTF decoder + populate.** `convert_to_mesh_buffer` stops baking a kind + stops
   packing/discarding — it produces a `GeometrySource` (retain the attrs). `populate_gltf` registers
   the source + the renderer material, then `add_mesh`. Delete `mesh_buffer_geometry_kind`.
> **Step-6 RAW done + runtime-verified ✅** (the glTF migration §5b is the remaining producer). The
> sync-caller problem (gizmos can't `await commit_load`) resolved cleanly + preserving
> default-equals-today: **`add_raw_mesh` = `register_geometry` + `add_mesh` + an EAGER `resolve_one`**
> (extracted from `resolve_geometry` — both share it). One-off raw meshes upload + draw immediately,
> sync, no caller changes. `add_raw_mesh_transparent` deleted; material-agnostic now. Verified: the
> editor (:9085) "Add a Sphere" primitive renders via the new path (1 mesh / 1.2k tris, no
> VisibilityGeometryBufferNotFound); model-tests Fox regression-clean. `into_geometry_source` does the
> raw pass-independent packing.
>
> **Step-5b plan (glTF decode → GeometrySource), incremental:** **5b-i done** (GeometrySource carries
> authored `tangents`; resolve uses them else generates). **5b-ii DONE** + **5b-iii DONE +
> RUNTIME-VERIFIED:** `populate_gltf_primitive` builds a `GeometrySource` from the retained typed source
> (`source_positions/normals/uvs0/tangents[authored]/indices` + new `source_front_face`), the
> pass-independent custom-attribute + attribute-index slices, the native `vertex_attributes`, AABB, and
> the morph/skin keys + layout infos, then calls `register_geometry` + `add_mesh(…, AddMeshOpts{…})`
> instead of `meshes.insert`. `AddMeshOpts` gained `double_sided: Option<bool>` (Some = the glTF
> thin-shell single-sided override; None = derive from material). `AwsmGltfError` gained
> `From<AwsmError>`. Verified on :9080: Fox + DamagedHelmet (normal-map tangents at commit) +
> previously-black CompareTransmission (transmission/transparency path) all render clean, NO
> VisibilityGeometryBufferNotFound, NO "not compiled, skipping", no console errors. **5b-iv (next):** in
> `renderer-gltf/src/buffers/mesh.rs` `convert_to_mesh_buffer`, after `ensure_normals`, extract the
> TYPED retained source — `resolve_attribute_buffers` + `decode_vec3s` for positions/normals, the
> TexCoord-0 attr → uv0, the optional `TANGENT` → authored tangents, `triangle_indices` flattened →
> indices — and STOP calling `create_visibility_vertices`/`create_transparency_vertices` + drop
> `ensure_tangents` (generated at commit). Carry the retained source on `MeshBufferInfoWithOffset`
> (replacing the vis/transp offset fields); keep the custom-attr/triangle/morph/skin packing.
> `populate_gltf_primitive` builds a `GeometrySource` (retained source + the custom-attr slice from
> `custom_attribute_vertex_bytes` + `attribute_index_bytes` + morph/skin keys+layouts via the existing
> `From` impls + aabb) → `register_geometry` + `add_mesh(geometry, material_key, transform_key,
> AddMeshOpts{ hud: hints.hud, … })`. Delete `mesh_buffer_geometry_kind` + `GltfMeshBufferGeometryKind`
> (the material decides the kind at commit; the `GltfGeometryOverride` HUD/material cases fold into the
> material classification + `AddMeshOpts.hud`). **5b-iv:** drop the vis/transp shared buffers from
> `GltfBuffers` + the decode driver (buffers.rs ~347-440); keep `create_visibility_vertices` /
> `reference_visibility_vertices` + the byte-identity proptests as `cfg(test)` packer-parity tests.
> Runtime-verify Fox + DamagedHelmet + the previously-black models on :9080.

6. ✅ (raw) / 🟡 (glTF = §5b) **Migrate every raw call site** (the ~14 in the inventory: editor node_sync `:875`/`:996`,
   particles, thumbnail/preview/light_icons, scene-loader `:843`/`:1216`/`:1402`/particles,
   web-shared point_handle, render-worker): `add_raw_mesh_transparent` → `add_raw_mesh`; drop the
   opaque-vs-transparent choice. Each is followed (as already wired) by a `commit_load`.
7. **Live material reassignment through the same path.** `set_mesh_material` becomes an "append": it
   updates the binding, and the next `commit_load` re-routes the mesh among the geometry's
   ALREADY-built kinds (a both-rep geometry's instance flips opaque↔transparent for free). A
   reassignment that needs a kind the geometry never built (its source is gone, §1 ②) **re-registers**
   the geometry — the editor's material-edit path re-materializes the affected meshes from authored
   data. Either way it routes through register/add_mesh/commit, never a side channel — proves
   invariant ① end-to-end.
8. **Delete the dead model.** Remove `add_raw_mesh_transparent`, `mesh_buffer_geometry_kind`,
   `GltfMeshBufferGeometryKind`, the per-insert `MeshResource`, the eager `insert_resource` staging,
   and any now-unused `insert`/`insert_public` kind args. Verify each removal (compiler + §7).

## 7. Verification (standards gate: no perf regression; default-equals-today; one-way; impossible-bad-state)

- **The footgun is gone by construction:** there is no way to create a mesh whose routing disagrees
  with its geometry (flags derive from the shared resource's reps), and no way to pick the wrong kind
  (one `geometry_kind` fn). Add a test: register one geometry, bind an opaque + a transparent
  material, commit → the resource has BOTH reps, the opaque instance routes opaque, the transparent
  routes transparent, uploaded **twice total** (once per kind), not four times.
- **Screenshot-verify (chrome-devtools :9080)** the models that were black before — `CompareTransmission`,
  `ClearCoatTest`, a transmission/blend mix — plus Fox + DamagedHelmet (regression). ALWAYS navigate
  + screenshot to CONFIRM before trusting console. No `VisibilityGeometryBufferNotFound`, no
  `not compiled, skipping`, MSAA edges intact.
- **Dedup proof:** load a scene reusing one geometry under multiple materials/transforms; confirm the
  geometry source uploads each needed kind once (trace / a count assertion), not once per instance.
- **Editor live path (:9085):** reassign a mesh's material opaque↔blend; it re-renders correctly
  after the commit. A flip among already-built kinds is free; a flip to a never-built kind
  re-materializes (the editor re-registers from authored data — the renderer holds no source, §1 ②).
- **Granular loading UI:** on both viewers, the loading overlay shows distinct live geometry /
  texture / pipeline progress from `LoadingStats` (not a single spinner) — screenshot-verify the
  phases are visible during a cold load.
- **Source is freed:** after a commit, the registry holds no `GeometrySource` bytes (the GPU
  resource + offsets remain) — confirm via a memory/asserts check that source isn't retained.
- **`task lint` clean + the test gate green throughout.** Commit per step with explicit paths
  (NEVER `git add -A`, NO backticks in `-m`), end messages with the Co-Authored-By trailer; do NOT
  push or open a PR.

## 8. Open issues (not part of this design)

- Minor model-tests picker quirks (`Sponza`, some names → "Not Found"; `IridescenceDishWithOlives`
  framing) are cosmetic/pre-existing — out of scope.
