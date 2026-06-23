# awsm-scene MCP — fixes from the acceptance run

Companion follow-up to `mcp-implementation-test.md` and `more-mcp-improvements.md`
(the test suite + the issues found running it). The acceptance run completed
**16 of 20 rows**; this plan closes the gaps. Two buckets:

- **Part 1 — Confirmed fixes (§A).** Reproduced defects with known root cause.
  Implement + add regression tests.
- **Part 2 — Re-verify-then-fix (§B).** Rows blocked because the editor tab was
  **backgrounded** (hidden tab pauses `requestAnimationFrame`, so
  `screenshot_scene` / `canvas_stats` / per-frame world-transform recompute time
  out). Re-run **foregrounded** first; some are likely already-passing, one or two
  may surface a real fix.

> Self-verify everything with a **foregrounded** editor tab via chrome-devtools.
> Start each test from a clean slate (`new_project`), per the suite's preamble.

---

## Part 1 — Confirmed fixes

### F1 (🔴 gating) — `patch_kind` rejects every patch (untyped param → stringified over the wire) — ✅ DONE

**DONE (commit on this branch).** Added `coerce_patch` (pure, host-tested) in
`editor-protocol/merge_patch.rs`: a `Value::String` patch is parsed back to JSON
(unparseable string → clear error), non-strings pass through. Wired into the
`EditorCommand::PatchKind` handler (`state.rs`) so it covers **both** `patch_kind`
and `dispatch_command`. Tightened the schema: `PatchKindParams.patch` now
`#[schemars(with = "serde_json::Map<String, serde_json::Value>")]` (declares an
object) + tool/field descriptions say "send as an object." Host test
`coerce_stringified_patch_then_merge_flips_only_named_field` (+ pass-through +
reject-non-json) added to `merge_patch.rs` — all 10 pass. **Live (foregrounded,
T3-b):** `insert_primitive box` → stringified `patch_kind {"mesh":{"shadow":
{"cast":false}}}` → `ok`, `get_node_details` shows `shadow.cast=false`,
`receive=true`, `material`/`mesh` intact; object-form patch (`shadow.receive`)
also `ok`.

**Test:** T3 (§3) FAIL. `patch_kind` rejects *every* patch with
`patched kind is not a valid NodeKind: unknown variant `{"mesh":...}``.

**Root cause (confirmed by reading the code, not the symptom).** The full type
chain is already correct — `PatchKindParams.patch`, `EditorCommand::PatchKind.patch`,
and the merge core are all `serde_json::Value`, and `merge_patch.rs` has passing
unit tests. The defect is at the **MCP param boundary**:
`serde_json::Value` derives an **unconstrained** JSON Schema (`true`/any), so the
client serializes the patch as a **JSON string** rather than an object. The string
arrives as `Value::String`, `json_merge_patch` takes its "patch is not an object →
replace target wholesale" branch (`merge_patch.rs:20-23`), the merged value becomes
that string, and `from_value::<NodeKind>(string)` fails with the observed
"unknown variant" error. The escaped-quotes variant in the report is the same path.

**Files**
- `packages/mcp/src/mcp.rs:687` — `PatchKindParams { patch: serde_json::Value }`
- `packages/mcp/src/mcp.rs:2795` — `patch_kind` handler
- `packages/frontend/editor/src/controller/state.rs:875` — `PatchKind` command
- `packages/mcp/editor-protocol/src/merge_patch.rs:19` — merge core (correct; leave)

**Fix (do both — tolerant input + a tighter schema):**
1. **Coerce at the handler.** Before dispatch, if `p.patch.is_string()`, parse it:
   `serde_json::from_str(s)` and use the result; if that fails, return a clear
   error. This makes the tool robust to any client that stringifies. (A `Value`
   that is genuinely a string is never a valid NodeKind patch, so coercion is
   safe.) Put this normalization where it benefits both the typed tool and
   `dispatch_command`.
