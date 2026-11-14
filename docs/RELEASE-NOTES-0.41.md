# Release notes — 0.41

0.41.0 is two things:

1. **A change of default:** opaque PBR materials no longer get one shading
   kernel per exact feature set. Materials that differ only in features that
   are free to share now share a kernel, so a cold load compiles fewer of
   them. What a committed scene renders does not change, and neither does
   what a frame costs.
2. **Everything that was prepared as 0.40.1 and never published:** a faster
   cold load, ETC1S normal maps on ETC2-only GPUs, a transcoder half the
   size, two pipeline leaks fixed. If you are on 0.40.0, all of it is new to
   you; it is listed under "Also in this release".

It is a minor version rather than a 0.40 patch because the first item amends
the "specialize-only" rule the renderer has documented since the materials
overhaul (one kernel per exact feature set), and every app picks it up
without asking. `PbrSpecialization::EveryFeature` on the builder is the old
behaviour, one line away.

## BEHAVIOUR: a PBR kernel per combination of the features that are worth one

Until 0.40 every distinct `PbrFeatures` set among a scene's opaque PBR
materials got its own bucket and its own kernel. A kernel is ~240 KB of WGSL
and compiles at roughly two seconds per megabyte, cold. The features are a
small part of that text — all thirteen add 20.7 KB between them — so a kernel
per combination costs a load far more than it saves.

Sharing a kernel means compiling a feature into a kernel whose materials may
not use it. That is only free if the unused code costs nothing to RUN, so
each small feature was measured that way: compiled into every kernel of a
scene none of whose materials has it, GPU timestamp queries on the opaque
pass (`ssr-arena`, 1080p, Apple M1; the pass takes 2.18 ms with a kernel per
exact feature set).

| Feature | WGSL it adds | Opaque pass when carried unused | In 0.41 |
|---|---|---|---|
| IOR | 0 bytes | 2.19 ms | in every opaque PBR kernel |
| emissive strength | 66 | 2.04 ms (one kernel where there were two) | in every opaque PBR kernel |
| vertex colour | 385 | 2.22 ms | in every opaque PBR kernel |
| dispersion | 7 | 2.18 ms | its own kernel |
| transmission | 16 | 2.45 ms, +12 % | its own kernel |
| volume | 47 | 2.46 ms, +12 % | its own kernel |
| specular | 19 | 2.49 ms, +14 % | its own kernel |
| all seven | 540 | 3.58 ms, +64 % | — |
| diffuse transmission | 1,527 | not neutral or not cheap: see "Not in this release" | its own kernel |
| anisotropy | 2,853 | | its own kernel |
| clearcoat | 2,935 | | its own kernel |
| iridescence | 3,144 | | its own kernel |
| sheen | 3,414 | | its own kernel |
| secondary maps | 6,309 | | its own kernel |

Bytes are not cost: the seven smallest features are 540 bytes together and,
compiled into every kernel, make the opaque pass 64 % slower on that scene
(12 % of the frame). The code is not executed, but it is no longer dead.

So, by default (`PbrSpecialization::ExpensiveFeatures`), **IOR, emissive
strength and vertex colour are compiled into every opaque PBR kernel**, and
the other ten features still get a kernel each. A material's payload already
carries an index per extension, and the loaders return the glTF-absent value
when it is 0 (IOR 1.5, emissive strength 1); the vertex-colour multiply is
skipped when the material has none. Sharing them needed no shader change.

A scene compiles one opaque PBR kernel per combination of those ten features
that its materials use. DANCE-OFF had five PBR kernels that differed by an
emissive strength, a specular factor and a clearcoat; it now has three.

Two other policies are selectable with
`AwsmRendererBuilder::with_pbr_specialization`:

- **`LobesOnly`** shares specular, transmission, volume and dispersion too:
  one kernel per combination of the six lobes (DANCE-OFF: two). The image is
  the same; the frame is not — every opaque PBR pixel runs the unused code.
  For an app whose cold-load time matters more than its frame time.
- **`EveryFeature`** is 0.40: one kernel per exact feature set.

The transparent pass is untouched by all three: each transparent material
still compiles a pipeline for exactly its own features.

Things that look different under the default:

