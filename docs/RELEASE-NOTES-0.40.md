# Release notes — 0.40.x

0.40 is about the cold first view: what a renderer compiles the first time a
scene loads on a device, and in what order. Nothing about how a committed
scene renders is meant to change — single-commit renders of the test scenes
are pixel-identical to 0.39 — but *when* pipelines compile has changed, a
handful of pool APIs changed shape, and several bugs found on the way are
fixed.

## 0.40.1

A patch release: no behaviour or API changes for an embedder, a faster cold
load, one device-class bug and one leak fixed.

- **Nothing is compiled and awaited ahead of a load's batch any more.** The
  two blit pipelines (awaited in `build()`), the SSR composite (awaited in
  `set_post_processing`), the default BRDF LUT (awaited at the top of the
  commit) and the sRGB / mipmap upload helpers (awaited by the first upload
  that needed them) are now started early and collected later. DANCE-OFF
  cold: the batch is issued at 0.60 s instead of 0.96 s and nothing compiles
  after the last kernel — about 0.7 s off the load (7.8 s against 8.5 s,
  alternating runs). A scene whose SSR composite is rejected by the driver
  loads with SSR off, like any other pipeline of that pass.
- **ETC1S normal maps load on ETC2-only GPUs** (most phones). The transcoder
  is rebuilt with ETC1S → EAC RG11 enabled; upstream's web build has it off,
  so such a normal map failed to transcode and the surface rendered flat.
  `basis_transcoder.wasm` grows by 5,130 bytes (+0.6 KB brotli);
  `basis_transcoder.js` and `basis-worker.js` are unchanged, and every other
  target's output is byte-identical. **If you copy the vendored transcoder
  into your own app, copy the new `.wasm`.** UASTC normals were never
  affected.
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

New in `awsm-renderer-core`: `TexturePool::upload_pipeline_needs` and
`UploadPipelineNeeds::compile_in_background`. Nothing removed or changed.

## Cold-load numbers

M1 Max, Chrome 157, Metal, cold GPU caches (fresh profile, never-used site).
Time from navigation to the load returning.

| Scene | 0.39 | 0.40 | pipelines compiled |
|---|---|---|---|
| DANCE-OFF stage (player) | 13.5 s | 8.4 s | 117 → 54 |
| `contact-shadows` | 8.6 s | 4.0 s | 95 → 41 |
| `alpha-cutoff` | 9.8 s | 5.4 s | 97 → 53 |
| `kitchen-sink` | 10.4 s | 5.9 s | 100 → 45 |
| `ssr-arena` | 10.4 s | 6.5 s | 107 → 64 |
| `pbr-extensions` | 13.6 s | 8.7 s | 109 → 52 |

Shader source compiled for the DANCE-OFF load: 5.34 MB → 2.65 MB. Warm loads
are unchanged (about 0.2 s).

What is left is the browser's compiler working through one batch; it is
throughput-bound, so the remaining lever is less shader text (the per-feature
PBR kernels are large and mostly identical), not ordering. One known wobble:
with many large kernels in a batch, one of them sometimes resolves after
everything else and adds about half a second (DANCE-OFF: 7.7–8.6 s run to
run). It is the browser's scheduling of an identical batch; see
`docs/time-to-first-render.md`.

## BEHAVIOUR: pipelines are reserved until the first commit

Until a scene is committed nothing is drawn, so nothing needs its compile to
have finished. Everything set up before the first `commit_load` — lights and
shadows, `set_post_processing`, `set_anti_aliasing`, `set_shadows_config`,
content — now only **reserves** its pipelines, and the commit compiles the
whole load as one concurrent batch together with the material kernels. In
0.39 each of those awaited its own compile in turn, with the kernels last.
`begin_load()` puts a running renderer back into this mode for a full reload.

What that means for an embedder:

- **Set everything up, then commit.** That is the fast path. Awaiting a
  setter before the commit no longer means its pipelines exist.
- **Your own pipelines too.** A key from `renderer.pipelines.compute` /
  `.render` (`get_key`, `ensure_keys`) obtained before the first commit or
  after `begin_load()` is reserved: `get(key)` reports `NotFound` until the
  commit. On a committed scene these calls compile and await as before.
