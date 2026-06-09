# Unified mesh model + scene/player split

**Status:** IN PROGRESS. Prereqs + the geometry unification + per-vertex authoring
are landed + live-verified (overnight). See progress below.

## Implementation progress (overnight, autonomous)
Order was corrected from the doc's (split-first was infeasible — see §"the finding"
in chat; the split's runtime/authoring line is defined by the unification).
- ✅ **Step 0 — dev-loop prereqs** (`8beb7146`): trunk watches editor-protocol/
  meshgen/glb-export; editor auto-reconnects to the MCP server. Both verified.
- ✅ **Step 1 — unify NodeKind** (`6eaf0d9c`): Primitive + Sweep → one
  `Mesh{ mesh, material, shadow }` backed by `MeshDef{ stack: ModifierStack }`.
  Verified live: insert→magenta, add_modifier subdivide+twist→re-bake, material
  →render, export-path→correct geometry.
- ✅ **Step 2 — import normalization** (`0dbb3e3c`): glTF imports bake to captured
  `Mesh{Captured}`; `NodeKind::Model`/`model_source_cache` retired (geometry
  unification complete). Verified live: Box.glb → a `mesh` node renders with its
  material. **KNOWN GAP: skinned imports bake to bind pose** (see below).
- ✅ **Step 5 — per-vertex authoring** (`ddad79dc`): `MeshDef.overrides`
  (sparse pos/color/normal/uv), terminal-collapse rule, PaintVertexColors/
  SetVertexNormals/BakeAll + GetVertexData/GetMeshLayers, position-verb migration.
  Verified live: soft_transform pulls a sphere pole into a teardrop (collapse +
  override + re-materialize). Paint-render + the new typed verbs need a server
  restart + a vertex-color material to demo (deferred).
