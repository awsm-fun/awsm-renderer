# docs/plans — implementation order

Numbered plans, implemented **in order** on branch `updates`, one giant change
separated into commits (at least one commit per plan; more per phase/axis where a plan
says so). Each plan carries its own acceptance criteria.

**Why reverse-Z is early (003), not last:** z-fighting is still reproducing in the
field (the clip-plane ratio fix did NOT eliminate it), so reverse-Z is a fix, not a
luxury — and it changes depth fundamentals everywhere. Landing it before SSR sign-off,
the test scenes, and the optimization sweep means all of those are built once, on the
final convention, instead of being invalidated and redone.

| # | Plan | One-liner |
|---|------|-----------|
| 001 | [branch-ssr-cleanup](001-branch-ssr-cleanup.md) | Green CI + the confirmed mechanical SSR/bloom bugs + exposure gaps. Smoke-verify only — full SSR sign-off waits for 004 |
| 002 | [agent-verify-and-editor-defaults](002-agent-verify-and-editor-defaults.md) | Clean screenshots (MCP view toggles; follow-agent/overlay off by default) + hidden-tab capture — the verification workflow 003+ depends on |
| 003 | [reverse-z](003-reverse-z.md) | ✅ COMPLETE — infinite-far reverse-Z default ON (?noreversez rollback); shadows migrated in-pass; z-fight repro proven fixed (forward stripes → reverse clean) |
| 004 | [ssr-verification](004-ssr-verification.md) | Explicit SSR verification matrix under the new convention + Hi-Z promote-or-delete decision |
| 005 | [save-load-residuals](005-save-load-residuals.md) | VerifyRoundtrip harness + the last roundtrip gaps (captured-mesh edit, morph multi-track, procedural textures) |
| 006 | [optimizations](006-optimizations.md) | THE sweep: permanent test scenes in `examples/test-scenes/` (authored on the reverse-Z convention) + 8 optimization axes + feature gaps (global shadows config, env 3-slot verify) |
| 007 | [player-tests](007-player-tests.md) | `examples/player-tests/` — runtime scenarios over the baked test-scene bundles (instancing stress, nanite streaming, prefab churn, load transaction) |
| 008 | [mcp](008-mcp.md) | Comprehensive MCP exposure + documentation parity pass, with a CI parity test so it stays fixed |

Deleted as complete: `nanite-follow-up.md` (all items shipped + on-device verified),
`save-load-roundtrip.md` (rewritten to 005 residuals), `offscreen-editor-screenshots.md`
(merged into 002). History in git.

## Working rules for the implementation session
- `task lint` (fmt + clippy -D warnings) + `cargo test --all-features` green at every
  commit. Never weaken a test to pass.
- Shader-interface / WGSL edits are runtime-only — always browser-verify them; use the
  002 workflow (clean screenshots, hidden-tab capable) once it lands.
- Renderer `tracing` output surfaces in the BROWSER console, not the editor log buffer.
- Dev infra: exactly ONE dev task — `task mcp-dev` (editor :9085 + media :9077 + MCP
  dev port). Probe ports before starting; never run `editor-dev` and `mcp-dev` together.
- No player performance regressions, ever; editor-only costs must be gated editor-only.
- Update the plan file (tick checkboxes / record numbers) as part of each commit.
