# What two rounds of TAA work established

Everything learned in rounds 1 and 2 (2026-11-23 to 25) that the final round
needs and that is not already in [`docs/TAA.md`](../../TAA.md). TAA.md is the
reference for what shipped: modes, options, API, how the resolve works, known
limits (§6), measurements (§7). This file holds the reasoning: why each
default is what it is, what was tried and dropped, how to measure without
fooling yourself, and how the team work went wrong.

Raw records (local only, not in git): `~/Documents/AWSMFUN/AWSM-REPOS/taa-evidence-2026-11/`.

- `round1/report.md`, `round2/report.md`: the two final reports.
- `round2/evidence/gates-record.md`: every gate line at the final head (400 KB).
- `round2/evidence/ghost/`, `round2/evidence/dreidel/final/` (with `INDEX.md`): the GIFs.
- `round2/*-notes.md`: lead, senior, measurement and fallback engineers' logs, in time order.
- `round2/review/`: every review, numbered as in the round-2 report §9.
- `round2/meas/`: the Dreidel capture, ghost-hunt and cost scripts (CDP-driven, `ghost.mjs`, `cost-run.mjs`, `*.py`).

The original design and phase plan (01-04) are in git at `57a3ef94:docs/plans/taa-upscaling/`.

## 1. State at 0.46.0

- TAA native, Quality 0.67, Balanced 0.58 and Performance 0.5 ship as opt-in modes. **The default AA is still MSAA 4×.**
- **TAA off is free:** with TAA off, output is bit-identical to 0.45.0 (36/36 harness captures, 6/6 Dreidel). Frame time is within ±0.11 ms, and the shader sources are byte-identical. Every TAA option that is off compiles, allocates and binds nothing.

Dreidel Derby, 1920×1435, M1 Max, Chrome, fixed jitter phase, final head (`57a3ef94`):

| | Title error | Mid-round error | Title GPU | Mid-round GPU |
|---|--:|--:|--:|--:|
| TAA native | 0.311 | 0.337 | 16.56 ms | 13.12 ms |
| TAA 0.67 | 1.351 | 0.855 (before `bloom_source`) | 9.50 ms | 8.62 ms |
| MSAA 4× | 1.322 | 1.918 | 19.38 ms | 14.97 ms |
| SMAA | 1.127 | 1.750 | | |
| No AA | 1.172 | 2.014 | 13.99 ms | 10.69 ms |

Error is mean absolute error against a 64-sample reference, in 0-255 display units.

The game's ghost hunt:

| | Stale pixel-frames | Events of 2+ frames | Longest |
|---|--:|--:|--:|
| Mid-round, round 2's frozen head | 4,276 | 24 | 11 frames |
| Mid-round, final | 373 | 2 | 4 frames |
| Title, round 2's frozen head | 18,246 | 138 | 14 frames |
| Title, final | 2,044 | 17 | 5 frames |

## 2. Why each default is what it is

