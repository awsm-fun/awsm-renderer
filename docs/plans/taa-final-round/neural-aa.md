# Neural AA without special hardware (investigation, future)

**Status:** research only (2026-11-23). Out of scope for the final TAA round; kept for a later investigation. Classic TAA shipped in 0.46.

The question: can awsm-renderer get an AI-based anti-aliasing (or upscaling)
like DLAA or FSR's ML modes, **without depending on vendor hardware**? That
means a small neural network run as ordinary WebGPU compute next to the scene.

**Short answer:** technically yes, but nothing open exists that can be dropped
in. At native resolution the published networks cost more than our whole
frame on an M1 Max. A network about 10× smaller, run at reduced resolution and
used to *steer* the classic TAA resolve rather than replace it, is a realistic
experiment once classic TAA ships.

Vendor facts below move quickly. The FSR 4.1 and XeSS 3 items come from
mid-2026 reporting. Each claim has its source; anything marked *unverified*
couldn't be confirmed.

## What the named products are

- **FSR "Native AA"**: AMD's temporal upscaler (FSR 2/3) run at 1.0×. It is
  an official FSR 3 quality mode ("pure anti-aliasing", "the largest
  performance overhead"), and it still needs reactive and transparency masks.
  **It is not neural:** FSR 2/3 are hand-written shaders, MIT licensed, with
  DX12 / Vulkan backends.
  ([FSR 3](https://gpuopen.com/fsr3/),
  [FSR2 repo](https://github.com/GPUOpen-Effects/FidelityFX-FSR2))
  In other words, FSR Native AA is the same kind of thing as the classic TAA in `docs/TAA.md`.
- **FSR 4 / "Redstone"** is ML-based. It launched requiring RDNA 4 (FP8); an
  INT8 "FSR 4.1" for RDNA 3 followed in mid-2026. AMD-only, shipped as a
  driver / DLL. Source including INT8 models was briefly published under MIT
  by mistake in August 2025 and withdrawn. Legally murky; don't use it.
  ([GPUOpen](https://gpuopen.com/learn/amd-fsr-redstone-developers-neural-rendering/),
  [Tom's Hardware, 4.1](https://www.tomshardware.com/pc-components/gpu-drivers/amd-brings-official-fsr-4-1-support-to-rx-7000-series-gpus-int8-model-now-available-in-300-games-rdna-3-apus-also-getting-fsr-4-1-soon),
  [Tom's Hardware, leak](https://www.tomshardware.com/pc-components/gpus/amd-accidentally-marks-fsr-4-open-source-source-code-reveals-potential-support-for-older-radeon-gpus))
- **DLAA**: DLSS run at native resolution. NVIDIA Tensor Cores only, closed.
  ([NVIDIA](https://developer.nvidia.com/rtx/dlss))

## The landscape

| Option | Neural | Hardware | Source / licence | Usable from WebGPU |
|---|---|---|---|---|
| FSR 2/3 incl. Native AA | no | any | MIT | as a hand port to WGSL (none exists) |
| Arm ASR (from FSR 2.2.2, mobile-tuned) | no | any | MIT ([repo](https://github.com/arm/accuracy-super-resolution)) | as a hand port; the best classic reference |
| Snapdragon GSR 2 (temporal) | no | any | BSD-3 per a mirror, *unverified* ([repo](https://github.com/SnapdragonGameStudios/snapdragon-gsr)) | as a hand port |
| FSR 4 / 4.1 | yes | AMD RDNA 4 / 3 | DLL | no |
| DLAA / DLSS | yes | NVIDIA Tensor Cores | closed | no |
| XeSS (DP4a path runs on other GPUs) | yes | any SM 6.4 GPU, on Windows | binaries; licence reportedly bars modification, *unverified* ([repo](https://github.com/intel/xess)) | no |
| MetalFX temporal | yes (Apple calls it ML-based) | Apple | closed ([WWDC25](https://developer.apple.com/videos/play/wwdc2025/211/)) | no |
| **Arm Neural Super Sampling** (2025) | yes: 4-level UNet, ~10 GOPs at 540p→1080p | runs on Vulkan ML extensions or Arm's emulation layer | **weights open** (Arm AI Model Community License, terms *unverified*); training code Apache-2.0 ([weights](https://huggingface.co/Arm/neural-super-sampling), [Model Gym](https://github.com/arm/neural-graphics-model-gym), [how it works](https://developer.arm.com/community/arm-community-blogs/b/mobile-graphics-and-gaming-blog/posts/how-arm-neural-super-sampling-works)) | the weights, if inference is rewritten in WGSL |
| KIT "Minimal CNN TAA" (HPG 2023) | yes: 4 conv layers, predicts kernels, recurrent | any | paper says code is provided; repo *not found* ([paper](https://cg.ivd.kit.edu/publications/2023/neuraltaa/2023_neuraltaa.pdf)) | if reimplemented |
| Meta NSRR (SIGGRAPH 2020) | yes | any | no code / weights found; patented ([US11367165](https://patents.google.com/patent/US11367165)) | avoid |
| Qualcomm neural supersampling (ICCV 2023) | yes | any | dataset only (research use) ([paper](https://arxiv.org/abs/2308.01483)) | if reimplemented |
| Anime4K-style CNN shaders | yes, single image | any | MIT; already on WebGPU ([anime4k-webgpu](https://npmjs.com/package/anime4k-webgpu), [WebSR](https://github.com/sb2702/websr)) | yes, but not temporal, so no TAA replacement |

No neural *temporal* AA or upscaler running in WebGPU or WebGL was found.

## Running a network in the browser

- **A runtime (onnxruntime-web, WebGPU EP).** It can take our own
  `GPUDevice`, and input / output GPU buffers, without readback
  ([docs](https://onnxruntime.ai/docs/tutorials/web/ep-webgpu.html)).
  But inputs are storage buffers, not textures, it runs from JS, and getting
  our device out of the Rust renderer into it is *unverified*. It's useful for
  trying a trained model quickly, not for shipping inside the frame.
- **LiteRT.js** falls back to CPU for a whole graph if one op is unsupported.
  **WebNN** was still unstable in Chrome origin trials during 2026. Don't
  depend on either.
- **Hand-written WGSL** is the practical route for a network of about five
  conv layers. It reads our textures directly, fuses layers, and follows the
  0.45 lessons (no indexed private arrays, etc.).
- **WebGPU features that help without special hardware:** `shader-f16`
  (Chrome 120) and `subgroups` (Chrome 134; Google Meet measured 2.3–2.9× on
  matrix-vector shaders). Subgroup-matrix / cooperative-matrix support is
  still experimental, at about 0.04% reach.
  ([Chrome 134](https://developer.chrome.com/blog/new-in-webgpu-134),
  [Dawn](https://dawn.googlesource.com/dawn/+/refs/heads/main/docs/dawn/features/subgroup_matrix.md))
- **f16 is not a 2× speedup on Apple M1.** FP16 and FP32 throughput are about
  equal there (~10.6 TFLOPS on M1 Max). f16 helps register pressure and
  bandwidth only.
  ([metal-benchmarks](https://github.com/philipturner/metal-benchmarks))

## Cost

Published numbers:

| Network | Published cost |
|---|---|
| Qualcomm ICCV 2023, 1080p out, RTX 3090, TensorRT FP16 | 1.1 ms (small), 3.5 ms (large); NSRR on the same setup ~13.9 ms |
| Arm NSS, 540p → 1080p | ~10 GOPs; target ≤ 4 ms on mobile neural accelerators |
| KIT Minimal CNN TAA | ~40k FLOPs per pixel; < 3 ms in TensorFlow FP32 on an RTX 3060 |

**Estimates** for 1920×1435 on an M1 Max, assuming hand-written WGSL reaches
20–40% of peak (2–4 effective TFLOPS; the assumption itself is *unverified*):

| | Work | Time |
|---|--:|--:|
| KIT-class at native resolution | 52–116 GFLOP | 13–58 ms |
| NSS-class at native resolution (pure AA) | ~53 GOP | 13–27 ms |
| NSS-class upscaling 960×718 → 1920×1435 | ~13 GOP | 3–7 ms |

Our whole frame is about 10–11 ms. A budget of around 1.5 ms for the network
allows 3–6 GFLOP: about 1–2k FLOPs per output pixel, or 4–8k per pixel if it
runs at half resolution per axis. That is **roughly 10× smaller than any
published network**. So:

- **A neural replacement for TAA at native resolution doesn't fit.** That is
  what DLAA is, and DLAA can afford it because Tensor Cores run it outside
  the shader ALUs.
- **A tiny network that steers the classic resolve might.** It wouldn't
  produce pixels. Per pixel, at reduced resolution, it would predict the few
  numbers the resolve (`docs/TAA.md` §3.5) tunes by hand: the blend weight, how hard to clip, maybe a
  small filter. The classic resolve then does the per-pixel work at full
  resolution. This is close to what the KIT paper and Arm NSS do (they predict
  kernels and parameters, not colours), but much smaller.

## Training data: we can make our own

Every published method trains on jittered 1-sample-per-pixel frames plus
motion vectors and depth, against a supersampled reference:

- Arm NSS: 540p inputs, 1080p 16-spp references, ~100-frame sequences.
- Qualcomm: Halton(2,3) jitter, 16 phases, 8,760 frames over 13 scenes.
- KIT: 3 scenes, 16-spp references.

The TAA plan already needs most of this. 05 §3's reference capture (time
frozen, 64 jittered frames averaged) plus the deterministic `player-tests`
playback *is* a training-data generator. It needs one addition: dump the
1-spp colour, motion vectors, depth and jitter for each frame. Animation and
particles must be deterministic, which the harness already requires.

Training cost isn't stated by any source. A network under 200k parameters
plausibly trains in hours to a day on one consumer GPU (*unverified*). Arm's
Model Gym (Apache-2.0, needs CUDA) is a ready-made starting point.

## Recommended path

1. **Ship classic TAA** (done in 0.46). Everything below depends on it: motion
   vectors, the reactive mask, history, the reference capture and the tests.
   For the resolve, Arm ASR and FSR 2 are the MIT-licensed references worth
   reading. Their structure matches the resolve (`docs/TAA.md` §3.5).
2. **Add a capture mode** that writes training data: inputs and the
   supersampled reference, from the test scenes and from games (Dreidel Derby
   is a good one: fast rotation and sharp highlights).
3. **Experiment offline:** train a tiny recurrent parameter-prediction network
   in PyTorch (Model Gym or the KIT design as templates) that outputs the
   blend weight, clip strength and possibly a 3×3 kernel at half resolution.
   Judge it with the 05 §4 metrics against the hand-tuned resolve. Only if it
   wins clearly on ghosting *and* flicker, go on.
4. **Hand-write the inference in WGSL** (f16 storage, subgroups where
   available), behind a flag, with classic TAA as the fallback and the
   default.

## Open questions

- Arm AI Model Community License: may the NSS weights ship in a commercial
  product? (Even if yes, NSS is too big for us as is. It is more useful as a
  design reference and a pretrained starting point.)
- Where the KIT Minimal CNN TAA code is hosted.
- Real conv-layer efficiency of hand-written WGSL on M1 / Chrome. Measure a
  single 3×3 conv layer at 16 channels before committing to anything.
- Whether the renderer's `GPUDevice` can be shared with onnxruntime-web for
  quick experiments.
