# KTX environment silently lost on project load→save round-trip

Status: **reproduced minimally, root cause not yet found** (2026-07-26). Found by
dogfooding against LOCKSTEP-GAMES' dance-off stage.

## Minimal reproduction

A project whose `specular` and `irradiance` slots are `EnvSlot::Ktx` (skybox left
as a sky gradient), saved to a directory and served over HTTP:

1. `load_project_from_url <base>` — no edits of any kind
2. `save_project`

**Expected:** the saved `project.toml` still has `specular`/`irradiance` as
`{ktx = {asset_id = …}}`, and the two `assets/<id>.ktx2` side files are re-emitted.

**Actual:** both slots come back as `{sky_gradient = …}` and **neither `.ktx2`
file is written**. Save shrinks 4.33 MB → 2.21 MB. No error, no toast — the
editor logs only `Project loaded`. The IBL is gone and the scene silently
re-lights from the built-in gradient.

Ran twice, the second time with literally zero commands between load and save.

## Workaround (confirmed)

`set_environment {specular: <asset-uuid>, irradiance: <asset-uuid>}` using the
ids already in the project's asset table restores it immediately — the bytes are
evidently reachable — and the next `save_project` persists both slots and both
side files correctly (2.21 MB → 4.33 MB).

## Ruled out

- **Path mismatch.** `env_ktx_path(id)` = `assets/<uuid>.ktx2`, exactly what Save
  writes and what the dev server returns (verified `200`, 2096960 B / 24784 B).
- **`restore_ktx` not running on this path.** All three loaders call it —
  `load_from_dir` (:1195), `apply_inmem` (:1291), and `load_project_from_url`
  (:1388).
- **`ktx_asset_ids()` missing non-skybox slots.** It covers all three.
- **A fallback write in `env_sync`.** It has no code that resets a slot to
  `BuiltInDefault`/`SkyGradient` on failure.

## Where to look next

The saved output showing `sky_gradient` is about `ctrl.scene.environment` *state*,
not about the byte stash — `apply_project` does
`ctrl.scene.environment.set(project.environment)`, so if the TOML parsed as
`Ktx` the state should be `Ktx` and Save should emit it. Two candidates:

1. ~~**The parse.**~~ **Ruled out.** Added
   `environment::tests::ktx_specular_and_irradiance_parse_from_project_toml_shape`
   — parses a `project.toml`-shaped `[environment]` block with a gradient skybox
   plus KTX specular + irradiance and asserts both slots come back as `Ktx`. It
   **passes**, so serde handles this shape correctly. Kept as a regression guard.
2. **A later writer** — now the leading candidate. Something after
   `apply_project` re-sets `scene.environment` from a default. Next step: a
   `tracing` line on every `environment.set` to count how many times it fires
   during a load and find the second writer.

## Why it matters

Silent, total loss of a scene's lighting environment on an ordinary open→save.
Any project authored with an HDR IBL loses it the next time someone opens and
saves — with no error to notice. It cost real time here: several lighting
iterations were tuned against a scene that had quietly lost its IBL, which made
the direct/ambient ratio look far more broken than it was.