| Choice | Why (numbers in TAA.md §1 and §7) |
|---|---|
| Resolve averages in the **display's own space** (`D(exposure·c)`), not Karis-weighted HDR | Round 2 found 98% of the trail band's excess over the oracle was a converged-quality floor from averaging in the wrong space. Far-floor error 0.570 → 0.145 (oracle 0.095); `taa-thin` 1.74 → 0.56. Consequence: very bright thin features saturate near white, so anything reading HDR energy *after* the resolve loses it (hence the two rows below). |
| **f16 store rounding** (`taa_round_f16`) | Compute stores to `rgba16float` truncate on Apple GPUs. Re-stored every frame, the history sat one 8-bit level too dark on 7-16% of the pixels of flat scenes. This one fix took the Dreidel title from worse than SMAA and no AA to the best mode. Any new pass that stores f16 history needs the same rounding: the DoF pass had the same bug. |
| `disocclusion: DepthMotionAndId` | Depth alone misses the 1-px contact line under a moving object, where the two depths touch. `taa-motion` trail events longer than one frame: 266 (depth) → 26 (+ last frame's vectors) → 1 (+ last frame's object id). The id test costs +0.07 / +0.18 ms and an 8 B/px copy. `DepthAndMotion` is the recommendation for camera-only content. |
| The motion test compares the camera's last two view-projections | The first version compared last frame's vector with this frame's and reset 25-65% of the screen whenever the camera started or turned (173k / 452k px on `taa-orbit`). A review caught it. Compare like with like. |
| `motion_edge_speed` 0.5 (was 2.0) | Slow silhouettes (skinned legs) kept part of their history for 15-45 frames: a real ghost on the fox. |
| `max_history_moving` 1.0 (was 2.0) | Softness in motion: `taa-orbit` strafe shimmer 1.21 → 1.06 (MSAA 4.00); pan 1.01 → 0.86. |
| `reactive_particles` 1.0 on every covered pixel | Writing `alpha × 0.3` let sprites' soft skirts keep a stale colour for up to 32 frames. Particle ghost pixel-frames 51,094 → 4,137. With alpha scaling, even weight 1.0 left 28,027. |
| `reactive_new_objects` 1.0 | When something nearer *appears*, no history test fires, and the clip keeps the background colour because both colours are in the box. A Dreidel coin's rim held the table's colour for about 16 frames. The flag must wait for the **first drawn** frame, and re-arm if the mesh jumps within 6 frames: the game shows a respawned coin one frame before moving it. Opaque route only. |
| `bloom_source: CurrentFrame` | Bloom read from the resolved image lost the halo of thin bright sources by 12-25%, permanently (see the display-space row). Reading this frame's HDR render restores it. Cost: a pulse at the 8-frame jitter period around sub-pixel sources in a *still* image (see §3). |
| `dof_order: BeforeResolve` | DoF on the resolved image dimmed small bright bokeh by 18-20% on the Dreidel title menorah. Its CoC, read through a 2×2 depth footprint that moves with the jitter, also changed every frame at depth edges, so slight defocus flickered (0.391 p99 6, fails). Before the resolve, on the HDR render: energy 98.9-100.8% of the reference, flicker 0.198 p99 1, DoF ghost events 533 → 1. It is cheaper as well (`kitchen-sink@dof` 17.5 → 14.5 ms native, 16.3 → 8.0 ms at 0.67). Epic ("between Depth of Field and Motion Blur") and AMD FidelityFX ("DoF should be applied at render resolution, i.e. before upscaling") document the same order. Unreal's source was not checked. |
| `edge_release` **off** | It removes the last test-scene event of more than one frame (`taa-motion` k211: 1 px, 4 frames, 20 levels). In exchange, still-image flicker on thin content rises: `taa-thin` 0.301 → 0.323, `instancing-stress` 0.105 → 0.151. A flicker-free version needs a per-mesh "moved this frame" bit, but instances share an id and shared-arena physics writes transforms off-thread, so it needs a GPU-side compare pass. Not built. |
| `reactive_lines` 0.3 (1.0 is an option) | 1.0 removes the double behind an animated line (184,562 → 15,633 ghost pixel-frames) but fails the flicker gate on still lines (p99 3 → 4). |
| Default AA left at **MSAA 4×** | At native, TAA beats MSAA on quality everywhere that matters and is cheaper in the game. It was held back because the bloom pulse (§3) has not been judged by eye, and the user wanted to see the GIFs. Recommended flip: TAA native for *new* scenes, written explicitly into new files. Files with no AA block keep MSAA, so no existing project changes look and none of the 38 goldens moves. Quality 0.67 should not be the default: it is worse than MSAA on the Dreidel title (1.351 against 1.322). |
| Fallback velocity (32 B/sample devices) | A small raster pass after prep redraws only the meshes that deformed this frame, and an identity test against the visibility targets decides which pixels it writes. A depth-equal test would lose 0.75-1.69% of pixels. Forced on the M1, vectors are 120/120 and match the geometry path to 3-4 decimals. Tiers by storage-buffer limit: 10 or more, everything; 7-9, vertex hooks reactive; below 7, rigid only. Not yet run on a real 32 B device. |

## 3. Open trade-offs (the final round decides these)

1. **Bloom pulse in still images.** With `CurrentFrame`, a sub-pixel bright source is hit on some of the 8 jitter phases and missed on others.
   - Inside its halo ring the frame-to-frame change is mean 1.6-3.4, p99 8-14 (whole frame passes: 0.086 p99 2), and the halo's brightness swings 0.6-4.5%.
   - Dreidel frozen title: whole frame 0.200 p99 3, menorah tile 0.80 p99 9.
   - `Resolved` is steady but dim.
   - The candidate fix is a temporal blend of the bloom input reprojected with TAA's vectors (estimated half a day, +0.03-0.05 ms).
   - An *unreprojected* blend was rejected: halos would trail 4-8 frames behind moving lights.
   - Carrying energy through the resolve (a second, linear reconstruction plus a history store) was also rejected: +0.1-0.2 ms, when reading the pre-resolve frame does the job for nothing.
