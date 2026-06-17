# Future work — uber-shader (single branching shading pass)

**Status:** north star, NOT scheduled. Do **not** start until Plan B
(`deferred-shared-prep-pass.md`) has landed and the bugs the current per-material approach is
surfacing are fixed. Recorded here so the direction isn't lost.

## The idea

Replace the N per-material compute dispatches with **one** indirect compute dispatch over all
tiles-with-any-material. After the shared prep + shadow passes have consolidated everything common, the
only per-material work left is the shading itself, so the pass becomes:

```
read prep buffers (world_pos, UVs, vcolor, normal, shadow) → switch(material_variant) { … } → write
```

## Why this is the actual win vector vs three.js

Today the comparison is **structurally symmetric**: three does N forward passes (one draw per
mesh.material); awsm does N compute dispatches (one per material) **plus** a geometry pass + G-buffer
bandwidth. Both are O(N) in material count, and awsm carries strictly more — so losing is expected, not
a bug. The single-branching-dispatch is the one move three **structurally cannot make**: its shading is
welded to its per-material draws (forward), so it can never collapse N shadings into one pass. awsm's
deferred decoupling is exactly what makes one shading pass possible — that asymmetry is the win.

## What it buys (beyond removing N-dispatch overhead)

- **Precompile collapse.** Today ~230 s to compile 1024 modules. A bounded-variant uber-shader is
  **one** compile — a massive cut to the original precompile showstopper (for real scenes; see scope).
- **The MSAA edge machinery dissolves.** The accumulator / slot-map / per-shader-`cs_edge` /
  `final_blend` dance exists only because shading is split across pipelines. With one branching shading
  pass, a single `cs_edge` shades all 4 samples (branch per sample), averages, writes — no accumulator,
  no `final_blend`. The edge complexity is the *same symptom* as the per-material floor.

## The hard scope condition (this is probably "what killed us before")

The uber-shader branches over the set of distinct shading **programs/variants** (PBR, toon, unlit,
custom-A, custom-B…), NOT over material **instances**. It works when that set is **bounded** — the real
world: a handful of material shaders reused across thousands of meshes. It does **NOT** work for the
benchmark's pathological **1024 genuinely-unique WGSL bodies**: you can't put 1024 distinct custom
bodies in one module behind a switch (compile time, register pressure, i-cache all blow up). That
regime needs N pipelines (today) or runtime-uploaded code (impossible in WGSL).

⇒ The runtime win is for **bounded-variant** scenes, and needs the realistic benchmark in
`better-benchmarking.md` (many meshes, few shared materials) to even be visible. The current
1024-unique benchmark measures the wrong axis (size/precompile, not runtime architecture).

## Selective grouping — the real design (MUST be decided before implementing)

The uber-shader is NOT all-or-nothing. Sometimes branching wins (dispatch-bound, coherent tiles);
sometimes separate pipelines win (divergent, or a few hot materials). So the real construct is a
**partition of materials into groups, where each group compiles to one branching pipeline** — and
group-of-1 (today's per-pipeline) and group-of-all (one global uber-shader) are just the extremes. The
optimum is in between, and probably *per-group*. This generalizes the "bounded variants + N-pipeline
tail" idea above into "choose the grouping."

This is a planning problem in its own right; **starting the uber-shader work means first deciding the
grouping policy.** Questions that must be answered then (not now):

- **First-party:** are the PBR feature-variants always one uber-shader group? (Likely yes — fixed,
  bounded set.) Toon/unlit/flipbook each their own group, or merged?
- **Dynamic/custom:** may custom materials be grouped into uber-shaders **arbitrarily**? If so, how is
  the grouping chosen — automatic heuristic (by include-set / cost similarity / spatial coherence) vs
  **author-controlled**?
- **Authoring surface:** if author-controlled, expose grouping in the **editor + MCP** (e.g. "assign
  these materials to shading group X"), the same way `ShaderIncludes` opt-in is exposed today.
- **Per-group policy:** allow a group to opt OUT (stay its own pipeline) when profiling says branching
  loses for it — i.e. the grouping is a tunable, measured per group.
- **Overflow / cap:** max variants per group (register pressure / module size), and what happens past
  it.

Benchmark hook: once selective grouping exists, take the current 1024-unique benchmark and partition it
into **groups of N dynamic materials per uber-shader** (e.g. N=10 → ~102 branching pipelines) and sweep
N from 1 (today) to 1024 (one global) — that directly measures the grouping sweet spot and turns the
"simplistic" benchmark into a real experiment about partition granularity.

⇒ `uber-shader.md` exists to force these decisions at implementation start. Do not begin coding the
uber-shader until the grouping policy + authoring surface are specified.

## Costs / risks to design against

- **Branch divergence:** a wavefront straddling two materials runs both branches serially. Mitigate
  with material-**coherent** tiling (classify already groups by bucket → tiles mostly one variant).
  Net: trades N-dispatch overhead for divergence; wins when tiles are coherent (usually true spatially).
- **Module size / register pressure** of the combined shader — bounded only if the variant set is
  bounded (hence the scope condition).
- **Bandwidth at 4K is orthogonal** — one dispatch or N, the G-buffer write+read is identical. The
  uber-shader does NOT fix bandwidth; the win is clearest in dispatch/draw-bound regimes (high instance
  count, moderate res), which is most real content.

## Open questions for when we tackle it

- How to **bound the variant set**: first-party bases + a capped number of distinct custom variants?
- **Overflow policy** when distinct customs exceed the cap — fall back to per-pipeline dispatch for the
  overflow set (hybrid: uber-shader for the common variants + N-pipeline tail)?
- Divergence mitigation: how aggressively to sort/tile by variant.
- Interaction with the prep buffers (it consumes exactly Plan B's outputs).

## Sequence to actually beat three

1. Land Plan B (`deferred-shared-prep-pass.md`) — isolates shading as the only branch, produces the
   buffers.
2. Add the realistic benchmark (`better-benchmarking.md`).
3. Prototype the uber-shader for the bounded-variant case; measure dispatch-bound (expected win) vs
   bandwidth-bound (4K, expected wash/loss). Expect the win at step 3 on the step-2 benchmark — NOT on
   the current 1024-unique one.