- **Fewer buckets, fewer pipelines, different kernel text.** Every opaque
  PBR kernel is 451 bytes longer; a scene whose materials differ in the
  shared features has fewer of them.
  `dynamic_materials.bucket_entries_cached()` is shorter, the bucket cap
  (`BucketConfig`, default 32) has more headroom, and
  `first_party_variant_of(id)` reports the KERNEL's feature set (the
  material's own plus the shared three), not the material's.
- **IOR, emissive strength or vertex colour gained on a running scene shows
  on the next frame.** `update_material` that gives a committed material one
  of them takes effect at once: the kernel already has the code
  (player-tests checks it with IOR). In 0.40 the material kept its old
  kernel until the next `commit_load`. That edit no longer changes the
  bucket set either, so the commit after it compiles nothing. (Any other
  feature gained still needs the commit, and still adds a bucket.)

### Cold-load numbers

M1 Max, Chrome 157, Metal, cold GPU caches (fresh profile, never-used site),
alternating runs, medians. Time from navigation to the first frame.

| Load | MSAA | `EveryFeature` (= 0.40.1) | **default** | `LobesOnly` | PBR kernels | WGSL compiled |
|---|---|---|---|---|---|---|
| DANCE-OFF stage (player), 6 runs | on | 8.22 s | **6.75 s** | 6.36 s | 5 / 3 / 2 | 2.65 / 2.18 / 1.94 MB |
| DANCE-OFF stage (player), 6 runs | off | 7.34 s | **6.35 s** | 6.05 s | 5 / 3 / 2 | 2.66 / 2.25 / 2.03 MB |
| `pbr-extensions`, 6 runs | on | 8.71 s | **8.48 s** | 7.96 s | 8 / 7 / 6 | 3.34 / 3.11 / 2.88 MB |
| `pbr-extensions`, 6 runs | off | 7.60 s | **7.24 s** | 6.88 s | 8 / 7 / 6 | 2.99 / 2.79 / 2.59 MB |
| `ssr-arena`, 6 runs | on | 6.20 s | **5.89 s** | 5.73 s | 2 / 1 / 1 | 1.69 / 1.46 / 1.46 MB |
| `ssr-arena`, 6 runs | off | 5.80 s | **5.27 s** | 5.35 s | 2 / 1 / 1 | 1.50 / 1.30 / 1.30 MB |
| `kitchen-sink`, 3 runs | on | 5.77 s | **5.80 s** | 5.79 s | 1 | +451 B |
| `kitchen-sink`, 3 runs | off | 5.32 s | **5.32 s** | 5.30 s | 1 | +451 B |
| `alpha-cutoff`, 3 runs | on | 5.46 s | **5.41 s** | 5.29 s | 1 | +451 B |
| `alpha-cutoff`, 3 runs | off | 4.79 s | **4.89 s** | 4.78 s | 1 | +451 B |
| `contact-shadows`, 3 runs | on | 4.00 s | **4.02 s** | 4.11 s | 1 | +451 B |
| `contact-shadows`, 3 runs | off | 3.63 s | **3.67 s** | 3.66 s | 1 | +451 B |

The test scenes are one build under its three policies. DANCE-OFF is the real
player, which has no policy switch, so its three columns are three builds of
it: against the 0.40.1 candidate, against this release, and against this
release's predecessor on the branch, which compiled the kernels `LobesOnly`
compiles. The player turns MSAA off by default on a touch device.

DANCE-OFF run by run, MSAA on: 8.25 / 8.10 / 8.24 / 8.32 / 8.02 / 8.20 s
before; 6.75 / 6.70 / 6.70 / 6.83 / 6.76 / 6.85 s now. A scene with a single
PBR feature set gains nothing and loses nothing measurable: the six rows at
the bottom move by 0.1 s either way, inside their own run-to-run spread.
(The machine was busier than for the 0.40 notes; compare columns, not
releases.)

### Frame cost

Not a phone: an Apple M1 Max, Chrome 157, Metal. WebGPU `timestamp-query` on
every pass, unquantized (`--enable-webgpu-developer-features`; Chrome
otherwise rounds timestamps to 2^16 ns). Frames run back to back — each
starts when the GPU has finished the last one — so the frame interval is CPU
plus GPU time rather than vsync. Six alternating rounds of ~4 s each;
medians. "Opaque pass" is the one compute pass that runs every bucket's
kernel.

