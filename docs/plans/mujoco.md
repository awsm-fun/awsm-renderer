# MuJoCo — remaining gaps

The feature is complete and verified. This doc is only what is left; how the
feature works lives in `docs/mujoco.md` and should stay there.

Branch `mujoco`, never pushed, `main` untouched. Templates work is in
`../templates/physics-mujoco`, its own repo, also unpushed.

**One item remains.** Everything else on this plan is closed — see "Closed"
below before re-litigating anything.

## Open — quadratic flex weights are still 7x f32 per vertex

Body-attached flexes are **done** (see Closed). What remains is the quadratic
case, which no format tweak reaches.

### Where it stands

`shipped` is after the bundle's meshopt + quantization — what a player actually
downloads.

| model | kind | shipped before | shipped now |
|---|---|---|---|
| `flag.xml` | body-attached | 24.7 KB | **16.1 KB** (−35%) |
| `poncho.xml` | body-attached | 71.3 KB | **47.1 KB** (−34%) |
| `bunny.xml` | trilinear | 41.6 KB | 41.1 KB |
| `bunny_quadratic.xml` | quadratic | 260.9 KB | **259.5 KB** |

Quadratic did not move, and that is expected: the export-side joint narrowing
only recovered what the bundle compressor was already doing (`joints_fit_u8`).
The remaining cost is the **weights**, and they cannot be quantized — Lagrange
goes negative, and glTF's `WEIGHTS_n` allows only FLOAT, normalized
UNSIGNED_BYTE, or normalized UNSIGNED_SHORT. No signed option, no scale/offset
carrier to bias into. So: seven f32 VEC4 sets, 112 bytes of weights per vertex,
274 KB raw on a 2503-vertex bunny.

### The fix, and why it is not a shader change

A quadratic flex's 27 weights are a **pure function of 3 numbers** — the
vertex's normalized cage coordinate, which MuJoCo already stores in
`flex_vert0`. The tensor product of the 1D Lagrange basis reconstructs all 27
exactly. Store the 3, drop the 54.

It does **not** need a shader change. The renderer reads joint indices and
weights from a storage buffer, not from vertex attributes — the GLB is only
transport — so the expansion happens on the CPU at load, filling the identical
buffer the GPU reads today.

- **on disk**: 3 values per vertex (12 bytes f32, or 6 as normalized u16 — these
  ARE in `[0,1]`, so unlike the weights they quantize cleanly) replacing 27
  weights + 27 indices, i.e. 172 bytes → 6.
- **at runtime**: byte-identical to today. No shader variant, no new skinning
  path, nothing already working is at risk.
- **estimated**: `bunny_quadratic` 259.5 KB → roughly 45 KB, in line with the
  trilinear bunny's 41.1 KB.

The same mechanism covers **trilinear** (8 weights from the same 3 numbers, 2
sets → 1 attribute), so build it for `order` 2 and 3 at once — `cage_weights` in
the exporter is already written that way.

### Where to put it — this is the part to get right

The one real design question, already answered: **expand at
`ExtractedSkin::packed_index_weights()`, and carry the compact form
everywhere else.**

`packed_index_weights()` (`glb-export/src/extract.rs:796`) is the SINGLE
chokepoint both production consumers go through:

- editor: `packages/frontend/editor/src/engine/bridge/node_sync.rs:1227`
- player/loader: `packages/crates/scene-loader/src/lib.rs:3794`

Expand there (plus `set_count()`, same file) and both paths get it for free,
with no loader or editor change at all.

**Do NOT expand inside the extractor's reader.** `reexport_clean`
(`extract.rs:40`) is GLB → `GlbScene` → written back out, and the editor runs it
on every save. Expanding on read means the re-export writes the fat form back
and the entire saving evaporates on first save — silently. This is the same
trap that produced the original dropped-`JOINTS_1` bug; `reexport_clean` must
carry the compact encoding through untouched.

So the compact form is a first-class field, parallel to
`extra_influence_sets`:

| layer | file | change |
|---|---|---|
| exporter | `packages/tools/mujoco-export-cli/src/mesh.rs` | `flex_influences` returns the cage encoding (coords + node list + `order`) instead of expanded sets; `cage_weights`/`cage_lattice` already exist and stay |
| schema | `packages/crates/glb-export/src/lib.rs` | new field on `ExportNode` beside `extra_influence_sets` |
| write | `packages/crates/glb-export/src/write.rs` | emit coords as an attribute + the marker (order + joint list) |
| read | `packages/crates/glb-export/src/extract.rs` | read it back into the SAME field — that is what makes `reexport_clean` preserve it |
| expand | `extract.rs` `packed_index_weights()` / `set_count()` | synthesize the 8 or 27 influences on demand |