2. **DoF before the resolve below scale 1.**
   - Bokeh energy, flicker and cost are better.
   - Whole-frame title error is 3% worse (1.307 → 1.351), because the blur is computed at render resolution.
   - A 1-px highlight can overshoot the reference by about 10%: a feature 0.67 render pixels wide is hit by 5 or 6 of the 8 phases.
   - Options: keep it; switch automatically to `AfterResolve` below scale 1; or fix the quantisation. Decide with numbers.
3. **Opaque-path HUD at 0.67** reads 0.984 against a bound of < 1 (worst frame; mean 0.829; native 0.018). It is rasterised at render size. A thin margin: find out why before a scene tips it over.
4. **Coverage holes in `reactive_new_objects`:**
   - transparent meshes that appear fade in through `reactive_transparent` (0.3 per unit of alpha);
   - instances appended to an existing instanced mesh share its meta and are never "new".
   Effects and spawned crowds will hit both.
5. **Upscaled sub-pixel dropout:** isolated features smaller than a render pixel drop out of a still image 4.3% of the time at 0.67, 7.3% at 0.58 and 14.5% at 0.5 (round 1: 5.0 / 31.8 / 73.0%). Performance (0.5) is above native MSAA on `taa-orbit` (3.555 against 3.184).

## 4. Explored and rejected (with numbers; don't redo without a new idea)

- **Plane-based disocclusion:** static false flags on `taa-thin` rise 159 → 686 per frame, with no trail gain. The missed pixels are a contact line, which no depth test separates.
- **A depth test for "dilated without relative motion":** false flags 159 → 387, flicker 0.301 → 0.327.
- **Wider single-frame kernels for first-frame pixels:** seven estimators tried, and the resolve's own is the best. At k31: 3.81 against 4.15-14.3.
- **Fetching history through the pixel's own vector on dilated pixels** (for the `taa-thin` 2-frame rim events): removes 2 of 3, but doubles still flicker (0.300 → 0.587, p99 12, fails). Restricted to relative motion, it no longer touches them.
- **Half-strength Karis working space:** better than round 1 in both reference spaces, but it is tuning between two yardsticks. Superseded by the display-space resolve.
- **Showing the own-object colour on a reveal frame:** +6-15k current-frame pixel-frames, and the `taa-reveal` trail mean fails.
- **Resolve cost cuts:**
  - a gather for the previous-depth footprint: no gain;
  - a combined reprojection matrix: ≤ 0.025 ms, and not output-exact;
  - slope from raw depth: 0;
  - the clip-box floor only for bright means: not output-identical.
  The resolve is 1.53-1.55 ms native (`taa-thin` 1.83). Where it goes: tile loads, 3×3, clip, blend and stores about 0.6; disocclusion 0.3-0.4; Catmull-Rom 0.19; floor 0.07-0.09. The 0.6 ms target was dropped by the user. The bar is "cheaper than MSAA in Dreidel".
- **Extra reactivity from DoF near-layer coverage:** at weights 0 / 0.5 / 1, events 482 / 468 / 421, longest 20 each. Removed.
- **A 1-px ring around a new object's footprint:** the coin's remaining strip was bloom outside the geometry, not history.
- **Dreidel bodies as `temporal: stable`:** 0.684 against 0.685. No effect.
- **TAA + SMAA:** never measured. Native TAA (0.31) is already far below SMAA (1.13) in Dreidel, so there is little room. A single measured experiment would close the user's original "maybe a combination" question for good.
- **The governor's CPU hitch** (round 1: +5.3-5.8 ms on a step): does not reproduce on the final code (0.7-1.2 ms step frames against a 0.60 ms median).
- **Neural AA:** see [neural-aa.md](neural-aa.md). Classic TAA had to ship first. The training-data path (fixed-phase capture plus 64-sample references) now exists.

## 5. Measuring without fooling yourself

