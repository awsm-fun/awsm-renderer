# Renderer Optimisations — Implementation Plan

## How to consume this document (read this first)

**You are the implementing agent.** This document is the entirety of
your brief. Work it **start to finish**, in the order given by §11
(Picking order). Do not stop after Cluster 1, do not stop after
Cluster 2 — keep going until every cluster's Definition of Done is
satisfied. The user has already approved the full plan; further
permission to continue is not required.

**Breaking compilation mid-flight is expected and welcome.** This is
a large structural change. The honest way to do it is to rewrite the
load-bearing seams (mesh insert/remove, `update_world`, the light
loop, the material dispatch shape) and then re-stitch the
dependents. Intermediate states where the crate does not compile —
even for hours of work — are the **correct** way to do this. They
are vastly preferable to:

- Defensive shims that translate between "old API" and "new API."
- Leaving a parallel code path alive "until the new one is proven."
- `#[allow(dead_code)]` on the old struct so the test suite keeps
  passing during the transition.
- Re-exporting old names as aliases for the new ones.
- Feature flags / config knobs that toggle between old and new
  behaviour.

All of those produce technical debt that lingers. **Do not produce
them.** Delete code outright when it's superseded. The only build
that must be clean is the final one, against the Definitions of Done
in §14 (and §14.1, §14.2 as the corresponding clusters land).

**Context usage is not a constraint.** Read every file the plan
links into. Read its neighbours when ambiguous. Open the test files
you'll need to update. Don't summarise prematurely; don't skip
follow-up reads because "you already have the gist." The plan exists
to remove guesswork — confirm by reading, then act. Long sessions
are expected.

**When in doubt, prefer the choice that produces less code.** Many
of the §12 open questions have a "Default:" line that picks the
simpler path. Follow it. Do not invent extra abstraction (factories,
traits, generic parameters) beyond what each step's signature sketch
calls for. If a sketch shows a concrete `struct` and you find
yourself adding a generic, stop and re-read §0 of [CLAUDE.md
guidance](../../CLAUDE.md) about not designing for hypothetical
future requirements.

**Code organisation rules.**

- **`mod.rs` is for visibility and re-exports only.** No struct
  definitions, no `impl` blocks, no function bodies. Its only
  contents are `mod foo;`, `pub mod bar;`, `pub use bar::Bar;` and
  similar. If you find yourself writing a function in `mod.rs`, that
  function belongs in its own file (or in an existing file in the
  module) and `mod.rs` should re-export it. The existing
  `crates/renderer/src/shadows/mod.rs` is a good reference — 47
  lines, all `mod` and `pub use`.
- **One concern per file.** When a step introduces multiple things
  (e.g., Cluster 1.2 introduces `SceneSpatial`, `SceneNode`,
  `NodeFilter`, dirty-tracking state, the rebuild policy), put each
  concern in its own file under the module's directory rather than
  letting one `index.rs` grow past ~300 lines. The scaffolding in
  §2.1 (`scene_spatial/{mod.rs, index.rs, query.rs,
  frustum_selector.rs, tests.rs}`) is the minimum split; split
  further if any file outgrows ~300 lines or mixes two unrelated
  responsibilities. For Cluster 6.1 specifically, expect
  `material_classify/{mod.rs, bucket_builder.rs, tile_table.rs,
  indirect_args.rs, ...}` rather than one large file.
- **Names should reason themselves.** A reader who opens a file
  cold should understand what a type or function does from its name
  before reading the body. Prefer `mesh_light_slices` over
  `slices`; prefer `rebuild_if_dirty_threshold_exceeded` over
  `maybe_rebuild`; prefer `FrustumPlaneAabbTest` over `Test`. The
  existing codebase is consistent about this — match its style.
