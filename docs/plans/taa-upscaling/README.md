# TAA + temporal upscaling

**Status:** implemented on `feat/taa-upscaling` (phases 0-5); gates: see
[05 Results](05-verification.md#results); not all met. The durable reference
is [`docs/TAA.md`](../../TAA.md).

Temporal anti-aliasing that also upscales: render fewer pixels each frame,
jitter them, and rebuild a full-resolution image from this frame plus the
reprojected history of earlier frames. It replaces MSAA and SMAA when it is on.

This is the largest frame-cost lever the renderer has left. Measured on
Dreidel Derby (M1 Max, Chrome, 1920×1435 output, 0.45.0):

| | GPU per frame |
|---|--:|
| Mid-round, no AA | 10.7 ms |
| Mid-round, MSAA 4× | 14.9 ms |
| Title screen (adds DoF), MSAA 4× | 19.8 ms |

MSAA costs about 4 ms here. Nearly all the rest scales with pixel count:
shading, the shadow prep and blur, SSR. Rendering 67% of the width and height
is 45% of the pixels. Expect the frame well under 16.7 ms with better edges
than MSAA (MSAA does nothing for specular or shading aliasing; TAA does).

Measured after the implementation (renderer `0d878578`, same machine and
canvas, the game's own GPU stats; "unpaced" draws on every 120 Hz refresh,
"paced" is the game's own 60 fps, which lets the GPU clock down between cheap
frames; [05 Results](05-verification.md#results)):

| Mid-round | Paced | Unpaced |
|---|--:|--:|
| No AA | 10.64 ms | 10.74 ms |
| MSAA 4× | 14.75 ms | 14.77 ms |
| TAA native | 12.13 ms | 12.22 ms |
| TAA 0.67 | 8.43 ms | 7.30 ms |
| TAA 0.5 | 8.17 ms | 6.16 ms |

The title screen with MSAA 4× measured 19.35 ms, TAA native 15.43 ms and TAA
0.67 10.28 ms. Converged image error against a 64-sample reference,
mid-round: TAA native 0.873, MSAA 1.864.

## Why there is a plan at all

TAA has been tried here more than once (the forward-renderer version was
removed in `c42b6805`; its jitter is still in `camera/buffer.rs` behind
`APPLY_JITTER = false`) and every attempt ended in ghosting. Ghosting is not
inherent to TAA. It comes from a short list of specific mistakes, and the old
implementation made most of them. [01-why-it-ghosted.md](01-why-it-ghosted.md)
goes through them one by one; the design is built so that each is impossible
or caught by a test.

## Reading order

1. [01-why-it-ghosted.md](01-why-it-ghosted.md): the old attempt, and the
   causes of ghosting in general. Read this first.
2. [02-current-renderer.md](02-current-renderer.md): what exists today that
   TAA builds on or must change (camera history, transforms, skinning, MRT
   budget, the HUD, the other temporal passes).
3. [03-design.md](03-design.md): the algorithm, exactly: jitter, motion
   vectors, history, the resolve shader step by step, upscaling, reactive
   pixels, resets, where it sits in the frame.
4. [04-implementation-plan.md](04-implementation-plan.md): phases, files,
   API, and what each phase must show before the next starts.
5. [05-verification.md](05-verification.md): the test scenes, debug views and
   numbers that decide whether it works. Ghosting is judged by these, not by
   eye on a static scene.

Separate, not part of the phases:

- [06-neural-aa.md](06-neural-aa.md): whether an AI-based AA (like DLAA or
  FSR 4) could work here without vendor hardware, as plain WebGPU compute.
  What exists, what it would cost, and a path that builds on this plan.

## The decisions, in one place

| Question | Decision | Where | As implemented |
|---|---|---|---|
| Jitter | Halton(2,3), ±0.5 render pixel, constant amplitude, 8 phases × upscale² | 03 §1 | as designed; SSR and volumetric jitter indices offset by 7 under TAA |
| Motion vectors | Per-surface, unjittered, from the geometry pass (5th target, `rg16float`) | 03 §2 | as designed |
| Devices at 32 B/sample | Camera + rigid-object motion reconstructed in the prep pass; non-rigid meshes reactive | 03 §2.4 | `rg32float` storage texture (`rg16float` is not a core storage format); non-rigid meshes reactive at 0.5 only while they deform; device test is 36 B/sample **and** 10 storage buffers per stage |
| Previous transforms, skins, morphs | New `prev_*` GPU buffers, copied at the end of each frame | 03 §2.3 | one `prev_state` buffer with three regions (10 storage buffers per stage on the M1); transforms at the full 112 B stride; new / reused ranges fixed up at frame start |
| History | Output resolution, `rgba16float`, alpha = accumulated frame count | 03 §3 | as designed; previous depth is a depth-format copy (`depth32float` cannot copy to `r32float`) |
| History sampling | 5-tap Catmull-Rom | 03 §4.5 | as designed |
| Rejection | Variance clip in YCoCg, in tonemapped space; depth disocclusion test; off-screen reset | 03 §4.6–4.7 | plus a clip-box floor (1.5e-3) and a second, own-surface disocclusion test; both disocclusion distances are previous-frame distances |
| Blend | 1/(n+1), n capped at 16 (TAA) / 24 (upscaling); raised by motion, reactivity, low confidence | 03 §4.8 | upscaling cap = 16 × pixel ratio (35.6 at 0.67, 64 at 0.5); cap shortened to 2 with speed; motion-boundary weight; clip weight 1.0 (not 0.5); still γ widened when upscaling |
| Things without motion vectors | Reactive mask: transparents, particles, UV-flow, the HUD | 03 §5 | transparents 0.3 per unit alpha (not 0.8), reactive materials 0.2 (not 0.6) written by a seed draw; `temporal: Auto / Stable / Reactive`; HUD drawn after the resolve (transparent-pipeline HUD) or passed through (opaque-path HUD) |
| Placement | After SSR composite, before bloom / DoF / effects / display | 03 §6 | as designed; RCAS and the HUD-after-resolve pass follow the resolve |
| MSAA / SMAA | Off while TAA is on (`AntiAliasing::Taa`) | 03 §6 | `AntiAliasing { taa: Option<TaaConfig>, .. }`; MSAA + SMAA kept as a mode; TAA falls back to MSAA 4x if its pipelines fail |
| Texture LOD | Gradients scaled by render/output (negative mip bias) | 03 §7 | via the geometry pass's stored derivatives (a `mip_bias` variant), `mip_bias_extra` 0 by sweep |

## Phases

| Phase | Scope | Gate |
|---|---|---|
| 0 | Debug views, test scenes, reference capture, jitter fixed but off | Scenes and views in place, nothing visible changes |
| 1 | Motion vectors only (camera + objects + skin + morph), with a debug view | Velocity view correct in every test scene |
| 2 | TAA at native resolution | Ghosting tests pass; static scenes converge; MSAA-comparable edges |
| 3 | Reactive mask (transparents, particles, UV flow, HUD) | Transparent and particle tests pass |
| 4 | Upscaling (render scale < 1) + mip bias + optional sharpening | Quality tests pass at 0.67; frame-time target met |
| 5 | Fallback for 32 B/sample devices; resolution-governor integration | Mobile path works; governor changes scale without history resets |

Delete this folder when phase 5 ships, as with the other plans.