| Scene | Resolution | Opaque pass: `EveryFeature` / default / `LobesOnly` | Frame: `EveryFeature` / default / `LobesOnly` |
|---|---|---|---|
| `pbr-extensions` | 1920×1080 | 1.69 / **1.61** / 1.95 ms | 9.40 / **9.20** / 9.60 ms |
| `pbr-extensions` | 3840×2160 | 3.58 / **3.57** / 4.78 ms | 22.9 / **22.9** / 24.1 ms |
| `lights-many` | 1920×1080 | 1.34 / **1.31** / 1.69 ms | 9.70 / **9.60** / 9.65 ms |
| `lights-many` | 3840×2160 | 2.83 / **2.91** / 4.04 ms | 18.9 / **19.0** / 20.4 ms |
| `ssr-arena` | 1920×1080 | 2.15 / **2.14** / 3.61 ms | 11.2 / **11.2** / 12.7 ms |
| `ssr-arena` | 3840×2160 | 6.36 / **6.27** / 9.62 ms | 31.3 / **31.2** / 34.9 ms |
| `kitchen-sink` | 1920×1080 | 0.91 / **0.95** / 1.27 ms | 8.30 / **8.70** / 8.90 ms |
| `kitchen-sink` | 3840×2160 | 2.79 / **2.90** / 3.95 ms | 20.2 / **20.4** / 21.5 ms |

The default against `EveryFeature`: the opaque pass moves by −0.09 to +0.11 ms,
which is −0.8 to +0.5 % of the frame. `LobesOnly` against `EveryFeature`: the
opaque pass is 15–68 % longer (+0.26 to +3.26 ms), 2.7–13 % of the frame.

The DANCE-OFF player, measured the same way inside its own page at 1920×1080
(the loaded stage, three builds as in the cold-load table, six rounds): the
frame takes 37.8 / **35.5** / 43.8 ms and the opaque pass 9.4 / **9.4** /
15.8 ms. The default costs nothing there either; `LobesOnly` costs 16 % of
the frame.

Read across: the default costs nothing measurable against a kernel per
feature set. Its opaque pass is at most 4 % slower (on the scenes where
nothing is merged) and faster where kernels are merged — fewer kernels means
fewer dispatches over the same tiles. The frame interval is noisier than the
pass and depends on which arm of a round runs first: `kitchen-sink` at 1080p
reads 0.4 ms higher for the default in the table, and 0.2 ms LOWER in eight
more rounds run in the opposite order (8.35 against 8.55 ms).
`LobesOnly` — which this release was going to ship as its default until it
was measured — costs 3–13 % of the frame.

### Rendering

Checked target by target, not screenshot by screenshot: the test harness is
frozen after exactly 120 rendered frames and every live render target is
read back (the HDR colour targets as half floats) and compared byte for
byte.

- **All three policies produce bit-identical targets in all 23 single-commit
  test scenes**, and so do the 0.40.1 candidate and this release. That
  includes the HDR opaque target, the SSR descriptor, trace, resolve and
  history, bloom and the composite. (One scene, `lights-many`, differs from
  ITSELF between two runs of one build in its HDR targets; its 8-bit image
  is identical every time.)
- Both `player-cameras` goldens are unchanged.
- `player-tests` builds a scene of sixteen PBR materials with fifteen feature
  sets — bare, each of the seven small features, combinations, clearcoat at
  full strength and at factor 0, iridescence, sheen, secondary maps —
  renders it under all three policies and compares the frames: identical,
  0 of 480,000 pixels, with and without MSAA.

(A screenshot is the wrong tool for this comparison. SSR's march jitter
rotates every frame, so two captures of ONE build taken a frame apart differ
by 1/255 in about 0.4 % of `ssr-arena`'s pixels; compare builds on different
frames and that noise looks like a difference between them.)

One caveat, stated rather than hidden: a kernel without IOR has the literal
1.5 and the driver folds the Fresnel F0 at compile time, while a kernel with
IOR divides at runtime. Those agree to the last bit on Metal (above). WGSL
does not promise it on every driver, where the difference could be one unit
in the last place of F0.

