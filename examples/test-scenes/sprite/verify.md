# verify: sprite

This is primarily a **Layer-B (player-tests)** scene — the SPRITE half of the
sprite/decal/particle texture-binding lock. A particle emitter binds a billboard
sprite texture (`set_particle_emitter {texture}`), baked to KTX2 in the bundle.

Note on Layer A: the live fountain does NOT render in the headless/automated
editor — the editor render loop is dirty-driven and idles, so the dt-driven
particle sim never advances (the golden is a setup frame: floor only). The
continuous fountain simulates only in the player runtime (Layer B), which is
exactly where the texture-binding assertion runs. So the meaningful verification
is Layer B.

## Layer B (the lock — machine-checked)
Run `examples/player-tests` with `?scenes=sprite` (or in a full run). Assert:
  - `load-transaction:sprite` PASS.
  - `counts:sprite` PASS with **pool_textures ≥ 1** — the emitter's sprite KTX2
    transcodes + binds on-device. A silent drop of the sprite texture would
    leave the pool short and FAIL. (Observed: pool_textures=2.)

## Layer A (optional, needs a continuously-rendering context)
To see the fountain, load the bundle in the real player runtime (continuous rAF)
or an interactive editor session with playback running:
  1. `load_project_from_url {base_url: http://localhost:9084/sprite/project}`.
  2. Play the transport so the emitter simulates.
expect: an upward cone fountain of large blended BILLBOARDS, each sampling the
  bound sprite texture (the logo image reads on the quads) — textured sprites,
  not flat color squares; billboards face the camera.
fail: flat untextured squares (sprite texture dropped), or no particles at all in
  a continuously-rendering context.
