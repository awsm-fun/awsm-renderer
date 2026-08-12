# MuJoCo — remaining gaps

The feature is complete and verified. This doc is only what is left; how the
feature works lives in `docs/mujoco.md` and should stay there.

Branch `mujoco`, never pushed, `main` untouched. Templates work is in
`../templates/physics-mujoco`, its own repo, also unpushed.

**Two items remain, both needing David.** Everything else on this plan is
closed — see "Closed" below before re-litigating anything.

## Open 1 — file the parry3d issue

Drafted and fact-checked against the 0.28.0 source at
`docs/plans/parry3d-issue-draft.md`; needs a go-ahead to post to
`dimforge/parry` under David's account (`gh` is authenticated).

`bvh_binned_build.rs:60` computes `(k1 * (centroid - min)) as usize` and indexes
an 8-bin array with it, unclamped. Re-verified: for any well-formed leaf set the
value tops out at exactly `7.999920` at any scale (the range cancels), the
`1e-5` relative guard is ~170x `f32::EPSILON` so rounding cannot close it, and a
zero extent yields `NaN`, which `as usize` saturates to 0. So an out-of-bounds
index means malformed input, not drifting arithmetic — a one-line
`.min(NUM_BINS - 1)` makes it unreachable.

We saw the panic once (`index out of bounds: the len is 8 but the index is 8`),
never reproduced. `dde3bf98` added a `debug_assert` at `to_parry_aabb` — the
single funnel into parry — so next time the malformed box gets named instead of
parry taking the blame.

## Open 2 — ratify the flex GLB cost

Not a blocker for anything; it is a "is this acceptable" call that nobody has
made. Measured (geometry GLB, uncompressed):

| model | kind | joints | sets | GLB |
|---|---|---|---|---|
| `flag.xml` | body-attached, 171 verts | 171 | 1 | 43.5 KB |
| `poncho.xml` | body-attached | 2503 | 1 | 126.0 KB |
| `bunny.xml` | trilinear | 8 | 2 | 237.5 KB |
| `bunny_quadratic.xml` | quadratic | 27 | 7 | 536.1 KB |

Body-attached flexes cost ~5x a plain mesh (one joint node per vertex is
inherently redundant when the mapping is the identity, and glTF has no cheaper
way to say so). Interpolated ones cost their influence sets — the same bunny is
2.3x larger as a quadratic than as a trilinear. Absolute sizes stay small, and
both trade disk for the thing that matters at runtime: 8 or 27 joint matrices
per frame instead of 2,503 vertex positions.

The numbers are already recorded in `docs/mujoco.md`; ratifying just means
deciding this needs no further work, at which point this doc can be deleted.

## Closed — do not redo

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
