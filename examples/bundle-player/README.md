# bundle-player — player visual regression through authored cameras

A minimal shipped-player-shaped page: loads ONE exported test-scene bundle
(`examples/test-scenes/<scene>/bundle`, served by `task test-scenes` on :9084)
through the real player path (`load_scene_for_player` + `HttpAssets`,
`RendererFeatures::default()`) and renders it through an **authored Camera
node exported in the bundle** — per frame `view_from_world(node world)` +
`set_camera(view, params)`, so perspective, orthographic, and animated cameras
all go through one code path with no page-side camera math.

This is the pixel half of the player story: the editor goldens render through
editor machinery, and `examples/player-tests` checks structure, never pixels.
Here a driver screenshots the fixed 800×600 canvas against the scene's
committed `golden-<camera>.png` files.

## Run

```
task test-scenes     # :9084 (bundles)
task bundle-player   # :9092 (this page)
```

Open `http://localhost:9092/?scene=player-cameras&camera=cam-perspective`
(params: `scene` default `player-cameras`; `camera` = Camera node NAME,
default the first authored camera; `bundles` = server origin). The `#hud`
line (positioned below the 800×600 capture area) reports
`… READY frames=<n>` or `FAIL — <why>`.

## Capturing / judging goldens

Set the browser viewport to exactly 800×600 (chrome-devtools
`emulate {viewport: "800x600x1"}`) so a full-viewport screenshot is exactly
the canvas, wait for `READY`, screenshot, compare with
`examples/test-scenes/<scene>/golden-<camera>.png`. The drive + expectations
live in each scene's `verify.md`; the process is wired into the
`awsm-renderer-browser-tests` skill as Layer B-vis.
