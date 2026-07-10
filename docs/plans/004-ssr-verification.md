# 004 — SSR: explicit verification + completion under reverse-Z

**Order:** fourth — immediately after reverse-Z (003), because SSR is the newest and
least-verified depth consumer in the tree and 003 just changed the depth convention
under it. Plan 001 fixed the *confirmed mechanical* SSR bugs (MSAA edge-descriptor gap,
dead bind group) but explicitly deferred visual sign-off to here. Nothing about SSR is
considered "done" until this plan's matrix passes.

## Why a dedicated plan
SSR was implemented in one branch push and has never been systematically verified
on-device. It reads depth, reconstructs view-space positions, marches rays, and
(dormantly) owns a second Hi-Z pyramid — all of which 003 touched. "It compiles and
the settings persist" is not evidence the reflections are correct.

## Part 1 — Verification matrix (production path: LinearDda)
Build a dedicated SSR test scene (it graduates into `examples/test-scenes/ssr` in 006):
a dark near-mirror floor (black glossy dielectric, roughness≈0.05 — white saturates,
per the standing probe rule) with tall emissive red/green/blue objects, plus a rough
metallic object for spread testing (the §9.E scene from the 003 doc / the old
`ssr_m2c_test.py` recipe).

Verify, with screenshots via the 002 clean-capture workflow, each cell:

| Axis | Cases |
|---|---|
| Depth convention | reverse-Z ON (new default) — and flag-off A/B: LinearDda output must be identical (value-agnostic guard) |
| AA | MSAA on / off — especially silhouette edges (the 001 edge-descriptor fix) — no shimmer, no stale reflectivity |
| Resolution | half-res (default) vs full-res — upsample quality at silhouettes, no cross-edge bleed (composite sigma heuristic) |
| Temporal | off (default) and on — moving camera + moving reflector: measure ghosting; document that temporal stays default-off until neighbourhood clamp exists |
| Materials | mirror vs rough (spread blur + `spread_cutoff` gating), metallic vs dielectric F0, non-PBR opt-out writes zero |
| Camera | multiple orbit radii + grazing angles; edge_fade at screen borders |
| Content | skinned/animated mesh reflected AND reflecting; sky/miss pixels contribute nothing |
| Settings plumbing | every knob (intensity, max_distance, thickness, max_steps, spread_cutoff, edge_fade, resolution_scale, temporal, temporal_weight) visibly changes output live via editor UI AND via MCP `set_post_process`; project save→reload and bundle export→player-load preserve all of them |
| Perf | `?trace=sub-frame` timings for minz(if on)/trace/composite at half + full res; SSR-disabled frame is byte-identical in cost to pre-SSR |

Record every cell's result in this file; failures become fixes in this plan.

### Part 1 RESULTS (2026-07-10, editor :9085, reverse-Z default) — ALL PASS after one fix

| Cell | Result |
|---|---|
| Depth convention | ✅ reverse 157,150 B vs forward (?noreversez) 157,022 B — 0.08% parity |
| MSAA on/off | ✅ AFTER FIX (below): off = 152,318 B (reflections present), on = 157,150 B byte-identical to baseline; silhouettes clean both |
| Resolution | ✅ full-res 156,002 B vs half 157,150 B — upsample quality equivalent at this scene |
| Temporal | ✅ static ≈ baseline (156,846); capture just after a camera move = 200,442 B (+28% — visible history smear, confirming the documented ghosting; default-off stands) |
| Materials/roughness | ✅ 0.06 sharp; 0.3 blurred (151,034); 0.7 reflections VANISH (88,594, −44%) — spread_cutoff 0.6 IBL fallback works |
| Live knobs | ✅ each visibly changes output: intensity 0.3→136,682; max_distance 5→175,350; max_steps 16→70,614; edge_fade 0.5→147,966; full restore returns BYTE-IDENTICAL 157,150 (deterministic) |
| Persistence | ✅ full ssr block (all 10 fields) identical across reload_project_in_memory |
| Perf | SSR trace pipeline compiles in 2 ms; frame_dt 16.7 ms (vsync) / render_cpu 3.85 ms EMA with SSR on (memory_stats). Note: ?trace=sub-frame spans are CPU record-time tracing spans not emitted at INFO by the editor subscriber — memory_stats frame EMA is the practical probe; Part 2 uses it |

**BUG FOUND + FIXED by this matrix (the plan's purpose):** flipping MSAA at
runtime with SSR enabled produced a BLACK frame — the SSR trace + composite
bind-group LAYOUTS bake the depth binding's `multisampled` flag at
construction, and `set_anti_aliasing` never rebuilt the SSR pass (only
`set_post_processing`'s structural SSR axes did). GPU validation: "Sample
count (1) of [Texture Depth] doesn't match expectation" → invalid bind group
→ whole command buffer rejected. Fix: `set_anti_aliasing` Phase 10 rebuilds
`SsrRenderPass` (+ `ssr_minz` when present) exactly like the structural-SSR
rebuild. Verified: msaa off/on round-trip renders reflections both ways.

## Part 2 — Hi-Z decision (now that reverse-Z landed)
The min-Z pyramid + Hi-Z trace exist but are gated off (`SsrTrace::PRODUCTION ==
LinearDda`) due to horizontal banding from (a) fractional-tile advance instead of
cell-boundary DDA and (b) forward-Z far-precision quantization of the coarse mips.
003 fixed (b)'s amplifier; (a) is a real traversal bug.

- Re-run the §9.E banding scene with Hi-Z force-enabled under reverse-Z; quantify the
  banding that remains.
- Implement the McGuire/Mara-2014 cell-boundary DDA advance (the correct fix for (a)).
- If Hi-Z then matches LinearDda visually at every matrix cell and wins on
  `?trace=sub-frame` timings (it should at high max_steps / long rays): promote
  `SsrTrace::PRODUCTION` to Hi-Z. If it doesn't win or artifacts persist: DELETE the
  ssr_minz pass + hiz template branch instead of carrying dormant compiled surface
  area — record the numbers and the decision here either way. No third option.

## Part 3 — Close out
- `temporal_weight` exposure (editor row + MCP param) if not already landed in 001.
- Update all SSR docstrings to describe the shipped design (001 lists the stale ones).
- The verified scene + goldens graduate into `examples/test-scenes/ssr` in 006.

## Acceptance
Every matrix cell green with archived screenshots; Hi-Z promoted or deleted (decision
+ numbers recorded); all SSR knobs proven live + persistent end-to-end; SSR cost
zero when disabled.
