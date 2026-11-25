# TAA: the final round

**Status:** ready to run (written 2026-11-25, after 0.46.0 shipped TAA as
opt-in). The goal is to finish anti-aliasing so it does not need revisiting:
close the known gaps, verify everything on one final head, flip the default,
release, and delete this folder.

Read in this order:

1. This file: scope, rules, delivery.
2. [`findings.md`](findings.md): what rounds 1 and 2 established. Why each
   default is what it is, the open trade-offs, everything already tried and
   rejected (with numbers), how to measure without fooling yourself, and how
   the team work went wrong.
3. [`docs/TAA.md`](../../TAA.md): the shipped reference. §1 covers the
   options and the recommended configurations, §3 how it works, §6 the known
   limits, §7 every measurement.
4. [`verification.md`](verification.md): the gates (05 of the original plan),
   with the user-approved restatements and the latest results. Gates are
   never loosened; the only restatements allowed are the ones recorded there.
5. Optional: [`neural-aa.md`](neural-aa.md). Research for a later
   investigation, out of scope here.

## The bar (the user's words, carried forward)

- "We do NOT want nasty ghosting trails. We do want state-of-the-art."
- No class-(a) ghost event longer than one frame anywhere in the test scenes
  or Dreidel Derby at the default configuration (findings §5). Each exception
  has to be shown to the user as a GIF and accepted by them.
- One-frame current-frame softness and oracle-level limits are fine if they
  look at least as good as MSAA in motion.
- Cost: TAA must be cheaper than MSAA 4× in Dreidel Derby (title and
  mid-round, paced) at the shipped default scale, and look great. The old
  0.6 ms resolve target is dropped.
- **TAA off, and every TAA option that is off, costs nothing.** Prove it every
  time: pixels bit-identical to 0.46.0 (and to 0.45.0 for no AA / MSAA /
  SMAA), plus A-B-A-B timing.
- Make new techniques configurable where that's sensible. Document each
  option's cost and what it fixes, and keep the recommendation table in
  TAA.md §1 current.

## Scope

### Must do

1. **Stabilise bloom in still images** (findings §3.1).
   - Build the temporal blend of bloom's input, reprojected with TAA's motion
     vectors, as a third `TaaBloomSource` (or replace `CurrentFrame` if it is
     strictly better).
   - It must keep `CurrentFrame`'s energy (thin-source halo ≈ the reference),
     remove the pulse (halo-ring p99 ≤ 4 in `bloom-post@thinstill` and on the
     frozen Dreidel title menorah), and not trail behind moving lights.
   - Pick the default from those numbers plus the user's look at the GIFs.
2. **Close the `reactive_new_objects` holes** (findings §3.4):
   - transparent meshes that pop in;
   - instances appended to an existing instanced mesh.
   Add harness variants for both (e.g. `taa-transparent@popin`,
   `instancing-stress@spawn`), count ghosts before and after, and keep it
   zero-cost when nothing appears.
3. **The opaque-path HUD at 0.67** (0.984 against < 1). Find the cause and
   leave a real margin, or document why it can't have one.
4. **DoF order below scale 1** (findings §3.2). Decide between keeping
   `BeforeResolve`, switching to `AfterResolve` automatically below scale 1,
   or fixing the jitter-phase quantisation. Use numbers on the Dreidel title
   and the `@dof*` scenes.
5. **Deterministic game captures.** Recreate the fixed-jitter-phase hook
   (`reset_temporal_history` on the first advancing frame) and the
   `?bloomsrc=` / `?dof=` overrides in the scratch copy of Dreidel Derby
   (findings §5). Every Dreidel number in this round uses a fixed phase.
6. **Flip the default.** TAA native becomes the default for new scenes:
   - the editor's new project and the renderer's "new scene" config write the
     TAA block explicitly;
   - scene files with no AA block keep MSAA 4×, so no existing project
     changes look and no golden moves.
   Re-confirm the choice of native over 0.67 with the final numbers.
   Document in TAA.md §1 ("Should TAA be the default?") and in the release
   notes. Decide what the player, the model tester and `RendererFeatures`
   default to, and say why.
