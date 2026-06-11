# Phase 2 — browser/MCP verification queue

Filled by the **Phase A headless build** (it can't open a browser), drained by the
**Phase B browser run** (manual kickoff with a live editor tab). See
`docs/plans/OVERNIGHT-HANDOFF.md` §4 for both `/loop` prompts and the gotchas.

Format per item: `- [ ] <what was built> — VERIFY: <exact steps via :9086/debug + screenshot> — EXPECT: <result> — (commit <sha>)`.
Phase B ticks `[x]` only after seeing it correct in the tab; if wrong, fix + note the fix.

## Queue (Phase A appends below)

_(empty — Phase A populates this as it builds things that need a live editor to confirm)_

## Verified / resolved (Phase B moves items here)
