# docs/plans — active plans

Plans live here while there is still work to do; a plan whose work has fully
shipped gets DELETED (history in git). The numbered 001–008 implementation
sequence for branch `updates` completed 2026-07-10..13 and was removed as
done: branch-ssr-cleanup, agent-verify/editor-defaults, reverse-Z,
ssr-verification, save-load-residuals, the optimization sweep, mcp parity —
plus the later bvh-reflections design (shipped behind `ssr.bvh_reflections`),
volumetrics.md (shipped: froxel light shafts, per-light `volumetric_intensity`,
temporal accumulation, test scene — deleted 2026-07-27 on completion)
and compression.md (shipped: `EXT_meshopt_compression` + `KHR_mesh_quantization`
mesh codec + KTX2/Basis textures block-compressed into VRAM; KTX2 the bundle
default). taa-upscaling/ (TAA + temporal upscaling, shipped opt-in in 0.46; design in git at `57a3ef94`, remaining work in taa-final-round/). Earlier deletions: nanite-follow-up.md, save-load-roundtrip.md,
offscreen-editor-screenshots.md, crashes.md (the editor-tab 70 GB VA leak —
shipped as the zero-cost-by-default tracing/logging redesign, 221cf2a5; the
soak harness it produced lives on at `tools/soak/`).

| Plan | Status | One-liner |
|------|--------|-----------|
| [007-player-tests](007-player-tests.md) | shipped; 2 optional scenarios open | `examples/player-tests/` runtime harness (29/29 — the standing regression gate). Open: animation stress (5), post/SSR/bloom runtime toggles (7) |
| [browser-test-suite](browser-test-suite.md) | ✅ shipped; kept as skill SSOT | 3-layer on-device suite (A visual verify.md, B player-tests, C native audits) driving the `awsm-renderer-browser-tests` skill. Done — retained as the design record the skill references, not an active plan |
| [ssr-followups](ssr-followups.md) | dormant queue (no active SSR work planned) | Reflections roadmap + what shipped (probe, BVH fallback, ssr_mask, zero-cost-off). All remaining items are future tiers, none are defects: planar reflections (content-triggered), prefiltered scene mips, glass-shell shading aliasing, probe tier 2, BVH thin-emitter hit quality |
| [atmosphere](atmosphere.md) | Phase 1 shipped; **Phase 2 not started** | Reflection-path haze — SSR miss + IBL specular + BVH hits. Blocked on one decision: what Phase 2 means under `mode = volumetric`, now that volumetrics ships |
| [flat-skybox](flat-skybox.md) | designed, not started | 2D backdrop for static-camera scenes as a structural shader axis (no new binding — binding 11 re-typed). Breaking `EnvSlot` change, by choice |
| [taa-final-round/](taa-final-round/README.md) | ready to run (0.46 shipped TAA as opt-in) | The last TAA round: stable bloom, new-object coverage for transparents / appended instances, DoF order below scale 1, full re-verification, TAA native as the new-scene default, release. Holds the gates (`verification.md`), everything rounds 1-2 learned (`findings.md`) and the neural-AA investigation |
| [parry3d-issue-draft](parry3d-issue-draft.md) | draft, not filed | Upstream issue text for dimforge/parry: the binned BVH build indexes its bin array unclamped |
| [shadows-all-verify-vs-golden](shadows-all-verify-vs-golden.md) | retracted as a regression | `shadows-all` verify.md prose disagrees with its golden; a test-suite inconsistency, not a renderer bug |

## Working rules (unchanged)
- `task lint` (fmt + clippy -D warnings) + `cargo test --all-features` green at
  every commit. Never weaken a test to pass.
- Shader-interface / WGSL edits are runtime-only — always browser-verify them.
- Renderer `tracing` output surfaces in the BROWSER console, not the editor
  log buffer.
- Dev infra: exactly ONE dev task — `task mcp-dev`. Probe ports before
  starting; never run `editor-dev` and `mcp-dev` together.
- No player performance regressions, ever; editor-only costs must be gated
  editor-only.
- Update the plan file (tick boxes / record numbers) as part of each commit;
  delete the file when it is fully done.
