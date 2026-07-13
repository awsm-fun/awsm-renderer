# SSR / reflections — follow-ups

Queue agreed with David (2026-07-12) after the SSR fundamentals rework
(bilinear depth, crossing acceptance, per-sample descriptor resolve,
prefiltered-env fallback, SSR↔IBL crossfade). Reference architecture:
the third-party reflection plan (`~/Downloads/awsmrenderer-reflection-plan.md`)
— SSR as a confidence-weighted contribution over layered fallbacks; no
camera jitter.

## Bugs

1. **MCP screenshot readback colorspace** — FIXED (2026-07-13): a double
   sRGB encode — the display pass hand-encodes sRGB into the non-sRGB
   canvas format, then the exporter converted again. Swapchain captures now
   go through `export_display_texture_as_rgba8` / `mark_display_encoded`
   (readback verified byte-matching the on-screen luminance on the arena).
   All 23 pre-fix goldens were CORRECTED OFFLINE (no authoring round-trips):
   the double-encode is a deterministic transform, so one sRGB decode per
   pixel recovers the single-encoded image — proven exact to ±1 LSB over
   all 256 byte values, and validated empirically (converted ssr golden's
   sky matches a fresh fixed-readback capture within 1 LSB; the old bytes
   were ~40 levels lifted). No footgun remains.

## Roadmap (in order)

2. **Debug views** — DONE (ssr_debug on set_post_process; structural axis):
   1 = confidence (green hit-blend / red env), 2 = travel heat, 3 = source
   (green hit / blue env / black none), 4 = traversal steps (gray ramp,
   white = budget). Dev-only + transient (serde(skip) — never persisted).
   The encodings ride the normal resolve/temporal/composite chain
   (additive over the scene; read on dark content). Verified on-device.
3. **Confidence-weighted composition** — DONE at the appropriate depth:
   `confidence = hit_conf (refined-penetration quality) x edge fade x travel
   fade` is a named first-class value in the trace and drives the SSR-over-
   fallback lerp (and the confidence debug view). The composite-side split
   (trace exports confidence; composite evaluates the fallback stack and
   lerps) is DELIBERATELY deferred until local probes add a second fallback
   source — doing it today would duplicate the reflected-dir/fresnel/mip
   math in the composite with zero behavioral gain (the env is the only
   fallback and the trace owns all of its inputs).
4. **Perf check** — DONE (2026-07-12, arena @1338x768, MSAA, M-series):
   vsync-locked 60.0 fps in ALL configs; render_cpu EMA 1.41 ms with
   full-res SSR, 1.40 off, 1.28 half-res — SSR's CPU record cost is noise.
   GPU-side per-pass timing is NOT measurable (`?trace=sub-frame` spans are
   CPU record-time; timestamp-query unused) — at vsync-lock there is GPU
   headroom on this hardware, but the plan's ~1–2 ms GPU budget can only be
   verified once timestamp queries are wired (follow-up if mobile/weak-GPU
   targets matter). Arena ships full-res SSR; half-res remains one knob away
   (resolution_scale 0.5).