Keep the expanded path working too: a rig authored before this change, or any
ordinary glTF skin, still arrives as real `JOINTS_n`/`WEIGHTS_n`.

### Verification gates — all of these must hold

1. `MUJOCO_DIR=~/.local/share/mujoco/3.11.0 cargo test -p awsm-renderer-mujoco-export-cli --test flex_skin`
   — the nanometre oracle. `skinning_reproduces_a_quadratic_flex` and
   `the_exported_glb_deforms_like_mujoco` must stay exact. If the expansion is
   wrong by even a weight, these fail; they are the reason to trust this at all.
2. `cargo test -p awsm-renderer-glb-export` — extend
   `reexport_clean_preserves_skin_and_morph` (`extract.rs:1269`) with a cage
   case, so the save-path trap above is pinned by a test rather than a comment.
3. Sizes: re-measure with a throwaway test that calls `write_glb` then
   `compress_glb_with(&glb, &CompressOptions::default())` — that is `shipped`.
   Target ~45 KB for `bunny_quadratic`.
4. On device: replay `examples/test-scenes/mujoco-flex/author.js` and confirm
   the golden still reproduces **pixel-identically** (it did, exactly, through
   the last writer change — so any drift is a real regression, not noise).
   Note that scene is body-attached, so it exercises the trilinear/quadratic
   path only if you also import `bunny_quadratic.xml`.
5. `cargo clippy --all --all-features --tests -- -D warnings` — what CI runs.

## Picking this up cold

Environment:

- `MUJOCO_DIR=~/.local/share/mujoco/3.11.0` (3.11.0, `model/flex/` has all the
  demos). `MUJOCO_MENAGERIE_DIR=~/.local/share/mujoco_menagerie`. Without these
  the MuJoCo tests SKIP rather than fail — check for `SKIP:` in the output
  before believing a green run.
- `task test-scenes` serves `examples/test-scenes` on **:9084** (fixtures are
  fetched from there by `author.js`).
- `task mcp-dev` runs the editor on **:9085** and the MCP server on **:9186**
  (dev port, not the :9086 prod default). Open
  `http://localhost:9085/?mcp=http://localhost:9186` to attach.
- The template: `task dev` in `../templates/physics-mujoco`, app on **:9000**,
  media on :9001. `?scene=flag` is the deformable.

Tooling note: this session drove the MCP server with a small HTTP client at
`/tmp/mcp.py`, which is **ephemeral and will be gone**. It is ~90 lines
(initialize → keep the session id in `/tmp/mcp-sid` → `tools/call`, parsing SSE
frames); rebuild it if the harness has not registered the awsm-scene tools.
`screenshot_scene` returns base64 PNG in the tool result — decode it locally,
never route image bytes back through the harness.

Two behaviours that cost time this session and are not obvious:

- **`load_player_bundle` is one-shot after an import.** import → export → load
  → drive works; calling `load_player_bundle` a SECOND time clears the resolved
  instances and `editor_apply_mujoco_bodies` then answers
  `error: no sim instance 0`. Re-import to reset.
- The editor's default project can be **empty**, so "wait until `scene_tree` is
  non-empty" never returns. Wait on the snapshot query SUCCEEDING instead.

## Closed — do not redo

- **Body-attached flex size, and every glTF we write** (`9bbffaab`).
  Two facts measured here: `flex_vert` is identically zero (every body-attached
  vertex sits exactly on its body's origin) and those bodies never rotate (0.00
  degrees after 400 real steps). So such a joint carries a translation and
  nothing else. The writer now omits node TRS components equal to glTF's own
  defaults — free, spec-correct, and it shrinks EVERY export, not just flexes —
  and narrows `JOINTS_n` to `u8` when the skin has under 256 joints. Shipped:
  flag 24.7 → 16.1 KB, poncho 71.3 → 47.1 KB, both −34%.
