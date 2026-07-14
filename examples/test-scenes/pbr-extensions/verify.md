# verify: pbr-extensions

drive:
  1. `load_project_from_url {base_url: http://localhost:9084/pbr-extensions/project}`; wait ~4s (extension shader buckets compile), then `wait_render_settled`.
  2. `set_view_options {grid:false, gizmos:false, light_gizmos:false}`.
  3. `set_camera_orbit {yaw:0.0, pitch:0.8, radius:14.5, look_at:[0,0.2,0]}`; `wait_render_settled`; screenshot the grid (state `grid`).
  4. (Alternatively replay `author.js` from scratch — it ends at the same pinned orbit and is the golden's source.)

expect:
  A 3-column × 4-row grid of 12 spheres on a neutral plane, every one visibly DISTINCT from the plain-PBR `reference` (top-left, matte blue-grey). Reading the grid left→right, top→bottom:
  - reference     — plain matte blue-grey, single soft highlight.
  - transmission  — glassy near-white, reads translucent (lighter/see-through vs reference).
  - volume        — GREEN body (green attenuation tint through the glass).
  - clearcoat     — RED with a sharp lacquer highlight over the diffuse red (double-highlight look).
  - sheen         — PURPLE with a soft fuzzy rim (retroreflective fabric sheen).
  - iridescence   — dark teal body with a rainbow/orange–purple thin-film RIM.
  - anisotropy    — tan/gold with a STRETCHED ring-shaped highlight (not a round dot).
  - specular      — saturated BLUE, tinted specular F0.
  - dispersion    — near-white glass, subtly prismatic at edges.
  - diffuse-trans — warm brown/tan with soft light bleed (diffuse transmission).
  - emissive-str  — glowing ORANGE/peach (emissive_strength lifts it above lit range).
  - ior-only      — white high-IOR glass (strong refraction/reflection vs transmission).
  Status bar reads `13 buckets` (specialize-only pipeline per feature-set) and `13 materials`.

fail:
  - Any sphere looking identical to the reference (extension not applied / shader bucket missing).
  - Anisotropy highlight round instead of stretched; iridescence with no colored rim; volume not green; clearcoat with a single (not double) highlight; emissive not glowing.
  - Fewer than 13 shader buckets, or a solid-color/untextured flat sphere (pipeline failed to specialize).
  - Any sphere rendering black or missing (bucket compile failure).
