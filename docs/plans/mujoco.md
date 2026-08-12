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
exactly.

Earlier note here proposed evaluating that basis in the vertex shader. **That is
the wrong layer.** The renderer already reads joint indices and weights from a
storage buffer, not from vertex attributes — the GLB is only transport. So the
expansion can happen **at load**, on the CPU, filling the exact same skin buffer
the GPU reads today:

- **on disk**: 3 values per vertex (12 bytes as f32, 6 as normalized u16 —
  and these ARE in `[0,1]`, so unlike the weights they quantize cleanly)
  replacing 27 weights + 27 indices, i.e. 172 bytes → 6.
- **at runtime**: byte-identical to today. No shader variant, no new skinning
  path, no risk to anything already working.
- **estimated**: `bunny_quadratic` 259.5 KB → roughly 45 KB, in line with the
  trilinear bunny's 41.1 KB.

What it costs: a marker on the rig GLB saying "this skin is a quadratic cage,
expand it", plus the cage coordinate attribute — an awsm-specific addition to
our own rig format, carried through exporter → glb-export schema → extractor →
importer/loader. Four layers and a format addition, which is why it is its own
change rather than a tail-end edit: the failure mode of getting it subtly wrong
is, once again, geometry that looks plausible and deforms incorrectly.

Worth noting the same trick applies to the **trilinear** case (8 weights from
the same 3 numbers, 2 sets → 1 attribute), so the two would share one mechanism.

## Closed — do not redo

- **Body-attached flex size, and every glTF we write** (`__SIZECOMMIT__`).
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
