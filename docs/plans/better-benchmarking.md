# Future work — better benchmarking (measure the right axis)

**Status:** proposed. The existing benchmark lives at
`AWSM-REPOS/experiments/compare-threejs-materials` (sibling repo, not part of the renderer crate);
see its `reports/report.md`.

## The problem with the current benchmark

It places N cubes, each with its **own UNIQUE shader** (1 object = 1 material = 1 pipeline, all
distinct). That measures **unique-shader scaling** — a *size / precompile* axis — and conflates
draw-count with material-count (they're locked 1:1). Consequences:

- It's the **wrong axis** to show the deferred/visibility-buffer *runtime* win: today awsm (N compute
  dispatches + geometry + G-buffer bandwidth) is structurally symmetric to three (N forward passes), so
  it can't win — and the configuration that *would* win (the uber-shader, one branching dispatch) can't
  even be built for 1024 *unique* bodies (see `uber-shader.md`).
- So across every GPU-bound regime we tried (high res, high overdraw, dense geometry) three's
  forward+early-Z wins — correctly, for this workload.

## What the current benchmark DOES measure well — keep it, reframe it

Shader **size + precompile + correctness at high unique-material counts**. That's a real axis and
where the slimming work (Tier-A gating, shadow gate, Plan B) is a genuine win. Reframe its headline as
exactly that — not "runtime FPS vs forward".

## The benchmark we're missing — realistic, decouples mesh-count from material-count

Many **meshes** sharing a **few** materials (the real world). Knobs:

- `meshes=N` — instance/draw/dispatch load (independent of material count).
- `variants=V` — number of distinct material *shaders* (small: 1–8), reused across the N meshes.
- existing knobs already added to the experiment: `heavy=K` (per-pixel cost), `spread=F` (overdraw),
  `tess=S` (geometric density), `w`/`h` (resolution), `aa` (MSAA toggle).

This is where deferred should pull ahead: forward pays N draws + N pipeline binds; deferred pays one
geometry pass + (eventually) one shading dispatch. Measure **720p AND 4K** to separate **dispatch-bound**
(expected awsm win at high N) from **bandwidth-bound** (4K, awsm's weak axis).

## Axes to sweep + what each reveals

| axis | reveals |
|------|---------|
| `meshes` ↑ at fixed small `variants` | draw/dispatch-overhead scaling — the deferred runtime win vector |
| `variants` ↑ | unique-shader scaling — size/precompile (the current bench's real axis) |
| resolution (720p vs 4K) | dispatch-bound vs bandwidth-bound (awsm's weak axis) |
| `heavy` ↑ | fragment/shading-bound behaviour |
| `spread`/`tess` | overdraw / geometric-density (quad-overdraw, early-Z interaction) |
| `aa` | MSAA cost (deferred edge-resolve vs forward fixed-function) |

## Honest expectations

- The realistic benchmark should let awsm **approach** three even with today's per-material dispatches
  (fewer distinct pipelines than the 1024-unique case), and **beat** it once the uber-shader lands — in
  the dispatch-bound regime (high `meshes`, moderate res).
- At 4K, bandwidth may keep awsm behind regardless (orthogonal to dispatch structure). That's expected
  and should be reported, not hidden.

## Implementation notes

- Add to the existing experiment app (both engines, kept algorithmically identical per `shared/spec.md`):
  a `variants=V` knob that builds V distinct material shaders and assigns meshes round-robin, plus a
  `meshes=N` knob decoupled from V.
- Keep the current 1:1 unique mode available (it's the size/precompile test) but relabel it.
- Report per the measurement-gates style already in `reports/report.md`.
