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
