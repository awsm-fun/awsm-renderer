# verify: player-cameras (bundle-player tier — PLAYER pixels, not editor pixels)

This scene's goldens are captured from **`task bundle-player`** (:9092), NOT
from the editor: the page loads the committed `bundle/` through the real
player path (`load_scene_for_player` + `HttpAssets`, `RendererFeatures::default()`)
and renders through an **authored Camera node exported in the bundle** —
per-frame `view_from_world(node world)` + `set_camera(view, params)`, the
consolidated camera API a shipped game uses. One golden per camera:

- `golden-cam-perspective.png` — `cam-perspective` (45° fov, elevated 3/4 from
  the front-RIGHT)
- `golden-cam-ortho.png` — `cam-ortho` (orthographic, half_height 3.2,
  elevated from the front-LEFT)

drive:
  1. `task test-scenes` (:9084) + `task bundle-player` (:9092).
  2. Set the browser viewport to exactly **800×600**
     (chrome-devtools `emulate {viewport: "800x600x1"}`) — the canvas sits at
     the page's top-left at its native 800×600, and the `#hud` line is placed
     BELOW y=600, so a full-viewport screenshot is exactly the render.
  3. Open `http://localhost:9092/?scene=player-cameras&camera=cam-perspective`.
     Poll `#hud` until it reads
     `bundle-player: player-cameras camera=cam-perspective (perspective) READY …`
     (a `FAIL —` line means stop and report it verbatim). Screenshot → compare
     against `golden-cam-perspective.png`.
  4. Same with `camera=cam-ortho` → `golden-cam-ortho.png`.

expect:
  - PERSPECTIVE state: viewed from the front-right — red box on the LEFT,
    large green sphere front-center, small yellow box center-right behind the
    sphere, tall blue pillar on the RIGHT with visible perspective
    convergence; gradient sky above the floor's horizon; soft shadows under
    every object.
  - ORTHO state: viewed from the front-LEFT (arrangement mirrors: red box
    front-center-left with the yellow box peeking behind it, blue pillar
    upper-center, green sphere on the RIGHT), NO perspective convergence (the
    pillar's verticals are parallel; boxes read axonometric), no horizon (the
    narrow parallel frustum sees only floor).
  - The two states MUST differ in both viewpoint and projection — identical
    images mean the `?camera=` selection is broken.

fail:
  - `#hud` shows `FAIL — no camera named …` or `scene has no Camera node` ⇒
    the bundle lost its camera nodes (export regression).
  - Both cameras render the same image ⇒ camera selection / params routing
    broken.
  - The ortho state shows converging verticals ⇒ orthographic
    `CameraProjectionParams` lost on the player path.
  - Geometry visible but no shadows / black surface ⇒ player feature-default
    regression (this page runs `RendererFeatures::default()` exactly).