7. **Full verification on the final head.** Nothing shipped in 0.46 was
   re-verified as a whole after the last three changes, so run all of it:
   - every gate in verification.md, native and 0.67 / 0.58 / 0.5;
   - the ghost tracker on every `taa-*` scene and variant;
   - zero-cost-off as above;
   - the frame-cost table (TAA.md §7.6), including the Balanced preset and the
     "as a player gets it" row (the game's own sizing and governor);
   - TAA 0.67 mid-round quality in Dreidel;
   - the browser suite, all three layers, **including the full 41-scene
     Layer A pass** (use the `awsm-renderer-browser-tests` skill);
   - Dreidel Derby title and mid-round: quality, the fixed-phase ghost hunt,
     paced cost against MSAA;
   - the two templates (`physics-multithreaded`, `physics-mujoco`);
   - the model tester (skinned, morph, transmission, clearcoat, iridescence);
   - the editor (live switching, debug views, save / load of every option,
     picking at 0.67).
8. **User sign-off.** Build one page of the key GIFs, at 1× and 4×, panels
   TAA | MSAA | reference:
   - every remaining ghost event;
   - the bloom before and after;
   - the menorah bokeh;
   - ordinary title and mid-round motion;
   - one upscaled (0.67) example.
   The main session publishes it with the Artifact tool and asks the user.
   The agents cannot watch animations; the user is the judge of "looks great
   in motion".
9. **Release 0.47.0** (after the sign-off; see Delivery), then delete this
   folder, moving anything durable into TAA.md first.

### Evaluate, then decide with numbers and document either way

- **TAA + SMAA** (SMAA T2x-style: SMAA edges on the current frame before the
  resolve). One measured experiment closes the user's original "maybe a
  combination" question. Ship it only on a clear win for its cost; otherwise
  record the numbers in TAA.md and keep the combination rejected.
- **Motion vectors for particles and large, mostly opaque transparents.** The
  real fix for what full reactivity only hides (transparent and line trails:
  TAA.md §6). Measure the trail with and without, and the cost.
- **The default for `reactive_lines`**, and whether `edge_release` can become
  free of flicker without a GPU-side moved-bit pass (findings §2). If it can't,
  leave it off and say so.
- **Balanced / Performance presets:** sub-pixel dropout and `taa-orbit` error
  (findings §3.5). Tune them, or state plainly what they are for.
- Anything new you find. The pattern so far: the game finds what the test
  scenes miss, so run the Dreidel ghost hunt early.

### Out of scope

- Neural AA (neural-aa.md).
- Further resolve-cost micro-optimisation without a new idea (findings §4).
- The non-AA bugs in findings §7. File them, don't fix them here.
- The Android 32 B/sample device check belongs to the user
  (`docs/checklist-android.md`). The team verifies the fallback by forcing
  it on the Mac, and lists the device check as outstanding.

## Team and process

- **Lead:** Claude Fable if the allowance allows, otherwise Opus. **Senior
  engineer:** Opus. Spawn the senior at once, brief them fully, and exchange
  agent IDs so messages go to each other, not to "main".
- **Pair review both ways is mandatory.** No commit until the other has read
  the actual diff against the ghosting rules (TAA.md §4), the math in TAA.md §3,
  the Apple GPU pitfalls (no `return` inside `switch`, no loop-indexed
  per-invocation arrays, unrolled 3×3; f16 store rounding for any new
  history), and zero-cost-off. Findings are resolved or rejected with a reason.
- At most one or two more agents (a measurement engineer is the useful one).
  Brief each fully.
- **Keep going until the lead and the senior both agree nothing worthwhile is
  left to explore**, then deliver. Record what was explored, including dead
  ends with numbers.

### Binding rules (each one cost hours last time; findings §6)

- **Disk:**
  - at most two build trees in total: the main checkout plus one worktree;
  - no per-agent `target/` dirs and no repo copies, except one scratch copy
    of Dreidel Derby and one each of the two templates;
  - the scratchpad stays under 40 GB; check `df -h /` before builds and stop
    to clean below 60 GB free;
  - clean everything at the end and report the final size.
- **Browsers:**
  - each driving agent has its own Chrome, launched only with
    `tools/chrome-retina/chrome-retina.sh <port> <profile-dir> [flags] [url]`;
  - for Dreidel, add `CHROME_SIZE=1200x984` and `&res=80`, then log the
    canvas (1920×1435), DPR 2 and the refresh rate before any number;
  - goldens need `--force-color-profile=srgb`;
  - never focus windows, never use port 9086 (the production MCP), and never
    use the chrome-devtools MCP's own Chrome for numbers;
  - stop servers and browsers when done.
- **GPU:** one agent timing at a time, with a lock file that has an owner and
  a timeout. Nobody builds during another agent's timing window. Use the
  repeat-N slope for passes, and paced plus unpaced runs for the game.
- **Other repos:** Dreidel Derby
  (`/Users/dakom/Documents/AWSMFUN/AWSM-REPOS/games/dreidel-derby`), the
  templates and the experiments are tested only through scratch copies with a
  temporary `[patch.crates-io]`. **Never commit in those repos.** The real
  Dreidel repo holds the user's own uncommitted work: don't touch it.
- **Git:**
  - commit only your own files, after review;
  - no `git stash` or whole-tree operations in the shared checkout;
  - no force-push;
  - use the commit attribution your harness specifies.

## Delivery

1. Branch `feat/taa-final` from `main` (0.46.0). Commit as you go, reviewed,
   and push regularly. Open a draft PR early.
2. When the round is done and the full verification (Must 7) passes:
   - write the report to the scratchpad, opening with what needs the user's
     eye;
   - prepare the GIF sign-off page (Must 8);
   - hand back to the main session.
   **Do not merge or release before the user signs off.**
3. After sign-off:
   - mark the PR ready and wait for CI;
   - merge;
   - from an up-to-date `main`, write `docs/RELEASE-NOTES-0.47.md`, move what
     is durable into `docs/TAA.md`, delete `docs/plans/taa-final-round/`, and
     update `docs/plans/README.md`;
   - commit and push, wait for CI, then run `task publish -- 0.47.0`. It
     bumps, tags, publishes the crates, deploys the editor and the model
     tester, and pushes the tag, which builds the MCP binaries.
   Follow `docs/RELEASING.md` and the 0.45.0 / 0.46.0 precedent.
4. If any gate fails and can't be fixed, don't merge. Say why, with numbers.

## The report

Write it to `<scratchpad>/taa-final-report.md`, with evidence in
`<scratchpad>/taa-final-report/`. Copy both to
`~/Documents/AWSMFUN/AWSM-REPOS/taa-evidence-2026-11/round3/` before cleanup,
so it survives the scratchpad. It covers:

- what needs the user's eye, with GIF links;
- what changed and why;
- dead ends with numbers;
- every gate before and after (original and restated thresholds);
- the cost table;
- the zero-cost-off proof;
- the regression sweep per consumer and AA mode;
- the default decision, and any goldens changed;
- the review log;
- what both seniors agreed was not worth pursuing;
- what's outstanding (Android);
- PR, merge and release status;
- the final scratchpad size.
