# GLB Export + Editable Meshes

## Context

The editor can be driven (via MCP agent or UI) to do a lot with materials, animation, and
object placement — but it cannot **modify mesh geometry**, and it cannot **write a GLB file**.
Two motivating use-cases:

1. **Authoring geometry** — tell the agent "make a baseball bat" (or "create a cylinder and mold
   it into a bat") and have it construct/shape the mesh, then get the result out as a `.glb`.
2. **Slimming an import** — import a textured model, reassign a lightweight material, and write a
   **new GLB with the heavy textures omitted**. (Today saving is additive into a project; there's
   no path to emit a fresh, slim GLB.) **No dedicated "slim" mode is needed:** export emits only the
   textures the *assigned* materials reference, so reassigning a light material makes the heavy textures
   unreferenced and a normal export simply drops them. For slimming an entire over-large project, publish the
   player runtime bundle (Phase 6, which prunes + bakes) and re-import the relevant GLB.

Both share one missing enabler: **glTF/GLB export** (none exists — the renderer is import-only).
The geometry side wants two coexisting editing models (confirmed with the user): a **procedural,
non-destructive modifier stack** (LLM-friendly, reuses the existing curve/sweep infra) *and*
**raw per-vertex editing** (general, for fine work). Everything is expressed as `EditorCommand`s, so
**any websocket/MCP caller can drive the full capability set**. Mesh editing is **fully command-driven** —
the "edit mesh" screen is just a *view* of the mesh plus a generated capabilities reference; there is **no
manipulation UI** (not even a vertex picker). Direct vertex editing survives only as one of the commands an
LLM may choose (see *Interface philosophy* below).

### Key facts established during exploration
- **Command spine is ready.** `EditorController::dispatch(EditorCommand)` → `apply_inner()` returns an
  inverse for undo; commands broadcast across tabs; `Batch` collapses to one undo entry; `SetKind`
  is the universal re-materialize path and coalesces. ~70 MCP tools already funnel through this.
- **Editable representation already exists:** `meshgen::MeshData { positions, normals, uvs, colors,
  indices }` ([mesh_data.rs](packages/crates/meshgen/src/mesh_data.rs)) with `compute_vertex_normals()`.
  `AwsmRenderer::add_raw_mesh(RawMeshData, …) -> MeshKey` ([raw_mesh.rs:130](packages/crates/renderer/src/raw_mesh.rs))
  lowers it to the GPU. The renderer's own per-triangle *exploded* visibility layout is NOT for editing.
- **The mesh-asset schema is half-built.** `CapturedMesh` / `MeshDef` / `CapturedSource` /
  `AssetSource::Mesh` / `mesh_asset_filename` all exist in [material.rs:277](packages/crates/scene-schema/src/material.rs),
  and a "capture as mesh" button exists ([inspector.rs:276](packages/frontend/editor/src/scene_mode/inspector.rs)).
  **But the bytes are only stored in a session-local thread-local `HashMap`**
  ([bridge/mesh_cache.rs](packages/frontend/editor/src/engine/bridge/mesh_cache.rs)) — nothing writes them to
  disk or reads them back. Closing this is foundational.
- **Export tooling is available:** `gltf-json` 1.4.1 is already transitively in `Cargo.lock`; the git
  gltf dep is already gone (crates.io `gltf` 1.4.1, import-only). GLB container = trivial hand-rolled header.
- **Imported Model geometry is NOT retained editor-side** (only renderer `MeshKey`s + the original
  `.glb` bytes on disk at `assets/<content_hash>`). Model export must **re-read the source file**.

## Approach

Six phases delivering value incrementally. **Phase 1 (export) and Phase 2 (persistence) are the
high-value core and are largely parallelizable; recommended order 1 → 2 → 3 → 4 → 5 → 6 (Phase 5 depends on
the modifier-stack base slot; Phase 6 — the player publish pipeline — is the capstone but its data shape
constrains Phase 1's writer, so design the IR for it up front).** Cross-cutting
rules: every capability is one `EditorCommand`/`EditorQuery`; whole-object "replace" commands follow the
existing idempotent/coalescing idiom (`SetCustomMaterialLayout`, `SetKind`); all new schema fields use
`#[serde(default)]` for round-trip safety; the bridge observes a single **mesh-asset revision counter**
(mirroring the `affects_animation` pattern in [command.rs](packages/crates/editor-protocol/src/command.rs))
so no edit silently skips re-materialize.

**Representation & storage (decided — GLB does not constrain authoring).** Two distinct representations:
- **Authoring/source = the recipe**, stored in the *project* (scene-schema): the modifier stack / SDF graph /
  lathe profile. Tiny, non-destructive, infinitely editable.
- **Runtime/interchange = baked triangles** (the GPU visibility buffer, and GLB).

Because the renderer **rasterizes** (not raymarches), every representation — SDF, CSG, procedural, primitive —
must evaluate down to triangles before it can be drawn (`add_raw_mesh` → vis-buffer). Triangles are mandatory
at runtime regardless of GLB, so GLB imposes no loss the rasterizer doesn't already impose. Three internal
storage cases, **none of which stores meshes as GLB internally**:
1. Pure procedural → recipe in TOML; triangles are a regenerable `.mesh.bin` cache.
2. Collapsed / raw-vertex-edited → no recipe; the `.mesh.bin` triangle buffer *is* the source.
3. Imported model → source of truth is the original `.glb` on disk (kept as-is).

**GLB export = a one-way bake** (flatten any of the above to triangles + materials) for sharing/slimming/
external tools. **Decision: GLB is pure baked interchange — it does NOT carry the procedural recipe.**
Editability lives in the project; re-editing means opening the project, not re-importing a GLB. (High-res SDF
bakes can be triangle-heavy while the recipe is bytes — another reason the recipe stays in the project and we
bake at a chosen resolution on export.)

**Interface philosophy (decided).** The command/query layer **is** the product surface — every mesh
capability is drivable by any websocket/MCP caller. Mesh editing has **zero bespoke manipulation UI**:
- **The "edit mesh" screen is a *view* + a capabilities reference.** Entered from a mesh node, it frames the
  mesh in the viewport (and renders the current vertex selection read-only, for observability/screenshots)
  alongside a **generated capabilities reference** — categories of available actions (generators, modifiers,
  selections, constraints, SDF ops, queries), each expanding to the exact command/tool + parameters. Exact
  presentation is open (a side panel, a searchable list, or modal(s) — not necessarily multiple buttons); the
  point is it *documents and reveals* the command surface, it does not manipulate geometry.
- **The reference is generated from the command/query surface**, not hand-maintained, so it never drifts —
  reuse the existing docs-resource pattern (`get_material_contract`, `awsm://docs/…`); surface the same doc
  both as an MCP resource (`awsm://docs/mesh-tools`) and in the editor screen.
- **No vertex picker / gizmo / hit-test.** Direct vertex editing is just one command path an LLM may pick
  (`SetVertexSelection` by index/predicate → `SetVertexPositions` / `SoftTransformVertices`). A human who
  wants a freehand tweak asks the agent; the editor builds no spatial manipulation tooling.
- **Read-only inspector summary** for mesh nodes (e.g. "3 modifiers · 1.2k verts"). The result always renders
  live via the bridge, so humans *see* state without a bespoke editor for it.
- **ALL LLM-native capabilities are in scope** (none optional): superquadric base, formula displacement,
  predicate selection, constraint/measurement transforms, mirror/array, geometry introspection, and SDF/CSG.

**Geometry introspection queries (LLM closed-loop).** Across Phases 2–4, add read-only `EditorQuery`
variants so the agent can *measure → self-correct*: mesh bbox, vertex/tri counts, centroid, cross-section
radius at a given axis height, and a vertex histogram along an axis. Pure agent benefit (a human just looks
at the viewport); cheap, no mutation/undo. They make formula/constraint editing iterable ("measure tip
radius → adjust profile → re-measure"). MCP tools: `get_mesh_stats`, `get_mesh_cross_section`.

### Phase 1 — GLB export (ships independently for Primitive/Sweep/Model)
- **New crate `packages/crates/glb-export`** (no editor/GPU deps, natively unit-testable). Promote
  `gltf-json` to a workspace dep. **Design the IR scene-complete up front** (Phase 6 reuses it — do not paint
  it into a mesh-only corner): `GlbScene { nodes: Vec<ExportNode{name, Trs, Option<MeshData>, Option<MaterialDef>,
  light, camera, children}>, animations: Vec<ExportAnimation>, env: Option<EnvRef> }`. Phase 1 only needs to
  populate mesh+material; the light/camera/animation/env slots stay empty until Phase 6 but exist now.
  `write_glb(&GlbScene) -> Vec<u8>` builds accessors/buffer-views/meshes (with required POSITION min/max) and
  the 12-byte GLB header + JSON + BIN chunks; lights/cameras/animations map to `KHR_lights_punctual`, glTF
  cameras, and glTF animations when present.
- **Material mapping (policy — keeps geometry portable, materials lossless):**
  - **Built-in PBR** → real glTF PBR material (base_color/metallic/roughness/emissive/alpha). A plain GLB
    stays useful standalone.
  - **Unlit** → `KHR_materials_unlit` (standard glTF).
  - **Non-PBR (custom WGSL, Toon, anything not glTF-representable)** → emit the primitive with an
    **`AWSM_materials_none`** extension and **no embedded material**. The editor/player/scene supplies the
    real material via its assignment mechanism on (re)import. **No lossy baking** (this replaces the earlier
    "bake to PBR approximation" idea).
  - **Textures: referenced-only (one rule everywhere).** Export embeds exactly the images the assigned
    materials use; unreferenced textures are never carried. No `slim`/omit flag — lightweighting falls out of
    reassigning a lighter material (heavy textures become unreferenced → dropped). The player bundle (Phase 6)
    applies the same rule plus optional compression.
- **Import round-trip for `AWSM_materials_none`:** the importer today maps every primitive through
  `pbr_material_mapper` ([populate/mesh.rs:206](packages/crates/renderer-gltf/src/populate/mesh.rs)) and does
  **not** recognize this token yet. Add handling so an imported primitive carrying `AWSM_materials_none` gets
  an **empty material slot** left for scene-level resolution, rather than a default material. Define the
  extension name/shape once (primitive-level `extensions.AWSM_materials_none`) in `scene-schema`/`renderer-gltf`.
- **Editor extraction** (`controller/export.rs`): `node_to_export_mesh` — Primitive → `primitive_to_mesh`
  (reuse from `node_sync`); Mesh → resolve via the Phase-2 store; Sweep → `meshgen::sweep_along_curve`;
  **Model → re-read source `.glb` via `GltfLoader::load` and pull POSITION/NORMAL/TEXCOORD/indices**.
- **`EditorQuery::ExportGlb { node: Option<NodeId> }`** (`None` = whole scene; export is a read, no undo)
  returning base64 bytes. **MCP:** `export_scene_glb`, `export_node_glb`. **UI:** "Export GLB" button in the
  inspector header + per-node export; writes via `ProjectDir::write_bytes` or blob-download. This is the
  standalone "get geometry out" path (e.g. export an edited bat); the player bundle (Phase 6) is the separate
  whole-runtime publish.
- **Risk / biggest unknown:** re-reading `ImportModelFromFile` blobs at export time (session-local `blob:`
  URLs may be revoked) — mitigate by persisting imported source bytes into the project at import.

### Phase 2 — Editable mesh asset + persistence (foundational; prereq for 3 & 4)
- **No new NodeKind** — reuse `NodeKind::Mesh` + `MeshRef` + `AssetSource::Mesh(MeshDef)`. Extend `MeshDef`
  with `#[serde(default)] editable: bool` (+ `modifiers` in Phase 3); add `CapturedSource::Editable`/`Imported`.
  Bytes encoded with `bitcode` to `assets/<id>.mesh.bin` (`mesh_asset_filename`).
- **Close the persistence gap** in [persistence.rs](packages/frontend/editor/src/controller/persistence.rs):
  add `mesh_files()` (binary sibling of `material_files`); `save_to_dir` writes them; `load_*` reads them back
  into the mesh store (replacing session-local-only `mesh_cache`, keeping its `get_raw`/`store` API so
  `node_sync` is untouched).
- **Commands:** `ConvertToEditableMesh { node, mesh: AssetId }` (caller-minted id; bakes current geometry →
  `CapturedMesh`, swaps node kind; inverse = `Batch[SetKind(prior), DeleteAsset(mesh)]`);
  `SetMeshData { mesh, data }` (raw replace; inverse = prior bytes). **Bridge:** mesh-revision observer
  re-fires `apply_kind` for all referencing nodes. **MCP:** `convert_to_editable_mesh`.
- **Risk:** the game-runtime **player** also reads `AssetSource::Mesh` — keep the side-file scheme symmetric
  so both editor and player load it (player loader parity is the unknown; don't break it).

### Phase 3 — Procedural modifier stack (primary, LLM-friendly path)
- **Schema** (`scene-schema/src/modifier.rs`): `ModifierStack { base: MeshBase, modifiers: Vec<Modifier> }`;
  `MeshBase = Primitive | Lathe{profile, segments, angle} | Superquadric{e1, e2, …} | Sweep(def) | Captured(MeshRef)`;
  `Modifier = Taper | Bend | Twist | Lathe | Subdivide | SubdivSurface{catmull_clark} | Smooth | Inflate |
  Spherify | Roughen | RoundEdges{angle} | Bulge{center, radius} | Displace{expr} | DisplaceTexture |
  CurveDeform | Shrinkwrap | Cast | Lattice | Wave | Symmetrize | Mask{predicate} | Mirror{plane} |
  Array{count, offset|radial}`. Stored on `MeshDef.modifiers` (`#[serde(default)]`); `.mesh.bin` becomes a
  regenerable cache.
- **LLM-native generators/modifiers folded in here** (symbolic, not spatial — cheap, no new machinery):
  - `MeshBase::Superquadric{e1,e2}` — one exponent pair morphs box↔sphere↔cylinder↔octahedron.
  - `Modifier::Displace{expr}` — formula displacement over (position, normal, uv, index). Evaluate via a small
    CPU expression evaluator (e.g. a tiny shunting-yard/`evalexpr`-style pass), or reuse the WGSL vertex path.
  - `Modifier::Mirror{plane}` / `Modifier::Array{count, linear-offset | radial}` — agent drives by counts/axes.
  - `MeshBase::Lathe` profile is authored as numeric `(height, radius)` samples — the agent emits these from
    real-world knowledge (a baseball bat *is* a 1D radius profile).
- **Evaluation** (`meshgen/src/modifiers.rs`): `evaluate(&ModifierStack) -> MeshData`. Base via `primitives.rs`
  / **reuse `sweep_along_curve`** ([sweep.rs:52](packages/crates/meshgen/src/sweep.rs)) for lathe/revolve;
  each modifier a pure per-vertex deformer; `compute_vertex_normals()` after. Native per-modifier tests.
- **Command:** `SetMeshModifiers { mesh, stack }` — **whole-stack replace** (idempotent, coalesces per mesh,
  inverse = prior stack); add/remove/reorder/param are all UI/agent computing the new stack (same idiom as
  `SetCustomMaterialLayout`). **UI: none bespoke** — modifier/base editing is command-only (driven via MCP);
  the inspector shows only a read-only summary (modifier count, vert/tri counts). The result renders live via
  the bridge, so humans see the effect without a stack editor.
  **MCP:** `set_mesh_modifiers` + convenience `add_modifier`/`set_modifier_param` (read-modify-write one float).
- **Risk / unknown:** lathe-via-sweep parameterization (revolve needs circular path + cap handling) may
  warrant a dedicated `revolve()` in meshgen. A baseball bat = a 1D radius profile lathed around Y.

### Phase 4 — Raw per-vertex editing (escape hatch + fine work)
- **Commands:** `CollapseMeshStack { mesh }` (bake modifiers → raw, clear stack; inverse = restore prior
  `MeshDef`); `SetVertexPositions { mesh, indices, positions }` (batched);
  `SoftTransformVertices { mesh, indices, transform, falloff }` (server computes falloff weights).
  Selection is **transient** (`SetVertexSelection`, like `SetSelection`, not undoable).
- **LLM-native selection & transforms** (command-only; the agent expresses geometry as data):
  - **Predicate selection** (transient): `select_vertices_where { mesh, predicate }` — by `normal·dir > t`,
    position threshold, top-N% along an axis, or within radius of a point. Command-only (there is no UI
    selection tool); the resulting selection is rendered read-only in the mesh-edit view via `SetVertexSelection`.
  - **Constraint/measurement transforms** (commands): "scale so bbox height = X", "set tip radius at height
    y to r" — server solves the numeric transform and emits the same sparse-diff `SetVertexPositions`. A human
    eyeballs; the agent works to spec in real-world units.
- **Undo strategy:** **sparse inverse** — record only `(indices, prior_positions)` of touched verts, never a
  whole-mesh snapshot (meshes can be 100k+ verts); coalesce a drag into one undo. `CollapseMeshStack` is the
  one deliberate heavy snapshot.
- **UI — none.** No vertex picker, gizmo, or hit-test. Every selection/transform is a command (by index or
  predicate). The mesh-edit *view* may render the current selection read-only (highlight selected verts) so
  humans and screenshots can see what the agent did — observability only, never manipulation.
- **Ergonomics to get right (replacing the picker risk):** since selection is command-driven, the
  predicate-selection vocabulary and the introspection queries must be rich enough that an agent can reliably
  target verts without a cursor (`select_vertices_where` + `get_mesh_stats`/`get_mesh_cross_section` close the
  loop). Read-only selection-highlight rendering is the one small view addition.

### Phase 5 — SDF + CSG (the most LLM-native authoring path; sequenced last, not optional)
- **We do NOT build a mesher.** The meshing is an existing crate (`fast-surface-nets` / `isosurface` /
  `block-mesh`). Our value-add is small: an **SDF graph as a new `MeshBase::Sdf(SdfNode)`**, where
  `SdfNode = Primitive{sphere|box|cylinder|torus|capsule, …} | Union{smooth: f32} | Subtract{smooth} |
  Intersect{smooth} | Transform{Trs, child}`. Pure data → trivially agent-composable ("a mug = cylinder
  minus a smaller cylinder, union a torus handle").
- **Why SDF over mesh-booleans (the deliberate paradigm choice):** SDF combinations are always closed
  manifolds after meshing (no robustness failures on degenerate/non-manifold imported inputs, which is
  exactly where `csgrs`-style mesh booleans break), and they give **smooth/rounded booleans** for free —
  which mesh booleans cannot do at all. Trade-off: uniform-grid resolution loses hard edges unless we use
  dual-contouring/surface-nets (the chosen crates do surface nets), and meshing cost scales with grid size.
- **Eval:** `evaluate` for `MeshBase::Sdf` samples the SDF over a bounded grid (resolution a parameter the
  agent/UI sets) → surface-nets crate → `MeshData` → existing modifier stack still applies on top.
- **Command/MCP/UI:** same `SetMeshModifiers` whole-stack replace (the SDF graph is just the `base`); MCP
  `set_mesh_modifiers` carries the SDF JSON. **No bespoke UI** — the SDF graph is command-only (per the thin-UI
  model); it appears in the capabilities modal and renders live via the bridge.
- **Risk:** meshing cost + grid resolution tuning; sharp-edge loss; bounding-box estimation for the sample
  grid. Biggest unknown: picking the surface-nets crate that round-trips cleanly to `MeshData` with normals.

### Phase 6 — Player runtime bundle (the editor→player publish pipeline)
**Goal.** Lower an editor *project* to the most compressed *runtime* deliverable the player consumes — **baked
GLB + resolved materials + environment, no scene project**. Broader than Phase 1's single-node/scene export
(which is just "get geometry out"): the bundle **resolves and ships the entire runtime** — scene structure,
materials manifest, environment — compressed. Both share the same referenced-only texture rule and the
`write_glb`/`GlbScene` core (hence the scene-complete IR in Phase 1).
- **Bundle layout** (e.g. `publish/<name>/`):
  - `scene.glb` — whole scene baked: geometry (all recipes flattened to triangles at a chosen resolution),
    node hierarchy + transforms, **lights** (`KHR_lights_punctual`), **cameras**, and **animations** (editor
    clips lowered to glTF animations: TRS + morph weights natively; **`KHR_animation_pointer`** for
    material-uniform / light / camera tracks). Standard-PBR materials embedded as glTF PBR.
  - `materials/` — a **pruned subset of the project's existing material side-files** (`material.wgsl` +
    `material.toml`) for every custom/non-PBR material actually referenced (those marked `AWSM_materials_none`
    in the GLB). The `AWSM_materials_none` extension carries a stable **material id** the manifest resolves
    (node/primitive → material). Reuses the Phase-2/existing material serialization; ships only referenced ones.
  - `textures/` — only the images the *final* materials use (PBR + custom), copied/compressed; unreferenced
    source textures dropped. (KTX2/basis compression is a stretch goal, not required.)
  - `env/` — skybox/IBL refs + config sidecar (glTF can't carry IBL).
  - **Excluded:** recipes, modifier stacks, undo metadata, the asset table, unreferenced assets — the entire
    authoring layer.
- **Command/MCP:** `EditorQuery::ExportPlayerBundle { name }` (a read; returns a manifest + the file set, or
  writes to a picked dir via `ProjectDir`). MCP `export_player_bundle`; UI "Publish for player…" button.
- **Persistence:** writes a fresh bundle dir; never mutates the project.
- **Verification:** publish a scene with a custom-WGSL material + a light + a rotation clip → confirm
  `scene.glb` parses (geometry + `KHR_lights_punctual` + a glTF animation), the custom material appears in
  `materials/` with `AWSM_materials_none` wiring in the GLB, and unreferenced textures are absent. Ultimately:
  the player loads the bundle and renders it equivalently to the editor (screenshot parity).
- **Risk / unknowns:** `KHR_animation_pointer` coverage for awsm-specific animated targets (fallback: a small
  animation sidecar for anything the pointer extension can't address); whether the player already has a
  bundle loader (vs only the project loader) — likely net-new on the player side, called out as a dependency.

## Command capability menu (target vocabulary)

**All of these are committed** (the value of the arc is breadth of agent-drivable capability). Because mesh
editing is fully command-driven with **no UI per capability**, each item is cheap — one `EditorCommand`/
`EditorQuery` + one evaluation function + one generated reference entry — so they're added incrementally and
tiered by cost, not gated by phase. Items marked **★** directly serve the two motivating use-cases; items
marked **⇄** plug into systems that already exist.

- **Cleanup / repair** (makes imported & baked meshes editable): weld / merge-by-distance, delete loose &
  degenerate, recalc-normals (consistent winding) / flip, fill holes, make-manifold, triangulate/quad.
- **Lightweighting ★** (use-case #2 — drop weight, not just textures): decimate/simplify to a tri-count target,
  voxel/uniform remesh, UV unwrap (planar/box/spherical/auto), generate tangents (reuse the MikkTSpace path ⇄).
- **Local modeling operators** (the "molding" verbs ★): extrude, inset, bevel/chamfer, bridge edge loops,
  solidify/shell (thickness), dissolve/delete, separate (split selection → new mesh), merge/join meshes,
  local smooth/relax. Operate on predicate-/index-selected regions.
- **Deformers** (modifier-stack additions): noise displacement (perlin/simplex) ★, displace-by-heightmap/texture ⇄,
  curve-deform (bend along a `Curve`) ⇄, shrinkwrap (conform onto another mesh), cast (toward sphere/cylinder),
  lattice/FFD cage, wave/ripple, skin/wireframe (edges→tubes).
- **Deformers — organic/shaping** (cheap, intuitive, very LLM-tunable): **subdivision surface** (Catmull-Clark,
  for smooth organic forms — distinct from linear subdivide), **inflate / shrink-fatten** (offset along normals,
  "puff it up"), **spherify** (morph toward a sphere by factor), **roughen / jitter** (random per-vertex —
  natural/eroded look, vs coherent noise), **round-edges / global bevel** (soften the silhouette by angle),
  **bulge / pinch / dent** (local radial deform from a center+radius), **symmetrize** & **radial (N-fold)
  symmetry**, **mask** (keep/remove by predicate or named set), weighted-normals / shade-smooth-by-angle.
- **Generators / bases**: capsule, tube/pipe, rounded-box, prism, gear, helix/spring/screw, loft (multi-profile),
  convex hull (wrap a point set), parametric surface `(u,v)→xyz` / `z=f(x,y)`, supershape (3D superformula),
  metaballs (SDF infra ⇄), heightfield/terrain, voronoi fracture, **torus-knot / Möbius / Klein** (parametric
  exotics), **crystal / gem** (faceted hull), **rock / boulder** (noise-displaced icosphere), **truss / lattice**
  (grid wireframe), **pipe/cable network along curves** ⇄.
- **Generators — high-value, meatier**: **3D text** (string + font → extruded glyphs — signs/labels/scores in a
  game scene ★), **L-system tree / plant** (branching from rules — iconic AI-native procedural growth).
- **Simulation deformers (stretch tier — the most "wow" for agents, heavier lifts):** cloth drape, soft-body
  relax/settle, gravity sag, explode/shatter (voronoi + impulse). "Drape a tablecloth," "let this pile settle."
  Aspirational; gated behind a sim step, but pure-data-drivable like everything else.
- **Transform / origin utilities**: recenter origin (center / **bbox-bottom so it sits on the ground** ★), apply/
  normalize transform into geometry, align/flatten/project a selection onto a plane.
- **Selection vocabulary** (command-only, so make it rich): by island/linked, loop, ring, boundary, by-material,
  grow/shrink, invert, by-curvature, random-%, by-proximity-to-node, plus **named selection sets** ("barrel",
  "handle") the agent saves and re-references.
- **Introspection queries** (close the perceive→act loop): volume & surface area, is-watertight / is-manifold /
  boundary-edge count, read raw vertex positions, **silhouette/profile-along-axis** (returns the radius curve to
  read-then-modify) ★, nearest-vertex-to-ray (LLM computes its own "click"), curvature / sharp-edge detection.
- **Cross-system synergies ⇄**: morph-target capture (sculpt a state → blend shape → animatable via existing
  animation morph tracks), scatter / instance-on-surface (reuses `InstancesAlongCurve`), vertex colors by
  formula/predicate (`COLOR_0`), skin-weight preservation + predicate weight assignment, mesh-mesh boolean.

**Tiering (rough):** *cheap, add early* — cleanup, transform/origin, selection vocabulary, introspection
queries, noise/formula/parametric generators, vertex colors, and most organic deformers (inflate/spherify/
roughen/bulge/symmetrize/mask/cast/wave). *Meatier* — local modeling operators (extrude/bevel/inset/bridge/
solidify), subdivision surface, round-edges, shrinkwrap/lattice, morph capture, skin-weight preservation,
torus-knot/exotic & rock/crystal/truss generators. *Heavy* — decimate/remesh, UV unwrap, voronoi, mesh-mesh
boolean, 3D text (font glyph triangulation), L-system trees. *Stretch* — simulation deformers (cloth/soft-body/
gravity/explode). None of these add UI; the reference surface lists whatever exists.

## Critical files
- [material.rs](packages/crates/scene-schema/src/material.rs) — `MeshDef`/`CapturedMesh`/`CapturedSource`; add `editable`/`modifiers`
- [persistence.rs](packages/frontend/editor/src/controller/persistence.rs) — **mesh side-file save/load (closes the gap)**
- [node_sync.rs](packages/frontend/editor/src/engine/bridge/node_sync.rs) — mesh-revision re-materialize; `MeshData → add_raw_mesh`
- [command.rs](packages/crates/editor-protocol/src/command.rs) + [query.rs](packages/crates/editor-protocol/src/query.rs) — new commands/queries + inverse contracts
- [sweep.rs](packages/crates/meshgen/src/sweep.rs) (+ new `modifiers.rs`) — lathe/deformer evaluation reusing sweep infra
- [mesh_cache.rs](packages/frontend/editor/src/engine/bridge/mesh_cache.rs) — promote thread-local cache to persisted store
- new crate `packages/crates/glb-export/` (scene-complete `GlbScene` IR: meshes/materials + lights/cameras/animations/env) + [mcp.rs](packages/mcp/src/mcp.rs) (new tools) + [inspector.rs](packages/frontend/editor/src/scene_mode/inspector.rs) (UI)
- `controller/export.rs` — both single-node/scene `ExportGlb` (Phase 1) and `ExportPlayerBundle` (Phase 6); the publish path reuses `write_glb`, prunes referenced material side-files, lowers clips to glTF animations, and emits the env sidecar
- new `scene-schema/src/modifier.rs` (ModifierStack / MeshBase incl. `Sdf`, Modifier) + `meshgen/src/modifiers.rs` (eval) + `meshgen/src/sdf.rs` (Phase 5, wraps a surface-nets crate)
- [populate/mesh.rs](packages/crates/renderer-gltf/src/populate/mesh.rs) — recognize `AWSM_materials_none` on import (leave material slot empty for scene resolution)
- mesh-edit view + capabilities reference: a generated `docs/mesh-tools.md` (from the command/query surface) exposed as MCP resource `awsm://docs/mesh-tools` + rendered in the editor's mesh-edit screen (in/near [inspector.rs](packages/frontend/editor/src/scene_mode/inspector.rs)); plus read-only selection-highlight rendering in the bridge/viewport. This view + reference is the **entire** new mesh UI — no manipulation tooling

## Verification
- **Native unit tests:** `cargo test -p glb-export` (cube round-trip: `write_glb` → re-parse with
  `gltf::Gltf::from_slice`, assert vertex/index counts + material factors); `cargo test -p awsm-meshgen`
  (per-modifier bbox/vertex-count asserts; falloff math).
- **Lightweighting via referenced-only export:** `import_model_from_url` a textured glb → `assign_material` a
  plain PBR (no textures) → `export_node_glb` → re-parse and assert `images.len() == 0` with geometry preserved
  (the heavy textures are now unreferenced, so they're dropped without any flag).
- **Editable-mesh persistence:** insert primitive → `convert_to_editable_mesh` → `get_node_details` shows
  `NodeKind::Mesh` → save to dir → confirm `assets/<id>.mesh.bin` exists → reload → still renders
  (`screenshot_scene` / `canvas_stats`).
- **Modifier + vertex edits:** `set_mesh_modifiers` (add twist) → `get_node_bounds`/`screenshot` shows
  deformation → undo restores; `collapse_mesh_stack` → `select_vertices_in_box` → `transform_vertices` →
  bounds reflect the move → undo restores exactly.
- **Non-PBR export round-trip:** assign a custom-WGSL material → `export_node_glb` → assert the primitive
  carries `AWSM_materials_none` and no embedded material → re-import → material slot is empty and the scene's
  assignment re-binds the real material.
- **LLM closed-loop:** `set_mesh_modifiers` a lathe with a `(height,radius)` bat profile → `get_mesh_cross_section`
  at the barrel confirms the target radius → adjust → re-measure. **Phase 5:** `set_mesh_modifiers` with an SDF
  `union/subtract` graph → `screenshot`/`get_mesh_stats` shows a closed manifold (mug-like) result.
- **Mesh-edit view:** open a mesh's edit screen → it frames the mesh and renders the current command-driven
  selection read-only (highlighted verts visible in `screenshot_scene`); the capabilities reference lists the
  full action set, matching the `awsm://docs/mesh-tools` MCP resource (same generated source). No manipulation
  controls exist to exercise — confirm editing happens purely via commands.
- **Build gates:** workspace `cargo build` / `clippy` green; reload the editor and exercise the mesh-edit view,
  the capabilities reference, and the Export button in-browser.