- **The parry3d issue is filed**: https://github.com/dimforge/parry/issues/438
  (`bvh_binned_build.rs:60` indexes an 8-bin array unclamped). Re-verified
  against the 0.28.0 source before filing: for any well-formed leaf set the
  value tops out at exactly `7.999920` at any scale (the range cancels), the
  `1e-5` relative guard is ~170x `f32::EPSILON` so rounding cannot close it, and
  a zero extent yields `NaN`, which `as usize` saturates to 0 — so an
  out-of-bounds index means malformed input, not drifting arithmetic. We saw the
  panic once, never reproduced; `dde3bf98`'s `debug_assert` at `to_parry_aabb`
  is our own defence meanwhile. Draft kept at
  `docs/plans/parry3d-issue-draft.md`.
- **Quadratic cages are supported**, not refused (`648127ed`). 27 influences in
  seven joint sets. The basis is quadratic *Lagrange*, not Bernstein — only a
  bent cage distinguishes them (Bernstein is 0.99 mm wrong on
  `bunny_quadratic.xml`, Lagrange exactly zero). Lagrange goes NEGATIVE to
  −1/8, which partitions unity but is unrepresentable as unorm8, so
  `glb-export`'s `weights_fit_unorm8` keeps those accessors f32 — without it the
  player-bundle export clamped the negative lobes and deformed subtly wrongly
  with no error. Exact to the nanometre against `flexvert_xpos`; guard pinned
  from both sides.
- **The body channel is recorded and baked** (`fff69b03`), so a flex replays
  offline like the humanoid ragdoll already did. Additive `body_count` /
  `body_poses`; a partial channel is refused. The bake runs one shared routine
  over both id spaces, which stay strictly apart.
- **`examples/test-scenes/mujoco-flex`** (`49442412`) — a deformable with a
  golden, plus its bundle, covering the editor import → bundle → sink chain.
- **The reference template drives a deformable** (templates `c46349f`):
  `?scene=flag`, a body region on the wire, 171 of 173 bodies bound, a cloth
  that hangs and ripples under live wind.
- **The body channel bound the wrong transforms** (`7dd44bb8`). A flex's bodies
  resolved to their bone nodes' scene transforms; a skin reads the rig glb's
  baked joint transforms. Every pose landed on a real transform nothing was
  skinned to — no error, no warning, a wind-blown flag rendering perfectly
  still. Found by wiring the template up and measuring 0.000% of pixels
  changing between frames while the sim was demonstrably stepping.
- **Flexes deform by linear blend skinning exactly** — zero error at nanometre
  resolution against `flexvert_xpos`, and the exported GLB reproduces MuJoCo to
  sub-micron through the real extractor. All four `model/flex/` demos import as
  skinned meshes (1047 joints bound), console clean. Round-trips: save → load,
  `export_player_bundle`, `load_player_bundle`, `apply_body_poses`.

## Also worth knowing

- **`../templates/physics-mujoco/README.md` is still the Box3D template's.**
  Its "Run it" section documents rolling a ball with WASD, a `vendor/box3d`
  submodule and a clang-for-wasm requirement, none of which exist here. The
  Scenes section is new and correct; everything around it is not. That matters
  more than usual for a template whose whole purpose is being copied.
- A double-sided material's back faces light differently in the editor's live
  material than through the exported bundle (black vs lit). Deterministic on
  both paths, unrelated to MuJoCo, noticed while building the flex golden.

## Harness gotchas worth keeping

- `load_player_bundle` resolves no instance on a **fresh page** — it needs a
  project loaded first. This is the editor's test seam, not the player path (a
  non-flex bundle fails identically), but it will waste an hour if rediscovered
  as a suspected flex bug.
- The editor's trunk watcher frequently produces a new wasm *before* a naive
  baseline snapshot is taken, so "no rebuild" loops lie. Compare the wasm's mtime
  against the source's instead.
- The template's `trunk serve` does **not** watch the renderer crates, and
  `touch` alone will not retrigger it — a renderer-side change needs a real
  content edit in the template to rebuild.
- For the templates repo use `task dev`; hand-rolled RUSTFLAGS break the threaded
  build (`-Z build-std` recompiles `std` with atomics) and trunk then serves a
  stale `dist/` while every rebuild fails silently.
- **Screenshots lie about whether something is animating.** Diff two consecutive
  frames and count changed pixels; "it looks static" and "it is static" are not
  the same claim, and one flat-looking frame sent this session chasing the wrong
  cause for a while.