2. **Constrain the schema** so well-behaved clients send an object: give `patch` a
   `schemars` attribute typing it as a JSON object (e.g. a `#[schemars(with =
   "serde_json::Map<String, serde_json::Value>")]` or an explicit
   `schema_with`/description that declares `"type": "object"`), and update the tool
   description to state the patch is an object.

**Regression tests**
- Host test: feed a `Value::String` containing `{"mesh":{"shadow":{"cast":false}}}`
  through the coercion → assert it becomes the object and the merge flips only
  `shadow.cast` (siblings `receive`/`material`/`mesh` survive). The merge-core test
  for this already exists in `merge_patch.rs`; add the **string-coercion** case.
- Live (foregrounded): `insert_primitive box` → `patch_kind {node, patch:{"mesh":
  {"shadow":{"cast":false}}}}` → `get_node_details` shows `shadow.cast=false`, all
  else intact. Pass = T3-b.

---

### F2 (🟡) — opaque **unlit** material silently ignores `base_color_texture` — ✅ DONE

**DONE (commit on this branch).** Real root cause was *not* the shader — the opaque
unlit shader (`compute_unlit_material_color`) already samples `base_color_tex_info`
when its runtime `exists` flag is set (no compile-time feature gate; unlit is pure
runtime). The gap was the **editor lowering**: `insert_material_vc`
(`bridge/material.rs`) had `MaterialShading::Unlit` fall through to the texture-less
`material_to_renderer` path (comment: "Unlit / Toon don't carry texture slots in the
editor yet"), so `UnlitMaterial.base_color_tex` stayed `None` → `exists=false` →
flat. Added an explicit Unlit branch that `resolve_texture`s base-color (Albedo,
sRGB) + emissive (Emissive, sRGB) onto the `UnlitMaterial`, mirroring the PBR/
FlipBook branches. No shader change, no new feature bucket (unlit gates on runtime
`exists`, not a feature bit). **Live (foregrounded):** unlit box, region
`canvas_stats` min_luma 216 (no tex) → **0.0** (checker bound — pure-black tiles
render, correct for unlit's raw-color output). The player path (`scene-loader`)
binds unlit textures via its own post-lower step; this fix is the editor bridge.

### ~~F2 (🟡) — opaque **unlit** material silently ignores `base_color_texture`~~

**Test:** A2 (found building the T★ fixture). Binding a texture to an **unlit**
builtin material round-trips in `get_node_details` but renders **flat white**; the
**identical** flow on **PBR** renders correctly.

**Root cause (confirmed).** The opaque unlit shader stores the tex-info but never
samples it. The transparent material path *does* sample it, which is why it's
unlit-specific and opaque-specific.

**Files**
- `packages/crates/materials/src/wgsl/unlit_material.wgsl:28-61` —
  `unlit_get_material` loads `base_color_tex_info` but only stores it.
- `packages/crates/renderer/.../helpers/material_color_calc.wgsl:499` —
  `unlit_get_material_color` (transparent path) **does** sample correctly; mirror
  this in the opaque unlit variant.
- `packages/crates/.../material_color_calc.wgsl:130` — PBR `base_color` sampling,
  the gating/specialization pattern to follow.
- `packages/mcp/src/mcp.rs:2780` — `set_node_texture` handler.

**Fix (pick one, matching §5/§11's "never store-and-ignore" rule):**
- **(preferred)** Sample `base_color_texture` in the opaque unlit variant —
  re-specialize the unlit shader when a texture is bound (mirror the transparent
  path / PBR feature gate). Unlit+texture is the cleanest way to show a texture's
  raw colors, so it should work.
- **(fallback)** Reject `set_node_texture` on a texture-less unlit variant with a
  clear error, exactly as §11 intends for PBR.

**Verify (foregrounded):** assign unlit + 8×8 checker to a plane; `canvas_stats`
inverts cleanly under a UV offset (same assertion that already passes for PBR).

---

### F3 (🟡 doc/contract) — `new_project` seeds a Directional Light; docs say "empty / IBL-only" — ✅ DONE

**DONE (commit on this branch).** Kept the seeded key light (friendlier default).
Updated `new_project` tool description (mcp.rs) to state it seeds default env+IBL
**and a key Directional light** (not empty) + how to get the IBL-only baseline
(delete the seeded light). Updated `mcp-implementation-test.md`: the "How to run"
preamble + T17 now say to delete the seeded Directional light for the punctual-
light-free baseline, and note T5/T15/T18 start from it. Clean deletion of the
seeded light is exercised in V7. (Description change needs a server restart to
surface; behavior unaffected.)

### ~~F3 (🟡 doc/contract) — `new_project` seeds a Directional Light~~

**Test:** A3. The test doc states `new_project` re-seeds "empty scene" and T17
assumes "IBL-only, no punctual lights," but `NewProject` seeds a Directional Light
+ default IBL.

**Files**
- `packages/frontend/editor/src/controller/state.rs:1064` — `NewProject`
  (seeds Directional Light at ~1100-1110 + default environment).
- `packages/mcp/src/mcp.rs:1944` — `new_project` tool description
  ("Start a fresh, empty project…").

**Fix (decision: keep the key light — friendlier default — and make docs agree):**
1. Update the `new_project` tool **description** to state it seeds a key
   (Directional) light + default IBL environment, not an "empty" scene.
2. Update `mcp-implementation-test.md`: T17 (and any IBL-only assertion) must
   **delete the seeded directional light first** to get the punctual-light-free
   baseline. Note the seeded light in T5/T15/T18 premises too.

(No code change to the seeding itself unless re-running T17 shows the seeded light
can't be cleanly deleted — then make delete work.)

---

### F4 (🟢 doc nit) — stale "evaluation is a follow-on" note on `Modifier::displace` — ✅ DONE

**DONE (commit on this branch).** Deleted the stale "(Evaluation is a follow-on;
carried in the schema now so stacks round-trip.)" parenthetical in `recipe.rs`
(displace IS evaluated, per A4). Replaced with a factual note: expr is evaluated
by the generic math evaluator, + pointers to the vertex-WGSL hook (GPU) and
`displace_from_texture` (data-driven). Did **not** add any `noise()`/`fbm()`
feature-preset (Meta-check held).

### ~~F4 (🟢 doc nit) — stale "evaluation is a follow-on" note~~

**Test:** A4. The `Modifier::Displace { expr }` doc says *"(Evaluation is a
follow-on; carried in the schema now so stacks round-trip.)"* — but it **is**
evaluated (a `sin/cos` displace moved the bbox as expected).

**File:** `packages/crates/meshgen/src/recipe.rs:156-158`.

**Fix:** delete the stale parenthetical so agents don't avoid a working feature.
(Do **not** add `noise()`/`fbm()` to the expr vocabulary — that's a feature-preset
and an explicit Meta-check ❌; the generic WGSL vertex hook + heightmap are the T16
answer and already work.)

---

## Part 2 — Re-verify foregrounded, then fix only if still broken

Run each with the editor tab **visible**. Most are expected to pass at the visual
level (tool/compile halves already ✅). Promote to a fix only on a real failure.

### V1 (possible 🔴) — T8 facing hint / world matrix — ✅ DONE (not a bug; was a backgrounded-tab artifact)

**Re-verified foregrounded — NOT a real 🔴.** With the tab visible:
`insert_primitive box` → `set_rotation_euler [0,π/2,0]` → `set_translation
{value:[3,0,0]}`. `get_node_transforms.world` for the box is the **correct**
90°-Y rotation (column-major `[~0,0,-1, 0,1,0, 1,0,~0]`), *not* identity.
`get_node_bounds`: `forward=[-1,0,~0]` (default -Z forward rotated +90° about Y →
-X ✓), `right=[~0,0,-1]`, `up=[0,1,0]`, and bounds X = `2.5..3.5` (the +3
translation applied). The §B identity-world / unrotated-forward was the paused
render-tick artifact, exactly as suspected. No code change. (Note: `set_translation`
takes `value`, not `translation`.)

(original note below)

### ~~V1 (possible 🔴) — T8 facing hint / world matrix~~

After `set_rotation_euler [0,π/2,0]` + `set_translation [3,0,0]` on a box,
`get_node_transforms.world` returned **identity** and `forward = [0,0,-1]`
(unrotated) — *local TRS was correct*. Strongly suspected to be a
backgrounded-tab artifact (world transforms recompute on the paused render tick).
**Re-test foregrounded.** If `world`/`forward` still ignore the rotation with the
tab visible → the §8 facing hint genuinely doesn't read the world matrix; fix the
world-transform readback so it reflects orientation. **This is the one §B row that
could be a real 🔴 — verify first.**

### V2 — T9 papercuts — ✅ DONE (all 4)

**Verified foregrounded with RiggedSimple.glb (2-bone chain Armature→Bone→Bone.001).**
- **solve_ik chain:** `solve_ik {end_node:Bone.001, root_node:Armature,
  target:[3,5,0]}` → tip reaches **exactly** [3,5,0] (`reach:1.0`); result names
  `mid_node:Bone` + the passed `root_node:Armature` — targets the named chain, not
  a parent-walk into wrong joints.
- **clip-clear / reset_pose:** added a clip with a rotation track on Bone (90° about
  Z); previewing it bends the cylinder horizontal; `set_current_clip {}` leaves the
  pose **baked in the viewport**; `reset_pose {Armature}` restores the base
  (cylinder back to vertical = base screenshot). (`get_node_transforms` reads scene
  base, not the clip preview — verified via screenshots, the renderer-mirror state
  reset_pose operates on.)
- **frame_node {padding:0}:** box tightly fills the viewport (vs the padded default).
- **screenshot error text:** already ✅.

### ~~V2 — T9 papercuts (3 of 4 pending)~~

- `solve_ik` explicit `root_node`/mid-joint chain (schema supports it — needs a
  rig). Confirm you can target a named 2-bone chain, not walk parents into fingers.
- clip-clear / `reset_pose` restores a posed joint to its base transform.
- `frame_node { padding:0 }` tightly fills the viewport.
- (screenshot-error-text papercut already ✅.)

### V3 (critical) — T11 per-node texture override on **PBR** — ✅ DONE (renders; no fix)

**Verified foregrounded — PBR re-specializes and renders, no silent drop.**
`add_builtin_material pbr` (no texture) → box → `set_node_texture base_color
<checker>`: region `canvas_stats` min_luma 216 → **75.5** with the texture
(unbind restores 216, rebind drops again — fully reversible). The PBR per-node
override re-specializes and samples correctly (consistent with §11). No code
change needed.

### ~~V3 (critical) — T11 per-node texture override on **PBR**~~

Confirm `set_node_texture` on a PBR node with a **texture-less** variant either
re-specializes and **renders** the checker, or returns a clear error — not a silent
store-and-drop. (Related to F2, which is the unlit case; if the PBR path silently
drops, fix it the same way as F2.)

### V4 — T13 visual transparency — ✅ DONE

**Verified foregrounded.** Red PBR box behind, blue PBR sphere in front,
`set_builtin_alpha_mode blend` + `base_color [0,0,1,0.4]`. Screenshot: the sphere
is clearly see-through — the red box reads as **magenta** where the sphere
overlaps it (red+blue blend), and the grid floor is visible through the lower
hemisphere. No fix needed.

### ~~V4 — T13 visual transparency~~

Tool/param level ✅. Confirm a blend-mode builtin with `base_color` alpha 0.4 in
front of another object actually renders see-through.

### V5 — T14 particle sprite alpha + forces — ✅ DONE

**Verified foregrounded.** PASS-a: emitter with an agent-uploaded soft radial
sprite (white→transparent falloff, `create_texture`) + `blend:true` → particles
render as **soft round cloud-puffs with gradient edges**, not hard squares (the
emitter samples the sprite alpha). PASS-b: adding `forces:[{gravity:{acceleration:
[6,0,0]}}]` is accepted + round-trips in `get_node_details`, and the particle
stream is visibly **blown to +X** (motion changed) vs the symmetric no-force
cloud. No fix needed.

### ~~V5 — T14 particle sprite alpha + forces~~

Primitives present (`set_particle_emitter` has `texture`/`blend`/typed `forces`).
Confirm soft radial sprite → soft round blobs (not hard squares), and a turbulence/
force changes motion.

### V6 — T16 vertex-hook visual — ✅ DONE

**Verified foregrounded.** 80×80-segment plane (CPU bbox stays flat, y=0) + a
custom material whose `set_material_vertex_wgsl` hook displaces
`position.y += sin(x*3)*cos(z*3)*0.5` (returns VertexDisplaceOutput in local
space). Screenshot shows a clear sinusoidal egg-carton wave surface — the GPU
vertex displacement is unmistakably visible on screen even though `get_mesh_stats`
is unchanged (GPU-only, as expected). `get_material_diagnostics ok:true`. No fix
needed.

### ~~V6 — T16 vertex-hook visual~~

Readback ✅ (fbm vertex-WGSL compiles; `displace_from_texture` grew the bbox).
Confirm the GPU vertex displacement is visible on screen (it doesn't change CPU
`get_mesh_stats`, so this needs pixels).

### V7 — T17 custom material IBL + lights — ✅ DONE

**Verified foregrounded (after deleting the seeded light — F3 caveat confirmed:
deletes cleanly, tree empties).** PASS-a: custom material with `ibl` include
(`sample_ibl(albedo, world_normal, surface_to_camera, roughness, metallic)`),
IBL-only baseline → sphere renders **pink/non-black** with a sky-to-floor
gradient, purely from IBL. PASS-b: second material with `light_access` include
(Lambert loop over `get_lights_info().n_lights` × `light_sample`) + an inserted
directional light → sphere goes from near-black at `set_light_intensity 0` to
bright white at intensity 10. Both `get_material_diagnostics ok:true`. No fix
needed.

### ~~V7 — T17 custom material IBL + lights~~

Compile path ✅ (`ibl`/`sample_ibl`, `light_access`/`light_sample`). Delete the
seeded directional light (F3) for the IBL-only baseline, then confirm: (a) IBL-only
custom-material sphere is **non-black**; (b) a second `light_access` material
brightens with `set_light_intensity`.

### V8 — T18 environment from agent data — ✅ DONE

**Verified foregrounded, both agent-data paths.** (a) `set_environment
{zenith:[0,0.8,0.1], nadir:[0.4,0,0.6]}` → sky turns green at top, floor picks up
the purple nadir tint (skybox + IBL both change from the neutral default). (b)
`set_environment {equirect: <agent-drawn 64×32 PNG with orange/cyan/magenta
bands>}` → horizon skybox shows the cyan mid-band, floor reflects the magenta
bottom-band via IBL. Sky/IBL visibly become the agent's content. No fix needed.

### ~~V8 — T18 environment from agent data~~

Primitives present (`set_environment` `equirect`/`zenith`/`nadir`). Confirm the
sky/IBL visibly changes to agent-supplied content.

---

## Sequencing

1. **F1** (gating 🔴) — the only fully-broken generic deliverable. Do first.
2. **V1 (T8)** — re-verify foregrounded; if real, it's the second 🔴.
3. **F2 / V3 (T11)** — the unlit + PBR texture-override pair; fix together (same
   "specialize or reject, never silently drop" rule).
4. **F3, F4** — doc/contract fixes (cheap).
5. **V2, V4–V8** — foregrounded visual re-runs; fix only on real failure.

## Done criteria

Every row in `mcp-implementation-test.md`'s results checklist is ✅ (or a ⚠️
explicitly signed off), the Meta-check stays PASS (no feature-presets added), and
F1–F4 ship with regression tests where a host test is possible (F1 string-coercion
in `merge_patch`/handler tests).
