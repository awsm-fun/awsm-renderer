# SSR / reflections — follow-ups

Queue agreed with David (2026-07-12) after the SSR fundamentals rework
(bilinear depth, crossing acceptance, per-sample descriptor resolve,
prefiltered-env fallback, SSR↔IBL crossfade). Reference architecture:
the third-party reflection plan (`~/Downloads/awsmrenderer-reflection-plan.md`)
— SSR as a confidence-weighted contribution over layered fallbacks; no
camera jitter.

## Bugs

1. **MCP screenshot readback colorspace** — `screenshot_scene`'s GPU
   swapchain copy produces a pastel/lifted-shadows image vs the on-screen
   render (obvious on dark HDR scenes: the arena; invisible on bright flat
   scenes, which is why goldens never caught it). Likely a missing/double
   sRGB transfer in the copy→PNG path. Until fixed, dark-scene previews are
   captured via the browser (canvas crop) instead — the arena previews
   currently carry minor editor chrome for this reason. Goldens captured via
   the readback are self-consistent (compared against each other), but not
   display-accurate.

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
5. **Local reflection probes** (content-triggered) — box-projected cubemap
   probes + editor authoring. Build when a scene with interiors/occluded
   reflections exists; open arenas gain little.
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