- ⏸ **Step 4 — crate split**: NOT started — deferred. It's the most delicate
  cross-crate work AND its `awsm-scene` runtime-mesh type depends on the
  **skinning decision** (the known gap). Best done with that decided + fresh
  context. (Step 3's bake/collapse is effectively covered by collapse + BakeAll.)
- ⬜ Steps 6 (bundle = awsm-scene dir — needs step 4), 7 (watch-it-work UI —
  ungated but visual), 8 (splat test — needs paint-render + a splat material).

This plan unifies how geometry is represented, edited, persisted, and exported,
and splits the schema into a lean runtime crate and an authoring crate. It folds
in the open TODOs from the mesh-editing arc (model export, animation, bundle,
the agent-activity UI) and is the spine for the next implementation arc.

---

## 1. The core model — every mesh is `base + edits`

Today geometry is fragmented across four node kinds (`Primitive`, `Mesh`,
`SweepAlongCurve`, `Model`) with three different "is it editable / does it have a
recipe" stories. **Collapse them into one.**

**Every geometry node is a single `Mesh` kind = `{ base, edits }`:**

- **base** — a generator:
  - procedural: `Primitive(box/sphere/cylinder/cone/torus/plane)`, `Lathe`,
    `Superquadric`, `Sweep(curve + cross-section)`, `Sdf(graph)`;
  - **blob**: captured/imported triangle geometry (an asset).
- **edits** — an ordered, non-destructive list, in two tiers:
  1. **Procedural modifiers** (`taper`, `twist`, `bend`, `inflate`, `spherify`,
     `roughen`, `subdivide`, `smooth`, `mirror`, `array`, `displace`): re-run from
     the base on every evaluation → freely add/remove/reorder/re-parameterize.
     Topology-changing ones (subdivide/array/mirror) are fine because they
     re-evaluate from scratch.
  2. **Per-vertex authoring** (sculpt position, paint color, set normal, set UV):
     **index-based on a fixed topology**. Cannot re-run from a base that might
     change underneath it.

The on-disk `.bin` for a mesh is a **cache** of evaluating `base → edits`.

`NodeKind::Primitive` / `SweepAlongCurve` / `Model` are **retired**. A box is
`Mesh { base: Primitive(box), edits: [] }`; an import is `Mesh { base: Blob(baked
geometry), edits: [] }`. This is consistent with the material-model collapse
(one path, no special cases).

### The one rule (and the only irreversible step)

> **Per-vertex authoring bakes the procedural stack below it.**

Everything *above* the bake line is freely editable; crossing into per-vertex
authoring collapses `base + modifiers` into a fresh **blob base**, after which
edits are sparse per-vertex overrides on that fixed topology. That is the *only*
collapse boundary. Asked plainly: procedural modifiers are reversible/
re-parameterizable; per-vertex authoring is not (it's index-bound), so it's the
one thing that bakes.

### Attributes

- **Normals & tangents** are **derived**, never edits: recomputed after
  evaluation (a deformer changes the surface). A blob base carries source normals
  as the starting attribute; they regenerate the moment edits run. Tangents are
  UV+normal-derived. **Exception:** a per-vertex *authoring* edit may override
  normals (custom/hard-edge normals) — applied last, so nothing recomputes over
  it.
- **Vertex colors / UVs** are **attributes carried through evaluation**:
  interpolated on topology change (subdivide), untouched by deformers, sourced
  from a blob base or *authored* by a per-vertex edit (vertex paint).

Principle: **positions are edited; everything else rides along (interpolated on
topology change, normals/tangents recomputed) unless a per-vertex authoring edit
overrides it.**

---

## 2. Editing is MCP-only; the UI is informational

There is **no manipulation UI** — every edit is an MCP command. The UI's job is
to *show* what the agent did (this is what unlocks capabilities a human can't do
by hand). So the model needs **more MCP verbs**, plus **informational** UI.

**Perceive → act → re-perceive is the contract.** The agent has no eyes; it
works blind unless it queries. So *every* authoring verb has a **read counterpart**,
and the design rule is: the agent reads state, acts, then reads again to verify —
it never assumes. Verbs return a structured result describing what changed (not
just "ok"); queries are cheap and total. The MCP docs (`awsm://docs/mesh-tools`)
spell out the loop so an agent with zero prior knowledge can self-orient.

Act (the agent's hands) — existing: `insert_primitive`, `add/set/remove_modifier`,
`set_mesh_modifiers`, `set_vertex_positions`, `soft_transform_vertices`,
`set_vertex_selection`, material + animation tools. New/expanded:
- `paint_vertex_colors { node, indices, color }` — attribute authoring (color).
- `set_vertex_normals { node, indices, normal }` — authored/hard normals.
- `set_vertex_uvs { node, indices, uv }` — authored UVs (later).
- `collapse_mesh { node }` — **bake**: `base + edits` → blob base, edits cleared.
- `bake_all` / bake-on-export — finalize the whole scene to the runtime form.

Perceive (the agent's eyes) — existing: `get_snapshot`, `get_node_details`,
`get_node_bounds`, `get_node_transforms`, `get_mesh_stats`,
`get_mesh_cross_section`, `get_mesh_modifiers`, `select_vertices_where`,
`get_material_*`, `canvas_stats`, `screenshot_scene`, `get_console_logs`,
`wait_render_settled`. New/expanded:
- `get_mesh_layers { node }` — the layer stack: base + each edit + **where the
  bake line is** (so the agent knows what's still re-parameterizable vs locked).
- `get_vertex_data { node, indices, attrs }` — read back positions/normals/colors/
  uvs for selected verts → verify a paint/sculpt/normal edit actually landed.
- `get_capabilities` — the agent's own verb/param catalogue (same content the
  human capability modals show), so a cold agent can discover what it can do.
- `preview_bundle { }` — what `bake_all` + export *would* produce (asset/clip
  inventory, sizes) without writing — lets the agent reason about the artifact.

All per-vertex authoring verbs implicitly **collapse first** (the one rule), say
so in their structured result, and are a discrete undo step (byte-level inverse).
`wait_render_settled` remains the barrier before any `screenshot_scene` so the
agent never reads a mid-recompile frame.

### Informational UI (read-only)

- **Layer panel**: base + each modifier + the bake line, **color-coded**
  (green = live/procedural/re-editable, locked = baked-below) with **info
  buttons** explaining each layer and *why* it's locked. Mirrors `get_mesh_layers`.
- **Capability info modals**: human-readable "here's what the agent can do" (the
  same content as `awsm://docs/mesh-tools`, surfaced for humans).

### Watch-it-work (the wow factor) — concrete plan

The experience: a human opens the editor, an agent is connected, and they
**watch the model build itself** — narrated and spotlit. Mechanism (all on the
existing command stream the editor already receives over `remote.rs`, which today
only drives the 🤖 idle/working pulse):

1. **Command → UI focus map.** Each inbound `EditorCommand` resolves to a
   *focus target* + a short human phrase: `add_modifier` → layer panel +
   "added a twist"; `paint_vertex_colors` → the viewport + "painted 240 verts";
   `insert_primitive` → outliner + "added a box"; `collapse_mesh`/export →
   their affordances. One table, command-kind → (target, phrase).
2. **Transient highlight.** The focus target pulses/outlines for ~1s (reuse the
   `mcp-pulse` keyframe + the vertex-highlight overlay we already have). The
   relevant panel auto-reveals/scrolls so the human's eye lands there.
3. **Activity feed.** A compact, auto-scrolling narration strip — "🤖 added a
   twist · painted 240 verts · baked mesh · exported bundle" — built from the
   same command stream (the editor already gets it; we just render it). This is
   the spine of the wow factor: a live, readable story of what the agent did.
4. **Camera follow (optional).** When a command targets a node, gently frame it
   (reuse `frame_node`) so off-screen work isn't missed.

It degrades gracefully: with no agent connected it's silent; the pulse already
shipped, so this is *additive* on a proven channel. Capability modals (above)
give the idle human the "here's the cool stuff it can do" tour; the activity
feed + highlights give the live show.

---

## 3. Persistence: A-default with an explicit bake

- **Default (A):** the editor project persists the full `base + edits` stack →
  reopen and the agent re-dials a modifier. Re-editable across sessions.
- **Bake (explicit):** `collapse_mesh` (per-mesh) / `bake_all` collapse stacks →
  blob bases (cheap *primitive* bases stay procedural). You bake for fast load,
  to lock something in, or to finalize. Bake is just *collapse, when you choose*.
- **Export always bakes** (the runtime artifact is finalized regardless).

So C ("everything baked") is simply "A after `bake_all`". You get re-editability
by default and a lean fast-loading artifact on demand.

---

## 4. Two schemas, two crates (separation of concerns)

```
awsm-scene                      (runtime: scene.toml schema + project-dir layout + read/write)
   ▲                            └─ depended on by: renderer, player, and the bake target
   │ depends on (reuses core types)
awsm-editor-protocol  (authoring: bases + edits + EditorCommand/EditorQuery + editor↔mcp shared)
                                └─ depended on by: editor frontend, awsm-renderer-mcp (native server)

awsm-glb-export                 (interop "Export GLB" download ONLY — no bundle role)
```

### `awsm-scene` (new, lean, canonical runtime)
- The **`scene.toml`** type: node hierarchy + transforms + **runtime meshes
  (`blob | primitive` only)** + materials + lights + cameras + animation clips +
  env — all referencing assets **by id**.
- The **project-directory** model + read/write: `scene.toml` + `assets/`
  (`<id>.bin` mesh blobs, `<id>.png` textures, custom-material `.wgsl`/`.toml`
  side-files). Self-contained: "load a player project directory."
- **No** edit types, no modifier stacks, no bases beyond primitive. The player
  repo + renderer touch only this crate.

### `awsm-editor-protocol` (today's protocol crate, extended)
- Stays the editor↔MCP crate it already is (no need to spell out "mcp" — that's
  what the protocol *is*); we grow it rather than add a new crate.
- Depends on `awsm-scene` and **reuses** its core types (transforms, materials,
  lights, cameras, clips, node hierarchy).
- Adds the **authoring layer**: the full base set, the modifier stack, per-vertex
  authoring metadata, the editor's `Mesh = base + edits`.
- Holds the **`EditorCommand` / `EditorQuery` protocol** (must compile for the
  native MCP server — the agent sends a `ModifierStack`/base over the wire), plus
  any other editor↔mcp shared types.

### `awsm-glb-export` reverts to interop-only
With `scene.glb` gone from the bundle, glb-export's bundle assembler/`write_to_dir`
are **superseded** by `awsm-scene`'s directory writer. glb-export keeps **only**
the standalone interop GLB path (the "Export GLB" download button): bake a scene/
subtree → a portable `.glb` for Blender/other engines (lossy for custom WGSL
materials, as today). Orthogonal to the player pipeline.

---

## 5. The player bundle = a finalized `awsm-scene` directory (no `scene.glb`)

**Bake = editor → player:** evaluate `base + edits` → collapse to blobs (keep
cheap primitive bases procedural), drop authoring metadata, emit an `awsm-scene`
project directory:
- `scene.toml` (the `awsm-scene` scene),
- `assets/<id>.bin` baked meshes, `assets/<id>.png` textures,
- custom-material `.wgsl`/`.toml` side-files,
- animation clips in **our** format (full fidelity — material-uniform / light /
  camera / morph tracks, not just TRS; no `KHR_animation_pointer`, no glTF — the
  player is ours and reads our clips directly).

**Player meshgen = primitives only.** The player generates `Primitive` bases
(+ their normals/tangents) from params; **everything else bakes to blobs**
(sweep, SDF, any edited mesh). The player stays dumb: primitive-gen + blob-load +
materials + clips + env. (Procedural-where-cheap is therefore limited to
primitives; sweeps/SDF bake — accepting slightly heavier road/CSG geometry for a
simple player.)

`scene.glb` is **removed entirely** — the bundle is an `awsm-scene` directory.

---

## 6. Imports normalize at import (kills the source-glb problem)

Importing a glTF **bakes its geometry into blob meshes at import** (reusing the
`extract_node_mesh` path), creating native `Mesh { base: Blob }` nodes + our
materials + our clips. No foreign "model source" survives past import → the
`model_source_cache`, blob-URL revocation, source re-read, and external-`.bin`
limitation all disappear. (The full `GltfLoader` resolves external buffers at
import, so there's no raw-bytes re-read to choke on.)

---

## 7. Texture splatting — the headline test case

`paint_vertex_colors` (MCP) + a custom WGSL material that blends textures by
vertex color = **texture splatting for free** — a thing a human can't hand-author
but an agent can. End-to-end scenario: import/insert terrain → agent paints
vertex-color weights → assign a splat-blend custom material → export bundle →
verify the player reproduces it. Exercises attribute-authoring × custom materials
× the bundle in one shot.

---

## 8. Migration order (each step: `task lint` + tests green, MCP/browser-verified)

0. **Dev-loop fixes (§10.1–2)** — trunk watches the protocol/scene crates +
   editor auto-reconnects to the MCP server. Tiny, and it removes the manual
   browser-reload friction so every later step verifies cleanly.
1. **Crate split.** Carve `awsm-scene` (lean runtime + project-dir) out of
   today's `awsm-scene-schema`; fold the remaining authoring types into the
   existing `awsm-editor-protocol` (which gains a dep on `awsm-scene`). Point the
   renderer at `awsm-scene` + update its `From` impls. Retire `awsm-scene-schema`.
   (No behavior change; pure restructuring — do it first, lint-gated, since
   everything else builds on it.)
2. **Unify `NodeKind`** to one `Mesh { base, edits }`; retire `Primitive`/`Sweep`/
   `Model` kinds (bases instead). Bridge materialize through one path.
3. **Import normalization** (bake to blob at import; drop `model_source_cache`).
4. **Bake/collapse** as the single recapture op + `bake_all` + bake-on-export.
5. **Attribute-general per-vertex authoring** (extend the override layer to
   color/normal/UV) + the new act/perceive MCP verbs (`paint_vertex_colors`,
   `set_vertex_normals`, `get_vertex_data`, `get_mesh_layers`, `get_capabilities`,
   `preview_bundle`) — each act verb shipped with its read counterpart.
6. **Player bundle = `awsm-scene` directory**; remove `scene.glb`; full-fidelity
   clips; `awsm-glb-export` → interop-only.
7. **Informational UI + watch-it-work**: layer panel, capability modals, and the
   command→focus highlight + activity-feed narration (§2 wow factor).
8. **Texture-splatting** end-to-end verification.

## 9. Verification checklist (GPU/MCP, per step)
- Box/sphere/sweep/SDF/import all render as `Mesh{base,…}`; unassigned → magenta.
- `add/set/remove_modifier` re-parameterize live; `get_mesh_layers` reflects the
  stack + bake line.
- `paint_vertex_colors` / `set_vertex_normals` collapse-then-author; visible in
  the vertex highlight; survive export.
- `collapse_mesh` / `bake_all` → fast-loading blobs; geometry unchanged.
- Import → native blob mesh; save/reload/export all uniform; no source re-read.
- Export → an `awsm-scene` directory the player loads (primitives as params, rest
  blobs); animated material uniforms replay; texture splatting reproduces.
- `awsm-glb-export` still produces a portable interop `.glb` (download button).
- **Perceive:** `get_vertex_data` reflects a paint/sculpt/normal edit; `get_mesh_layers`
  shows the live-vs-baked line; `preview_bundle` matches what export writes.
- **Watch-it-work:** agent commands narrate in the activity feed + spotlight the
  right panel; **a server restart reconnects with no manual tab reload** (the §10
  dev-loop fixes — verified by restarting `awsm-renderer-mcp` mid-session).

## 10. Dev-loop prerequisites (so this is implementable start-to-finish)

The friction last arc was the **verify loop**, not the code — and it cost manual
browser reloads. Two small fixes up front remove it; do them as **step 0**:

1. **Make trunk watch the schema/protocol crates.** trunk watches only the
   editor crate dir, so an edit to `awsm-scene` / `awsm-editor-protocol` didn't
   reliably trigger an editor rebuild → the wasm ran stale and new commands
   "didn't exist" until forced. **Fix:** add those crate paths to `Trunk.toml`
   `[watch] paths`. One-line ergonomics win; reliable rebuilds on protocol edits.
2. **Editor auto-reconnect to the MCP server.** New `EditorCommand`/`EditorQuery`
   variants + new tools require rebuilding/restarting the native `awsm-renderer-mcp`
   (it deserializes the wire types + registers tools), and a server restart drops
   the editor's WebTransport session — today that needs a **manual tab reload**
   (the exact thing that slowed us). **Fix:** in `remote.rs`, when the session
   ends and a `?mcp=` origin was set, **re-dial with backoff** — so restarting the
   server reconnects seamlessly, no human in the loop.
3. *(Caveat, client-side, non-blocking)* an MCP client may cache the server's
   tool list at connect, so brand-new typed tools can be invisible until the
   client reconnects its MCP session. The raw `dispatch_command` / `run_query`
   escape hatches reach any rebuilt editor without needing the new typed tool, and
   a fresh client session picks them up — so with §10.1–2 this is a non-issue.

## Resolved decisions (formerly open questions)
- **Runtime clip format** = the editor clip schema **verbatim**. The clip types
  live in `awsm-scene` (shared, runtime-owned); the editor uses them directly, so
  bake is a *copy*, not a translation. Any editor-only authoring metadata is kept
  *out* of `awsm-scene` (the authoring crate adds it on top if needed).
- **Baked-asset ids are content-hashed** (reuse the existing
  `content_hash` / `asset_disk_path` path), so re-baking identical geometry is
  stable — no `assets/` churn, no dangling references.
- **Per-vertex authoring is terminal** for a mesh: once it has a vertex-authoring
  layer, no new procedural modifiers stack above it. Further procedural work
  requires an explicit new base (re-import / new generator). This is the simplest
  rule and sidesteps index-vs-topology fragility; chosen over re-collapse.
- **Player runs primitives only**, but `awsm-scene`'s mesh enum is designed to
  *tolerate* procedural non-primitive bases (sweep/SDF) so a future smarter player
  needs no format change; **bake defaults to baking everything except primitives.**

(All decided; nothing blocks starting. Verify-during-implementation only: confirm
no editor-only field actually leaks into the `awsm-scene` clip type during the
crate split.)

## Skinning — DECIDED: editable XOR skinned, `drop_skinning` is the terminal door
**Premise (agreed):** per-vertex skin weights cannot survive topology-changing
edits (subdivide / boolean / decimate remap or delete vertices, so the per-vertex
JOINTS/WEIGHTS arrays become meaningless). So **editing and skinning are mutually
exclusive** — we don't auto-rebind.

**Model:** a mesh is exactly one of:
- **Editable** — `Mesh{ base + edits }`, static, the unified model above.
- **Skinned** — carries skin data (JOINTS/WEIGHTS + a skeleton ref), deformed by
  the renderer's existing skinning path; **not editable** (no base/edits/overrides).

**`drop_skinning`** (MCP **and** a UI affordance) is the explicit, **terminal**
bridge: it bakes the skinned mesh to a static **Editable** `Mesh{Captured}` at bind
pose, discarding JOINTS/WEIGHTS. It is a **hard prerequisite** for *any* mesh op on
a skinned mesh — editing a still-skinned mesh **errors** ("drop_skinning first").
This mirrors the per-vertex-authoring "terminal" rule.

**Import:** skinned glTF nodes are **preserved as Skinned meshes** (they render +
animate); static nodes bake to Editable `Mesh{Captured}` (as step 2 does today).
→ **Fixes the step-2 regression**: today step 2 silently bakes skinned imports to
bind pose; the correct behavior preserves skinning until `drop_skinning`.

**`awsm-scene` (crate split) implication:** the runtime mesh is an enum
`Editable(blob|primitive) | Skinned(skin data + skeleton ref)`. (Justified
exception to "one mesh" — skinned is a genuinely different *rendering* category,
not an editing variant.)

**Future upside (noted, exciting):** with a Skinned representation + our clip/
skeleton model, the agent can author *our own* skinning/morphs — bind a skeleton,
**paint weights via the per-vertex authoring layer** (`paint`/`set_vertex_*`),
define morph targets. Agent-driven rigging. Out of the current arc's scope.

**Near-term work (revises step 2 + feeds step 4):** add a Skinned mesh kind +
route imported skinned nodes to it (renderer skin path) + `drop_skinning` (MCP +
UI) + the edit-guard. Do before/with the crate split (awsm-scene mesh enum).

## Out of scope (handoff)
- The **player runtime/loader** lives in the separate game-player repo; it
  consumes an `awsm-scene` directory. This plan defines that contract; it does
  not build the player.