5. **Local reflection probes** — STEP 1 SHIPPED (2026-07-13, triggered by the
   arena's platform-occlusion reflection gaps + periphery fade): a GLOBAL
   box-projected probe on `scene.environment.probe` ({enabled, center,
   half_extents}). When enabled, BOTH specular env consumers (IBL specular in
   brdf_pbr + the SSR miss fallback in the trace) parallax-correct their
   lookup through the shared `box_project_env_dir` (shared_wgsl/math.wgsl) —
   fallback reflections anchor to the scene bounds instead of sliding like an
   infinite sky. Runtime uniform gate (lights info bytes 48..80, mirrored
   into SsrParams 32..64), NOT a template axis; disabled = zeroed = exactly
   the old behavior. Wire: set_environment probe field (MCP) /
   PatchEnvironment (editor, partial semantics). REMAINING for full tier 4:
   multiple local probes w/ per-renderable assignment, editor authoring UI,
   in-engine capture (today the cubemap is authored offline — the arena's
   gen_interior()).
6. **Planar reflections** (content-triggered) — re-render from mirrored
   camera for explicitly-flagged hero mirrors. The real answer to
   perfect-mirror quality; SSR is not (undersides/off-screen content are
   fundamentally unavailable to screen space).
7. **Prefiltered scene-color mips** for glossy hit sampling (replaces the
   8-tap disk; quality + perf).

## Fixed-this-round context (for archaeology)

- Point-sampled depth → bilinear (`scene_depth_at`, sky+discontinuity
  guarded) — killed the dash/stripe quantization family at the source.
- Clause-pair acceptance → sign-change crossing + post-refine validation —
  angle-robust; `ssr_thickness` demoted to a leak threshold.
- Single-sample SSR descriptor → per-sample resolve through the edge
  accumulator (words 4..8) + final_blend — MSAA survives SSR.
- Raw skybox mip-0 fallback → prefiltered specular env at spread-scaled mip
  (starfield stars no longer reflect as bright blobs).
- Glossy HDR clamp (luminance ≤3 before filtering) + travel-cone floor 0.3 —
  bloom-hot contact reflections stopped crawling with the camera.
- SSR↔IBL crossfade over [0.15, 0.6] — mid-gloss band no longer
  double-counts reflection energy.

## New follow-up (2026-07-12, Phase B)

8. **Glass-shell aliasing (CORRECTED 2026-07-13** — the original "transparent
   pass has no MSAA" claim was WRONG, called out by David: the transparent
   pass is a forward pass into the MULTISAMPLED target (pipelines keyed on
   msaa_sample_count; the pass performs the hardware resolve — render.rs
   "handled by MSAA resolve in transparent pass"). The Phase B observation
   stands — the two-layer glass neon tube shells DID render with blocky
   bright-on-dark aliasing (attempted + reverted) — but the cause must be
   SHADING-space, which hardware MSAA cannot help: MSAA supersamples
   coverage, not shading (one fragment shade per pixel), so a fresnel-bright
   shell's steep gradient and the screen-space transmission/background fetch
   (pixel-granular refraction offsets) alias regardless. Candidate fixes
   when revisited: per-sample shading for flagged transparent materials, a
   mip-biased/supersampled transmission fetch, or shader-side gradient
   smoothing. Re-diagnose ON DEVICE before building anything — the reverted
   experiment predates the SSR fundamentals rework and the probe.

## Per-material SSR receive mask (2026-07-13)

`ssr_mask` (0..1, PBR-only, default 1.0) controls how strongly a surface
RECEIVES screen-space reflections, decoupled from roughness/metallic:
- The opaque pass bakes **Schlick fresnel × ssr_mask** into the reflection
  descriptor rgb (ssr_pbr_descriptor; wgsl_validation pins it). Fresnel
  lives in the DESCRIPTOR, not the trace — an F0-only mask is invisible at
  grazing where Schlick's unmasked (1-F0) term dominates; baking the
  finished fresnel makes fractional masks damp uniformly at every angle.
- brdf_pbr's `ssr_ibl_keep` reads the SAME masked value, so `ssr_mask 0`
  fully opts out of SSR while keeping IBL specular (an unmasked factor
  black-holed masked mirrors).
- Plumbing: PBR core word 39 (PBR_CORE_WORDS 40) · MaterialDef +
  PbrMaterialColor field · set_builtin_param `ssr_mask` (MCP + editor
  dispatch) · scene_mode inspector "SSR mask" row · animatable
  (BuiltinParamKind/BuiltinMaterialParam::SsrMask, add-track row).
- Zero-cost off: the whole descriptor path (fresnel bake included) sits
  inside the `write_ssr_descriptor` axis.
- Reference use: the jetpack arena floor ships `ssr_mask 0.7`.

## Occluder-shadow diagnosis (2026-07-13, arena platform — for the next SSR round)

David flagged dark smudges on the glossy floor "spreading far" from a
floating platform. Measured decomposition (influence masks + source-view
debug + geometric projection, all on-device):
- The patch near the arena CENTER is the platform's TRUE mirror image
  (mirrored-camera projection of the 7x0.9x7 slab at [0,7,24] lands at
  x 0.4..5.3, z -0.7..6.1) — physically correct, keep.
- The column just BELOW the platform is the classic SSR occluder shadow:
  the floor there mirrors wall content hidden behind the platform's screen
  rect (the true rays pass ~3.5 m UNDER the deck). The BVH fallback fires
  on exactly those pixels and supplies the true content (dim red base
  wall), which is why it's maroon, not black — but its one-ray,
  emissive-only shading of the thin 0.3 m ring tori reads dimmer/patchier
  than the neighboring screen-sampled streaks, so the column still smudges.
- Verdict vs industry: at-or-above shipped non-RT SSR standard; NOT a
  shipping gate. 9. **BVH thin-emitter hit quality** is the follow-up if
  it ever bothers again (few-ray jitter or cone-widened torus acceptance,
  bloom-matched intensity); content-side fix is lighting platform
  undersides.

## Post-sweep state (2026-07-13 evening — ..ea19b12c)

Shipped beyond the roadmap above (details in git history; the standalone
bvh-reflections.md design doc was deleted as fully implemented):
- **Software-BVH reflections** (`ssr.bvh_reflections`, default off): BLAS at
  mesh commit + linear TLAS (revision-gated — zero work per static frame) +
  bvh_trace pass; the trace's miss fallback prefers a real off-screen hit.
  Eligibility spread < 0.25. Open follow-ups: device tiering / editor toggle
  (MCP-only today), morph/skinned exclusion is permanent by design, TLAS
  tree if a scene exceeds a few hundred instances.
- **HDR probe support**: rgb9e5 KTX2 cubemaps load natively; probe content
  must be authored in probe-CENTER space and ENERGY-CONSERVED (see the
  jetpack arena's gen-assets.py for the reference implementation).
- **Zero-cost-off audit**: SSR/temporal/half-res/bvh verified idle-free;
  edge accumulator slots now narrow (16 B) with SSR off (-32 MB at desktop
  budget) — the classify/opaque/final_blend stride is an axis.
- **Atmosphere** extracted as its own plan (atmosphere.md): view-path fog +
  reflection-path haze; replaces the arena's probe-baked haze when it lands.

Still open from the list above — ALL deliberate future tiers, none of them
loose ends of the `updates` branch work: #6 (planar reflections,
content-triggered), #7 (prefiltered scene-color mips, quality/perf tier),
#8 (glass-shell shading aliasing — see the corrected entry; not an MSAA
gap), item #5
tier 2 (multiple local probes + editor authoring UI + in-engine capture),
and atmosphere.md. Bug #1 was the only defect in the queue and is fixed.
