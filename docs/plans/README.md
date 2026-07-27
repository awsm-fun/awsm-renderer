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
default). Earlier deletions: nanite-follow-up.md, save-load-roundtrip.md,
offscreen-editor-screenshots.md, crashes.md (the editor-tab 70 GB VA leak —
shipped as the zero-cost-by-default tracing/logging redesign, 221cf2a5; the
soak harness it produced lives on at `tools/soak/`).

| Plan | Status | One-liner |
|------|--------|-----------|
| [007-player-tests](007-player-tests.md) | shipped; 2 optional scenarios open | `examples/player-tests/` runtime harness (29/29 — the standing regression gate). Open: animation stress (5), post/SSR/bloom runtime toggles (7) |
| [browser-test-suite](browser-test-suite.md) | ✅ shipped; kept as skill SSOT | 3-layer on-device suite (A visual verify.md, B player-tests, C native audits) driving the `awsm-renderer-browser-tests` skill. Done — retained as the design record the skill references, not an active plan |
| [ssr-followups](ssr-followups.md) | dormant queue (no active SSR work planned) | Reflections roadmap + what shipped (probe, BVH fallback, ssr_mask, zero-cost-off). All remaining items are future tiers, none are defects: planar reflections (content-triggered), prefiltered scene mips, glass-shell shading aliasing, probe tier 2, BVH thin-emitter hit quality |
| [todo](todo.md) | active | Camera API consolidation (~7 overlapping ways to build one — collapse to a single module, view/projection decoupled, reverse-Z default but not hardcoded, all consumers migrated) + branch leftovers: instancer materials, `SetActiveCamera` + the 2 nanite goldens, camera frustum gizmo |
| [atmosphere](atmosphere.md) | Phase 1 shipped; **Phase 2 not started** | Reflection-path haze — SSR miss + IBL specular + BVH hits. Blocked on one decision: what Phase 2 means under `mode = volumetric`, now that volumetrics ships |
| [flat-skybox](flat-skybox.md) | designed, not started | 2D backdrop for static-camera scenes as a structural shader axis (no new binding — binding 11 re-typed). Breaking `EnvSlot` change, by choice |

## Open decision — publishing 0.27 (carried over from volumetrics.md)

Still open as of 2026-07-27: workspace is 0.26.0, there is no `v0.27.0` tag,
and branch `volumetrics` is 33 commits ahead of `main` and unmerged.
`task publish -- 0.27.0` does considerably more than publish: it bumps + tags,
publishes 14 crates to crates.io (irreversible — yank is the only undo),
deploys BOTH frontends to Cloudflare Pages (replacing the live
`scene.awsm.fun` editor), and pushes branch + tag, firing the cargo-dist
MCP-binary release on CI. Releasing off an unmerged feature branch is a call
about the release process, not a mechanical step — left for a human.

Verified: `task bump -- 0.27.0` is clean and idempotent, and
`crates-publish-dry-run` packages `awsm-renderer-core@0.27.0` successfully,
then stops at crate 2 because dependency-ordered publishing needs
`awsm-renderer-core = "^0.27.0"` on the index, which a dry run never uploads.
That is a limitation of dry-running a multi-crate workspace, not a packaging
fault. The bump was reverted; the tree is back at 0.26.0.

Consumers do not have to wait: an *uncommitted* `[patch.crates-io]` in
LOCKSTEP-GAMES pointing at this checkout unblocks iteration, and authoring a
SCENE never needed the pin at all — `scene.toml` carries the settings
regardless; the pin only decides whether the game RENDERS them.

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
