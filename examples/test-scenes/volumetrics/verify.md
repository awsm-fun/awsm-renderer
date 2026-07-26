# verify: volumetrics

One spot aimed straight down through three separated bars into a hazy room.
Haze `mode = volumetric`, density 0.03, `volumetric_distance` 45,
`scattering_anisotropy` 0.3, `volumetric_temporal` off. Bloom off.

The gaps between the bars are the test. Floor stripes only prove the shadow map
works and say nothing about the medium; what this scene is for is the beam being
occluded **in the air**.

**No `golden.png`.** Authored on a portrait-shaped window, and goldens follow the
live viewport aspect — a capture from here would mislead anyone on a normal
window. Replay `author.js` on a standard window to make one.

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/volumetrics/project}`;
     wait ~3.5s; `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:1.35, pitch:0.22, radius:22, look_at:[0,4,0]}`;
     `wait_render_settled`; screenshot — **state A** (volumetric).
  4. `set_post_process {atmosphere_mode: "fog"}`; `wait_render_settled`;
     screenshot — **state B** (analytic fog, same medium).
  5. `set_post_process {atmosphere_mode: "off"}`; `wait_render_settled`;
     screenshot — **state C**.
  6. Restore: `set_post_process {atmosphere_mode: "volumetric"}`.

expect:

  **A — volumetric**
  - A bright cone of lit air ABOVE the bars, widening downward from the fixture,
    with soft edges. This is the whole feature: light visible in the medium.
  - Below the bars, a darker shaft where the middle bar blocks the beam, and
    lighter air either side of it where the gaps let light through.
  - The floor shows lit pools with dark stripes under the bars.
  - Edges are SMOOTH. The volume is 16 px columns × 32 slices, so a nearest
    fetch would show a visible staircase; the composite samples it trilinearly.
  - The far floor is slightly hazier than the near floor.

  **B — fog (same medium, analytic)**
  - The cone in the air is GONE. Analytic fog never asks which lights reach a
    point in the medium, so it cannot draw a beam.
  - The floor pools and shadow stripes are unchanged — surface lighting doesn't
    move between modes.
  - Distance haze persists — the whole frame reads washed grey-blue and the
    floor fades out toward the back. Same air, integrated per-pixel instead of
    through a volume.

  **C — off**
  - No haze anywhere: no cone, no distance fade. The sky gradient shows through
    at its authored colour and the floor's far edge is a CRISP horizon line
    rather than fading away.
  - The floor's lit pools and shadow stripes are unchanged from A and B, and the
    stripes read darker now that nothing is washing over them.

fail:
  - A and B look the same → the volumetric path isn't running; check
    `get_post_process` reports `mode: "volumetric"`, then
    `get_console_logs` for GPU validation errors.
  - The beam stair-steps in visible blocks → the composite is `textureLoad`-ing
    froxel centres instead of sampling trilinearly.
  - The beam leans away from the fixture, or the shafts sit beside the bars
    rather than under them → the half-texel offset in the composite's slice
    coordinate is wrong (froxel values live at centres: slice `i` is at
    `(i + 0.5)/n`).
  - The whole frame is a flat wash with no visible geometry → the volume is
    covering too much depth. `volumetric_distance` too large makes the far
    slices kilometres thick and they saturate.
  - Lit air appears where a bar should be blocking it → light injection isn't
    sampling the shadow map, so the medium disagrees with the floor about what
    is occluded.
  - Beams visible but the frame rate collapses → shadow filtering is not being
    forced to the hard path inside the volume (`shadow_force_hard`); a PCSS
    light there costs a blocker search per froxel.