- **Comments explain the WHY, never the WHAT.** Existing comments
  in the renderer are exemplary on this point: see
  [`shadows/state.rs:1038-1046`](../../crates/renderer/src/shadows/state.rs)
  where the per-cascade caster-AABB clipping is justified by
  reference to a specific failure mode ("a 10 km × 10 km ground
  plane whose union AABB stretches thousands of metres along the
  tilted light direction"). Write comments at that bar: every
  comment should answer a question the code can't answer for itself.
  Do not narrate steps; do not restate the function name; do not
  write multi-paragraph headers. If a comment would just describe
  what the next line does, delete it.
- **Match the existing style.** Read a neighbouring file before
  writing a new one. Doc-comment placement, error-handling idioms
  (`AwsmError`, `Result`), buffer-naming conventions
  (`*_dirty: bool`, `gpu_dirty: bool`, `*_scratch: Vec<…>`),
  bind-group layout numbering — all of these have settled
  conventions. Diverging from them creates noise; matching them
  makes the new code disappear into the codebase.

**Order of operations within each step.**

1. Read the file(s) the step links into. Read enough surrounding
   context to confirm the linked anchor still matches the source
   (line numbers may have drifted since the plan was written —
   navigate by symbol if so).
2. Make the change. If it cascades, follow every cascade in the
   same step rather than stubbing things out. Half-implementations
   are worse than no implementation.
3. Update or delete tests that no longer apply. Do not skip them.
   Do not `#[ignore]` them. Either fix them in place or remove
   them because the behaviour they covered no longer exists.
4. When a cluster's Definition of Done is reached, run the full
   `cargo check --target wasm32-unknown-unknown -p awsm-renderer`
   and confirm green. **Then move to the next cluster.** Do not stop
   to report progress unless you hit a genuinely unresolvable
   blocker (one not covered by §12).
5. Only at the very end of the plan — after every cluster's DoD is
   satisfied — open a PR / report completion.

**What "completion" looks like.** Every checkbox in §14, §14.1, §14.2
ticked, every appendix-table row in §15 filled with measured
numbers, every reference path in §16 still resolves. The plan itself
should be archived (move to `docs/plans/archive/` or delete) when
done; it has served its purpose.

**Blockers.** The §12 questions are pre-resolved by the "Default:"
lines. If you hit something §12 doesn't cover and cannot resolve by
reading the code, **make the call yourself** using the principles
above (less code, less abstraction, no compatibility shims) and
write a one-line note in §15's "Notes" column. Do not pause to ask
the user unless the call is genuinely irreversible (e.g., would
require a database migration or break external API contracts —
neither applies to this renderer).

**Do not modify this document while implementing it.** It is your
spec. The appendix table (§15) and the DoD checkboxes (§14) are the
only mutable sections; everything else is read-only until the work
is complete.

---

**Status:** ready for hand-off. Every cluster below is sequenced into
concrete steps with file/line touchpoints, signatures, and acceptance
criteria. Pick up cold and start at Step 1.1; do not stop until §14.2
is satisfied.

**Audience:** an engineer or agent who has not yet read the
renderer in detail. Each step links into the codebase at the point of
change; read the linked file before editing.

**Scope.** This is the renderer's internal performance plan,
covering CPU culling, shadow scheduling, vis-buffer-native lighting
and material dispatch, and (long-horizon) GPU-driven culling. The
adjacent [open-world plan](../PERFORMANCE_OPEN_WORLD_PLAN.md) covers
asset streaming, LOD, and chunking — those belong upstream and are
referenced where the two plans touch.

**Shape of the plan.** Eight clusters, sequenced by dependency
rather than by historical accident:

- **1.** Scene BVH (the keystone — everything depends on it).
- **2.** Light and shadow scheduling, including the **per-mesh
  light list** path that's the right culling model for our
  visibility-buffer architecture.
- **3–5.** Authoring/debug UX, quality tiers, robustness scaffolding.
- **6.** Visibility-buffer-native optimizations: material classify
  with indirect dispatch, coverage-driven skinning skip and material
  LOD, decals as a material class.
- **7.** GPU-driven culling and HZB (long-horizon).
- **8.** Buffer-upload and animation polish (independent).

§0.3 explains why the visibility-buffer architecture is the central
lever for Clusters 2.1 and 6; skim it before reading those clusters.

---

## 0. Context and problem statement

### 0.1 What the renderer does today

The current culling path is a per-view linear sweep over every mesh,
testing each mesh's cached `world_aabb` against the view's frustum.
Concretely:

- Camera pass: [`renderable.rs:55-60`](../../crates/renderer/src/renderable.rs)
  iterates `self.meshes` and calls `Frustum::intersects_aabb` once per
  mesh.
- Shadow generation: [`shadows/render_pass.rs:125-144`](../../crates/renderer/src/shadows/render_pass.rs)
  rebuilds a `Frustum` per shadow view and iterates `ctx.meshes` end to
  end, doing the same AABB-vs-frustum test.
- Cascade fit: [`shadows/state.rs:1046-1055`](../../crates/renderer/src/shadows/state.rs)
  rebuilds `caster_aabbs_scratch` from scratch every frame by walking
  every mesh and filtering on `cast_shadows && !hidden && !hud`.

The shadow system materializes ~52 views for a "rich" scene (4
directional cascades + 8 shadowed point lights × 6 faces); at the
open-world target of ~100 shadowed views and ~10 000 meshes this is
~1 M plane-vs-AABB tests per frame, all CPU.

The frustum and AABB primitives themselves are fine. They live at
[`frustum.rs`](../../crates/renderer/src/frustum.rs) (6 planes) and
[`bounds.rs`](../../crates/renderer/src/bounds.rs) (min/max). The
problem is the *iteration cost*, not the per-mesh test.

### 0.2 Mesh-count tiers we plan for

| Tier            | Meshes  | Views/frame | Notes                                      |
| --------------- | ------- | ----------- | ------------------------------------------ |
| **Current**     | ~100    | ~10–20      | Already fine. BVH lands as future-proofing.|
| **Near-term**   | ~1 000  | ~20–50      | The break-even point for BVH cost over linear. |
| **Open-world**  | ~10 000 | ~50–100     | The structure has to scale here.           |

The plan sizes data structures for the open-world tier but calls out
which knobs are load-bearing now vs only at scale.

### 0.3 The visibility-buffer architecture as an optimization lever

The renderer is a **visibility-buffer** (deferred-attribute) pipeline,
not a classical forward or G-buffer-deferred. The geometry pass at
[`render_passes/geometry/render_pass.rs`](../../crates/renderer/src/render_passes/geometry/render_pass.rs)
writes four targets per pixel — `visibility_data`, `barycentric`,
`normal_tangent`, `barycentric_derivatives` — plus depth. The
visibility target encodes `(triangle_index, material_meta_offset)`
([`material_opaque_wgsl/compute.wgsl:120-121`](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/compute.wgsl));
the material pass is a compute pass that walks pixels, reconstructs
attributes from the triangle index, and shades them
([`material_opaque/render_pass.rs:47-97`](../../crates/renderer/src/render_passes/material_opaque/render_pass.rs)).

This architecture is the central lever for the optimizations in
Clusters 6 and 7. Two facts in particular dominate the design:

- **The material pass already dispatches one compute per material
  pipeline key over the entire screen.** Pixels whose material
  doesn't match the current dispatch early-out. That's the
  uber-deferred wasted-lane tax. Replacing the screen-wide dispatch
  with an indirect dispatch driven by a per-material tile bitmask
  ("material classify") is a near-mechanical change and a large win
  once material count grows. See Cluster 6.1.
- **The visibility buffer already encodes mesh identity per pixel
  (via `material_meta_offset` → `material_mesh_metas[…]`).** Combined
  with the `rstar` BVH, this lets us do **per-mesh light lists** —
  for each visible mesh, the set of lights whose AABB overlaps it —
  instead of standard tile/cluster culling. Per-mesh lists are
  strictly tighter than per-tile clusters because a tile that
  contains "wall + air + light volume" still binds the light, where a
  per-mesh list binds it only to the actual surface. See Cluster
  2.1.

The plan treats those two changes as load-bearing for the open-world
tier and sequences them after the BVH so they can consume its query
API directly.

References: Burns & Hunt, "The Visibility Buffer" (JCGT 2013);
Schied & Dachsbacher, "Deferred Attribute Interpolation" (HPG 2015);
Wihlidal, "Optimizing the Graphics Pipeline with Compute" (GDC 2016);
El Mansouri / Activision, "Rendering of Call of Duty: Infinite Warfare"
(Digital Dragons 2017).

### 0.4 Engine-bridge story

The renderer is one consumer in a larger game engine that also runs
physics, AI, and gameplay queries. The renderer keeps **its own**
spatial structure (different update cadence, different quality bar
than physics), but exposes a narrow read-only query trait so other
crates can piggy-back on it without owning a second copy.

- The renderer **owns** the structure and is the only writer.
- A `SpatialQuery` trait (defined alongside the structure) exposes
  frustum / AABB-overlap / kNN / ray queries to outside crates.
- Physics and AI may have their own broad-phase if they need
  different invariants. The trait is a *convenience* boundary, not a
  contract that forces unification.

This matches three.js, Bevy, and Unity practice. Concretely it means:
the BVH lives in `awsm-renderer`, and the trait is exposed via a small
`SpatialQuery` accessor on `AwsmRenderer`.

---

## 1. Decision record: spatial structure choice

### 1.1 What we considered

| Candidate                | Verdict | Why                                                                                  |
| ------------------------ | ------- | ------------------------------------------------------------------------------------ |
| `rstar` (R\*-tree)       | **Pick**| Hierarchical pruning via `SelectionFunction`, zero glue for 3D AABBs, clean wasm, richer query API (kNN + envelope-overlap + custom selection). See § 1.2. |
| `bvh` crate (svenstaro)  | Backup  | Better ray traversal, has explicit `update_shapes` refit. Drawbacks: pulls `nalgebra` (we're glam), `rayon` (wasm-hostile by default), no first-class kNN. |
| Hand-rolled SAH BVH      | Backup  | Best traversal quality for rays. ~600 LoC of code we'd own and maintain — not worth it before we know we need ray traversal. |
| `parry3d::Qbvh`          | Reject  | Drags all of parry + nalgebra. Only justified if we also use rapier for physics, which we don't.  |
| Loose octree             | Reject  | Simpler but worse traversal at 1–10 k meshes. R-tree dominates for our query mix.   |
| Embree / nanort          | Reject  | Native-only; no wasm path.                                                          |

### 1.2 Why `rstar`

The most common pushback against R-trees in renderers is "they're tuned
for spatial databases, not graphics." For our exact workload — frustum
+ AABB-overlap + future kNN — that critique is mostly folklore.

- **Frustum queries** descend hierarchically via
  [`SelectionFunction`](https://docs.rs/rstar/latest/rstar/trait.SelectionFunction.html).
  `should_unpack_parent(&envelope)` is called on inner-node envelopes,
  so a 6-plane test prunes entire sub-trees the same way a BVH would.
  Pseudocode:

  ```rust
  struct FrustumSel { planes: [Plane; 6] }
  impl SelectionFunction<SceneObj> for FrustumSel {
      fn should_unpack_parent(&self, e: &AABB<[f32;3]>) -> bool {
          aabb_vs_frustum(e.lower(), e.upper(), &self.planes) != Outside
      }
      fn should_unpack_leaf(&self, o: &SceneObj) -> bool {
          aabb_vs_frustum(o.aabb.min, o.aabb.max, &self.planes) != Outside
      }
  }
  let visible = tree.locate_with_selection_function(FrustumSel { planes });
  ```

- **3D AABBs are zero glue.** `rstar::Point` is implemented for
  `[f32; 3]`, `Envelope` for `AABB<P>`, and we wrap our entity ids in
  the ready-made `primitives::GeomWithData<AABB<[f32;3]>, MeshKey>`.
  Conversion to/from `glam::Vec3` is `.to_array()`.

- **wasm-clean.** No `rayon`, no SIMD intrinsics, no platform syscalls.
  Compiles to `wasm32-unknown-unknown` with default features.

- **Richer query API for the engine bridge.** `nearest_neighbor_iter`,
  `locate_in_envelope_intersecting`, `locate_at_point`, and
  `locate_with_selection_function` are all in the crate. We get
  light-culling AABB-overlap (`envelope_intersecting`) and any future
  "K nearest lights" / "AI sight cone" query for free, where on a
  hand-rolled BVH we'd write each one.

### 1.3 Known weaknesses and mitigations

| Weakness                          | Mitigation                                                                |
| --------------------------------- | ------------------------------------------------------------------------- |
| No `refit` operation. Inserts after bulk-load gradually loosen the tree. | Detect dirty-ratio. When > 20 % of leaves moved in the last N frames, full `bulk_load` rebuild. Same effective cost as bvh-crate's refit at the rates we expect. |
| Ray queries lack ordered traversal. | Picking is interactive, not per-frame. Either accept slower picks or add a small bvh-crate ray index when picking ships. Defer until picking actually needs it. |
| Per-frame remove-then-reinsert for animated meshes is noisier than refit. | Bucket "highly-dynamic" meshes (skinned, particle, manually flagged) into a small linear-scan sidecar; only static + moderately-animated meshes go in the R-tree. See § 2.1.3. |

### 1.4 The fallback plan

If `rstar` underperforms in profiling — specifically, if the dirty
overhead at the open-world tier dominates the query savings — fall
back to a hand-rolled SAH BVH (~600 LoC). The `SpatialQuery` trait
shields the call sites from this swap. Treat that as a contingency,
not a likely outcome.

Reference reading the implementer should skim before starting:

- [rstar docs](https://docs.rs/rstar/latest/rstar/) — `RTree`,
  `SelectionFunction`, `RTreeObject`, `Envelope`, `primitives::GeomWithData`.
- Jacco Bikker's TLAS/BLAS series (the renderer is TLAS-only for now).
- [Bevy issue #1333](https://github.com/bevyengine/bevy/issues/1333)
  (validates the broader scene-BVH-for-culling pattern).

---

## 2. Architecture overview

Before the cluster-by-cluster steps, this is the shape of the final
system so the steps fit into a known target.

### 2.1 New module: `crate::scene_spatial`

```
crates/renderer/src/scene_spatial/
├── mod.rs              // public surface
├── index.rs            // RTree wrapper, dirty tracking, rebuild policy
├── query.rs            // SpatialQuery trait + concrete query types
├── frustum_selector.rs // SelectionFunction impl for Frustum
└── tests.rs            // unit tests for the wrapper (not the RTree itself)
```

Public surface (sketch):

```rust
pub struct SceneSpatial {
    rtree: rstar::RTree<SceneNode>,
    dynamic: Vec<SceneNode>,           // sidecar for highly-dynamic meshes
    dirty_static_count: u32,           // for rebuild-policy heuristics
    static_count: u32,
    pending_rebuild: bool,
}

pub struct SceneNode {
    pub aabb: Aabb,
    pub mesh_key: MeshKey,
    pub flags: SceneNodeFlags,         // cast_shadows, receive_shadows, hidden, hud, dynamic
}

impl SceneSpatial {
    pub fn insert(&mut self, node: SceneNode);
    pub fn update(&mut self, mesh_key: MeshKey, aabb: Aabb);
    pub fn remove(&mut self, mesh_key: MeshKey);
    pub fn set_dynamic(&mut self, mesh_key: MeshKey, dynamic: bool);
    pub fn rebuild_if_needed(&mut self);  // called once per frame after update_world

    // Queries
    pub fn query_frustum<'a>(&'a self, f: &Frustum, filter: NodeFilter) -> impl Iterator<Item = &'a SceneNode>;
    pub fn query_envelope<'a>(&'a self, aabb: &Aabb) -> impl Iterator<Item = &'a SceneNode>;
    pub fn nearest<'a>(&'a self, point: Vec3) -> Option<&'a SceneNode>;
    pub fn kth_nearest<'a>(&'a self, point: Vec3, k: usize) -> impl Iterator<Item = &'a SceneNode>;
}
```

`NodeFilter` is a small struct: `{ require_cast_shadows: bool,
exclude_hud: bool, exclude_hidden: bool }`. Each call site fills it
in once; the iterator handles the filtering and the AABB-frustum
test, so callers see only the surviving nodes.

### 2.2 `SpatialQuery` trait — the engine-bridge boundary

```rust
pub trait SpatialQuery {
    fn query_frustum(&self, f: &Frustum, filter: NodeFilter) -> Vec<MeshKey>;
    fn query_envelope(&self, aabb: &Aabb) -> Vec<MeshKey>;
    fn nearest(&self, point: Vec3) -> Option<MeshKey>;
}
```

`AwsmRenderer` implements it; external crates depend on the trait, not
the concrete `SceneSpatial`. Return shape is owned `Vec<MeshKey>`
(not iterators) at the trait boundary — outside callers don't need
the borrow lifetime and a `Vec` is friendlier across crate boundaries.

For the renderer's own internal call sites (geometry pass, shadow
pass) bypass the trait and use the borrowing iterator API directly to
avoid the allocation per query.

### 2.3 Lifecycle hooks

Single source of truth: the SceneSpatial mirrors `mesh.world_aabb`.

- **Insert**: every `Meshes::insert` / `insert_public` path
  ([`meshes.rs:596,632`](../../crates/renderer/src/meshes.rs)) ends by
  calling `scene_spatial.insert(SceneNode { … })`.
- **Update**: `Meshes::update_world`
  ([`meshes.rs:1119-1186`](../../crates/renderer/src/meshes.rs)) is the
  one place world AABBs are recomputed. After it writes
  `mesh.world_aabb = world_aabb;` it must also call
  `scene_spatial.update(mesh_key, aabb)`. This is the load-bearing hook.
- **Remove**: `Meshes::remove_mesh` /
  `remove_meshes_by_transform_key` ([`meshes.rs:133,157`](../../crates/renderer/src/meshes.rs))
  call `scene_spatial.remove(mesh_key)`.
- **Flag flips**: `set_mesh_hidden` / `set_mesh_hud` /
  `set_cast_shadows` update the node's flags but don't reinsert.

### 2.4 Tier knobs

| Knob                                  | 1 k tier              | 10 k tier                                  |
| ------------------------------------- | --------------------- | ------------------------------------------ |
| Initial RTree creation                | `RTree::new()`        | `RTree::bulk_load(initial_nodes)`          |
| Dynamic-bucket cutoff                 | flagged manually      | auto-flag when avg AABB delta > 10 %/frame |
| Rebuild threshold                     | every 1 000 dirties   | every `0.2 * static_count` dirties         |
| Per-view filter struct                | shared, single alloc  | shared, single alloc                       |

The "tier" isn't a hard mode switch — it's just which constants we
tune as scenes grow. Codify the knobs in `SceneSpatial::Config` so
they're discoverable.

---

## 3. Cluster 1 — Spatial structure + per-view culling

The keystone work. Everything else either depends on this or is
optional polish.

### 3.1 Step 1.1 — Add the `rstar` dependency and `SceneNode` type

**Files:**

- `crates/renderer/Cargo.toml` — add `rstar = "0.12"` (or current).
- `crates/renderer/src/scene_spatial/mod.rs` — create the module
  skeleton.
- `crates/renderer/src/lib.rs` — `pub mod scene_spatial;`.

**Acceptance:** `cargo check --target wasm32-unknown-unknown -p
awsm-renderer` passes with the module empty, the dep added, and the
`SceneNode` struct + `Bounded` (via `GeomWithData`) wired through
the `rstar::RTree` type alias.

### 3.2 Step 1.2 — Implement `SceneSpatial` with insert / update / remove

**Files:**

- `crates/renderer/src/scene_spatial/index.rs` — main impl.
- `crates/renderer/src/scene_spatial/query.rs` — `NodeFilter`,
  `SpatialQuery` trait.

**Notes:**

- `rstar` lacks an in-place "update" — implement it as
  `remove(mesh_key) → insert(new_aabb)`. Use the GeomWithData's data
  field (`MeshKey`) to find the node.
- `remove` requires the *exact* envelope to locate the leaf. Cache
  the last-known AABB per mesh in a `SecondaryMap<MeshKey, Aabb>` on
  `SceneSpatial` so `update` and `remove` can rebuild the old
  envelope for lookup. Do **not** rely on `mesh.world_aabb` being the
  *previous* value at call time — it may already be the new one.
- Add the `dynamic` sidecar (a `Vec<SceneNode>`) and the
  `set_dynamic` API, but leave the auto-flagging heuristic for step
  1.6.

**Acceptance:** unit tests in `scene_spatial/tests.rs` cover insert,
update (verifies old envelope removed), remove, and the
dynamic-bucket transition. Tests are pure CPU and run on the host
target — no `wasm-bindgen-test` needed.

### 3.3 Step 1.3 — `FrustumSel` selection function

**Files:**

- `crates/renderer/src/scene_spatial/frustum_selector.rs`

The current [`Frustum`](../../crates/renderer/src/frustum.rs) has
private `Plane` fields. Expose them (`pub(crate)` enough for the
selector). The selector tests each inner-node envelope and leaf AABB
the same way `Frustum::intersects_aabb` does today; the only new
work is wrapping rstar's `[f32; 3]` corners back into `Aabb` for the
test.

**Acceptance:** unit test that inserts 100 random AABBs, builds a
frustum from a known view-projection, and asserts that the
`SceneSpatial::query_frustum` set is **exactly** equal to the
linear-scan `Frustum::intersects_aabb` set for the same inputs.
Parity is the bar — performance comes later.

### 3.4 Step 1.4 — Wire `SceneSpatial` into the `AwsmRenderer` lifecycle

**Files:**

- `crates/renderer/src/lib.rs` — add `pub scene_spatial:
  SceneSpatial` to `AwsmRenderer`.
- `crates/renderer/src/meshes.rs:1119` — at the bottom of the
  `update_world` loop, immediately after `mesh.world_aabb =
  world_aabb`, call `scene_spatial.update(mesh_key, aabb.clone())`.
  `update_world` takes `&mut self` but does not currently see
  `SceneSpatial`; pass it in as an extra parameter and update the
  one caller in [`transforms.rs:33`](../../crates/renderer/src/transforms.rs).
- `crates/renderer/src/meshes.rs:596,632` — every public `insert*`
  path must also insert into `scene_spatial`. Same trick: pass it
  in.
- `crates/renderer/src/meshes.rs:133,157` — every `remove*` path
  removes from `scene_spatial`.
- `update_all` in [`update.rs`](../../crates/renderer/src/update.rs)
  — after `update_transforms`, call `scene_spatial.rebuild_if_needed()`
  exactly once.

**Acceptance:** running the editor with logging enabled, the
spatial index's leaf count equals
`meshes.iter().filter(|m| m.world_aabb.is_some()).count()` at every
steady state. Add a `debug_assertions`-only invariant check inside
`SceneSpatial::rebuild_if_needed` that logs a warning if the counts
diverge.

### 3.5 Step 1.5 — Replace the geometry-pass linear sweep

**Files:**

- `crates/renderer/src/renderable.rs:49-84`

Replace the existing `for (mesh_key, mesh) in self.meshes.iter()
.filter(...)` block with:

```rust
let visible: Vec<(MeshKey, &Mesh)> = match &frustum {
    Some(f) => self.scene_spatial
        .query_frustum(f, NodeFilter::camera_default())
        .filter_map(|node| self.meshes.get(node.mesh_key).map(|m| (node.mesh_key, m)))
        .collect(),
    None => self.meshes.iter().filter(|(_, m)| !m.hidden).collect(),
};
```

Then continue with the existing classification (`hud / transparent /
opaque`) and sort.

**Acceptance:**

- Visual parity. Run the editor; geometry pass output is
  pixel-identical to the previous build for the existing test
  scenes. (Run `task render-test` if there's a snapshot harness; if
  not, just side-by-side the demo scenes.)
- The mesh count drawn drops correctly when the camera turns away
  from the scene.

### 3.6 Step 1.6 — Replace the shadow-pass per-view linear sweep

**Files:**

- `crates/renderer/src/shadows/render_pass.rs:125-144`

Replace the per-mesh `for (mesh_key, mesh) in ctx.meshes.iter()` +
inline frustum test with:

```rust
let shadow_frustum = Frustum::from_view_projection(view.view_projection);
let filter = NodeFilter::shadow_caster();  // cast_shadows && !hidden && !hud
for node in ctx.scene_spatial.query_frustum(&shadow_frustum, filter) {
    let mesh_key = node.mesh_key;
    let mesh = match ctx.meshes.get(mesh_key) { Ok(m) => m, _ => continue };
    // existing draw code
}
```

Also fix the cascade-fit `caster_aabbs_scratch` at
[`shadows/state.rs:1046-1055`](../../crates/renderer/src/shadows/state.rs):
the same `query` (without a frustum, full world) gives the
caster list. This subsumes "Cluster 1.2: cached caster-AABB list"
from the old plan — no separate dirty-tracked vec needed once the
spatial index exists.

**Acceptance:**

- Shadows render visually identical.
- Frame time at the 1 k-mesh tier drops measurably on scenes with
  many shadow views. Capture a before/after `tracing` span for
  `record_shadow_pass`; the linear scan is ~O(mesh_count ×
  view_count), the BVH path is ~O(visible_per_view × view_count).

### 3.7 Step 1.7 — Dynamic-bucket sidecar and rebuild policy

Heavily-animated meshes (skinned characters, particle quads,
manually flagged "dynamic" objects) should not be touched on
`update_world` for free. Add:

- A `flag_dynamic` API on `AwsmRenderer` that downstream code calls
  for known animated meshes (skinned mesh insert paths can call it
  automatically — search `mesh.instanced || mesh.skin_key.is_some()`
  patterns in [`meshes.rs`](../../crates/renderer/src/meshes.rs) and
  set the flag).
- In `SceneSpatial::update`, route updates to the `dynamic` sidecar
  if the flag is set; the sidecar is iterated linearly during
  `query_frustum`.
- `rebuild_if_needed`: full `RTree::bulk_load` rebuild when
  `dirty_static_count > config.rebuild_threshold` *or* every 600
  frames (whichever first). At the 1 k tier the threshold is 200; at
  10 k it's 2 000.

**Acceptance:** scenes with a 100-bone skinned character + 1 k
static meshes show no per-frame R-tree churn (verify with a
`tracing` counter on `SceneSpatial::update` static-vs-dynamic
branches).

### 3.8 Step 1.8 — Cube-face per-axis culling verification

Was item 1.3 in the old plan. After Cluster 1 is in, profile cube
shadow rendering and confirm that the per-face frustum naturally
excludes the other 5 faces' geometry as expected. If profiling shows
that's already free (it likely is — the frustum is 90° per face), no
action. If a fast-path major-axis test would help, add it as a
short-circuit before the full frustum check inside
`FrustumSel::should_unpack_parent`.

**Acceptance:** documented profile delta in this file's appendix
when complete; or "verified, no action needed."

---

## 4. Cluster 2 — Light + render scheduling

Depends on Cluster 1 only for item 4.1.

### 4.1 Step 2.1 — Per-mesh light lists (visibility-buffer-native)

**The big idea.** In a traditional forward+ or clustered-deferred
renderer, light culling produces *per-tile* (or per-cluster) light
lists. That maps well when pixels in a tile have no shared identity.
We have stronger information: the visibility buffer tells us the
**mesh** for every shaded pixel. So instead of culling lights against
screen tiles, cull them against **meshes** — at shading time, each
pixel reads its mesh's light list from a flat indexed array.

This is strictly tighter than tile/cluster culling: a tile containing
"wall + empty air + spotlight volume" still binds the spotlight; a
per-mesh list binds it only to the wall. It is also cheaper to build
(one BVH overlap query per light, fully CPU-side, no compute pass)
and shares its work with the shadow caster culling done in Cluster 1.

The existing light culling stub at
[`render_passes/light_culling/render_pass.rs:30`](../../crates/renderer/src/render_passes/light_culling/render_pass.rs)
(`fn render` is a TODO) is **not** a GPU compute pass in this
design — it's a thin CPU pre-pass that uploads two GPU-side buffers
and exits.

#### 4.1.1 Data shape

Two GPU storage buffers, both indexed by mesh:

```wgsl
struct MeshLightSlice { offset: u32, count: u32 };

@group(1) @binding(2) var<storage, read> mesh_light_slices: array<MeshLightSlice>;
@group(1) @binding(3) var<storage, read> mesh_light_indices: array<u32>;
```

- `mesh_light_slices[mesh_meta_offset_normalised]` → `{offset, count}`
- `mesh_light_indices[offset .. offset+count]` → indices into the
  existing `lights` storage buffer.

`mesh_meta_offset_normalised` is `material_meta_offset /
META_SIZE_IN_BYTES` — the same divisor already used by the material
shader at
[`compute.wgsl:179`](../../crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/compute.wgsl)
to fetch `material_mesh_metas[…]`. We reuse the same index space so
the slice lookup is `mesh_light_slices[meta_index]` with no extra
indirection.

#### 4.1.2 CPU build path

Build the lists once per frame from the BVH, sharing the work with
the shadow caster cull:

```rust
// Inside AwsmRenderer::record_frame, after update_world + BVH refit
// and BEFORE shadow + geometry passes:

for (light_key, light) in self.lights.iter_active_punctual() {
    let bounds = light_world_aabb(light); // sphere/cone → AABB
    let affected = self.scene_spatial
        .query_envelope(&bounds)
        .map(|node| node.mesh_key)
        .collect::<Vec<_>>();
    self.light_mesh_buckets.insert(light_key, affected);
}

// Transpose: per-mesh list.
self.mesh_light_lists.rebuild_from(&self.light_mesh_buckets);

// Upload both buffers via existing DynamicStorageBuffer machinery.
```

The same `light_mesh_buckets` map feeds shadow caster culling. There
is exactly one BVH `query_envelope` per active light, then a single
transpose.

#### 4.1.3 Shader change

The light loop at
[`shared_wgsl/lighting/lights.wgsl:167`](../../crates/renderer/src/render_passes/shared/shared_wgsl/lighting/lights.wgsl)
changes from a flat array walk to a sliced walk:

```wgsl
// `meta_index` is computed once at the top of `main` (compute.wgsl)
// from `material_meta_offset / META_SIZE_IN_BYTES` and passed in.
let slice = mesh_light_slices[meta_index];
for (var i = 0u; i < slice.count; i = i + 1u) {
    let light = get_light(mesh_light_indices[slice.offset + i]);
    // ...existing per-light shading...
}
```

Directional lights (no bounds) live in a small *global* prefix that
every mesh implicitly includes — at slice build time, prepend the
directional light indices to every mesh's bucket, or maintain a
separate `global_light_indices` array walked in addition to the
per-mesh slice.

#### 4.1.4 Oversized-mesh fallback

A few mesh archetypes have AABBs so large that "lights overlapping
my AABB" approaches "all lights" — terrain chunks, skybox proxies,
ocean planes. Two options:

- **(Preferred)** Encourage authors to split such meshes into
  spatial sub-meshes; this is good practice anyway and pays off for
  frustum culling too.
- **(Fallback)** Flag oversized meshes (e.g., AABB diagonal >
  `oversized_threshold`) and route them through a **secondary
  tile/cluster** light list. The shader picks which list to read
  based on a per-mesh flag in `material_mesh_metas`. The cluster
  path is the standard Olsson 2012 design; treat it as a backup
  that's only built when at least one oversized mesh is present in
  the frame.

#### 4.1.5 Files

- `crates/renderer/src/render_passes/light_culling/render_pass.rs`
  — replaces the TODO with a `prepare(&mut self, ctx)` method that
  runs CPU-side and uploads the two storage buffers. The `render`
  method becomes a no-op (no GPU pass).
- `crates/renderer/src/render_passes/light_culling/bind_group.rs`
  — adds `mesh_light_slices` + `mesh_light_indices` as group(1)
  bindings shared with the material pass.
- `crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/bind_groups.wgsl`
  — adds the two new bindings.
- `crates/renderer/src/render_passes/shared/shared_wgsl/lighting/lights.wgsl`
  — rewrites the light loop.
- `crates/renderer/src/lights.rs` — adds `iter_active_punctual()`
  and `light_world_aabb` helpers.

#### 4.1.6 Tiering and acceptance

- **Tier:** load-bearing at the 10 k-mesh / 50+ light scale.
  Nice-to-have at 1 k (≤ 8 lights walks fine flat). Ship it once
  scenes exceed ~16 active punctual lights or scenes that cause
  measurable shading-pass time.
- **Acceptance:**
  - Visual parity at 1, 2, 4, 8 lights against a reference snapshot.
  - At 64 lights spread across the scene, the per-pixel average
    `slice.count` (instrument via a debug buffer) is materially
    below `n_lights` for a typical camera angle.
  - Frame time on a 4 K viewport with 64 lights drops measurably vs.
    flat walk.

#### 4.1.7 Sub-steps

- **2.1.a:** Build `light_mesh_buckets` CPU-side via
  `query_envelope` and add a debug overlay showing per-light affected
  mesh count. Shader still walks flat. Lands alone; saves shadow
  setup cost only.
- **2.1.b:** Add the two storage buffers and bind-group entries; do
  the transpose; upload.
- **2.1.c:** Switch the shader loop.
- **2.1.d:** Oversized-mesh fallback (only build the cluster path
  conditionally, when at least one flagged oversized mesh is
  present).

### 4.2 Step 2.2 — 2D shadow per-tile clearing

The throttle logic at
[`shadows/state.rs`](../../crates/renderer/src/shadows/state.rs)
(search for `is_cube`) disables 2D-view throttling because
`LoadOp::Clear` is attachment-wide.

Two paths; **pick (a)**:

- **(a) Cascade texture array (recommended):** allocate the
  directional cascade atlas as a 2D texture *array*, one layer per
  cascade. Each layer has its own view; per-layer clear means a
  not-this-frame cascade keeps its prior layer's contents and skips
  both the depth pass and the EVSM compute.

  Touchpoints: cascade allocation in
  [`shadows/state.rs`](../../crates/renderer/src/shadows/state.rs)
  (atlas-place loop near line 1082) shifts from a packed atlas to
  per-cascade layers; the geometry/lighting shader that samples
  cascade depth needs the array-index path.

- **(b) Manual tile-local clear quad:** smaller change, smaller win.
  Skip unless (a) turns out to be infeasible.

**Acceptance:** with a directional light's far cascade configured
for `update_period = 4`, three frames out of every four show no
depth-pass work for that cascade in the render-pass timeline.

### 4.3 Step 2.3 — EVSM cascade batching / skip-unchanged

Once 2.2 lands, the `EvsmPass` queue at
[`shadows/evsm.rs`](../../crates/renderer/src/shadows/evsm.rs)
trivially gets an "if cascade.didnt_render_this_frame, skip" guard.

**Acceptance:** with throttling on, EVSM dispatches scale with
rendered-cascade count, not configured-cascade count.

### 4.4 Step 2.4 — Coarse light-space binning (skip)

Was Cluster 2.4 in the old plan. With `SceneSpatial` in place, this
approach is strictly dominated. **Mark as obsolete; do not implement.**

---

## 5. Cluster 3 — Authoring + debug UX

These are independent of Cluster 1 and can land anytime. Ship them
when an author hits the relevant pain.

### 5.1 Step 3.1 — Shadow debug views

A single new editor panel (`ShadowDebugPanel`) with five subviews,
each toggled independently. All five draw from data the renderer
already exposes; no renderer change needed beyond exposing one or
two read-only accessors.

| Subview                     | Source data                                                                  |
| --------------------------- | ---------------------------------------------------------------------------- |
| Atlas occupancy             | `Shadows::records()` → per-view `atlas_rect` + light name                    |
| Cube-slot ownership         | `Shadows::cube_slots`                                                        |
| Per-light descriptor index  | `Shadows::params.get(light_key)`                                             |
| Cascade splits              | output of `cascade::fit_cascades` — record + return alongside the cascade rects |
| Throttled vs rendered badge | per-view `should_render` flag                                                |

**Files:**

- `crates/editor/...` — new panel.
- `crates/renderer/src/shadows/state.rs` — expose accessors if not
  already public.

### 5.2 Step 3.2 — Cascade-split editor overlay

Sub-feature of 3.1. The cascade-split planes are already computed by
`cascade::fit_cascades`. Draw them as world-space line gizmos in the
viewport; add a draggable handle for `cascade_split_lambda`.

### 5.3 Step 3.3 — Stable texel-snap controls

The cascade texel-snap quantum at
[`shadows/cascade.rs::fit_cascade`](../../crates/renderer/src/shadows/cascade.rs)
is implicit (`diameter / resolution`). Expose it as an editor slider
with a tooltip "controls shadow swimming under specific camera
motion." No behavioural change unless the user moves the slider.

---

## 6. Cluster 4 — Product quality features

Independent of Clusters 1–3, but 4.2 lands materially better after
1.1.

### 6.1 Step 4.1 — Quality tiers

Add `pub enum ShadowQualityTier { Low, Medium, High, Ultra }` to
[`shadows/config.rs`](../../crates/renderer/src/shadows/config.rs)
with a preset table:

| Tier   | atlas | cascades | PCF taps | max points | EVSM | SSCS |
| ------ | ----- | -------- | -------- | ---------- | ---- | ---- |
| Low    | 1024  | 2        | 4        | 2          | Off  | Off  |
| Medium | 2048  | 3        | 8        | 4          | LastCascade | Off |
| High   | 4096  | 4        | 16       | 8          | LastTwoCascades | On |
| Ultra  | 8192  | 4        | 16       | 16         | LastTwoCascades | On |

A "Custom" sentinel preserves the existing per-knob authoring path
when the user wants control.

**Acceptance:** changing tier from Low → Ultra in the editor
applies the table verbatim; switching to Custom unlocks the
individual sliders.

### 6.2 Step 4.2 — Importance-based per-light budgets

Auto-scale `resolution`, `cascade_count`, `cube_face_update_rate`
based on the light's screen-space contribution. Lands materially
better with `SceneSpatial` in place because the cheap "is this
light's bounds even in the camera frustum?" check becomes a single
`query_envelope` call.

**Heuristic:**

```text
contribution = light_bounds_overlap_with_camera_frustum
             * intensity
             / (1 + distance_to_camera_squared)
```

Map contribution to a tier from 4.1 per light. Off-screen → Low,
fills-most-of-screen → Ultra.

### 6.3 Step 4.3 — True cube PCSS

The current cube `Pcss` is a widened-`Soft`. The honest fix is to
add a 2D-array depth view of the cube pool, write the face-projection
math in WGSL, and do a real blocker search.

**Files:**

- `crates/renderer/src/shadows/state.rs` — add the 2D-array view at
  cube-pool creation.
- New WGSL helper in
  [`shared_wgsl/lighting/`](../../crates/renderer/src/render_passes/shared/shared_wgsl/lighting/)
  for the face-projection math.

**Effort:** medium-large; defer until the current approximation is
visibly insufficient.

### 6.4 Step 4.4 — Spot PCSS scale tuning

Tiny: multiply the authored `pcss_penumbra_scale` by `tan(outer_angle
* 0.5)` inside the spot PCSS sampler in the lighting WGSL. Done in
one line plus authoring docs note.

---

## 7. Cluster 5 — Robustness scaffolding

Background polish. Do opportunistically when touching adjacent
code.

### 7.1 Step 5.1 — Shadow render-pass integration tests

**Out of scope.** The renderer is web-sys-only by design (not
`wgpu`), so a native headless harness would require a backend-trait
layer the project doesn't want. `wasm-bindgen-test` on headless
Chrome doesn't have reliable WebGPU support on Linux CI. The
`debug_assert_eq!` invariants in §7.2 cover the same descriptor /
view bookkeeping bugs at dev-build time, which is the level of
robustness we want here.

### 7.2 Step 5.2 — Descriptor + view bookkeeping invariants

Inside `Shadows::write_gpu` at function exit, add a
`debug_assertions`-only check:

```rust
debug_assert_eq!(
    self.records.values().map(|r| r.views.len()).sum::<usize>(),
    self.active_view_count as usize,
);
debug_assert_eq!(
    self.records.len(), // or however descriptors are counted
    self.active_descriptor_count as usize,
);
```

Catches off-by-one regressions immediately.

### 7.3 Step 5.3 — Lights/Shadows desync guard for kind changes

`Lights::update` can change a light's KIND (Directional → Point),
which moves it out of the shadow allocator's cube-slot expectations.
Two options; **pick (b)**:

- (a) Forbid kind changes via `update` (panic in debug, warn in
  release).
- (b) Detect the kind change inside `update_light` and call
  `Shadows::on_light_removed` then `on_light_added`. ~10 lines.

**Files:** [`crates/renderer/src/lights.rs:236`](../../crates/renderer/src/lights.rs)
(the `update` method).

---

## 8. Cluster 6 — Visibility-buffer-native optimizations

Each item in this cluster exploits the fact that we already have a
visibility buffer and a tile-aligned material compute pass. None of
them require new external dependencies; all of them depend on the
BVH (Cluster 1) or on each other.

### 8.1 Step 6.1 — Material classify + indirect dispatch

The material pass at
[`material_opaque/render_pass.rs:74-91`](../../crates/renderer/src/render_passes/material_opaque/render_pass.rs)
walks `renderables` and, **per unique material pipeline key**,
dispatches a **screen-wide** compute (`width/8 × height/8` workgroups).
The shader then early-outs on pixels whose `material_meta_offset`
points to a different material. With N materials in frame this is N×
the workgroup launches and a flood of wasted lanes.

**The fix**: classic vis-buffer material classify.

1. **Classify pass** (new compute pass, runs after geometry, before
   material_opaque): per 8×8 tile, scan the visibility buffer and
   produce a per-material bitmask of which tiles contain at least one
   pixel of that material. Concretely, build a storage buffer
   `material_tile_buckets[material_index] -> Vec<tile_id>` and an
   `IndirectDispatchArgs` buffer per material pipeline key sized to
   the number of populated tiles.
2. **Material pass change**: each material dispatch becomes an
   `dispatch_workgroups_indirect` reading its own args buffer. The
   shader's `gid` no longer maps directly to a screen pixel — it maps
   to a tile index in the material's bucket, and the workgroup reads
   the tile's `(x, y)` to find its 8×8 region.

**Reference:** Wihlidal, GDC 2016, "Optimizing the Graphics Pipeline
with Compute" — slides 30-50 cover exactly this pattern on Frostbite,
with ~25 % shading-pass cost reduction on representative scenes.

**Coupling with per-mesh light lists (Cluster 2.1)**: free synergy.
Each material's tile bucket is a list of tiles whose pixels share a
material; the **union of mesh light lists** for the meshes in that
material can be precomputed as a per-material light list, smaller
than any single mesh's list and bound once per dispatch. Skip this
union if a per-mesh list is already small enough — at 4–8 lights/mesh
the union doesn't save much.

**Files:**

- New: `crates/renderer/src/render_passes/material_classify/` (or
  fold into `material_opaque/`).
- `crates/renderer/src/render_passes/material_opaque/render_pass.rs`
  — switch from `dispatch_workgroups` to `dispatch_workgroups_indirect`.
- `crates/renderer/src/render_passes/material_opaque/shader/material_opaque_wgsl/compute.wgsl`
  — replace `let coords = vec2<i32>(gid.xy);` with a tile-table
  lookup: `let tile = tile_buckets[material_index][gid.x]; let coords
  = tile.origin + local_thread_offset;`.

**Tier:** load-bearing once material count > ~6. At 1–2 materials,
the win is small.

**Acceptance:**

- Visual parity (the material output is identical; only the dispatch
  shape changes).
- At a fixed 4 K viewport with 8 materials, total compute workgroup
  count across the material pass drops by ≥ 4× (because dispatches
  now cover only their material's tiles rather than the full screen).

### 8.2 Step 6.2 — Coverage-driven skinning skip

Skinned meshes pay a per-frame skinning cost in
[`meshes/skins.rs`](../../crates/renderer/src/meshes/skins.rs)
regardless of whether they were visible last frame. For a crowd
scene that's most of the GPU animation budget.

**The fix**: histogram pixel coverage per mesh during (or right
after) the geometry pass. Use last-frame coverage as a predictor for
this-frame visibility. Meshes with coverage 0 last frame skip
skinning.

**Pass design:**

- New tiny compute pass after geometry: one atomic-add per pixel
  into `mesh_pixel_counts[meta_index]`. ~1 ms on a full-HD viewport
  worst-case; usually well under.
- CPU reads back **last frame's** counts (one-frame latency is fine
  — the renderer already buffers transforms). For any mesh with
  count 0, skip its skinning update this frame; restore on the next
  frame it becomes visible.

**Pop-in mitigation**: use a small grace period (1–2 frames) and a
**conservative** AABB-vs-frustum override — if the BVH says the mesh
is in-frustum and coverage was 0 last frame, still skin it (it's
likely about to become visible behind disocclusion). This keeps the
optimization safe by default and only kicks in for meshes that are
*both* off-screen and coverage-zero.

**Files:**

- `crates/renderer/src/render_passes/coverage/` (new tiny pass).
- `crates/renderer/src/meshes/skins.rs::update_transforms` — add an
  early-return guarded by `mesh_pixel_counts_last_frame[meta_index]
  == 0 && !bvh_says_visible`.

**Tier:** transformative for crowd scenes (~10–100 skinned characters
visible at any time); zero-cost when no skinning is in use.

**Acceptance:**

- Hide a known character behind a wall — verify (via tracing span)
  that its skinning compute is skipped the frame after it goes
  fully out of view.
- Make it walk back into view — verify it skins on the *next* frame
  with no visible pop.

### 8.3 Step 6.3 — Coverage-driven material LOD

Same coverage buffer as 6.2, different consumer. A mesh occupying
< N pixels can be shaded with a cheap material variant
(no parallax, simpler BRDF, no SSCS contribution). Cooperates with
the quality tiers in Cluster 4.1: each tier defines its own
`pixel_threshold_for_cheap_material`.

**Implementation:** add a `cheap_material_key` field to `Mesh`
authored alongside the regular material; in
`AwsmRenderer::collect_renderables`, swap to the cheap variant for
any mesh with last-frame coverage below the threshold.

**Tier:** useful at the open-world tier when individual meshes are
small in screen space (foliage, debris, distant props). Marginal at
the 1 k tier where meshes are large.

### 8.4 Step 6.4 — Decals as a material class

Project-decal rasterization is currently absent (search for it in
[`material_transparent/`](../../crates/renderer/src/render_passes/material_transparent/)
to confirm — none of the existing passes implement projection
decals). When it's added, the vis-buffer architecture suggests
treating decals as a **material class** rather than a separate
forward pass:

- Decals are authored with a target mesh-set (decal-volume vs each
  mesh-AABB overlap test via the BVH).
- In the classify pass, tiles whose pixels belong to a decal-target
  mesh get an additional entry in the decal material's bucket.
- The decal material runs as another `dispatch_workgroups_indirect`
  consumer of the same tile-bucket plumbing as 6.1.

No decal-volume rasterization, no per-light decal stencil — just a
shader pass over the affected pixels reading the decal texture.
Reference: Detroit Become Human / Decima Engine 2017 Siggraph talk.

**Scope:** large but well-bounded. Sits behind 6.1 because the
bucket infrastructure is shared.

### 8.5 Step 6.5 — Per-mesh shadow-receiver filtering

A mesh that no light reaches doesn't need to sample shadow maps at
all. The per-mesh light list from 2.1 trivially gives us this: if
`mesh_light_slices[meta_index].count == 0` for the punctual lights
that have shadow descriptors, skip the shadow-sample branch in the
shader.

**Implementation:** a single 1-bit flag per mesh
(`receives_shadow_from_any_punctual`) bundled into
`mesh_light_slices` (steal a high bit of `offset`). The lighting
shader at
[`lights.wgsl:175-194`](../../crates/renderer/src/render_passes/shared/shared_wgsl/lighting/lights.wgsl)
already has a `receive_shadows` gate; this just AND-s with the
new flag.

**Tier:** small win, free synergy. Lands with 2.1 or right after.

---

## 9. Cluster 7 — GPU-driven culling and HZB

Long-horizon work. These items take over when the CPU `rstar` BVH
hits its query-throughput wall — typically at the >10 k mesh end of
the plan. None are needed for the 1 k or 10 k tier *unless* CPU
profiling shows BVH traversal dominating the frame.

### 9.1 Step 7.1 — Hierarchical-Z build from visibility depth

The geometry pass already writes a final-resolution depth buffer
([`geometry/render_pass.rs:107-114`](../../crates/renderer/src/render_passes/geometry/render_pass.rs)).
Build a mip chain via successive `min`-reduce compute passes;
`log2(width)` dispatches, each reading the previous mip with shared
memory. Output is `hzb_view` — a texture with mip 0 = depth, mip N =
single coarse value.

**Files:** new
`crates/renderer/src/render_passes/hzb/` with bind group, pipeline,
compute shader (~80 lines WGSL).

**Tier:** prerequisite for 7.2 and 7.3. Cheap to build (~0.2 ms at
4 K).

**Acceptance:** sampling `hzb_view` at mip M returns the conservative
min depth over the corresponding 2^M × 2^M region. Unit test with a
synthetic depth pattern.

### 9.2 Step 7.2 — Two-phase GPU occlusion culling

After 7.1, add two-phase occlusion (Sousa / Doom; Karis / Nanite;
Bevy's mesh-let occlusion):

1. **Phase 1:** draw the set of instances that were visible *last*
   frame. Build HZB from the resulting depth.
2. **Phase 2:** for every other instance (BVH-visible but not in
   phase 1 set), test its screen-space AABB against the HZB. Survivors
   draw into the depth buffer; the final color attachment receives
   both phases.

**Interaction with `rstar` BVH (Cluster 1)**: complementary, not
redundant. The CPU BVH culls frustum + (optionally) distance; the
HZB culls **occlusion** that the BVH can't see (wall A occludes wall
B). Pipeline: CPU `query_frustum` → GPU instance list → HZB test →
indirect draw.

**WebGPU gotcha**: `multi_draw_indirect` is **not** in core WebGPU
(experimental extension only — Chrome flag). Without it we issue one
`drawIndirect` per material bucket, which means CPU-side material
bucketing must happen first. That's fine — it's the same bucketing
Cluster 6.1's classify pass already does, just at draw rather than
shade time.

**Tier:** only when occlusion is significant (dense urban scenes,
interiors with lots of geometry behind walls). Skip for open-world
plains.

### 9.3 Step 7.3 — GPU instance compaction and indirect draw

The final step of the GPU-driven pipeline. Move the entire instance
list to a storage buffer; a compute pass does frustum + HZB + LOD
selection and *appends* survivors to per-material indirect-draw-args
buffers. `drawIndexedIndirect` per material bucket.

**Crossover point**: CPU wins below ~2 k draws if BVH is good. GPU
wins above ~5 k, dramatically above 10 k. Don't migrate until
profiling shows CPU traversal time exceeding ~2 ms.

**Tier:** open-world only, and only when 7.2 isn't enough.

**Files:** large. New
`crates/renderer/src/render_passes/instance_cull/` with the compute
pass + indirect-draw-args ring buffers. `Meshes` needs an instance
table layout that the compute shader can index by instance id.

---

## 10. Cluster 8 — Buffer upload and animation polish

Independent of all of the above. Items here are small, opportunistic.

### 10.1 Step 8.1 — Dirty-range coalescing on `DynamicStorageBuffer`

[`buffer/dynamic_storage.rs`](../../crates/renderer/src/buffer/dynamic_storage.rs)
(verify path) uploads via `write_buffer_with_dirty_ranges`. Confirm
it coalesces adjacent dirty ranges; if not, sort and merge before
upload. A single `writeBuffer` per coalesced range is markedly faster
than many small writes on Chrome's WebGPU implementation.

### 10.2 Step 8.2 — Persistent staging buffer ring

For per-frame uploads, allocate a triple-buffered staging buffer ring
sized to the 99-th-percentile frame upload bytes; reuse rather than
allocating per upload. Reduces GC pressure and `mapAsync` stalls.

### 10.3 Step 8.3 — Skinning update LOD by camera distance

Pair with 6.2. A character far from the camera doesn't need 60 Hz
skinning — 30 Hz or 15 Hz is fine. Add a per-mesh `skin_update_period`
defaulted to 1, autoselected by distance (or by quality tier).

---

## 11. Picking order and dependencies

```
Step 1.1 ─┬─ 1.2 ─┬─ 1.3 ─┬─ 1.4 ─┬─ 1.5 (camera cull)
          │       │       │       ├─ 1.6 (shadow cull) ─┬─ 1.7 (dynamic sidecar)
          │       │       │       │                      └─ 1.8 (cube verify)
          │       │       │       │
          │       │       │       └─ 2.1.a (CPU light AABB-overlap, share with 1.6)
          │       │       │            └─ 2.1.b → 2.1.c → 2.1.d (per-mesh light lists)
          │       │       │                 ├─ 6.1 (material classify + indirect)
          │       │       │                 │    └─ 6.4 (decals as material class)
          │       │       │                 └─ 6.5 (per-mesh shadow filter)
          │       │       │
          │       │       └─ 4.2 (importance budgets)
          │
          ├─ (parallel) 2.2 → 2.3
          ├─ (parallel) 3.1 → 3.2, 3.3
          ├─ (parallel) 4.1
          ├─ (parallel) 4.3, 4.4
          ├─ (parallel) 5.1, 5.2, 5.3
          │
          ├─ (after 1.5) 6.2 (coverage skinning skip) → 6.3 (material LOD)
          │
          └─ (long-horizon, after 1.7) 7.1 → 7.2 → 7.3
                                        └─ 8.1, 8.2, 8.3 (parallel)
```

**Critical path (Cluster 1):** 1.1 → 1.2 → 1.3 → 1.4 → 1.5 → 1.6 →
1.7. One person, ~5–8 working days.

**Then in priority order** (where load-bearing for the renderer's
long-term shape):

1. **2.1 (per-mesh light lists)** — the vis-buffer-native lighting
   path. Highest single-step performance lever beyond the BVH itself.
2. **6.1 (material classify + indirect)** — multiplies 2.1's win and
   removes the screen-wide-dispatch tax. Mechanical change once 2.1
   lands.
3. **4.2 (importance budgets)**, **6.5 (per-mesh shadow filter)** —
   small follow-ups that fall out of the work above.
4. **6.2, 6.3** (coverage skip + material LOD) — independent; ship
   when crowd scenes or distant-prop counts pressure the budget.
5. **2.2 → 2.3** (cascade array + EVSM skip) — independent shadow
   throttle work. Ship in any cluster-1-following window.
6. **4.1** (quality tiers) — product/release-readiness; not perf.
7. **3.x debug, 5.x robustness** — background polish.
8. **7.1 → 7.2 → 7.3** — long-horizon GPU-driven path. Defer until
   CPU BVH traversal exceeds ~2 ms in profiling.

Cluster 1 is **blocking** for any open-world push and for everything
in Cluster 2, 6, and 7. Cluster 2.1 is **blocking** for 6.1 and
6.5. Cluster 6.1 unlocks 6.4 (decals). Everything else ships
independently.

---

## 12. Open questions for the implementer

Resolve these before writing code in steps 1.4 / 2.1 / 6.1:

1. **`update_world` signature.** Threading `SceneSpatial` through
   `Meshes::update_world` makes the borrow story awkward because
   `Meshes` and `SceneSpatial` would both live on `AwsmRenderer`.
   Two patterns:
   - Pull the world-AABB compute logic *out* of `Meshes` into an
     `AwsmRenderer::update_world` method that owns both `&mut self.meshes`
     and `&mut self.scene_spatial`.
   - Have `Meshes::update_world` return a `Vec<(MeshKey, Aabb)>` of
     changes; the renderer applies them to both `meshes` and
     `scene_spatial` in a second pass.

   **Prefer the first pattern** — fewer allocations, clearer
   ownership.

2. **Light-bounds AABB.** Point and spot lights have a natural
   bounding sphere; directional lights have infinite bounds.
   Confirm the `query_envelope` light-culling pre-pass either:
   (a) special-cases directional lights (always-visible), or
   (b) clips the directional light's frustum to the camera frustum
   and queries that envelope.

3. **MeshKey lifetime.** `rstar` stores `MeshKey` by value inside
   `GeomWithData`. Confirm `MeshKey: Copy + Eq + Hash` (it should
   already; it's a `slotmap::Key`).

4. **Tier auto-detection.** Should the engine auto-pick a tier based
   on adapter info / scene mesh count, or always default to Medium
   and let the editor surface the choice? **Default to Medium**, let
   the host choose, but don't make tier detection part of Cluster 4.

5. **Picking integration with the BVH.** If/when picking moves to a
   ray query, decide between (a) adding `nearest_traverse_iterator`
   support by also building a small `bvh-rs` index, or (b) letting
   picks fall back to a linear scan (it's interactive; 10 k AABB-vs-
   ray tests is fine for one-click pick latency).

6. **`mesh_meta_offset` stability across frames.** The vis-buffer
   light-list and material-classify code both index by
   `material_meta_offset / META_SIZE_IN_BYTES`. Confirm with
   [`meshes/meta.rs`](../../crates/renderer/src/meshes/meta.rs) (or
   wherever `MeshMeta` lives) that this index is stable while the
   mesh is alive — if meshes can be moved/compacted within the meta
   buffer between frames, the slice table needs to be rebuilt after
   any compaction. Cheap to do; just needs to be wired.

7. **Oversized-mesh threshold.** Cluster 2.1.4's fallback kicks in
   when a mesh's AABB exceeds an `oversized_threshold` (diagonal or
   light-list-count). Pick a default — provisional: list-count > 16
   *and* AABB-diagonal > 50 m. Tune from profiling.

8. **HUD and transparent paths.** The vis-buffer-native lighting
   path applies to the opaque material pass. The transparent path at
   [`material_transparent/render_pass.rs`](../../crates/renderer/src/render_passes/material_transparent/render_pass.rs)
   has no vis buffer to consult — it must keep the flat-light-array
   walk or invent a per-mesh-binding-time light list. Decide:
   transparent stays flat (simpler; transparent draw counts are
   usually low) or transparent reads `mesh_light_slices` indexed by
   its draw-time mesh key (more uniform but new bind-group plumbing).
   **Default: transparent stays flat.**

9. **Coverage buffer atomic contention.** Cluster 6.2's per-mesh
   atomic-add is contended when many pixels share a mesh. Two
   alternatives: (a) one atomic per workgroup that reduces to a
   single add at the end, (b) the geometry pass's fragment shader
   emits a per-mesh occupancy bit and a tile-scan compute pass
   collects it. **Default: (a)** — simpler and usually under contention
   thresholds.

---

## 13. Out-of-scope explicitly

- **Renderer-physics shared broad-phase.** The trait is the
  boundary; physics may consume it but shall not mutate the index.
  If physics wants a different cadence or invariant, it owns its
  own structure. Revisit this only when both crates have shipped
  their own first.
- **HLOD / impostors / asset streaming.** Lives in
  [`PERFORMANCE_OPEN_WORLD_PLAN.md`](../PERFORMANCE_OPEN_WORLD_PLAN.md).
  Cross-references but does not duplicate.
- **Stochastic light culling (Heitz/Stachowiak).** Needs a denoiser
  to be useful; the denoiser cost dominates at our light counts.
  Park until we have a real >100-lights-per-pixel scene.
- **Mesh shaders / geometry shaders.** WebGPU doesn't expose them.
  Re-evaluate if the spec changes.
- **Variable-rate shading.** Not in core WebGPU. Skip.
- **Multi-draw-indirect** as a load-bearing path (Cluster 7.3 uses
  it). Currently an experimental extension behind a Chrome flag;
  treat as "available eventually" rather than a today-target.

---

## 14. Definition of done (Cluster 1)

The hand-off is "done" when:

- [x] `cargo build --target wasm32-unknown-unknown -p awsm-renderer`
      passes.
- [x] All existing renderer tests pass (84/84; 76 prior + 8 new
      `scene_spatial::tests`).
- [x] `SceneSpatial`'s leaf count equals `meshes.iter().filter(|(_,
      m)| m.world_aabb.is_some()).count()` at steady state (asserted
      in a debug-only invariant inside `update.rs::update_all`).
- [x] Geometry pass and shadow pass both call `query_frustum` for
      culling; no `Frustum::intersects_aabb` calls remain in
      [`renderable.rs`](../../crates/renderer/src/renderable.rs) or
      [`shadows/render_pass.rs`](../../crates/renderer/src/shadows/render_pass.rs).
- [ ] Editor demo scenes render visually identical to pre-change.
      Host-target parity test
      `scene_spatial::tests::frustum_query_parity_with_linear_scan`
      asserts the BVH and linear scan produce identical sets for 100
      random AABBs — visual side-by-side still needs a browser run.
- [ ] A `tracing` span around `record` in shadow pass shows ≥ 2×
      improvement in the 1 k-mesh / 20-view case. Profiling deferred
      to a browser run.

### 14.1 Definition of done (Cluster 2.1 — per-mesh light lists)

- [x] `mesh_light_slices` + `mesh_light_indices` storage buffers are
      uploaded per frame (`MeshLightSlicesGpu` in
      [`light_buckets/gpu.rs`](../../crates/renderer/src/light_buckets/gpu.rs))
      and bound at group(1) bindings 2/3 of the material-opaque compute
      pass via
      [`material_opaque/bind_group.rs::recreate_lights`](../../crates/renderer/src/render_passes/material_opaque/bind_group.rs).
- [x] Lighting WGSL reads the slice; the opaque path now calls
      `apply_lighting_per_mesh` (in
      [`shared_wgsl/lighting/lights.wgsl`](../../crates/renderer/src/render_passes/shared/shared_wgsl/lighting/lights.wgsl))
      with `meta_index = material_meta_offset / META_SIZE_IN_BYTES`. The
      transparent path keeps the flat walk per plan §12 Q8 default.
- [x] Directional lights are correctly applied via the flat-prefix
      walk inside `apply_lighting_per_mesh` (kind == 1 keeps the
      directional, kind != 1 falls through to the slice walk).
- [ ] Visual parity at 1, 2, 4, 8, 64 lights. **Browser-side
      verification still required** — the shader compiles and the data
      shape is correct, but a side-by-side render check vs the
      pre-2.1.c build is the remaining sign-off.
- [ ] Average per-pixel `slice.count` is materially below `n_lights`
      for the test scenes with 64+ lights. **Requires browser-side
      profiling** — `LightMeshBuckets::last_max_bucket` records the
      worst-case bucket size as a coarse proxy.

### 14.2 Definition of done (Cluster 6.1 — material classify)

- [ ] The classify pass produces a per-material tile bucket buffer
      that matches a CPU reference implementation (unit test).
- [ ] Each material's compute dispatch is now indirect.
- [ ] Total workgroup count across the material pass on the 8-material
      test scene drops by ≥ 4×.
- [ ] Visual parity vs pre-change.

---

## 15. Appendix — measured deltas

Fill in after each step lands.

| Step | Before                  | After                       | Notes |
| ---- | ----------------------- | --------------------------- | ----- |
| 1.5  | linear sweep            | BVH query                   | Landed. `renderable.rs:49-89` drives the visible set off `scene_spatial.query_frustum(NodeFilter::camera_default())`. Conservative tail-walk on meshes without world AABB kept. |
| 1.6  | per-view × meshes       | per-view × visible          | Landed. `shadows/render_pass.rs` calls `query_frustum(NodeFilter::shadow_caster())`; `shadows/state.rs::write_gpu` `caster_aabbs_scratch` populated from `iter_filtered(shadow_caster)`. |
| 1.7  | dirty churn             | sidecar churn               | Landed. Skinned + instanced meshes auto-route to the linear-scan dynamic sidecar; tree-static meshes rebuild via `RTree::bulk_load` at 200-dirties / 600-frames cadence. |
| 1.8  | cube cost               | verified / fast-path        | Verified via host-target test `cube_face_frustum_prunes_other_face_geometry`. |
| 2.1  | flat light loop         | per-mesh slice walk         | **Locked in.** 2.1.a/b/c/d all landed: `LightMeshBuckets` rebuilds per frame; `MeshLightSlicesGpu` uploads `mesh_light_slices` + `mesh_light_indices` storage buffers at group(1) bindings 2/3 of the opaque material pass. New `apply_lighting_per_mesh` + `apply_lighting_per_mesh_with_transmission` in `shared_wgsl/lighting/lights.wgsl` gate the punctual walk behind `mesh_light_slices[meta_index]`. Opaque compute + MSAA helper both call the per-mesh variant; transparent stays flat (plan §12 Q8 default). Oversized-mesh detection populates `oversized_meshes()` via the `OVERSIZED_*` thresholds. Browser-side visual parity verification still pending. |
| 2.2  | per-frame cascade       | throttled                   | Deferred — major cascade-atlas layout rework (packed atlas → 2D texture array) with browser-side validation. |
| 2.3  | evsm every cascade      | evsm only for rendered      | Deferred — depends on 2.2's per-layer "didn't render" signal. |
| 4.1  | flat shadow knobs       | preset tier table           | Landed. `ShadowQualityTier { Low, Medium, High, Ultra, Custom }` with `apply_to_config` / `apply_to_light_params` plumbing. |
| 4.2  | uniform light budgets   | importance-scored budgets   | Landed. `AwsmRenderer::refresh_light_importance_budgets` walks shadow-casting lights and maps the `intensity / (1 + dist²)` score to a tier; off-screen → Low, fills-screen → Ultra. |
| 4.3  | widened-Soft cube PCSS  | true cube PCSS              | Deferred per plan §6.3 — current approximation is visibly adequate. |
| 4.4  | uniform spot penumbra   | cone-scaled spot penumbra   | Landed. `shadows/state.rs` spot-light descriptor packing multiplies authored `pcss_penumbra_scale` by `tan(outer_angle * 0.5)`. |
| 5.1  | no integration tests    | (dropped from scope)        | Won't do — renderer is web-sys-only (not `wgpu`); `wasm-bindgen-test` has no reliable WebGPU on Linux CI; §7.2 `debug_assert_eq!`s cover the same bookkeeping bugs. |
| 5.2  | implicit bookkeeping    | debug-assert invariants     | Landed. `Shadows::write_gpu` debug-asserts `records.views.sum == active_view_count` and `records.len == active_descriptor_count` at function exit. |
| 5.3  | kind change → desync    | guarded `update_light`      | Landed. `AwsmRenderer::update_light` detects discriminant change, runs `Shadows::on_light_removed`, reinstates `params`. |
| 6.1  | N × screen-wide compute | N × tile-bucket compute     | Deferred — major WGSL + compute-pass rework needing browser validation. |
| 6.2  | all skinning every frame | only-visible skinning      | CPU consumer landed. `MeshCoverage` table; `Meshes::skin_all_consumers_zero_coverage` skips skin updates when every consumer was zero-coverage last frame. GPU coverage compute pass (atomic-add per pixel) deferred. |
| 6.3  | full material everywhere | cheap-mat at low coverage  | Landed. `Mesh::cheap_material_key` + `cheap_material_pixel_threshold`; `Mesh::effective_material_key`; `collect_renderables` classifies transparency by the effective key. |
| 6.4  | n/a                     | decals as material class    | Deferred — depends on 6.1's bucket infrastructure. |
| 6.5  | always-sample shadows   | mesh-receiver gate          | CPU side landed. `LightMeshBuckets::mark_shadow_receivers` populates a per-mesh "any shadow-caster reaches me" flag consulted via `is_shadow_receiver`. Shader-side OR with the existing `receive_shadows` gate is part of 2.1.c WGSL work. |
| 7.1  | n/a                     | hzb build cost              | Deferred per plan §9 — long-horizon; only needed when CPU BVH dominates. |
| 7.2  | bvh-only cull           | bvh + hzb cull              | Deferred per plan §9. |
| 7.3  | cpu instance loop       | gpu instance compaction     | Deferred per plan §9. |
| 8.1  | many small writes       | coalesced writes            | Verified. `buffer/helpers.rs::write_buffer_with_dirty_ranges` already sorts + coalesces; 4 unit tests pin the merge behaviour. |
| 8.2  | per-upload allocations  | staging ring                | N/A for current arch. `gpu.write_buffer` (WebGPU `queue.writeBuffer`) handles staging internally; explicit ring only matters under `mapAsync` upload path. |
| 8.3  | 60Hz skinning everywhere | distance-LOD'd skinning    | Landed. `Mesh::skin_update_period` field, `AwsmRenderer::set_mesh_skin_update_period(s)_by_distance` helpers, gate inside `Meshes::update_world` via `Skins::update_transforms` predicate. |

---

## 16. Remaining work — implementation queue

This section is the live punch-list for items that still need code,
profiling, or tuning. Completed items have been archived. Each entry
has a **Status** field — update it as you work through. Items are
ordered by priority (highest-leverage first).

**Status values:**
- `not started` — nothing done yet
- `in progress` — actively working it
- `done` — landed; ready to be deleted next cleanup pass
- `blocked: <reason>` — needs something else first

**How to consume this in a fresh session.** Every cluster referenced
below (`2.2`, `6.1`, `7.1`, etc.) has a full implementation spec in
§3–§10 of this document with file paths, signatures, and acceptance
criteria. Read the corresponding plan section before starting a step.
Browser preview is available via the Claude Preview MCP (see launch
config `.claude/launch.json`, server name `scene-editor`).

### Resume sequencing (recommended order for the next sessions)

The list below is the priority order for picking up cold. Each entry
is one focused session — don't try to bundle them; the diffs are
already large.

1. **§16.E1 + §16.E2 storage-budget refactor** (~300 LoC; ~1 session).
   Pack `attribute_indices` + `attribute_data` slice metadata into
   `MaterialMeshMeta` so the opaque main bind group drops from 10/10
   storage bindings to 8/10. Strict prerequisite for §16.7 / §16.8
   (each wants at least one new storage binding on the geometry
   pipeline) and for the dynamic-materials plan's `extras_pool`.
2. **§16.4.B Decals — scene-schema + editor** (~1 session). Adds a
   decal node type to `awsm_scene_schema`, an editor authoring panel
   with the unit-cube gizmo, and the `renderer_bridge` wiring so
   loaded projects call `AwsmRenderer::insert_decal`. The runtime
   path is already live — task **C2** in the chat task list.
3. **§16.7 Two-phase GPU occlusion culling — Phase 1** (~1 session).
   The instance-list storage buffer + GPU cull compute pass + the
   "draw last-frame survivors" geometry-pass split. No `drawIndirect`
   yet; the cull output is consumed by the BVH-survivor-set CPU walk
   so the shape is testable before §16.8.
4. **§16.7 Two-phase GPU occlusion culling — Phase 2 + §16.8 GPU
   instance compaction + indirect draw** (~1–2 sessions). The
   `drawIndirect` rewire of the geometry pass. Per-material
   `drawIndirect` reusing Cluster 6.1's tile-bucket idea for the
   draw-args side.
5. **§16.4.C HZB-tile classify for decals** (~½ session). Tighter
   decal tile coverage via HZB. Lifts the v1 "iterate every decal
   per pixel" cost.
6. **§16.4.D Decal MSAA path** (~½ session). Dedicated
   `decal_color_tex` + composite step so MSAA isn't a silent skip.
7. **§16 tuning items (T1–T7)** (~1 session of authoring + measuring).
   Hand-author the JSON scenes under `assets/world/`, instrument
   with `tracing` spans, fill in §15 numbers.

After all of the above land, archive §16 — its work is done.

### What has shipped (this conversation's session)

All committed to the `optimizations` branch as of `2034023`. Each
commit is a clean checkpoint:

```
682053a  shader split — opaque compute by shader_id
18b6b37  material_classify pass scaffolding
b51503b  material classify + indirect dispatch
5769d3f  docs(dynamic-materials): sync with material classify landing
bac0b1f  decals subsystem — runtime
6eef59c  HZB build from visibility depth (Cluster 7.1)
c60ba56  docs(optimizations): full handoff for resume session
2c03fa9  merged mesh geometry pool (§16.E1 / §16.E2)
def39a7  decals scene-schema + editor authoring (§16.4.B)
2034023  GPU occlusion cull pass — Phase 1 infrastructure (§16.7)
```

### Load-bearing constraints a resume session must respect

- **Web-sys only.** The renderer is intentionally not on `wgpu`; do
  not introduce a backend trait. WGSL changes ship via askama
  templates in `crates/renderer/src/render_passes/*/shader/`.
- **Storage bindings cap at 10/10 today** (the absolute max with
  `with_max_storage_buffers_per_shader_stage`). §16.7 and §16.8 each
  want at least one more — that's why §16.E1 + §16.E2 must land
  first. Do not bump past 10.
- **Each `MaterialShaderId` is its own compute pipeline** — the
  shader-split landed in `682053a`. Adding a new opaque shader
  variant means a new pipeline + a new classify bucket; the
  classify bucket count is fixed at 3 today (PBR / Unlit / Toon)
  and must grow if a 4th shader_id is added. See
  `material_classify/shader/material_classify_wgsl/compute.wgsl`
  for the bit mask + `classify_output` struct that needs extending.
- **Skybox ownership rule.** PBR pipeline owns skybox shading;
  Unlit / Toon early-return on skybox without writing. The decal
  compute follows the same convention — only writes pixels where
  a decal genuinely contributes. Future opaque variants must
  follow this rule or contribute their own dedicated skybox slot.
- **`update_transforms()` is the per-frame entry the editor uses**,
  not `update_all()`. Any new per-frame CPU bookkeeping a resume
  session adds MUST land in `update_transforms` or the editor will
  silently miss it (see history note in this section's archive).
- **Tests count: 100.** Don't drop any; add tests for new pure-CPU
  modules.

---

### Implementation items (code work)

These are real shader/compute-pass changes. Browser preview is
available — boot the scene-editor, add primitives + lights, screenshot
to validate.

#### 16.1 Cluster 2.2 — Cascade texture array

**Status:** `done`

Implementation notes:
- Directional cascades migrated off the packed 2D `shadow_atlas` onto
  a new 2D-array depth texture `cascade_array_texture`, with one layer
  per cascade and a per-layer render-attachment view. Spot lights still
  use `shadow_atlas`; cube faces unchanged.
- Per-light `params.resolution` is clamped to the config's
  `cascade_resolution` (uniform layer dimension). The cascade fills the
  top-left sub-rect of its layer; the descriptor carries the sub-rect
  size in normalised UV so smaller cascades clamp correctly.
- New descriptor kind = 3 (cascade-array PCF). EVSM (kind = 1) is
  unchanged from the receiver side; moment-write now reads from a
  layer of `cascade_array_texture` instead of `shadow_atlas`.
- `LightShadowView` gained `cascade_layer: Option<u32>`; the throttle
  now invalidates on layer reassignment (caster set change) the same
  way it invalidates on `atlas_rect` change. Per-attachment views
  (cube + cascade) throttle freely; spot remains forced-render until
  per-tile clear lands.
- New shadow bind-group binding at slot 8 (`shadow_cascade_array`).
  `ShadowGlobals` grew to 64 B with a new `cascade_array: vec4<f32>`
  field carrying `(layer.w, layer.h, max_layers, _)` for shader-side
  inv-texel-size math.
- `ShadowsConfig` gained `cascade_resolution` (default 2048) and
  `cascade_array_max_layers` (default 16). Each change triggers a
  cascade-array tear-down + EVSM moment-write bind-group rebuild via
  `PendingResourceRecreate::cascade_array`.

Replace the packed 2D cascade atlas with a 2D texture array, one layer
per directional cascade. Per-layer clear means a throttled cascade
keeps its prior layer's contents and skips both the depth pass AND
the EVSM compute.

See plan §4.2 for the full spec. Key touchpoints:
- `shadows/state.rs` atlas-place loop near line 1082 (cascade allocation).
- The lighting WGSL that samples cascade depth needs the
  array-index path.
- Authoring assumption: cascade resolutions are **uniform per
  directional light**. The texture-array forces this — non-uniform
  cascade sizes within one light won't work. Confirm before starting.

**Acceptance:** with a directional light's far cascade at
`update_period = 4`, three frames out of every four show no depth-pass
work for that cascade in the render-pass timeline. Atlas memory cost
≈ `cascade_count × resolution²` per directional light.

#### 16.2 Cluster 2.3 — EVSM cascade batching / skip-unchanged

**Status:** `done`

`EvsmDispatchEntry` carries a `should_render` flag set during the
throttle reconciliation pass in `Shadows::write_gpu`. The EVSM
dispatch loop in `render_pass::dispatch_evsm` skips entries whose
flag is false — that drops the moment-write + 2 blur passes for any
cascade whose layer wasn't refreshed this frame. With
`far_cascade_update_rate = Every4Frames` and EVSM on the far cascade,
EVSM dispatches scale with rendered-cascade count instead of
configured-cascade count.

Once 2.2 lands, the `EvsmPass` queue at
[`shadows/evsm.rs`](../../crates/renderer/src/shadows/evsm.rs)
gets an "if cascade.didnt_render_this_frame, skip" guard. Trivial
once the per-layer signal exists.

**Acceptance:** with throttling on, EVSM dispatches scale with
rendered-cascade count, not configured-cascade count.

#### 16.3 Cluster 6.1 — Material classify + indirect dispatch

**Status:** `done`

**A. Shader split — DONE.** The opaque compute pass is now specialized
per `MaterialShaderId` (PBR / Unlit / Toon). Concretely:
- [`shader/cache_key.rs`](../../crates/renderer/src/render_passes/material_opaque/shader/cache_key.rs)
  gained a `shader_id: MaterialShaderId` field.
- [`shader/template.rs`](../../crates/renderer/src/render_passes/material_opaque/shader/template.rs)
  passes `shader_id` into the compute template; the runtime `if
  (shader_id == X) {…}` branch in `compute.wgsl` became a
  `{% match shader_id %}` template choice.
- [`pipeline.rs`](../../crates/renderer/src/render_passes/material_opaque/pipeline.rs)
  caches pipelines per `(msaa, mipmaps, shader_id)` — 12 entries
  upfront.
- [`renderable.rs`](../../crates/renderer/src/renderable.rs) routes
  each mesh's `effective_material_key` through
  `Materials::shader_id()` to pick the matching pipeline.
- Per-pixel guard `if (shader_id != THIS_PIPELINE_ID) { return; }` at
  the top of each specialized shader catches the pre-classify case
  where every pipeline runs full-screen. Removed by the classify
  pass below — until then, expect a small perf regression on scenes
  with 2+ shader_ids because each pipeline runs full-screen with the
  guard tossing most pixels.

**B. Classify pass — to land next session.** Implementation design:

*Module skeleton (new):* `crates/renderer/src/render_passes/material_classify/`
mirroring the `light_culling` pass — `mod.rs` (visibility),
`render_pass.rs` (`MaterialClassifyRenderPass` struct + `render(ctx)`),
`bind_group.rs`, `pipeline.rs`, `shader/{cache_key.rs, template.rs,
material_classify_wgsl/{bind_groups.wgsl, compute.wgsl}}`.

*Compute shader:* one workgroup per 8×8 tile. Each invocation reads
its pixel's `visibility_data` → `material_meta_offset` → per-mesh
`material_offset` → `shader_id`. Skybox pixels (`triangle_index ==
U32_MAX`) are treated as if they were PBR — the PBR pipeline keeps
the existing skybox-fallback `textureStore(skybox_color)` block
and shades them; non-PBR pipelines gain a matching early-return on
skybox so a mixed-material tile shaded by Unlit + skybox doesn't
double-write its skybox pixels.

Per workgroup, an `atomicOr` on a 4-bit shared mask (one bit per
shader_id) collects the union; thread 0 then for each set bit
atomically appends the tile coords to `bucket[shader_id]` and
increments `indirect_args[shader_id].x`.

*Output buffer layout* (one storage binding, read-write in classify,
read-only in opaque):
```
ClassifyOutput {
    indirect_args: array<DispatchArgs, 3>, // 12 B × 3 = 36 B, aligned to 48 B
    bucket_offsets: array<u32, 3>,         // per-bucket starting index into `tiles`
    bucket_capacities: array<u32, 3>,      // overrun guard
    tiles: array<vec2<u32>>,               // packed tile coords, partitioned by bucket
}
```
Capacity per bucket = ceil(width/8) × ceil(height/8) (worst case: a
tile contains every shader_id). At 4K: ~138 K tiles × 8 B = ~1.1 MB
per bucket × 3 = ~3.3 MB total. Cheap.

*Material pass changes:*
- Add the classify-output buffer as a read-only storage binding to
  the material-opaque main bind group (storage count goes 8 → 9 of
  10 — still within budget; no packing needed).
- Replace `dispatch_workgroups(W/8, H/8, 1)` with
  `dispatch_workgroups_indirect(classify_buffer, indirect_args_offset)`
  per pipeline. The dispatch dedupe in
  [`render_pass.rs`](../../crates/renderer/src/render_passes/material_opaque/render_pass.rs)
  stays — each unique pipeline still dispatches once.
- Shader's first line replaces `let coords = vec2<i32>(gid.xy);` with
  `let tile = classify.tiles[classify.bucket_offsets[SHADER_ID] +
  workgroup_id.x]; let coords = vec2<i32>(tile * 8u + local_invocation_id.xy);`.
- The per-pixel guard added by the shader split becomes dead code
  (and DCE-removes itself) because each pipeline's dispatch only
  covers tiles its shader_id is in.

*Render graph:* classify runs after geometry and before
`material_opaque.render()`. The classify pass's pipeline + bind
group construction follows the `light_culling` template (which is
currently a TODO stub — `LightCullingRenderPass::render` is empty,
so material_classify will be the first non-trivial compute pass on
this scaffold).

*Acceptance:*
- Visual parity with the pre-classify build (each pipeline shades
  exactly its pixels; skybox handled by PBR).
- At a fixed 4K viewport with `N=3` shader_ids spatially separated,
  total workgroup launches across the opaque pass ≈ tile_count
  (one bucket-entry per tile) instead of `3 × tile_count`. The
  per-pixel guard's regression from the shader-split alone goes
  away.

Decals (Cluster 6.4, §16.4) reuse the same bucket infrastructure —
decals become a fourth shader_id with the same classify pipeline.

Replace the screen-wide compute dispatch per material with an
indirect dispatch driven by a per-material tile bitmask. See plan
§8.1 for the full spec.

- New classify compute pass: per 8×8 tile, scan the visibility buffer
  and produce a per-material bitmask of which tiles contain that
  material.
- Material pass dispatch: `dispatch_workgroups_indirect` per material
  bucket; shader `gid` no longer maps directly to a screen pixel.
- File scaffolding: `render_passes/material_classify/{mod.rs,
  bucket_builder.rs, tile_table.rs, indirect_args.rs, ...}`.

**Tier:** load-bearing once material count > ~6; small win at 1–2.

**Acceptance:**
- Visual parity (the material output is identical; only the dispatch
  shape changes).
- At a fixed 4K viewport with 8 materials, total workgroup count
  across the material pass drops by ≥ 4× (because dispatches now
  cover only their material's tiles).
- §14.2 DoD ticked.

#### 16.4 Cluster 6.4 — Decals as a material class

**Status:** `runtime done; schema + editor + HZB-tile classification pending`

**16.4.A Runtime (this session) — DONE.** Decima/D3-style projection
decals (oriented unit cube, project along local -Z, alpha-blend).
Landed pieces:

- `crates/renderer/src/decals/{mod,api,data,gpu}.rs` — `Decals` slotmap
  + per-decal GPU storage buffer + `AwsmRenderer::{insert,update,remove}_decal`
  public API.
- `Mesh::receive_decals: bool` (default true) packed into
  `MaterialMeshMeta` at the new `MATERIAL_MESH_META_RECEIVE_DECALS_OFFSET`;
  the decal shader checks it per-pixel and skips the per-decal volume
  test for opt-out meshes.
- New `material_decal` compute pass at
  `crates/renderer/src/render_passes/material_decal/`. Runs after the
  opaque→transparent blit; reads opaque (sampled), depth + visibility +
  mesh-meta + decals + camera + texture-pool; reconstructs world
  position from depth; iterates active decals and alpha-blends each
  whose inverse-transformed point lies inside the unit cube; writes
  to the transparent texture (which the blit already primed). v1
  ships alpha-blend only — additional blend modes are flagged at the
  `decal.blend_mode` u32 dispatch site.
- `transparent` render texture gained `STORAGE_BINDING` usage when
  MSAA is disabled. MSAA path skips the dispatch (the multisampled
  transparent texture can't be storage-bound); the §16.4.C note
  below tracks the follow-up.
- Browser-validated: no WebGPU validation errors with one box +
  PBR/Toon/Unlit pipelines + classify all wired. Decals are
  inert-but-correct until callers start invoking `insert_decal`.

**16.4.B Schema + editor — next session.** Scene-schema decal node
(transform + texture + alpha mode), editor authoring panel with the
unit-cube gizmo, `renderer_bridge` wiring so scene loads materialize
decals via `insert_decal`. Tracked as task **C2** in the chat task
list.

Implementation outline:

1. **Schema** — add `DecalNode` to `crates/scene-schema/src/`:
   ```rust
   pub struct DecalNode {
       pub transform: Transform,         // world-space (re-use existing Transform)
       pub texture: TextureReference,    // re-use the same texture-import flow
                                          // PBR/Unlit/Toon already use
       pub alpha: f32,                   // global alpha multiplier (1.0 default)
       pub blend_mode: DecalBlendMode,   // mirror of the runtime enum
   }
   pub enum DecalBlendMode {
       AlphaBlend,                       // v1 only — runtime supports just this
   }
   ```
   Add to `Project::decals: Vec<DecalNode>` (or fold into the existing
   node-tree alongside lights / meshes — match whichever pattern lights
   already follow).

2. **Scene-editor authoring panel** at
   `crates/frontend/scene-editor/src/panels/decal.rs` (or wherever the
   light panel lives — `panels/light.rs` is the reference shape).
   Required controls: transform widget (re-use), texture picker
   (re-use the PBR base-color picker), alpha slider, blend-mode
   dropdown (only "Alpha Blend" populated in v1 — comment notes the
   runtime supports additional modes via the `blend_mode` u32).
   Add a "Decal" entry to the "Insert ▸" menu next to "Light",
   "Primitive", etc.

3. **Editor viewport gizmo.** A wireframe unit cube drawn at the
   decal's world transform shows the projection volume. The line
   renderer (`crates/renderer/src/render_passes/lines/`) already
   handles wire gizmos for lights and cameras — replicate that
   pattern. The gizmo should highlight when the decal is selected
   in the scene tree.

4. **`renderer_bridge` wiring** at
   `crates/frontend/scene-editor/src/renderer_bridge/decals_sync.rs`.
   Mirror `shadows_sync.rs`: on project load, iterate
   `project.decals` and call `AwsmRenderer::insert_decal`. On node
   add/update/remove from the editor, mirror the change via the
   `update_decal` / `remove_decal` APIs. The `DecalKey` should be
   tracked by the bridge so editor updates can target the right
   runtime entry.

5. **Optional polish:** decal asset folder under
   `assets/decals/<name>/{decal.json, texture.png}` paralleling the
   dynamic-materials folder format — but the schema can hold the
   texture inline for v1 to ship faster. Treat folder format as
   follow-up if needed.

*Acceptance:* in scene-editor, insert a decal node, pick a
checker-pattern texture, drop it on top of a box mesh, see the
checker pattern alpha-blended onto the box's PBR shading. Move the
decal around and watch the projection follow. Save the project,
reload — decal round-trips through `project.json`.

**16.4.C HZB-tile classification follow-up.** v1 dispatches the
decal pass over every non-skybox tile; each pixel iterates every
active decal. Fine at low decal counts, scales worse. After
**16.6 (HZB)** lands, run a tile-decal classify step that, per
screen tile, builds a list of decals whose screen-space AABB
overlaps the tile, gated against HZB to skip occluded coverage.
Bucket layout reuses the classify infrastructure from 6.1.

**16.4.D MSAA path follow-up.** Multisampled `transparent` texture
can't be storage-bound, so v1's decal dispatch is gated on
"MSAA off". Lift the restriction by adding a dedicated
`decal_color_tex` storage texture sized to the resolved viewport
and a tiny composite step that reads opaque + decal_color into
transparent. Tracked here so the gate doesn't become a permanent
silent skip.

**16.4.E Storage-budget opportunity.** The opaque main bind group
is now at exactly 10/10 storage bindings. The dynamic-materials
plan needs at least one more slot for `extras_pool`. Refactor
candidate: pack `attribute_indices` (`u32`) and the
`visibility_geometry_data_offset` field of `MaterialMeshMeta` into a
shared buffer with offset-and-stride header — frees one slot.
Tracked in §16.E1 below.

#### 16.5 Cluster 4.3 — True cube PCSS

**Status:** `done`

The cube pool now exposes two views: the existing `cube_array_view`
(`texture_depth_cube_array`, used by `textureSampleCompare`) and a
new `cube_2d_array_view` (`texture_depth_2d_array`, bound at slot 9
of the shadow group) used by PCSS for raw `textureLoad` depth reads.
`sample_shadow_cube` now branches on hardness:

- `Hard` (1 tap) — unchanged.
- `Soft` (16-tap rotated Poisson, 15 cm world disc) — fixed kernel.
- `Pcss` — 16-tap blocker search on the 2D-array view (each tap
  inlined to a face-projection → `textureLoad` of raw NDC.z), then a
  16-tap variable-kernel PCF on the cube sampler. The penumbra width
  is the standard PCSS formula
  `(z_recv − z_blocker_avg) / z_blocker_avg`, mapped to a world-space
  disc radius and clamped to `[10 cm, 1 m]`.

The function-call-by-forward-reference path (`cube_dir_to_face_uv` as
a separate helper) tripped a WGSL validation error in Dawn — the
template-rendered shader resolves names in a strict declaration order
that doesn't always work across the bind-groups / compute split. The
projection math is inlined at the call site instead.

The current cube `Pcss` is a widened-`Soft`. Add a 2D-array depth view
of the cube pool, write the face-projection math in WGSL, and do a
real blocker search. See plan §6.3.

- `shadows/state.rs` cube-pool creation gets the 2D-array view.
- New WGSL helper in
  [`shared_wgsl/lighting/`](../../crates/renderer/src/render_passes/shared/shared_wgsl/lighting/).

**Effort:** medium-large (~120 LoC WGSL + view setup).

**Test:** open a point light at ~5m range, hardness = `Pcss`. Slide
`pcss_penumbra_scale` from 0.5 to 5.0 and watch the penumbra widen
smoothly (currently fixed-width regardless of scale).

#### 16.6 Cluster 7.1 — HZB build from visibility depth

**Status:** `done` (infrastructure landed; consumers will follow in 16.7 / 16.4.C / coverage)

Built per the §9.1 spec:
- New `render_passes/hzb/` module — `HzbRenderPass` owns the
  `r32float` mip-chain texture (sized to viewport, recreated on
  resize via `HzbRenderPass::ensure_size`) and the per-mip
  single-level storage views.
- Two compute shaders: `hzb_wgsl/seed.wgsl` copies depth → mip 0;
  `hzb_wgsl/reduce.wgsl` max-reduces a 2×2 of mip N-1 into mip N.
  One dispatch per mip transition; ceil-rounded workgroup sizes
  with the bounds check inside each shader.
- Stores **maximum** depth per tile so consumers run the canonical
  "candidate is occluded if its closest-screen-space-depth is
  greater than the HZB lookup at its footprint mip" test.
- Render-graph slot: after material_decal, before line/transparent.
  The depth buffer is fully written by that point.
- Bind-group recreate on `TextureViewRecreate` rebuilds both the
  seed bind group and the per-mip-transition reduce bind groups
  against the recreated texture.

No in-tree consumer yet — verified by the shader compiling clean
and no validation errors during the full render path.

Build a min-reduced mip chain from the final-resolution depth buffer
via successive compute passes. ~80 LoC WGSL + bind groups +
pipelines. See plan §9.1.

- New `render_passes/hzb/` module.
- Output: `hzb_view` texture, mip 0 = depth, mip N = single coarse value.

**Note:** with no consumer yet (7.2/7.3 below), this is pure infra.
Build it when 7.2 is about to land, not before — saves a maintenance
window.

**Acceptance:** sampling `hzb_view` at mip M returns the conservative
min depth over the corresponding 2^M × 2^M region. Unit test with a
synthetic depth pattern.

#### 16.7 Cluster 7.2 — Two-phase GPU occlusion culling

**Status:** `Phase 1 done; Phase 2 deferred (depends on geometry-shader restructure shared with §16.8)`

Phase 1 (infrastructure) shipped as commit `2034023`. The cull pass
writes `visible_this_frame[i]` per BVH-visible opaque instance; v1
does not consume the output (it's surfaced via tracing spans for
measurement).

**Phase 2 deferral note.** The Phase 2 split as written — "Pass 1
draws survivors; HZB rebuild; Pass 2 draws newly-visible
candidates" — needs Pass 2's draws to be gated by the fresh GPU
cull output. The only practical way to feed GPU compute results
back into a per-mesh draw count on the same frame is
`drawIndirect`. Async readback (`mapAsync`) gives a one-frame
delay and defeats the point.

So Phase 2 is effectively bundled with §16.8's drawIndirect
rewire. The bundled task additionally needs the geometry pass's
per-mesh dynamic-offset uniform bind group to be replaced with a
storage-buffer array indexed by `@builtin(instance_index)` (since
drawIndirect doesn't change bind groups per-draw). That shader
restructure is the load-bearing piece and deserves its own
focused session.

Sousa/Karis two-phase pattern. The HZB (from §16.6) is the
load-bearing input.

##### Phase 1 (this is the first sub-step to land)

*Goal:* introduce the GPU-side instance-list storage buffer and a
compute pass that classifies every BVH-visible instance as
"already-visible last frame" vs "newly visible candidate". No
`drawIndirect` rewire yet — keep the existing CPU-recorded draw
loop, but feed it from the new GPU buffer.

*Module:* `crates/renderer/src/render_passes/occlusion/`. Mirror
the `material_classify` shape — `mod.rs`, `bind_group.rs`,
`pipeline.rs`, `render_pass.rs`, `shader/{cache_key.rs,
template.rs, occlusion_wgsl/*.wgsl}`.

*New storage buffer* (`crates/renderer/src/occlusion.rs` or fold
into the render pass's `buffers.rs`):
```
struct OcclusionInstance {
    world_aabb_min: vec3<f32>,  // 12 B
    _pad0: u32,                  //  4 B
    world_aabb_max: vec3<f32>,  // 12 B
    _pad1: u32,                  //  4 B
    mesh_meta_offset: u32,        //  4 B
    instance_attr_base: u32,      //  4 B
    last_frame_visible: u32,      //  4 B  (read-write by the cull)
    _pad2: u32,                  //  4 B  → 48 B per instance
}
```
Capacity: bounded by mesh count × max instance per mesh; size for
the open-world tier (10K).

*Compute shader* (`occlusion_cull.wgsl`):
1. One workgroup per instance (or `workgroup_size(64)` over a
   per-instance index range).
2. Read `OcclusionInstance` at `gid.x`.
3. Frustum test against the camera planes (use the `Frustum` math
   already in `crates/renderer/src/frustum.rs` — replicate it in
   WGSL, or pass the 6 planes as uniforms).
4. Compute the screen-space AABB by projecting the 8 corners of
   the world AABB through `view_proj`; take min/max.
5. Pick the appropriate HZB mip from the screen-space AABB extent
   (`mip = ceil(log2(max(width_px, height_px)))`).
6. Sample HZB at that mip; if `closest_z_of_aabb > hzb_value`, the
   instance is occluded.
7. Output 0/1 to a per-instance `visible_this_frame: array<u32>`
   storage. CPU reads this back next frame to populate `last_frame_visible`.

*Render-graph slot:* runs between `material_decal` and `hzb` is
wrong order — HZB has to be built *before* the cull. Correct order:
1. Geometry pass — draws ALL instances (temporary; phase 2 splits
   this into two halves).
2. Material opaque + decal (as today).
3. HZB build (already in place after `6eef59c`).
4. **NEW** occlusion-cull compute → writes `visible_this_frame`.
5. Line + transparent + display passes (as today).

For phase 1 the cull output isn't consumed yet — it's CPU-readback'd
at the end of the frame and stored as `last_frame_visible` for next
frame. The win comes in phase 2.

*Acceptance for phase 1:* a debug overlay (or `tracing` span)
shows the per-frame "instances marked visible by GPU cull" count
roughly matching the BVH-visible count on a sparse scene and
shrinking on dense / heavy-occluder scenes.

##### Phase 2

After phase 1 lands and the cull output is trustworthy:

1. Split the geometry pass into two CPU-recorded loops:
   - **Pass 1 draws:** instances whose `last_frame_visible == 1`.
   - **Pass 2 draws:** instances whose phase 2 cull marked
     visible (newly visible candidates that survived the HZB test).
2. The HZB rebuild happens *between* Pass 1 and Pass 2 — Pass 1's
   depth seeds the HZB the Pass 2 cull tests against.
3. `tracing` spans on each half so the perf benefit can be measured.

*Acceptance:* on a hallway-style scene, "drawn instances" drops
toward "visible instances" (the actual unhidden subset). Frame
time at the open-world tier improves measurably; at sparse tiers
it's no worse than ±5%.

#### 16.8 Cluster 7.3 — GPU instance compaction + indirect draw

**Status:** `not started; depends on §16.7`

Final step: the geometry pass stops recording per-mesh
`draw_indexed` calls and switches to `drawIndirect` driven by GPU-
compacted args buffers.

*New buffer* — per-mesh `IndirectDrawArgs`:
```
struct IndirectDrawArgs {
    index_count: u32,
    instance_count: u32,  // atomically populated by compaction
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,  // base into the instance attribute buffer
}
```
One slot per `MeshKey`. The compaction compute walks
`visible_this_frame` from §16.7, writes `instance_count++` to the
matching `IndirectDrawArgs[mesh.material_meta_offset / META_SIZE]`,
and appends the per-instance attribute index to a packed list.

*Geometry pass change:* replace the
[`renderable.rs`](../../crates/renderer/src/renderable.rs)-driven
per-mesh `draw_indexed` loop with a `drawIndirect` loop. Order
matters — `multi_draw_indirect` is NOT in core WebGPU (experimental
Chrome flag); the v1 path is one `drawIndirect` per *material
pipeline key* (reusing Cluster 6.1's per-shader_id partitioning).
At N=3 active opaque shaders that's 3 `drawIndirect` calls instead
of N draw_indexed-per-mesh calls.

*Storage budget:* needs one more storage on the geometry pipeline
(the compacted instance attribute list). §16.E1 + §16.E2 must
have landed first so the geometry pass has slots free.

*Acceptance:*
- Visual parity with the per-mesh `draw_indexed` path.
- A `tracing` span around the geometry pass shows CPU command
  recording time dropped (the per-mesh loop is gone).
- 10K-mesh stress scene from §16.G measures a frame-time win vs
  the pre-`drawIndirect` baseline.

#### 16.9 Cluster 5.1 — Shadow render-pass integration tests

**Status:** `won't do`

The renderer is intentionally web-sys-only (not `wgpu`), so a native
headless harness isn't possible without inventing a backend-trait
layer the rest of the project doesn't want. `wasm-bindgen-test`
under headless Chrome on Linux CI doesn't have reliable WebGPU
support either. The descriptor / view bookkeeping invariants are
already enforced by the `debug_assert_eq!` checks at the bottom of
`Shadows::write_gpu` (Cluster 5.2), which fire on every dev build
that actually exercises the shadow path. That's the level of
robustness we want here; integration tests are dropped from scope.

#### 16.10 Cluster 2.1.b — Single-buffer `MeshLightSlicesGpu` cleanup

**Status:** `done`

Renamed `MeshLightSlicesGpu` → `MeshLightIndicesGpu`,
`MeshLightSlicesResize` → `MeshLightIndicesResize`, and the field
`mesh_light_slices_gpu` → `mesh_light_indices_gpu` across the
renderer. Slice metadata stayed where it lives (inside
`MaterialMeshMeta`); the renamed struct now matches the single
buffer it actually owns. No behaviour change.

`MeshLightSlicesGpu` now only owns one buffer (`indices_buffer`) since
F2 moved the slice metadata into `MaterialMeshMeta`. The struct and
module name still reference "slices". Rename to `MeshLightIndicesGpu`
(or fold into `LightMeshBuckets` itself) so the code shape matches
the data shape.

Small refactor — single rename + import updates. No behaviour change.

---

### Storage-budget refactor candidates

The opaque main bind group is currently at exactly 10 of 10 storage
bindings — the absolute cap with `with_max_storage_buffers_per_shader_stage`.
Several upcoming features want one more slot:

- **Dynamic materials** (`docs/plans/dynamic-materials.md`)
  needs `extras_pool` for variable-length per-material buffer slots.
- **GPU-driven culling (16.6/16.7/16.8)** wants the instance-list
  storage as a separate binding.

#### 16.E1 Pack `attribute_indices` into `MaterialMeshMeta`

**Status:** `done` (landed together with §16.E2 as the merged geometry pool refactor)

`attribute_indices: array<u32>` (binding 8 on the opaque main bind
group) holds vertex-attribute index buffers — one slice per mesh.
Slices are looked up via
`MaterialMeshMeta.vertex_attribute_indices_offset`, read at one
offset per pixel for the active triangle.

The relevant precedent: `mesh_light_indices` (`light_buckets/gpu.rs`)
already follows this shape — slice metadata in `MaterialMeshMeta`
(via the `light_slice_offset` + `light_slice_count` fields), data
in a shared storage buffer. Mirror that refactor.

*Implementation outline:*

1. **Confirm there's only one underlying buffer.**
   `attribute_indices` is currently bound from
   `ctx.meshes.custom_attribute_index_gpu_buffer()` —
   [`meshes.rs`](../../crates/renderer/src/meshes.rs) is already the
   single source. Good; the refactor is purely a binding-shape
   change, not a data layout one.
2. **Drop `attribute_indices` from the opaque main bind group**
   layout and entries
   ([`material_opaque/bind_group.rs`](../../crates/renderer/src/render_passes/material_opaque/bind_group.rs)
   binding 8). Also drop from the transparent pass + the picker
   pass + the material_classify and material_decal passes if any
   of them bind it (they don't today; verify).
3. **WGSL: replace** `attribute_indices[i]` in
   `material_opaque_wgsl/`, `material_transparent_wgsl/`, and any
   shared helpers with reads through the existing pattern. Likely
   already factored — confirm by `grep -rn attribute_indices`.
4. **No `MaterialMeshMeta` change needed** if the existing
   `vertex_attribute_indices_offset` already drives a different
   bound buffer. Worth double-checking in the refactor — the goal
   is to fold the lookup through a buffer that's *already bound*.

Net effect: opaque main goes from 10/10 storage to 9/10. Frees one
slot for §16.7.

*Acceptance:* visual parity with the pre-refactor build
(opaque/transparent/picker pixels identical), test suite stays at
100 green.

#### 16.E2 Pack `attribute_data` similarly

**Status:** `done`

Landed approach: rather than a binding-shape change (which the
original brief described but wasn't actually possible — three
separate underlying GpuBuffers can't share one binding), the
three per-mesh GpuBuffers were merged into a single
`mesh_geometry_pool` buffer that holds
`[visibility_data || attribute_index || attribute_data]`
contiguously per mesh. The existing `MaterialMeshMeta` offset
fields (`visibility_geometry_data_offset`,
`vertex_attribute_indices_offset`,
`vertex_attribute_data_offset`) now address sections within the
merged pool. The opaque main bind group dropped bindings 8 and 9
(`attribute_indices`, `attribute_data`); the WGSL `visibility_data`
binding is the one shared view, with `bitcast<u32>(visibility_data[i])`
for index reads and direct `visibility_data[i]` for attribute floats.
Storage-buffer peak across the compute stage dropped from 10/10 to
7/10, leaving room for §16.7's instance-list binding and
dynamic-materials' `extras_pool`. Buffer usage flags on the pool
are `copy_dst | vertex | storage | index` (the same buffer
underlies the transparent path's per-mesh vertex / index buffers).

### Tuning / profiling items (no new code)

These need browser-side measurements or authoring decisions. The
resume session running these should **save the authored projects
under `assets/world/`** so they're version-controlled reference
scenes, not throwaway JSON. One project per item (or sharing where
the same scene happens to suit multiple measurements).

#### 16.G Author scenes + fill §15

**Status:** `scenes shipped; §15 measurements pending`

The six tuning scenes ship as version-controlled reference data
under `assets/world/<name>/project.json`, generated by the
`generate_tuning_scenes` example in `awsm-scene-schema`. Re-run
with `cargo run --example generate_tuning_scenes -p
awsm-scene-schema` to regenerate (deterministic positions, fresh
NodeIds each run).

Sizes / contents:
- `tuning-1k-meshes` (1.8 MB) — 1024 box grid + 20 point lights.
- `tuning-64-lights` (115 KB) — 10 spheres + 64 mixed point/spot
  lights, first 10 shadow-casting.
- `tuning-mixed-intensity` (144 KB) — 64-box grid + 20 lights
  spanning 0.1× → 50× intensity.
- `tuning-open-world` (94 KB) — 1 km terrain plane + ocean plane +
  50 props + sun.
- `tuning-coverage` (181 KB) — 100 props receding from camera.
- `tuning-10k-meshes` (17.7 MB) — 100×100 box grid + sun.

Each scene loads via the editor's Load… button. The §15
measurements (filling the actual `__ ms / frame` rows) are
deferred — they need an interactive session: boot scene-editor,
File → Load… → pick `project.json` for each scene, capture span
timings via the browser console (`tracing_subscriber` emits
them), paste into §15.

The unifying entry — this is one focused session that author the
scenes T1-T6 need, runs the measurements, and fills in §15. Do it
after the code-shipping items (§16.E / §16.7 / §16.8) so the
"after" numbers reflect the optimized renderer.

*Scenes to author* (in priority order; each in its own
`assets/world/<name>/project.json`):

1. **`tuning-1k-meshes`** — 1024 boxes in a 32×32 grid + 20
   shadow-casting lights. Drives T1 (Cluster 1 shadow-pass timing).
2. **`tuning-64-lights`** — 64 mixed punctual lights + ~500K
   verts across ~10 meshes + 10 shadowed views. Drives T2.
3. **`tuning-mixed-intensity`** — 20 lights at varied intensities
   (0.1× to 50×). Drives T3 (importance-tier histogram).
4. **`tuning-open-world`** — terrain plane (1km×1km) + ocean
   plane + skybox + ~50 props. Drives T6 (oversized-mesh
   threshold tuning).
5. **`tuning-coverage`** — 100 small props at varying camera
   distances. Drives T4 (cheap-material-pixel-threshold decision).
6. **`tuning-10k-meshes`** — 10K boxes in a 100×100×1 grid.
   Drives T5 (SceneSpatial rebuild thresholds).

*Measurement workflow per item:*
1. Add `tracing` spans where they're missing (`shadows::record`,
   `material_opaque::render`, `material_classify::render`, the
   per-pipeline indirect dispatches, geometry-pass CPU loop).
2. Boot scene-editor via Claude Preview MCP, load the project,
   capture span timings via `tracing_subscriber` browser output.
3. Write the before/after numbers into §15.

*Acceptance:* every row in §15 has measured numbers, not
`__ ms / frame` placeholders.

#### 16.T1 Cluster 1 DoD #6 — Shadow-pass timing improvement

**Status:** `not started`

Capture a `tracing` span around `shadows/render_pass::record` at the
1k-mesh / 20-view tier. Plan expects ≥ 2× improvement vs the
pre-Cluster-1 linear sweep.

```
Before (linear sweep):  __ ms / frame
After  (BVH query):     __ ms / frame
Ratio:                   __ ×
```

#### 16.T2 F + E perf validation

**Status:** `not started`

At 64 lights / 500K verts / 10 shadow views, measure:

```
Material opaque (64 lights):  Before __ ms  After __ ms
Geometry pass  (500K verts):  Before __ ms  After __ ms
Shadow gen     (10 views):    Before __ ms  After __ ms
```

If E shows a regression, revert in
[`transforms.rs::update_inner_recursively`](../../crates/renderer/src/transforms.rs).

#### 16.T3 Cluster 4.2 — Importance-tier cutoff tuning

**Status:** `not started`

Cutoffs in `shadows/importance.rs` are provisional (`score > 4.0` →
Ultra, `> 1.0` → High, `> 0.1` → Medium). At typical authoring
intensities, what fraction of shadow casters end up in each tier?

Instrument the rebuild to log the histogram, run a representative
scene, re-tune.

```
Ultra: __ %
High:  __ %
Medium: __ %
Low:   __ %
```

#### 16.T4 Cluster 6.3 — Pixel-threshold default

**Status:** `not started`

`Mesh::cheap_material_pixel_threshold` defaults to 64. Decision: keep
the flat default, or thread `ShadowQualityTier` through
`collect_renderables` and pick from the tier (Low→16, Medium→64,
High→256, Ultra→1024)?

#### 16.T5 Cluster 1.7 — Rebuild thresholds for the 10k tier

**Status:** `not started`

`SceneSpatialConfig` defaults are sized for 1k meshes. At 10k they
should scale to `rebuild_dirty_threshold = 2000`, `rebuild_period_frames
= 1800`. Expose `AwsmRendererBuilder::with_scene_spatial_config` if
target scenes exceed 1k meshes.

#### 16.T6 Cluster 2.1.d — Oversized-mesh threshold tuning

**Status:** `not started`

Constants in `light_buckets/buckets.rs`:
- `OVERSIZED_LIST_COUNT_THRESHOLD = 16`
- `OVERSIZED_AABB_DIAGONAL_METERS = 50.0`

Re-tune after running a scene with terrain chunks / ocean planes /
skyboxes.

```
last_max_bucket at idle camera: ___
oversized_meshes().len():       ___
```

#### 16.T7 Browser-console regression watch

**Status:** ongoing

After every push, scan the browser console for new WebGPU validation
warnings. Likely culprits at the moment:
- Bind group resize races (`MeshLightSlicesResize`).
- `update_light` kind-change path failing to clean up cube slots fully.

---

### Items that don't apply

Documented here so we don't accidentally re-add them.

- **Cluster 2.4 (coarse light-space binning)** — obsolete per plan
  §4.4; superseded by `SceneSpatial`.

---

## 17. References

**Spatial structure:**
- [rstar — RTree, SelectionFunction, Envelope, primitives::GeomWithData](https://docs.rs/rstar/latest/rstar/)
- [Jacco Bikker — How to build a BVH part 5: TLAS & BLAS](https://jacco.ompf2.com/2022/05/07/how-to-build-a-bvh-part-5-tlas-blas/)
- [Bruno Opsenica — Frustum culling](https://bruop.github.io/frustum_culling/)
- [Bevy issue #1333 — scene BVH for culling](https://github.com/bevyengine/bevy/issues/1333)
- [gkjohnson/three-scene-bvh-prototype](https://github.com/gkjohnson/three-scene-bvh-prototype) — TLAS-style scene BVH for three.js

**Visibility buffer and material classify:**
- Burns & Hunt, "The Visibility Buffer: A Cache-Friendly Approach to Deferred Shading" (JCGT 2013).
- Schied & Dachsbacher, "Deferred Attribute Interpolation Shading" (HPG 2015).
- Wihlidal, "Optimizing the Graphics Pipeline with Compute" (GDC 2016) — material classify + indirect dispatch pattern.
- de Carpentier & Ishiyama, "Decima Engine: Advances in Lighting and AA" (Siggraph 2017) — Detroit/Decima decals-as-material-class.
- Karis, "A Deep Dive into Nanite Virtualized Geometry" (Siggraph 2021).

**Light culling:**
- [Olsson, Persson, Doggett 2012 — Clustered Deferred and Forward Shading](https://efficientshading.com/2012/01/01/clustered-deferred-and-forward-shading/)
- Persson, "Practical Clustered Shading" (Siggraph 2013).
- El Mansouri / Activision, "Rendering of Call of Duty: Infinite Warfare" (Digital Dragons 2017) — per-object light lists.
- Heitz et al., "Combining Analytic Direct Illumination and Stochastic Shadows" (i3D 2018) — out-of-scope but worth knowing.

**GPU-driven culling and HZB:**
- Haar & Aaltonen, "GPU-Driven Rendering Pipelines" (Siggraph 2015).
- Sousa & Geffroy, "The Devil is in the Details: idTech 666 (Doom 2016) Graphics" (Siggraph 2016) — HZB and two-phase occlusion.
- Bevy's mesh-let / two-phase occlusion implementation as a wasm reference.
- Existing docs:
  [`SHADOWS.md`](../SHADOWS.md),
  [`PERFORMANCE_OPEN_WORLD_PLAN.md`](../PERFORMANCE_OPEN_WORLD_PLAN.md),
  [`VERTEX_ATTRIBUTES.md`](../VERTEX_ATTRIBUTES.md).