### What is not measured

- **Phones.** Every number here is from one desktop-class Apple GPU. The
  default policy was chosen to cost nothing where it was measured;
  `LobesOnly`'s cost is likely to be at least as visible on a tile-based
  mobile GPU, and has not been measured there.
- **Other backends.** Bit-identical output was established on Metal through
  Chrome/Dawn. D3D12 and Vulkan have different shader compilers.

## Also in this release (prepared as 0.40.1, never published)

Two things behave differently from 0.40.0 and are worth knowing:

- **Every cold load now compiles three small texture-upload helper
  pipelines, used or not** (sRGB conversion and two mipmap variants for plain
  8-bit RGBA). They are started inside the load's batch so that a texture
  uploaded after the load — a HUD, a marquee, any `add_image_rgba_raw` —
  finds them ready instead of waiting ~0.2 s for them. A load that never
  uploads such a texture pays for one or two small extra compiles.
- **An SSR composite the driver rejects before the first commit now turns
  SSR off at the commit.** In 0.40.0 the composite was compiled and awaited
  by `set_post_processing`, which returned the error (the scene loader logged
  it and went on with `ssr.enabled` still true and no pass). It is now issued
  there and collected by the commit, where it is handled like any other
  rejected pipeline of the SSR pass: SSR off in `renderer.post_processing`,
  with a warning. On a committed scene the setter still returns the error.

And the rest:

- **Nothing is compiled and awaited ahead of a load's batch any more.** The
  two blit pipelines (awaited in `build()`), the SSR composite (awaited in
  `set_post_processing`), the default BRDF LUT (awaited at the top of the
  commit) and the sRGB / mipmap upload helpers (awaited by the first upload
  that needed them) are now started early and collected later. DANCE-OFF
  cold: the batch is issued at 0.60 s instead of 0.96 s and nothing compiles
  after the last kernel — about 0.7 s off the load.
- **ETC1S normal maps load on ETC2-only GPUs** (most phones). The transcoder
  is rebuilt with ETC1S → EAC RG11 enabled; upstream's web build has it off,
  so such a normal map failed to transcode and the surface rendered flat.
  `basis_transcoder.js` and `basis-worker.js` are unchanged, and every other
  target's output is byte-identical. **If you copy the vendored transcoder
  into your own app, copy the new `.wasm`.** UASTC normals were never
  affected.
- **The transcoder is half the size, and a KTX2 it cannot read is refused up
  front.** `basis_transcoder.wasm` is built without the formats the renderer
  never uses (UASTC HDR, XUASTC LDR, PVRTC1): 1,056,521 → 552,629 bytes
  against 0.40.0, about 150 KB less over the wire with brotli (360 KB →
  210 KB) on every cold 3D view. Every ETC1S / UASTC LDR source transcodes
  to byte-identical output. `sniff_basis_ktx2` now returns `None` for any
  supercompression scheme other than none, BasisLZ or Zstandard. A XUASTC
  LDR file (scheme 5) — which the 0.40.0 transcoder accepted, although
  glTF's `KHR_texture_basisu` does not allow it — is therefore refused up
  front as "not a Basis KTX2 this build can transcode" rather than reaching
  a transcoder that no longer reads it. `web/vendor/basis/README.md` says
  how to build those formats back in.
- **Toggling MSAA or SMAA no longer leaks render pipelines.** On a scene with
  a feature-set variant or a custom material, every bucket re-layout evicted
  every transparent shader and rebuilt its pipeline under a new key, leaving
  the old one in the pool: one leaked pipeline (and one needless recompile)
  per transparent mesh, per toggle. The editor showed it as +6 render
  pipelines per MSAA toggle.
- **Switching MSAA before the first commit compiles one mode, not two.** The
  geometry pipelines, HZB seed and classify pipeline reserved for the mode
  the renderer was built with are released when it is switched while
  loading.

## Bug fixes

