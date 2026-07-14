# verify: cutoff-anim-shadow

drive (replay author.js — authored clip needs fresh nodes; see memory
animation-scenes-need-authorjs-replay):
  1. Replay `examples/test-scenes/cutoff-anim-shadow/author.js` (custom Mask
     material on an upright panel + a spin clip about the panel normal Z; seeded
     directional light casts).
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`;
     `set_camera_orbit {yaw:0.35, pitch:0.35, radius:9, look_at:[0,1.4,0]}`.
  3. `set_playhead {t:0.0}`; `wait_render_settled`; screenshot (state `t0`).
  4. `set_playhead {t:0.5}` (=45°); `wait_render_settled`; screenshot (state `t05`).

  A masked mesh animated under a light — the shadow must track the moving cutout.

expect:
  - At BOTH playheads the floor shadow is HOLE-PUNCHED (circular light discs in
    the shadow, not a solid rectangle) — masked alpha respected in the shadow pass.
  - `t0`: panel is an axis-aligned square, holes in a straight 5×5 grid; the
    shadow holes are in the SAME aligned grid.
  - `t05`: panel rotated 45° to a DIAMOND, hole grid diagonal; the shadow holes
    ROTATED to a matching diamond arrangement — i.e. the shadow holes TRACK the
    panel's rotated cutout (the alpha is re-sampled at the animated pose).
  - The two shadow-hole patterns are DIFFERENT (aligned vs diamond), proving the
    shadow follows the animation frame-by-frame.

fail:
  - A STATIC shadow (holes don't move between t0 and t05) — the shadow pass
    isn't re-sampling the animated pose.
  - A SOLID shadow rectangle (cutout alpha ignored in the shadow pass).
  - The panel animating but the shadow frozen, or vice-versa (desynced).
  - Material fails to register → black/pink panel.
