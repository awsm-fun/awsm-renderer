# Mesh tools (authoring + editing geometry over MCP)

Mesh editing is **fully command-driven** — no cursor, gizmo, or vertex picker.
Every capability is one MCP tool. This doc is the exact JSON shapes + copy-paste
examples for the non-obvious ones (`set_mesh_modifiers`, `select_vertices_where`).

> **JSON args:** the object-valued args (`stack`, `predicate`) accept either a
> JSON object or a JSON-encoded string — both work.

## Workflow

1. `insert_primitive { shape }` → a node id. Every procedural-geometry node is a
   `Mesh` backed by an editable `MeshDef` (a `ModifierStack`), so the inserted
   node is already editable — there is **no separate "make editable" step**.
2. Get its mesh asset id: `get_node_details { node }` (the kind's `mesh` field), or
   `convert_to_editable_mesh { node }` — now a no-op that simply **echoes the
   node's existing mesh asset id** (geometry persists to `assets/<id>.mesh.bin`).
3. Shape it: `set_mesh_modifiers` (procedural recipe) and/or the vertex tools.
4. Measure: `get_mesh_stats`, `get_mesh_cross_section` (perceive → adjust loop).
5. `wait_render_settled` then `screenshot_scene` to see it; `undo` to revert.
6. Get it out: `export_node_glb` / `export_scene_glb` / `export_player_bundle`.

You can also drive a mesh purely procedurally: `set_mesh_modifiers` replaces the
whole recipe each call (idempotent, coalesces per mesh). The recipe lives in the
project; the baked triangles are a regenerable cache.

---

## set_mesh_modifiers — the modifier stack

```jsonc
{ "mesh": "<mesh asset uuid>", "stack": { "base": <MeshBase>, "modifiers": [<Modifier>...] } }
```

### MeshBase (the generator the stack starts from)

```jsonc
{ "primitive": { "box": { "dims": [1,1,1] } } }   // or plane/sphere/cylinder/cone/torus (PrimitiveShape)
{ "lathe": { "profile": [[h0,r0],[h1,r1],...], "segments": 24, "angle": 6.28318 } } // revolve (height,radius) around Y; angle=TAU → closed
{ "superquadric": { "e1": 1.0, "e2": 1.0, "segments_long": 24, "segments_lat": 16 } } // (1,1)=sphere, →0=box
{ "sweep": <SweepAlongCurveDef> }   // sweep a cross-section along a scene curve node
{ "captured": "<mesh asset uuid>" } // start from already-captured bytes, then deform
{ "sdf": { "node": <SdfNode>, "resolution": 56 } } // SDF/CSG graph → surface nets
```

### Modifier (applied in order; each is one object)

```jsonc
{ "taper":      { "axis": "y", "factor": 0.3 } }     // scale cross-sections 1→factor along axis
{ "twist":      { "axis": "y", "turns": 0.5 } }       // rotate progressively (turns × 360°)
{ "bend":       { "axis": "y", "angle": 1.57 } }      // bend the axis extent by angle (radians)
{ "inflate":    { "amount": 0.1 } }                    // offset along normals ("puff")
{ "spherify":   { "factor": 1.0 } }                    // morph toward a sphere (0..1)
{ "roughen":    { "amount": 0.05, "seed": 7 } }        // deterministic per-vertex jitter
{ "subdivide":  { "iterations": 3 } }                  // midpoint subdivision (×4 tris each)
{ "smooth":     { "iterations": 2, "factor": 0.5 } }   // Laplacian smoothing
{ "mirror":     { "axis": "x" } }                      // mirror across the origin plane (keeps both halves)
{ "array":      { "count": 3, "offset": [2,0,0] } }    // linear array of copies
{ "displace":   { "expr": "0.1*sin(y*8.0)" } }         // formula displ. along normal; vars: x,y,z,nx,ny,nz,u,v,i,pi,tau; fns: sin cos tan abs sqrt floor fract sign exp log, min max pow mod atan2 step (2-arg), clamp(v,lo,hi), noise(x,y)/noise(x,y,z) (smooth value noise, [-1,1])
// NOISE TERRAIN: noise() is the generic primitive — compose fbm by SUMMING octaves yourself, e.g. { "displace": { "expr": "noise(x*1.5,z*1.5)*0.6 + noise(x*4,z*4)*0.2 + noise(x*9,z*9)*0.08" } }; ridged = "abs(noise(x*3,z*3))", domain-warp = "noise(x + noise(x*0.5,z*0.5), z)". Subdivide the plane (segments_x/z) for resolution first.
```
`axis` is `"x"|"y"|"z"`.

### SdfNode (the CSG graph for `sdf` base)

```jsonc
{ "primitive": <SdfPrimitive> }
{ "union":     { "smooth": 0.06, "children": [<SdfNode>...] } }  // smooth>0 → rounded blend
{ "subtract":  { "smooth": 0.0,  "children": [a, b, ...] } }     // a minus the rest
{ "intersect": { "smooth": 0.0,  "children": [...] } }
{ "transform": { "trs": { "translation": [x,y,z], "rotation": [x,y,z,w], "scale": [x,y,z] }, "child": <SdfNode> } }
```
SdfPrimitive: `{"sphere":{"radius":1}}` · `{"box":{"half":[x,y,z]}}` ·
`{"cylinder":{"radius":r,"height":h}}` (along Y) · `{"torus":{"major":R,"minor":r}}` ·
`{"capsule":{"radius":r,"height":h}}`.

### Worked examples

**Twisted, tapered column** (subdivide first so the twist curves):
```jsonc
{ "base": { "primitive": { "box": { "dims": [1,2,1] } } },
  "modifiers": [ { "subdivide": { "iterations": 3 } },
                 { "twist": { "axis": "y", "turns": 0.5 } },
                 { "taper": { "axis": "y", "factor": 0.6 } } ] }
```

**Baseball bat** (a 1-D radius profile lathed around Y — emit real-world radii):
```jsonc
{ "base": { "lathe": { "segments": 24, "angle": 6.28318,
  "profile": [[0,0.05],[0.3,0.055],[0.7,0.07],[1.1,0.11],[1.5,0.14],[1.75,0.13],[1.9,0.07],[2.0,0.0]] } },
  "modifiers": [] }
```
Then `get_mesh_cross_section { node, axis: 1, samples: 8 }` reads the radius curve to verify.

**Mug** (cylinder minus inner cylinder, union a torus handle):
```jsonc
{ "base": { "sdf": { "resolution": 56, "node": { "union": { "smooth": 0.06, "children": [
  { "subtract": { "smooth": 0.0, "children": [
    { "primitive": { "cylinder": { "radius": 0.6, "height": 1.2 } } },
    { "transform": { "trs": { "translation": [0,0.18,0], "rotation": [0,0,0,1], "scale": [1,1,1] },
      "child": { "primitive": { "cylinder": { "radius": 0.48, "height": 1.2 } } } } } ] } },
  { "transform": { "trs": { "translation": [0.66,0,0], "rotation": [0.7071,0,0,0.7071], "scale": [1,1,1] },
    "child": { "primitive": { "torus": { "major": 0.32, "minor": 0.09 } } } } } ] } } } },
  "modifiers": [] }
```

---

## Incremental modifier editing (no whole-stack resend)

Once a mesh has a stack (set a base with `set_mesh_modifiers` first), tweak it one
modifier at a time instead of resending `{ base, modifiers }`:

- `get_mesh_modifiers { mesh }` → the current recipe `{ base, modifiers }` as JSON
  (or `null` if the mesh has no recipe yet). Read this to find modifier **indices**.
- `add_modifier { mesh, modifier }` — append one modifier to the **end** of the stack.
- `set_modifier { mesh, index, modifier }` — replace the modifier at `index` (0-based).
- `remove_modifier { mesh, index }` — remove the modifier at `index` (0-based).

`modifier` is a **single** Modifier object (the shapes under
"[Modifier](#modifier-applied-in-order-each-is-one-object)" above), e.g.
`{"twist":{"axis":"y","turns":2}}`. Each call re-bakes geometry and is **one
discrete undo step** (they do not coalesce). **Precondition:** the mesh must
already carry a stack — `add/set/remove` on a recipe-less mesh **errors** (call
`set_mesh_modifiers` to set a base first); out-of-range indices also error.

**Example — add a twist, then inspect:**
```jsonc
// mesh already has a stack (e.g. set_mesh_modifiers gave it a box base)
add_modifier { "mesh": "<uuid>", "modifier": { "twist": { "axis": "y", "turns": 2 } } }
get_mesh_modifiers { "mesh": "<uuid>" }
// → { "base": { "primitive": { "box": { "dims": [1,2,1] } } },
//     "modifiers": [ { "twist": { "axis": "y", "turns": 2.0 } } ] }
remove_modifier { "mesh": "<uuid>", "index": 0 }   // undo that twist
```

---

## Per-vertex attribute authoring (sculpt + paint)

Per-vertex authoring is **index-based on a fixed topology**, so it is
**terminal**: the *first* authoring op **collapses** the procedural stack to a
frozen `captured` base (topology + modifier params bake in). After that the mesh
carries a **sparse, index-keyed override layer** (`MeshDef.overrides`:
`positions / colors / normals / uvs`) re-applied on top of the frozen base on
every re-bake — non-destructive and uniform across positions/colors/normals/uvs.
Use `get_mesh_layers` to see whether a mesh is still procedural (editable
modifiers) or already frozen (terminal authoring).

- `select_vertices_where { node, predicate }` → returns matching vertex **indices**
  (a read — no cursor). Feed them to the authoring verbs below.
- `set_vertex_selection { node, indices }` → highlight those vertices in the
  viewport (read-only amber cross markers; no geometry change). Pairs with
  `select_vertices_where`: run the query, then call this so a human can SEE which
  vertices matched before you author them. Empty `indices` clears the highlight.
- `soft_transform_vertices { mesh, indices, translation:[x,y,z], falloff }` —
  translate a selection with a smooth radial falloff (`falloff:0` = hard move).
  Writes `overrides.positions`; normals auto-recompute (unless explicitly set).
- `set_vertex_positions { mesh, indices, positions:[[x,y,z]...] }` — sculpt set
  of specific verts (also writes `overrides.positions`).
- `paint_vertex_colors { mesh, indices, color:[r,g,b,a] }` — author per-vertex
  **colors** (linear RGBA). Painted colors only **display** under a material that
  reads vertex colors — built-in PBR with `vertex_colors_enabled`, or a custom
  material that samples them (see the splatting recipe below).
- `set_vertex_normals { mesh, indices, normal:[x,y,z] }` — author per-vertex
  **normals** (e.g. flatten a face / fake a crease); an explicit normal override
  always wins over the sculpt auto-recompute.
- `set_vertex_uvs { mesh, indices, uvs:[[u,v]...] }` — author per-vertex **UVs**
  (TEXCOORD_0), `uvs[k]` ↦ `indices[k]` (per-index parallel arrays, unlike the
  single-value color/normal verbs). This is the verb that lets you lay a
  **continuous strip UV** for conveyor/tread/road scrolling — pair it with
  `strip_parameterize` (below) to compute the coords, then a `texture_transform`
  V-scroll. With this, every vertex attribute (positions/colors/normals/**uvs**)
  now has a typed authoring verb. See the *Geometry-locked scroll* recipe in
  `awsm://docs/material-recipes`.
- `collapse_mesh_stack { mesh }` — explicitly bake one mesh's modifier recipe →
  frozen `captured` (the authoring verbs do this implicitly on first use).
- `bake_all {}` — project-wide finalize: collapse **every** Mesh's stack (freeze
  all topology + bake overrides into the cache). The pre-export/handoff bake.
  Undoable (restores every prior stack as one step).

### VertexPredicate (for `select_vertices_where`)

```jsonc
{ "kind": "top_percent",   "axis": 1, "percent": 0.12 }   // top 12% along Y (e.g. a rim)
{ "kind": "normal_dir",    "dir": [0,1,0], "threshold": 0.7 } // normals facing ~up
{ "kind": "axis_greater",  "axis": 0, "value": 0.0 }
{ "kind": "axis_less",     "axis": 1, "value": -0.4 }
{ "kind": "within_radius", "center": [0,0,0], "radius": 0.3 }
{ "kind": "within_aabb",   "min": [x,y,z], "max": [x,y,z] }   // local-space box
{ "kind": "connected_to_seed", "seed": [i, ...] }             // the whole connected PIECE
```
`axis`: 0=X, 1=Y, 2=Z. The geometry predicates above pick verts by position/normal;
**`connected_to_seed`** is topology — it grabs every vertex in the connected island(s)
containing the `seed` verts, position-welded so a UV/normal seam doesn't fragment a
solid piece. Use it to select "this whole bolt / belt / panel" from one seed (e.g. any
index from another predicate), and pair it with `separate_mesh` to detach that piece.

**Example — flare a cup's rim:** `select_vertices_where {node, {"kind":"top_percent","axis":1,"percent":0.12}}`
→ `soft_transform_vertices {mesh, indices, translation:[0,0.4,0], falloff:0.5}`.

---

## Region editing — `separate_mesh`

Detach part of a mesh into its **own node** so it can carry a different material or be
edited independently (the model is one-material-per-node, so multi-material = multiple
nodes — exactly what an imported multi-primitive glTF already destructures into).

- `separate_mesh { node, selection?|indices?, keep_remainder? }` — a triangle moves
  when **all 3** of its vertices are selected. Pick the region with
  `select_vertices_where` (the `connected_to_seed` predicate grabs a whole piece, or
  store a handle with `store:true`). The new sibling node inherits the source's
  transform + material — `assign_material` a different material to it next. By default
  the source is left intact (the new node is an extracted **copy**); pass
  `keep_remainder:true` to also **remove** those faces from the source (no overlap /
  z-fighting). Undoable.

**Example — re-skin one belt of a tank:** `select_vertices_where {node, {"kind":"connected_to_seed","seed":[<a vert on that belt>]}, store:true}`
→ `separate_mesh {node, selection:<handle>, keep_remainder:true}`
→ `assign_material {node:<new "Separated" node>, material:<belt material>}`.

---

## Introspection (perceive → adjust)

- `get_mesh_stats { node }` → `{ vertices, triangles, bbox_min/max, centroid,
  surface_area, volume, watertight }`.
- `get_mesh_cross_section { node, axis, samples }` → `[[height, radius]...]` (the
  silhouette profile; pairs with a lathe profile). Empty bins read `0` when the
  mesh has no vertices at that height — use a denser mesh or fewer `samples`.
- `get_vertex_data { node, indices, include_source? }` → the **final** (post-eval
  + override) per-vertex data: `{ vertex_count, vertices:[{ index, position,
  normal, color, uv }] }` (`color`/`uv` null when the mesh has no such channel).
  The read counterpart to the paint/sculpt verbs — verify what your last op
  produced. Pass `include_source:true` to add a per-vertex
  `source:{position,normal,color,uv}` block tagging each channel `"override"`
  (authored) or `"base"` (rides the evaluated geometry) — confirm *which* channels
  an op actually wrote.
- `get_mesh_data { node, offset?, limit? }` → mesh **topology**: the triangle index
  buffer (`triangles:[[a,b,c]...]`, paged by triangle — a full buffer overflows the
  token cap) plus `vertex_count`, `triangle_count`, `bbox`. The read counterpart to
  `set_mesh_data` and the connectivity source for loop-ordering / adjacency /
  arc-length. Per-vertex attributes come from `get_vertex_data`.
- `strip_parameterize { node, selection?|indices?, axis? }` → HEURISTIC conveyor/
  tread/road UV helper: per selected vertex, normalized `(along, across)` to feed
  straight into `set_vertex_uvs` (`along` = travel about the axle [0,1); `across` =
  lateral [0,1]). Band = selection handle / explicit indices / whole mesh.
  ⚠️ **Pass an explicit `axis` (the belt's axle) for treads** — the auto-fit (omit
  `axis`) uses a least-variance PCA direction and is **unreliable on near-isotropic
  bands** (a tube whose height ≈ diameter fits a radial direction instead of the
  axle). Winding/polarity may also come out flipped (flip `axis` or use `1-coord`).
- `get_mesh_layers { node }` → the layer summary / "what's live vs locked":
  `{ base, modifiers, modifier_count, frozen_topology, has_overrides,
  override_counts:{positions,colors,normals,uvs} }`. `frozen_topology:true` means
  per-vertex authoring already collapsed the stack (terminal).
- `get_uv_layout { node, uv_set?, offset?, limit? }` → the UV-island overlay:
  `{ has_uv, island_count, bounds:{min,max}, islands:[{count,min,max}], edge_count,
  edges:[[[u,v],[u,v]]…] }`. Diagnoses **"atlas vs strip"** in one read — a
  continuous strip UV is ONE island spanning ~[0,1] (good for scrolling/tiling); a
  packed atlas is MANY small islands (a global UV scroll slides samples onto unrelated
  content). `edges` is the UV wireframe (paged by `offset`/`limit`); island summaries
  are always full.

> **`set_mesh_data` safety (escape-hatch via `dispatch_command`):** replacing a
> mesh's geometry wholesale now **validates** before storing — empty/degenerate
> input (`positions:[]`/`indices:[]`), a non-multiple-of-3 index count, or an
> out-of-range index is **rejected** (it used to silently wipe the mesh and return
> `ok`). Pass `allow_empty:true` to deliberately clear a mesh to empty geometry.

---

## Recipe — texture splatting via vertex colors

Vertex colors are a cheap per-vertex **blend mask** for a multi-texture (splat)
material — no UV painting needed:

> ⚠️ **Footgun: unpainted vertex color is `(1,1,1,1)` WHITE, not 0.** So a splat
> shader doing `mix(base, snow, vColor.r)` reads **full weight everywhere** until
> you paint — the whole mesh comes out as `snow`, the *opposite* of intent. Two
> fixes: **(a) clear-to-0 baseline** — before painting the splat, zero the whole
> mesh: `paint_where { node, predicate: {"kind":"within_aabb","min":[-1e9,-1e9,-1e9],"max":[1e9,1e9,1e9]}, color:[0,0,0,1] }`
> (the `within_aabb` covers every vertex; `paint_where` keeps the index array
> server-side — see §10), *then* paint the splat band into the channel. **(b)**
> Or author the shader so the *zeroed* channel means "blend in" (`mix(snow, base,
> vColor.r)` with painted `r=0` patches). Always `get_vertex_data` to confirm the
> baseline before the band paint.

0. **Clear the blend mask to 0** (see footgun above): `paint_where` the whole
   mesh to `[0,0,0,1]`, so unpainted = "no blend".
1. Insert + shape the mesh (e.g. a ground `plane`, subdivided for resolution).
2. Select+paint the region to texture-A in one call with **`paint_where {node,
   predicate, color}`** (fused, scales to full-res — §10), or
   `select_vertices_where {node, predicate}` → `paint_vertex_colors` (e.g.
   `top_percent` along Y for peaks, or `within_radius` for a patch).
3. `paint_vertex_colors {mesh, indices, color:[1,0,0,1]}` — store the blend
   weight in a channel (R = grass, G = rock, B = sand, A = …). Paint other
   regions into other channels. The first paint collapses the stack (terminal).
4. Assign a **custom splat material** that samples each texture and `mix`es them
   by the interpolated vertex color: `albedo = mix(grass, rock, vColor.r)` etc.
   (declare the `vertex_color` fragment input + the texture slots; see the
   material tools / `awsm://docs/material-recipes`). A built-in PBR material with
   `vertex_colors_enabled` tints by the color but does **not** splat-blend
   textures — that needs the custom material.
5. Verify the painted weights with `get_vertex_data {node, indices}`.

## Export (get geometry out)

- `export_node_glb { node }` / `export_scene_glb` → base64 `.glb`. Whole-scene
  carries lights, cameras, and animations.
- `export_player_bundle { name }` → manifest: `scene.glb` + pruned custom-material
  side-files + env descriptor.

**Material mapping:** built-in PBR → glTF PBR; Unlit → `KHR_materials_unlit`;
custom-WGSL / Toon → `AWSM_materials_none` (no embedded material; re-import leaves
the slot empty for scene resolution, carrying the material id).
**Textures are referenced-only:** only images the assigned materials use are
embedded — reassign a no-texture material to "slim" an export.
