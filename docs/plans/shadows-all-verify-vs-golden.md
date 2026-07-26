# `shadows-all`: verify.md prose disagrees with golden.png

Status: **RETRACTED as a renderer regression** (2026-07-26). Downgraded to a
test-suite inconsistency. The original claim in this file — that three casters
had regressed to no shadow — was **wrong**, and is corrected here.

## What actually happened

While dogfooding against LOCKSTEP-GAMES' dance-off stage I ran `shadows-all`,
saw `tall-box`, `sphere` and `lowered-box` render with no visible shadow, matched
that against the scene's `verify.md` (which lists *"a caster with NO shadow"*
under `fail:`), and filed it as a regression on current `main`.

Then I compared against `examples/test-scenes/shadows-all/golden.png` — which I
should have done first. **The golden shows the same thing**: no visible shadow
under `tall-box`, `sphere` or `lowered-box`; a clear shadow off `thin-bar`; the
warm spot pool present. The live render matches the accepted reference, so
nothing has regressed.

## The real (minor) finding

`verify.md` and `golden.png` disagree. The prose promises:

> - `tall-box` (tan, left) casts a solid directional shadow; its base meets the
>   floor with a CONTACT-TIGHT shadow
> - `sphere` (blue, center) casts a round soft contact shadow directly beneath it

Neither is visible in the golden. The scene is dominated by bright IBL/ambient,
so the directional's contribution — and therefore any shadow that darkens only
that contribution — is nearly washed out. An agent-as-oracle reviewer following
`verify.md` literally will "fail" a correct build, which is worse than no recipe:
it burns a debugging session (it burned one here).

Pick one:

1. **Re-author the scene** so the directional actually reads — lower the ambient
   or raise the directional — and re-bake `golden.png`. This is what the prose
   describes and what the scene claims to guard (Peter-Pan gaps, the PR#169
   donut) — neither of which is currently visible enough to guard anything.
2. **Rewrite `verify.md`** to describe what the golden really shows, and drop the
   contact-shadow claims it can't demonstrate.

(1) is better: as it stands the scene cannot regress-detect the bugs it exists
for, because the features it guards are invisible in its own reference image.

## Carry-over lesson

A shadow only darkens *direct* light. Where ambient/IBL or emissive dominates,
shadows are invisible by construction — not broken. Same trap on the dance-off
stage, where the emissive floor made cast shadows impossible to see until the
tile emissive came down. Check the light ratio before concluding "shadows are
broken".
