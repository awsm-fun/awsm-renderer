# verify: anim-morph

drive (replay author.js — the live path; load_project does NOT reproduce the lock):
  1. Replay `examples/test-scenes/anim-morph/author.js` (needs media server :9082):
     `new_project` → `import_model_from_url` AnimatedMorphCube (poll snapshot
     until a `skinned_mesh` node appears) → `add_clip {id, name:'two-morphs'}` →
     two `add_track {target:'morph', node, index:0|1}` → keyframes: track 0
     ramps 0→1 over t=0..2, track 1 ramps 1→0 → `set_current_clip` →
     `set_playhead {t:1.0}`.
  2. `set_camera_orbit {yaw:0.7, pitch:0.4, radius:0.12, look_at:[0,0,0]}`;
     `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. SETTLE: after `set_playhead`, wait ~1s + `wait_render_settled` before
     querying — the imported "Square" clip can transiently contend on morph:0, so
     an immediate query may read a mid-transition value (e.g. [0.68,0]). A clean
     re-`set_current_clip`+`set_playhead`+settle reads the steady [0.5,0.5].
  4. Query `morph_data {nodes:[node]}` → `entries[node].weights`. Screenshot (state `wedge`).

  Why not load_project: the authored clip's track targets reference the
  import-minted node id, which changes on save/load, so a loaded project restores
  `current_clip:"Square"` + weights [0,0] and cannot re-drive the two-morphs clip.

expect:
  - `morph_data` weights read **[0.5, 0.5]** (±0.01) at playhead 1.0 — each track
    writes ONLY its own morph index (the plan-005 §3 per-index compose lock).
  - Visual: the cube deformed by BOTH morphs at half strength — a WEDGE shape
    (top face tilts, one edge pulled to a point), not an undeformed box.

fail:
  - Weights like [X, 0] or [0, X] — one track's padding zeros stomped the other
    index (the pre-005§3 whole-vector-blend regression this scene locks).
  - Weights [0, 0] / undeformed box (clip not current, or targets not resolving).
  - Weights not summing/settling to [0.5, 0.5] after a proper settle.
