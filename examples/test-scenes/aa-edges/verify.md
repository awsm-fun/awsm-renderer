# verify: aa-edges  (MSAA + SMAA view-toggle recipes)

This scene is Layer-A ONLY — MSAA/SMAA are editor `set_view_options` toggles, not
player-bundle state, so there is no `bundle/` and no player-tests coverage. Any
high-contrast-edge scene works; `aa-edges` is a dedicated minimal one (a dark box
rotated 45° against the light sky, so a crisp diagonal silhouette edge is the
whole subject).

drive:
  1. Replay `examples/test-scenes/aa-edges/author.js` (builds the box + pins the
     camera). It ends with `msaa:false, smaa:false`.
  2. `wait_render_settled`; screenshot (state `no-aa`).
  3. MSAA: `set_view_options {msaa:true}` — STRUCTURAL, recompiles AA-variant
     pipelines; wait ~2.5s + `wait_render_settled`; screenshot (state `msaa`).
  4. Back off: `set_view_options {msaa:false}`; then SMAA:
     `set_view_options {smaa:true}` — post-process, independent of MSAA; wait ~2s
     + `wait_render_settled`; screenshot (state `smaa`).
  5. Compare the SAME framed diagonal top edges across `no-aa` / `msaa` / `smaa`.

expect:
  - `no-aa`: the box's diagonal silhouette edges (top-left and top-right, against
    the light sky) show visible STAIR-STEPPING / jaggies.
  - `msaa`: the SAME edges are visibly SMOOTHER — stair-steps softened along the
    diagonals (4× multisample on the geometry edge). Pixels along the silhouette
    demonstrably differ from `no-aa`.
  - `smaa`: the edges are ALSO smoothed (post-process edge AA) — a different
    smoothing character than MSAA but clearly not the jaggy `no-aa`. Pixels differ.
  - Body interior is otherwise unchanged (flat box) — only the edges move.

fail:
  - `msaa` (or `smaa`) pixel-identical to `no-aa` — the toggle did nothing (a
    real assertion: "can't tell if AA is on" ⇒ FAIL). Cross-check the raw edge.
  - MSAA toggle NOT triggering a recompile / no settle (structural flag ignored).
  - The whole image blurring (post-AA over-blurring the interior, not just edges).
  - A crash / black canvas after the structural MSAA flip.
