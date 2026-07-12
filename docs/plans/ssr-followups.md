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

2. **Debug views** — SSR hit/miss, confidence, hit distance, traversal
   steps, reflection-source selection. Cheap; would have cut this debugging
   saga to a fraction. Template debug axis on trace + composite.
3. **Confidence-weighted composition** — formalize `hit_conf` × edge fade ×
   travel fade as the SSR confidence output that blends SSR over the
   fallback stack (env now, probes later), instead of being baked into the
   trace's own env mix. The socket probes plug into.
4. **Perf check** — `?trace=sub-frame` on the arena vs the plan's ~1–2 ms
   budget; bilinear sampling + refine added depth loads that were never
   measured.
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
