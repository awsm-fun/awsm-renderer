# verify: atmosphere

Six identical white pillars receding down -Z over a dark floor, one 14 m mast
crossing the haze layer, a dusk sky gradient. Haze on: color `[0.35,0.42,0.55]`,
density `0.045`, `base_height 2.0`, `height_falloff 0.25`. Bloom off.

The arrangement is a control experiment, not a diorama: the pillars are the same
box with the same material and the same light, so **any** difference down the row
is the haze and nothing else. The three states below are an A/B/C that separates
the distance term from the height term — run all three, because state B is what
tells you the height integral is real rather than the distance term in disguise.

**No `golden.png` in this scene.** It was authored on a portrait-shaped window
and goldens follow the live viewport aspect, so a capture from here would be a
misleading reference for anyone on a normal landscape window. Capture one by
replaying `author.js` on a standard window and commit it with the usual
explanation.

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/atmosphere/project}`;
     wait ~3.5s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.22, pitch:0.10, radius:34, look_at:[-1,6,-28]}`;
     `wait_render_settled`; screenshot — **state A** (haze as authored).
  4. `set_post_process {atmosphere_height_falloff: 0}`; `wait_render_settled`;
     screenshot — **state B** (uniform medium, height structure removed).
  5. `set_post_process {atmosphere_enabled: false}`; `wait_render_settled`;
     screenshot — **state C** (haze off).
  6. Restore: `set_post_process {atmosphere_enabled: true,
     atmosphere_height_falloff: 0.25}`.

expect:

  **A — haze on, height falloff 0.25**
  - The nearest pillar is crisp and near-white. Each successive pillar is greyer
    and bluer than the one before it; the farthest is only just separable from
    the background.
  - The mast is brightest at its top and greys steadily toward its base — the
    medium is full below `base_height` and thinned above it, and the mast spans
    both.
  - The sky is darkest overhead and brightens toward the horizon (a horizon ray
    stays in the medium; an upward ray leaves it).
  - Ground shadows are visible but soft and washed, not black.

  **B — haze on, height falloff 0 (uniform medium)**
  - The pillar depth ramp is UNCHANGED from A. Density didn't move, only the
    height profile.
  - The mast is now UNIFORM top to bottom — the vertical gradient from A is gone.
  - The sky is a FLAT wash of the haze colour with no vertical gradient: with no
    height structure, every sky ray saturates completely.

  **C — haze off**
  - Every pillar is identically crisp and white, including the farthest. No depth
    ramp at all.
  - The mast is uniform bright white.
  - The ground shadow is hard and dark.
  - The background is the raw dusk sky gradient (dark blue), not the haze colour.

fail:
  - A and C look the same → the fog term isn't being applied (check that
    `atmosphere.enabled` survived the load: `get_post_process`).
  - In A, the pillars ramp but the mast doesn't → the height integral is being
    skipped, or `base_height`/`height_falloff` aren't reaching the uniform.
  - A and B look the same → `height_falloff` isn't reaching the shader; the
    height integral is what B removes.
  - The far pillars go BLACK rather than toward the haze colour → the blend is
    inverted (`rgb * (1-t) + color * t`).
  - The sky is fully saturated in A as well as B → sky pixels aren't taking the
    height integral, or the sky-depth test is inverted.
  - Anything near the camera is hazed as heavily as anything far → distance isn't
    being reconstructed from depth (a constant `dist` looks exactly like this).
