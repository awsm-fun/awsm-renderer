# verify: env-ibl

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/env-ibl/project}`; wait ~4.5s (KTX2 env transcode); `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.5, pitch:0.25, radius:9, look_at:[0,1,0]}`; `wait_render_settled`; screenshot (state `three-slots`).
  4. Slot-swap check (independence): `patch_environment {specular: {ktx: {asset_id: <other env>}}}` (leaving skybox/irradiance) — the MIRROR sphere's reflection must change while the skybox stays; `patch_environment {skybox: ...}` alone changes the background without re-lighting; `irradiance` alone shifts the rough sphere's ambient. Screenshot each swap.

  The 3-slot environment (skybox / specular / irradiance) on KTX2 assets
  (photo_studio cubemaps via `import_ktx_env_from_url` + `patch_environment`).
  Probe trio: mirror metal, rough dielectric, glossy blue dielectric.

expect (three-slots):
  - The `mirror` sphere REFLECTS the studio interior sharply (recognizable studio
    window/wall in the reflection) — the specular slot.
  - The `rough` sphere is softly lit with no sharp reflection — irradiance slot
    (diffuse ambient) dominates.
  - The `glossy-dielectric` (blue) sphere shows sharp environment HIGHLIGHTS
    (window glints) over its blue body — specular slot on a dielectric.
  - The skybox shows the studio background (not a flat color).
  - (slot-swap) each slot changes ONLY its channel: specular→reflections,
    skybox→background, irradiance→ambient, independently.

fail:
  - Mirror sphere not reflecting the env (flat/black) — specular slot unbound.
  - Rough sphere unlit/black — irradiance slot unbound.
  - Flat/solid skybox (skybox slot not loaded, KTX2 transcode failed).
  - A slot swap changing the wrong channel (slots not independent).
