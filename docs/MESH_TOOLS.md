# Mesh tools (authoring + editing geometry over MCP)

Mesh editing is **fully command-driven** — no cursor, gizmo, or vertex picker.
Every capability is one MCP tool. This doc is the exact JSON shapes + copy-paste
examples for the non-obvious ones (`set_mesh_modifiers`, `select_vertices_where`).

> **JSON args:** the object-valued args (`stack`, `predicate`) accept either a
> JSON object or a JSON-encoded string — both work.

## Workflow

1. `insert_primitive { shape }` → a node id.
2. `convert_to_editable_mesh { node }` → a **mesh asset id** (the node becomes a
   `Mesh`; geometry persists to `assets/<id>.mesh.bin`). Use this mesh id with the
   modifier / vertex tools below.
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
{ "displace":   { "expr": "0.1*sin(y*8.0)" } }         // formula displ. along normal; vars: x,y,z,nx,ny,nz,u,v,i,pi,tau; fns: sin cos tan abs sqrt floor sign
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

## Raw per-vertex editing (escape hatch)

- `select_vertices_where { node, predicate }` → returns matching vertex **indices**
  (a read — no cursor). Feed them to the transforms below.
- `soft_transform_vertices { mesh, indices, translation:[x,y,z], falloff }` —
  translate a selection with a smooth radial falloff (`falloff:0` = hard move).
- `set_vertex_positions { mesh, indices, positions:[[x,y,z]...] }` — raw set.
- `collapse_mesh_stack { mesh }` — bake the modifier recipe → raw, clear the
  recipe (then edit vertices freely). Undoable.

### VertexPredicate (for `select_vertices_where`)

```jsonc
{ "kind": "top_percent",   "axis": 1, "percent": 0.12 }   // top 12% along Y (e.g. a rim)
{ "kind": "normal_dir",    "dir": [0,1,0], "threshold": 0.7 } // normals facing ~up
{ "kind": "axis_greater",  "axis": 0, "value": 0.0 }
{ "kind": "axis_less",     "axis": 1, "value": -0.4 }
{ "kind": "within_radius", "center": [0,0,0], "radius": 0.3 }
```
`axis`: 0=X, 1=Y, 2=Z.

**Example — flare a cup's rim:** `select_vertices_where {node, {"kind":"top_percent","axis":1,"percent":0.12}}`
→ `soft_transform_vertices {mesh, indices, translation:[0,0.4,0], falloff:0.5}`.

---

## Introspection (perceive → adjust)

- `get_mesh_stats { node }` → `{ vertices, triangles, bbox_min/max, centroid,
  surface_area, volume, watertight }`.
- `get_mesh_cross_section { node, axis, samples }` → `[[height, radius]...]` (the
  silhouette profile; pairs with a lathe profile). Empty bins read `0` when the
  mesh has no vertices at that height — use a denser mesh or fewer `samples`.

---

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
