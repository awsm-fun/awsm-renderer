# verify: contact-shadows

drive:
  1. Replay `examples/test-scenes/contact-shadows/author.js` (or
     `load_project_from_url {base_url: http://localhost:9084/contact-shadows/project}`).
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`;
     `set_camera_orbit {yaw:0.6, pitch:0.42, radius:13, look_at:[-0.3,0.8,0.3]}`;
     `wait_render_settled`; screenshot (state `pcss-sscs`).
  3. PCSS ablation: `patch_kind {id:<spot>, patch:{light:{spot:{shadow:{hardness:'soft'}}}}}`;
     re-settle; screenshot (state `soft`) — compare the sphere's contact shadow.
  4. SSCS ablation: `set_shadows {patch:{sscs_enabled:false}}`; re-settle; screenshot
     (state `no-sscs`) — compare short-range contact darkening at the ground contacts.

  A resting sphere + a standing pole under an overhead PCSS spot (PCSS is 2D-atlas
  only → a spot, not directional cascades), SSCS enabled renderer-wide.

expect (pcss-sscs):
  - The sphere sits on the floor with a CONTACT-HARDENING shadow: darkest/tightest
    right at the ground-contact point directly beneath it, softening (penumbra
    widening) outward — not a uniform-width blur.
  - The pole casts a base-contact shadow; contacts read grounded (no floating).
  - Short-range contact darkening (SSCS) deepens the shade right where each object
    meets the floor.
  - Scene is well-lit inside the spot cone; no shadow acne / peter-pan gap.
expect (ablations):
  - `soft` (hardness soft): the sphere shadow penumbra is more UNIFORM-width — the
    distance-varying contact-hardening of `pcss-sscs` is gone.
  - `no-sscs`: the tight short-range darkening at the ground contacts is reduced
    vs `pcss-sscs`.

fail:
  - No shadows at all (spot not casting / PCSS path broken).
  - The sphere shadow identical between `pcss-sscs` and `soft` (PCSS not applied —
    but note `hardness` is NOT settable via SetLightParam, only patch_kind).
  - `no-sscs` identical to `pcss-sscs` (SSCS flag not wired from scene.shadows).
  - Shadow acne, peter-pan gaps, or a solid black under-object blob.
