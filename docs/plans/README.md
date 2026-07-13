# docs/plans — active plans

Plans live here while there is still work to do; a plan whose work has fully
shipped gets DELETED (history in git). The numbered 001–008 implementation
sequence for branch `updates` completed 2026-07-10..13 and was removed as
done: branch-ssr-cleanup, agent-verify/editor-defaults, reverse-Z,
ssr-verification, save-load-residuals, the optimization sweep, mcp parity —
plus the later bvh-reflections design (shipped behind `ssr.bvh_reflections`).
Earlier deletions: nanite-follow-up.md, save-load-roundtrip.md,
offscreen-editor-screenshots.md.

| Plan | Status | One-liner |
|------|--------|-----------|
| [007-player-tests](007-player-tests.md) | shipped; 2 optional scenarios open | `examples/player-tests/` runtime harness (23/23 — the standing regression gate). Open: animation stress, post/SSR/bloom runtime toggles |
| [ssr-followups](ssr-followups.md) | living queue | Reflections roadmap + what shipped; open: readback colorspace bug, planar reflections, prefiltered scene mips, transparent-pass MSAA, probe tier 2 |
| [atmosphere](atmosphere.md) | designed, not started | Haze as a real feature: view-path fog (effects pass) + reflection-path haze; replaces the arena's probe-baked fake |
| [crashes](crashes.md) | planned, not started | Editor-tab 70 GB VA-leak crash investigation (soak harness) |

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