- **Compile errors before the first commit surface from `commit_load`.** The
  setter still creates and validates its shader modules (a WGSL error is
  still the setter's error), but a pipeline the driver rejects is reported
  by the commit. On a committed scene the setter returns it, as in 0.39.
- **`ensure_config_pipelines()` no longer compiles material kernels.** It
  compiles config pipelines only (the boot pool, material prep, lighting
  defaults, content-gated passes). Kernels depend on the scene's bucket
  layout and compile in `commit_load`. If you called it alone to "warm
  everything", call `commit_load`.
- **Loading progress.** The commit reports the size of the batch
  (`LoadingStats::in_flight_subcompiles`) before it starts compiling it,
  then per-kernel progress as before. The batch itself has no per-pipeline
  callback.

## BEHAVIOUR: a post-process effect the device cannot build is turned off

If a pipeline of the SSR, bloom or froxel-volumetrics pass is rejected during
the commit, the renderer logs a warning, drops that pass, sets the effect off
in `renderer.post_processing` (`ssr.enabled = false`, `bloom = false`,
`atmosphere.mode = Off`) and finishes the load. A rejected bloom-blend
variant of the effects pass turns bloom off the same way. A rejected plain
effects variant or display pipeline — which belong to the config as a whole —
falls back to the post-processing config the renderer was built with; if
that is already the config in use, it is an error. Any rejected pipeline
outside post-processing is still the commit's error.

In 0.39 `set_post_processing` returned such an error and the scene loader
logged it and carried on with the config half applied (`ssr.enabled` still
true, no pass). The outcome is now a clean "effect off". **Read
`renderer.post_processing` back after a load if your UI mirrors it.**

## BEHAVIOUR: only what the scene uses is compiled — the rest on demand

A commit compiles kernels and masked variants for the content it sees:

- a shading kernel only for buckets some material shades through (the sky,
  every registered custom material, and each first-party bucket in use). The
  canonical PBR bucket in particular is no longer compiled at a commit, since
  a commit routes every opaque PBR material to a feature-set bucket;
- masked (alpha-tested) geometry and shadow variants only for the built-in
  families that have an alpha-tested material. 0.39 compiled all four
  families on every load: 44 pipelines.

Two cases can therefore turn up on a running scene with no commit after
them. Both are handled by the render loop, and both show a **transient where
0.39 was immediate**:

- **A material inserted and assigned live** (`materials.insert` +
  `set_mesh_material`, no commit) whose bucket has no kernel — the first
  material of a family, or any new PBR material, which shades through the
  canonical PBR bucket until a commit. The kernel is compiled when the
  material appears; the mesh is **unshaded** until it resolves.
- **A built-in material that turns alpha-tested live** when its family had
  no masked variants. They are compiled when the material appears; the mesh
  draws **solid (un-cut)**, and casts a solid shadow, until they resolve.

Warm, either is a few frames. Cold it is one compile: measured 0.5–1.3 s for
the masked variants and 1.1 s for the canonical PBR kernel. Neither affects a
load that ends in `commit_load`, and the editor commits after every material
edit. If you edit materials live in a player and the first-use transient
matters, commit after the edit.

Anything that changes the bucket *layout* — a new PBR feature set, a custom
material — still needs a `commit_load`, as in 0.39.

## API changes

Pool-level APIs, used by embedders that drive the pipeline pools directly.

Removed:

- `MaterialEdgePipelines::ensure_compiled` and
  `MaterialEdgePipelines::merge_resolved`. They compiled the MSAA kernels for
  a caller-supplied bucket list, which was the bug (see below). The kernels
  have one compile path now, `AwsmRenderer::ensure_scene_pipelines`, driven
  by `commit_load`.
- `ComputePipelines::compile_pending`. Use
  `compile_pending_except(gpu, shaders, layouts, |_| false)`.

Changed:

- `ComputePipelines::compile_pending_except(…, skip)` and
  `RenderPipelines::compile_pending(…)` return `PendingCompile<Key, Error>`
  (`compiled`, `failed`) instead of a count. A rejected pipeline no longer
  aborts the batch: the rest is installed and the rejected slots come back
  in `failed`, still reserved. Both issue the batch largest shader first.
- `EffectsPipelines::install_resolved`, `DisplayPipelines::install_resolved`
  and `MaterialPrepPipelines::merge_resolved` return the keys they displaced,
  so the caller can release slots that never compiled.
- `MaterialEdgePipelines::build_descriptors` takes the set of buckets that
  want a kernel.

Added:

- `ComputePipelines` / `RenderPipelines`: `set_deferred` / `is_deferred`
  (reserve instead of compile), `is_reserved`, `release_reserved`;
  `RenderPipelines::release_reserved_by_shader_keys`;
  `Pipelines::set_deferred` / `is_deferred`.
- `GeometryMaskedPipelines::ensure_variants`,
  `ShadowMaskedPipelines::ensure_variants` (one batch for several variants)
  and `ShadowMaskedPipelines::has_variant`.
- `MaterialPrepPipelines::take_unwanted_reserved`.
- `RenderTextures::acknowledge_views_recreated`: `views()` now reports a
  rebuild on every call until the render preamble acknowledges it. Only the
  render loop should acknowledge.
- `Shaders::source_len`.
- `awsm-renderer-core`: `texture::mipmap::prewarm_pipeline`,
  `texture::convert_srgb::prewarm_pipeline`.

## Bug fixes

- **SSR scenes loaded through a single commit drew no sky and no opaque
  geometry.** A resolved classify / opaque compile was discarded when the
  generation of the material that launched it had been bumped meanwhile,
  even though the pipeline was still the one wanted; the classify pass was
  then skipped silently until an unrelated later commit. Installs are now
  validated by content (is this cache key still wanted), not by launcher
  generation. This affected every textured scene with SSR on in a player.
- **`ensure_config_pipelines` installed kernels built for the wrong bucket
  layout.** It compiled the MSAA kernels for the first-party-only bucket list
  and installed them over the live ones; on a scene with a feature-set
  variant or a custom material the sky and canonical buckets then read the
  classify tile lists at the wrong offsets until the next commit. Reachable
  from the editor's shadow-denoise toggle. Inside a commit the same compile
  was a wasted serial wave (about 2 s of a cold DANCE-OFF load).
- **"Destroyed texture [Texture "Opaque"] used in a submit" on every frame.**
  When the picker's compile was the first reader of rebuilt render textures
  (the first transmissive material grows the opaque mip chain) it swallowed
  the "views recreated" signal, the frame's bind groups stayed on the retired
  textures, and every submit failed until an AA toggle. The editor hit this
  at boot on projects with a transmissive material.
- **A KTX2 load hung for 120 s when the Basis worker script failed to load**
  (blocked by CSP, missing, wrong MIME type). The worker's error handler
  threw on the plain `Event` such a failure fires; in-flight requests now
  fail at once with a message that says so.
- **Stale boot reservations were compiled for nothing.** A scene that
  re-keyed material prep, the effects / display passes or a rebuilt SSR /
  volumetrics pass before its first commit still compiled the pipelines
  reserved for the boot-time config.
- **A rejected material-prep compile on a running scene no longer breaks the
  frame.** The pass keeps rendering through the pipelines it had and the
  commit returns the error. (New in the reserve model, fixed before release.)
- The sRGB-conversion and mipmap helper pipelines of a texture upload
  compile together instead of back to back, and masked variants compile as
  one batch per pool instead of one variant at a time.

## Basis Universal transcoder re-vendored

`web/vendor/basis/basis_transcoder.{js,wasm}` and `web/workers/basis-worker.js`
are replaced by a local build of Basis Universal master (0.39 shipped the
three.js r185 build): eval-free embind glue, so it loads under a CSP with
`'wasm-unsafe-eval'` and without `'unsafe-eval'`, and a 256 MiB memory cap
where the old build could grow to 2 GiB. The worker is patched to match.
Transcode output is byte-identical to 0.39 for every format and mip level
tested. The cost is size: the transcoder wasm goes from 0.53 MB to 1.06 MB
(about +156 KB over the wire with brotli). The encoder is unchanged. `web/vendor/basis/rebuild-transcoder.sh` reproduces the
build; `BUILD-METADATA.json` records it.

If you copy these files into your own app, copy all three together — the
worker and the transcoder glue are a matched pair.
