# Agent Guide — Building Scenes & Games over MCP

This is the playbook for an AI agent driving the **awsm-renderer editor** through
the MCP server. It assumes you can call the editor's MCP tools (see
[`MCP.md`](MCP.md) for the connection + full tool catalog). Read this first; it
turns the tool list into a workflow.

> **Companion docs** (also available as MCP resources):
> - `awsm://docs/agent-guide` — this file
> - `awsm://docs/mcp` — connection + complete tool reference
> - `awsm://docs/material-recipes` — copy-paste custom-material WGSL recipes
> - `awsm://docs/animation` — clips / tracks / keyframes
> - `awsm://docs/material-contract-opaque` / `-transparent` — the WGSL ABI

---

## 1. The core loop

Every visual change follows the same rhythm. **Do not skip the settle + verify
steps** — pipeline compiles are async, so a screenshot taken too early shows a
stale or half-compiled frame.

```
1. mutate        – call one or more scene/material/animation tools
2. settle        – wait_render_settled            (barrier: recompiles drain, a frame presents)
3. observe       – screenshot_scene   and/or      canvas_stats   and/or   get_snapshot
4. analyze       – did it match intent? if not, adjust and repeat
```

- **Always** `wait_render_settled` after a batch of mutations, before a
  screenshot. It returns `{ settled: true, waited_ms }`.
- `screenshot_scene { width? }` returns a PNG of the viewport. `canvas_stats {
  region? }` returns mean/min/max luma — cheap way to confirm "something is
  rendering" or "the object is in frame" without eyeballing every frame.
- After authoring a custom material, **always** check `get_material_diagnostics
  { asset }` → `{ registered, ok, errors }`. `ok:false` means the WGSL failed to
  compile; read `errors`.

## 2. Discover before you build

- `get_snapshot` — the whole world: node tree, selection, materials, textures,
  clips, project metadata. Call it to learn current ids before mutating.
- `get_material_contract { transparent? }` — the exact WGSL ABI (inputs,
  outputs, helpers, legal keys) you must follow when writing a custom material.
  **Read this before your first `set_material_wgsl`.**
- Resources (`awsm://docs/...`) and prompts (`author_lit_material`,
  `setup_rotation_clip`, `import_and_frame_model`) are step-by-step templates.

**IDs:** creation tools (`insert_primitive`, `add_custom_material`, `add_clip`,
`add_texture_asset`, …) return the new id as text — capture it. Many tools also
accept a caller-minted id so you can plan ids up front; either way, never guess
an id, read it back from the tool result or `get_snapshot`.

## 3. End-to-end: a lit, textured, animated scene

A complete first scene, tool by tool (capture returned ids where noted):

```jsonc
new_project                                             // empty scene
insert_light   { "kind": "directional" }                // → light id; a key light
insert_primitive { "shape": "sphere" }                  // → node id
add_texture_asset { "proc": "checker" }                 // → texture id
add_custom_material                                      // → material id
set_material_layout { "material": <mat>,
                      "textures": [{ "name": "tex", "ty": "texture_2d<f32>" }] }
set_material_wgsl   { "material": <mat>, "wgsl": "<see material-recipes>" }
get_material_diagnostics { "asset": <mat> }             // expect ok:true
assign_material     { "node": <node>, "material": <mat> }
set_material_texture { "node": <node>, "slot": "tex", "texture": <texture> }
frame_node          { "node": <node> }                  // fit it in view
wait_render_settled
screenshot_scene    { "width": 640 }                    // verify
```

If the sphere is **magenta**, no material is assigned (magenta is the
missing-material sentinel). If it's **flat black**, you likely have no light /
IBL, or your shader is unlit and returns black. See §6.

## 4. Lighting & environment

A new project seeds one directional light + a default environment, so scenes
aren't born black. To light deliberately:

- **Directional** (`insert_light { kind: "directional" }`) — a sun; the workhorse
  key light. Aim it by rotating the node (`set_rotation_euler`).
- **Point** (`kind: "point"`) — omni; set `set_light_range`.
- **Spot** (`kind: "spot"`) — cone; set `set_light_range` + `set_light_angles {
  inner, outer }` (radians).
- Per-light: `set_light_color { node, color }` (linear RGB 0..1),
  `set_light_intensity { node, value }`.
- **Environment / IBL**: `set_environment { skybox?, ibl_prefiltered?,
  ibl_irradiance? }`. Omit args for the built-in default sky + IBL; pass KTX URLs
  for custom. IBL is what makes PBR materials read correctly — keep it on.

Rules of thumb: a single directional light at intensity ~1–4 + default IBL gives
a readable scene. **"Scene looks dark"** → raise intensity, confirm a light
exists (`get_snapshot`), or call `set_environment {}` to restore default IBL.

## 5. Batch for fewer round-trips

Use `dispatch_batch { commands: [...] }` to apply many `EditorCommand`s as **one
undo step** and one round-trip — ideal for building a hierarchy or setting many
properties at once. Use `dispatch_command` / `run_query` as escape hatches for
anything without a dedicated typed tool (the typed tools cover the common path;
these cover 100% of the protocol).

Prefer one batch over dozens of single calls when laying out a scene. Still
`wait_render_settled` once after the batch.

## 6. Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Mesh is flat **magenta** | No material assigned (magenta = sentinel) | `assign_material { node, material }` |
| Mesh is **black** | No light/IBL, or unlit shader returns black | add/raise a light; `set_environment {}`; in WGSL add base color / `apply_lighting` |
| Screenshot is **blank/empty** | Took it before the frame presented | `wait_render_settled` first; confirm object framed (`frame_node`, `canvas_stats`) |
| `set_material_wgsl` returns an **error** | WGSL didn't compile | Read the error; it quotes the offending line. Re-read `get_material_contract` |
| `get_material_diagnostics` `ok:false` | Same as above | Fix the WGSL; the `errors` array has details |
| Custom material samples **wrong/black texture** | Slot not bound, or slot name mismatch | `set_material_texture { node, slot, texture }`; slot name must match the layout |
| Nothing changed visually | Forgot to settle, or mutated the wrong id | `get_snapshot` to confirm the id; `wait_render_settled` |

## 7. Determinism for screenshots

Temporal materials (anything reading `frame_globals.time`) and playing
animations advance with wall-clock time, so two screenshots differ. To pin a
frame for reproducible captures: `set_frame_time { seconds }` (and
`set_playhead { t }` for animations), screenshot, then `clear_frame_time` to
resume. See [`TEMPORAL_SHADERS.md`](TEMPORAL_SHADERS.md).

## 8. What's in scope

This is a **renderer + scene/material/animation editor**. In scope: meshes,
primitives, glTF import, transforms/hierarchy, PBR + custom-WGSL materials,
textures, lights, IBL/skybox, cameras, keyframe animation, screenshots. **Out of
scope** (no engine for them here): physics/collision, input handling, audio,
gameplay scripting, 2D UI/text. Build the *look* and *content* of a game here;
wire behavior/physics in your host engine.
