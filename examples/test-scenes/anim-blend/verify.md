# verify: anim-blend

drive (replay author.js — the live path; load_project can't re-drive the mixer):
  1. Replay `examples/test-scenes/anim-blend/author.js` (needs media server :9082):
     `new_project` → `import_model_from_url` Fox (poll snapshot until
     `animation.clips.length >= 3`) → resolve the Walk + Run clip ids by name →
     `add_layer` / `add_strip {layer:0, clip:walk, start:0, len:2.0}` →
     `add_layer` / `add_strip {layer:1, clip:run, start:0, len:2.0}` →
     `set_layer_weight {layer:1, weight:0.5}` → `set_playhead {t:0.35}`.
  2. `set_camera_orbit {yaw:1.2, pitch:0.3, radius:380, look_at:[0,45,0]}` (Fox is
     ~100 units tall — radius in the hundreds); `set_view_options {grid:false,
     gizmos:false, light_gizmos:false}`. Settle ~1s + `wait_render_settled`.
     Screenshot (state `blend`).
  3. Distinctness proof: `set_layer_weight {layer:1, weight:0.0}` (walk only),
     settle, screenshot (state `walk-only`) — the leg stance must visibly differ
     from `blend`.

  Why not load_project: the mixer strips reference the import-minted clip/node
  ids; a loaded project can't re-drive them (see memory
  animation-scenes-need-authorjs-replay).

expect:
  - `blend` (layer 1 @ 0.5): a valid gait pose — legs mid-stride, tail out,
    body level — a walk/RUN blend, NOT a T-pose or A-pose. `snapshot.animation`
    reads `mixer_layers:2`, `playhead:0.35`.
  - Textured orange Fox (base color bound), low-poly, no exploded/collapsed limbs.
  - `walk-only` (layer 1 @ 0.0): a DIFFERENT leg stance than `blend` — proving
    layer 1 (Run) actually contributes to the blended pose.

fail:
  - T-pose / A-pose / bind pose (mixer not evaluating).
  - `blend` and `walk-only` looking identical (layer weight not applied — the Run
    layer isn't blending in).
  - Popping / jitter, exploded limbs, or candy-wrapper collapse at a joint.
  - Untextured/white Fox, or `mixer_layers` != 2.
