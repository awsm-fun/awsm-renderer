# Material data-model unification

**Status:** IN PROGRESS. Step 1 (render-path unification) + the `AssignMaterial`
Mesh/Sweep fix are **done and GPU-verified live** (see below). Steps 3–5 (remove
the dead `material: MaterialRef` field, fold `inline_material` into overrides,
rename) remain — they carry serde-migration + exporter blast radius and should
likewise be done with the editor open (`task mcp-dev`), not shipped blind.

## Done + verified

- **Render paths unified.** `materialize_primitive`, `upload_simple_mesh`
  (captured Mesh), and `materialize_sweep` now all resolve through one helper
  `resolve_assigned_material` (assigned built-in/custom → render; unassigned →
  magenta). Instanced geometry stays on an explicit `MeshMaterial::Flat` (flat
  default + per-instance colours, no assignment concept).
- **`AssignMaterial` now handles Mesh + Sweep.** It previously had only
  `Primitive` + `Model` arms (`_ => Ok(None)`), so assigning a material to a
  converted mesh was a silent no-op.
- **Verified live (screenshots):** fresh Primitive → magenta; convert to Mesh →
  **magenta** (was incorrectly white); assign built-in PBR → renders, on both a
  Mesh and a Primitive; `custom_material` correctly populated.

This delivers the user-facing goal — **0-or-1 assignment, 0 → magenta,
uniformly across all geometry**. The remaining steps are internal cleanup.

## Goal (the model we want)

Every geometry node carries **exactly one optional material assignment plus a
separate per-instance override block**:

- **0 assignment → magenta** (the honest "nothing assigned" sentinel), for
  *all* geometry node types alike.
- **1 assignment** → a material that may be **custom** (user WGSL) **or
  built-in** (PBR / Unlit / Toon / Flipbook). The node *separately* carries the
  overridable settings (uniform values, texture assignments) that layer on top.

This is what the user articulated: "every mesh node carries exactly one or 0
material assignment. If 0, it renders magenta. If it has a material … the mesh
then separately has the overridable material settings."

Note: this is a *data-model* cleanup, **not** removing built-ins. Built-ins stay
(and are already compiled lazily — see the `material_opaque/pipeline.rs` module
docs); they're simply represented through the same one-assignment path.

## Current reality (the muddle)

Each geometry `NodeKind` variant
(`packages/crates/scene-schema/src/tree.rs:140-186`) carries **three**
material-related fields:

| field | type | role today |
|---|---|---|
| `material` | `Option<MaterialRef>` (`MaterialRef(AssetId)`) | **Read only by the GLB exporter** (`controller/export.rs:347,378,394,422`). The render bridge never reads it. Effectively dead for rendering. |
| `custom_material` | `Option<CustomMaterialInstance>` | The real "one assignment + overrides". `CustomMaterialInstance` = `{ material: AssetId, uniform_overrides, texture_overrides, buffer_overrides }` (`scene-schema/src/dynamic_material.rs`). Resolves **both** custom and built-in (built-in via `builtin_merged`). |
| `inline_material` | `MaterialDef` | A whole second per-node PBR material. Double duty: (a) the uniform-merge source for a built-in assignment, and (b) the *standalone* material that Mesh/Sweep render directly. |

`Model` (`scene-schema/src/model.rs:32,40`) is shaped slightly differently:
`material: Option<CustomMaterialInstance>` (not `MaterialRef`) + `inline_material`.

### The inconsistency — three behaviors across four node types

In `packages/frontend/editor/src/engine/bridge/node_sync.rs`:

- **Primitive** (`:323-338` → `materialize_primitive` → `:530,540`): resolves
  `custom_material` → (`builtin_merged(inst, inline)` | `insert_custom` |
  magenta); `None` → **magenta**. `material` (MaterialRef) is dropped.
- **Model** (`:656-665`): same shape — merge `custom_material` + `inline`, else
  **magenta**.
- **Mesh** (`:352-359` → `upload_simple_mesh` → `:919`): renders
  `inline_material` **directly** (`insert_material(&mut r, &inline)`).
  `custom_material` **and** `material` are **ignored**.
- **SweepAlongCurve** (`:345-349` → `materialize_sweep`): renders
  `inline_material` directly. `custom_material` and `material` ignored.

So an unassigned Primitive shows magenta, but an unassigned Mesh shows its
inline material. Assigning a `custom_material` to a Mesh does nothing. This is
the bug behind "set_node_texture data is correct but only renders once converted
to Mesh" and the magenta confusion.

## Target schema

