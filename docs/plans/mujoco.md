# MuJoCo — remaining gaps

The feature is complete and verified. This doc is only what is left; how the
feature works lives in `docs/mujoco.md` and should stay there.

Branch `mujoco`, never pushed, `main` untouched. Templates work is in
`../templates/physics-mujoco`, its own repo, also unpushed.

**One item remains.** Everything else on this plan is closed — see "Closed"
below before re-litigating anything.

## Open — flex GLB cost: investigated, needs a call on which fix

David's call: the cost is **not acceptable as-is**. Investigated; the findings
below say what to do, but each option trades bytes against something real, so
the choice is his.

### What ships today

`raw` is the exporter's GLB; `shipped` is after the bundle's meshopt +
quantization, which is what a player downloads.

| model | kind | raw | shipped | ratio |
|---|---|---|---|---|
| `flag.xml` | body-attached, 171 verts | 43.5 KB | **24.7 KB** | 1.76x |
| `poncho.xml` | body-attached, 484 verts | 126.0 KB | **71.3 KB** | 1.77x |
| `bunny.xml` | trilinear | 237.5 KB | **41.6 KB** | 5.71x |
| `bunny_quadratic.xml` | quadratic | 536.1 KB | **260.9 KB** | 2.05x |

The two kinds have **different problems**, and neither is "the geometry is big".

### Body-attached: half the file is JSON

| | flag | poncho |
|---|---|---|
| glTF JSON chunk | 21.4 KB (49%) | 62.6 KB (50%) |
| inverse bind matrices | 10.7 KB | 30.2 KB |
| all actual geometry | 8.7 KB | 25.7 KB |

The JSON is one glTF node per vertex — `name` + `translation` + `rotation`,
about 130 bytes each. Two measured facts make most of that removable:

- **`flex_vert` is identically zero.** Every body-attached vertex sits exactly
  at its body's origin (checked on flag and poncho: max `|flex_vert|` = 0.000000
  m, 0 of 171 and 0 of 484 nonzero).
- **The bodies never rotate.** A flexcomp vertex body is a pure point mass:
  `|1-|w||` is 0 at rest and the max rotation after 400 steps of real simulation
  is 0.00 degrees, on both models.

So a body-attached joint carries **a translation and nothing else**, and its
inverse bind matrix is a pure inverse-translation. Options, largest first:

1. **Omit `rotation` on joint nodes** (~13 KB on poncho). Free — it is provably
   identity for this flex kind. Low risk.
2. **Drop joint node NAMES** (~16 KB on poncho). The importer currently finds
   joints by the `{mesh}_joint_{i}` naming rule, but the skin's own `joints`
   array is already index-aligned with the sidecar's `flex.joint_bodies`, so the
   names are redundant with an ordering we already rely on. Medium risk: it
   changes the import contract.
3. **Omit `inverseBindMatrices`** by baking vertex positions in body-local space
   (~30 KB on poncho). Since `flex_vert` is zero, POSITION becomes **all zeros**
   and compresses to nothing too. But it leaves a bind-pose mesh that is a
   degenerate point at the origin — every tool that reads the bind pose (a glTF
   viewer, the LOD baker, MikkTSpace, any bounds code not already skin-aware)
   sees garbage. Biggest win, biggest robustness cost. **Recommend against
   unless bytes matter more than the artifact being sane on its own.**

1 + 2 take poncho's raw from 126 KB to roughly 97 KB with no loss of anything;
adding 3 would reach ~62 KB at the cost above.

### Quadratic: seven f32 weight sets, and the joint indices are all identical

`bunny_quadratic` is 6.3x the shipped size of the same mesh as a trilinear
(260.9 vs 41.6 KB). Two causes, both structural:

- **The weights cannot be quantized** — that is the `weights_fit_unorm8` guard,
  and it is correct: Lagrange goes negative and glTF's WEIGHTS_n allows only
  FLOAT, normalized UNSIGNED_BYTE, or normalized UNSIGNED_SHORT. There is no
  signed option and no scale/offset carrier to bias into, so f32 is the only
  spec-valid choice. The trilinear bunny quantizes to u8 and that is most of its
  5.71x.
- **`JOINTS_0..6` are 137 KB of pure repetition** — every vertex names the same
  27 cage nodes in the same lattice order. glTF cannot express a constant
  attribute, and meshopt only partly exploits it.

The real fix for both is that a quadratic flex's 27 weights are a **pure
function of 3 numbers** — the vertex's normalized cage coordinate, which MuJoCo
already stores in `flex_vert0`. Storing those 3 floats and evaluating the basis
in the vertex shader replaces 27 weights + 27 indices per vertex (172 bytes)
with 12 bytes, and needs no joint indices at all. That is a custom vertex-shader
path, not a format tweak — **a separate plan, not this one**.

Interim, cheap: emit `JOINTS_n` as `UNSIGNED_BYTE` at export when the cage has
under 256 nodes (it always does — 8 or 27), halving 137 KB to 68 KB raw. The
bundle compressor already does exactly this (`joints_fit_u8`); the exporter does
not.

## Closed — do not redo

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