- **A material, or the sky, that covers more than 4.2 megapixels is shaded.**
  The opaque kernels are dispatched indirectly, one workgroup per 8×8 tile of
  their bucket, and WebGPU silently skips an indirect dispatch whose count
  exceeds `maxComputeWorkgroupsPerDimension` (65,535). A bucket that covered
  more than 65,535 tiles was therefore not shaded at all — black sky, black
  geometry — on a 4K canvas, or any canvas where one bucket fills more than
  4.2 megapixels. Present in 0.40 (a sky-heavy scene at 3840×2160 has no
  sky), and sharing kernels makes it much easier to hit, because the fewer
  kernels a scene has the more tiles each one covers: at 3840×2160 DANCE-OFF
  kept its opaque geometry with five PBR kernels and lost it with three. The
  dispatch is now laid out in rows of 32,768 tiles, so a bucket can cover any
  canvas. Canvases up to 1920×1080 dispatch exactly as before.
  (`large-canvas-bucket-is-shaded` in player-tests.)
- **A cancelled commit no longer turns SSR off.** The commit took the SSR
  composite's pending compile out of the pass before awaiting it. An app
  that dropped the `commit_load` future at that point (a load raced against
  a cancel) left the composite marked failed, and the next commit turned SSR
  off over a pipeline the driver had never rejected. The compile is now
  awaited in place. (`ssr-composite-survives-a-cancelled-commit` in
  player-tests.)
- **A default BRDF LUT that fails to generate no longer fails a commit that
  does not need it.** If content installed its own LUT while the default was
  still generating, the default's error is dropped along with the default.

## API

New, nothing removed:

- `awsm_renderer_materials::pbr::PbrSpecialization` — `ExpensiveFeatures`
  (default), `LobesOnly`, `EveryFeature`; `always_compiled()`,
  `kernel_features(material)`. `#[non_exhaustive]`.
- `PbrFeatures::cheap()`, `neutral()`, `union()`.
- `AwsmRendererBuilder::with_pbr_specialization(PbrSpecialization)`.
  Build-time only; `remove_all` keeps it.
- `DynamicMaterials::pbr_specialization()`.
- `awsm-renderer-core`: `TexturePool::upload_pipeline_needs`,
  `UploadPipelineNeeds::compile_in_background`.

```rust
// Fewest kernels, at a cost per frame:
let renderer = AwsmRendererBuilder::new(gpu)
    .with_pbr_specialization(PbrSpecialization::LobesOnly)
    .build()
    .await?;
```

## Upgrading

From 0.40.0, nothing is required. Worth a look:

- If you assert on bucket or pipeline counts, expect fewer on scenes whose
  PBR materials differ in IOR, emissive strength or vertex colour.
- If your UI mirrors `renderer.post_processing`, read it back after a load:
  a rejected SSR composite now turns SSR off at the commit.
- If you ship your own copy of the vendored Basis transcoder, copy the new
  `.wasm`.
- If you maintain the PBR templates, read "Specialize-only materials" in
  `docs/SHADER_GUIDELINES.md`: code behind the gate of an always-compiled
  feature runs for every opaque PBR pixel and must be neutral and free for
  a material without the feature.

## Not in this release

**Sharing the lobes** (clearcoat, sheen, iridescence, diffuse transmission)
would take DANCE-OFF to one PBR kernel. It was measured and does not hold
up:

- Ungated, every PBR pixel runs the lobes with zero weight: +7 to +29 % of
  frame time on the M1 on top of `LobesOnly`, the opaque pass roughly
  doubling.
- Gated at runtime on the material having the lobe, clearcoat and
  iridescence still cost +2.5 to +11 %, and the image is no longer
  bit-identical: the branch makes Metal's fast-math compiler order the
  surrounding sums differently, a handful of HDR values per frame move by one
  half-float step, and now and then one pixel of the final image moves by
  1/255. Compiled with strict math the two are bit-identical, which is how
  the cause was pinned down; strict math is a Chrome developer feature, not
  something a shipped page can ask for.
- Sheen and diffuse transmission behave the same way when gated;
  anisotropy's lobe replaces the isotropic one outright (90 pixels, up to
  162/255, on materials without it).

**Specular** is the one that might still join the shared set: its unused path
is neutral, and most of its cost is an F0 / f90 that is recomputed per light
once specular is no longer a compile-time constant. Computing them once per
pixel would probably make it free, and DANCE-OFF would then compile two PBR
kernels at no frame cost. Not attempted here.