```rust
// On each geometry NodeKind variant (Primitive / Mesh / SweepAlongCurve) and Model:
material: Option<MaterialInstance>,   // the ONE assignment + its overrides; None => magenta
// (drop `material: Option<MaterialRef>` and the standalone role of `inline_material`)
```

Where `MaterialInstance` is today's `CustomMaterialInstance` (rename for clarity;
it already carries the assigned `AssetId` + `uniform_overrides` +
`texture_overrides` + `buffer_overrides`). A built-in is just an `AssetId` that
resolves to a built-in entry, exactly as `builtin_merged` already handles.

`inline_material: MaterialDef` is **removed** as a node field. The PBR knobs it
held become `uniform_overrides` / `texture_overrides` on the assignment. (If a
transitional default is needed, the convert/insert flows assign a built-in
"Standard PBR" instance with the desired overrides — *not* a magic fallback;
just a normal assignment the user can see and edit.)

## Migration (staged — verify each in-browser before the next)

1. **Unify the render bridge.** Make `upload_simple_mesh` + `materialize_sweep`
   resolve via the same `custom_material`-or-magenta path as
   `materialize_primitive` (factor the `:530-540` resolution into one helper used
   by all four). *Verify:* assigned Mesh/Sweep render their material; unassigned
   ones go magenta. **This is the behavior change that will make existing
   inline-rendered meshes magenta until assigned — do it with the editor open.**
2. **Insert/convert assigns a real instance.** `insert_primitive` /
   `convert_to_editable_mesh` (and the sweep insert) assign a built-in "Standard
   PBR" `MaterialInstance` (carrying today's default `MaterialDef` values as
   overrides) so freshly-created geometry renders without magic. *Verify:* new
   primitive/mesh renders as before.
3. **Fold `inline_material` → overrides.** Migrate the per-node `MaterialDef`
   fields into `uniform_overrides`/`texture_overrides` on the assignment; remove
   the `inline_material` field + `builtin_merged`'s inline arg. Add a serde
   migration so old `project.json` files map `inline_material` → a built-in
   assignment with overrides on load. *Verify:* load an old project; materials
   match.
4. **Remove `material: Option<MaterialRef>`.** Update the exporter
   (`export.rs:347-422`) to read the unified `material: Option<MaterialInstance>`
   instead. *Verify:* GLB export of PBR / Unlit / custom / unassigned nodes maps
   correctly (PBR→glTF PBR, Unlit→KHR_materials_unlit, custom/none→
   AWSM_materials_none, unassigned→no material). Round-trip test in
   `glb-export/tests`.
5. **Rename** `CustomMaterialInstance` → `MaterialInstance` and
   `custom_material` → `material` across schema + bridge + MCP + UI (mechanical).

## Touchpoints checklist

- `packages/crates/scene-schema/src/tree.rs` (Primitive/Mesh/SweepAlongCurve),
  `src/model.rs` (Model), `src/dynamic_material.rs` (rename), serde migration.
- `packages/frontend/editor/src/engine/bridge/node_sync.rs` (4 materialize
  paths + `builtin_merged`), `bridge/state.rs:150` (the `model_ref.material`
  reassign-on-delete scan).
- `packages/frontend/editor/src/controller/export.rs` (`map_material_def`,
  `:347-422`) + `packages/crates/glb-export`.
- `packages/frontend/editor/src/controller/state.rs` (command apply arms:
  `patch_builtin_param`, `SetBuiltinTexture`, material assign/copy) +
  `editor-protocol` commands.
- `packages/mcp/src/mcp.rs` (`assign_material`, `set_builtin_param`,
  `set_node_texture`, `copy_material_instance`, …) + `docs/MESH_TOOLS.md`.
- Material-editor + scene-inspector UI that displays/edits these fields
  (actively evolving — coordinate).

## Verification checklist (in-browser, per step)

- Primitive / Mesh / Sweep / Model: assigned renders correctly; unassigned →
  magenta (uniformly).
- Built-in PBR/Unlit/Toon assignment + per-instance uniform/texture overrides
  render correctly.
- Custom WGSL material assignment still works.
- Old `project.json` (with `material` + `inline_material`) loads with matching
  visuals.
- GLB export of each material class round-trips (existing `glb-export` tests +
  a live export).

## Risks

- **Behavior change:** step 1 makes existing inline-rendered meshes magenta
  until assigned — visible regression if shipped without step 2.
- **Persistence:** removing fields needs a serde migration or old projects lose
  their materials.
- **Export:** `material: MaterialRef` is load-bearing for the exporter; it must
  move to the unified field in lockstep (step 4).
- **No native-test coverage:** the render resolution is GPU-only; correctness
  must be eyeballed in the editor. This is why the work is staged and gated on
  live verification rather than committed blind.