- **Ghost classes** (`examples/taa-tests`, `?mode=ghost-track`), per pixel per frame:
  - (a) ghost: history kept, and more than 16 levels worse than both no AA and the perfect-information oracle;
  - (b) current frame only: alpha ≥ 0.985, because the 8-bit reactive mask caps at 0.988;
  - (c) oracle-level.
  (a) pixels are followed along their motion into events. The user's bar: no (a) event longer than one frame, and (b)/(c) no worse than MSAA in motion.
- **The oracle:** the same kernel with perfect vectors and perfect disocclusion knowledge. When it fails a gate too, the gate is unreachable for a 1-sample method. That is why the trail max < 16 and the transparent-trail gates were restated (verification.md).
- **Reference space:** display-linear decides; Karis is reported beside it. Both are reported for the reprojection test, which is run on supersampled frames because the literal no-AA version fails on aliased content even with exact vectors.
- **Game captures must fix the jitter phase.** The jitter index advances on every rendered frame, frozen ones included, so each capture session started on one of 8 phases. Sessions of one build differed on about 500,000 pixels. Every pre-fix "build A vs build B" pixel comparison in Dreidel compared phases. Fix: call `reset_temporal_history` on the first *advancing* frame of a sequence; sessions are then bit-identical. That hook (`resetadv`, with `?bloomsrc=` and `?dof=` overrides) lived in a scratch copy of the game and is gone. Recreate it there, never in the real repo. The harness always starts from a reset and was never affected.
- **Canvas size decides comparability, not the window.** Dreidel: `CHROME_SIZE=1200x984 tools/chrome-retina/chrome-retina.sh <port> <profile>` gives a 1200×897 CSS viewport at DPR 2. The game's resolution slider at 80% (`&res=80`) then gives a 1920×1435 canvas. The harness is 960×720 for quality. Log canvas size, DPR and refresh rate before any number.
- **Goldens on the Retina display:** Chrome captures in Display-P3 unless started with `--force-color-profile=srgb`. Without it every bundle-player golden "fails" by about 5 levels.
- **Timing:**
  - repeat-N slope for passes, (T(N) − T(1)) / (N − 1), because Apple pass timestamps overlap;
  - paced runs (the game paces to 60 on a 120 Hz display) and unpaced runs;
  - a sentinel scene to detect a busy GPU;
  - **one agent on the GPU at a time**. Four timing windows were spoiled by another agent building, and one by a stray keypress in a visible team window.
- **The game finds what the test scenes miss.** The three late defects (pop-in, bloom energy, DoF energy) were all found by the Dreidel ghost hunt after both seniors had signed off on the test scenes. The harness now has `taa-motion@popin`, `bloom-post@thin*` and `*@dof*` variants. Run the game early, not last.

## 6. How the team work went wrong (avoid these)

- **Disk.** Round 1 made about 20 worktrees, each with its own 3-8 GB `target/`, and filled the disk at 227 GB. Round 2 stayed under 40 GB with two build trees. The repo's own `target/` grew 115 → 158 GB over the two rounds. Run `cargo clean` before the next round if space is short.
- **Browsers.** Every agent that drives a browser needs its own Chrome instance and profile, launched with `tools/chrome-retina/chrome-retina.sh`, which places the window on the built-in display at natural size. Don't focus windows or open new ones. Never use the chrome-devtools MCP's auto-launched Chrome for numbers. Chrome on 9311 once picked up the user's own tabs; leave those alone.
- **Waiting.** Several agents' wait scripts deadlocked on a GPU lock, and one agent sat three hours inside a wait whose jobs had finished, until the main session stopped it. Use simple lock files with an owner and a timeout. End turns instead of polling.
- **API overload (529)** stopped the lead twice. Resuming with a "check `git status`, then continue" message was enough.
- **Process slips to avoid:**
  - a docs commit swept in another engineer's unreviewed harness edit;
  - a review happened after the commit;
  - one pushed tip was amended and force-pushed to fix its trailer.
  Commit only your own files, and review before committing.

## 7. Found along the way, not anti-aliasing, not fixed

- The player ignores a skeleton-root group's transform (CesiumMan lies flat).
- Morph clips don't drive weights in the player.
- A colour write mask of 0 is treated as "all" in renderer-core.
- Stale `author.js` in the `atmosphere` (`set_environment` needs an env) and `volumetrics` test scenes; `contact-shadows/author.js` hung once.
- Dreidel Derby should call `reset_motion_history(node)` where it teleports coins and letters. The renderer now covers the game's current pattern without it. That is a game-side change: suggest it, don't make it.
