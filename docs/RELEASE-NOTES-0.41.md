# Release notes — 0.41

One change, and it is a change of default: **opaque PBR materials that differ
only in cheap features now share one shading kernel.** A cold load compiles
fewer ~240 KB kernels; what a committed scene renders is not meant to change.

It is a minor version rather than a 0.40 patch because it amends the
"specialize-only" rule the renderer has documented since the materials
overhaul (one kernel per exact feature set), and because every app picks it
up without asking. `PbrSpecialization::EveryFeature` on the builder is the
old behaviour, one line away.

## BEHAVIOUR: the cheap PBR features no longer split kernels

Until 0.40 every distinct `PbrFeatures` set among a scene's opaque PBR
materials got its own bucket and its own kernel. A kernel is ~240 KB of WGSL
and compiles at roughly two seconds per megabyte, cold. The features are a
small part of that text: thirteen of them add 20.7 KB between them, and seven
add 540 bytes:

| Feature | WGSL it adds | Splits kernels in 0.41? |
|---|---|---|
| IOR | 0 bytes | no |
| dispersion | 7 | no |
| transmission | 16 | no |
| specular | 19 | no |
| volume | 47 | no |
| emissive strength | 66 | no |
| vertex colour | 385 | no |
| diffuse transmission | 1,527 | yes |
| anisotropy | 2,853 | yes |
| clearcoat | 2,935 | yes |
| iridescence | 3,144 | yes |
| sheen | 3,414 | yes |
| secondary maps | 6,309 | yes |

The seven cheap ones are now compiled into **every** opaque PBR kernel. A
material's bucket is keyed on the expensive features it uses, so a scene
compiles one PBR kernel per combination of EXPENSIVE features instead of one
per combination of all thirteen. DANCE-OFF had five PBR kernels that differed
by an emissive strength, a specular factor and a clearcoat; it now has two.

Nothing in the shaders changed. A material's payload already carries an index
per extension; the loaders return the glTF-absent value when it is 0 (IOR 1.5,
specular 1, emissive strength 1, no transmission, no volume, no dispersion),
the vertex-colour multiply is skipped when the material has none, and the
texture fetches and UV gradients of an absent extension are behind
`tex_info.exists`. A kernel that carries a cheap feature a material lacks
shades that material as a kernel without it would.

Things that look different:

- **Fewer buckets, fewer pipelines, different kernel text.** Every opaque PBR
  kernel is 540 bytes longer; a scene with several cheap-feature combinations
  has fewer of them. `dynamic_materials.bucket_entries_cached()` is shorter,
  the bucket cap (`BucketConfig`, default 32) has more headroom, and
  `first_party_variant_of(id)` reports the KERNEL's feature set (the
  material's own plus the cheap seven), not the material's.
- **A cheap feature gained on a running scene shows on the next frame.**
  `update_material` that gives a committed material an IOR, a specular
  factor or an emissive strength takes effect at once: the kernel already
  has the code (player-tests checks it with IOR). In 0.40 the material kept
  its old kernel until the next `commit_load`.
- **That edit no longer changes the bucket set.** The commit after it
  compiles nothing. (Adding an EXPENSIVE feature still creates a bucket, and
  a bucket-set change still recompiles every kernel.)
- The transparent pass is untouched: each transparent material still
  compiles a pipeline for exactly its own features.

### Cold-load numbers

M1 Max, Chrome 157, Metal, cold GPU caches (fresh profile, never-used site),
alternating runs against the 0.40.1 candidate. Medians.

| Load | 0.40.1 | 0.41 | PBR kernels | Pipelines | WGSL compiled |
|---|---|---|---|---|---|
| DANCE-OFF stage (player), 6 runs each | 7.60 s | **6.04 s** | 5 → 2 | 55 → 52 | 2.65 → 1.94 MB |
| `pbr-extensions`, 3 runs each | 8.10 s | **7.23 s** | 8 → 6 | 54 → 52 | 3.33 → 2.87 MB |
| `ssr-arena` | 6.06 s | **5.63 s** | 2 → 1 | 66 → 65 | 1.69 → 1.45 MB |
| `kitchen-sink` | 5.35 s | 5.37 s | 1 → 1 | 47 | +540 B |
| `alpha-cutoff` | 4.83 s | 4.84 s | 1 → 1 | 55 | +540 B |
| `contact-shadows` | 3.86 s | 3.90 s | 1 → 1 | 43 | +540 B |

DANCE-OFF, run by run: 7.59 / 7.78 / 7.62 / 7.58 / 7.69 / 7.44 s before,
6.05 / 6.01 / 6.03 / 5.93 / 6.21 / 6.07 s after. A scene with a single PBR
feature set gains nothing and loses nothing measurable: the three at the
bottom moved by 10–40 ms, inside their own run-to-run spread (about 150 ms).

### Rendering

- 22 of the 23 single-commit test-scene renders are pixel-identical to
  0.40.1, as are both `player-cameras` goldens and seven of eight editor
  project views (the eighth, `kitchen-sink`, animates and differs from
  itself between runs).
- `ssr-arena` is pixel-identical with SSR off. With SSR on, 2,557 of 480,000
  pixels differ: 2,549 by 1/255 and 8 by 2/255, scattered over the reflective
  floor, as many brighter as darker, and the same on every run of a given
  build. The shaded image going into SSR is the same to 8 bits; the shared
  kernel reads from the material a few values the exact kernel had as
  compile-time constants, and SSR's stochastic trace turns that sub-8-bit
  difference into speckle. No other SSR scene (`ssr`, `mirror`,
  `kitchen-sink`) shows it.
- `player-tests` now builds a scene of ten PBR feature sets (bare, each
  cheap feature an opaque material can carry, all of them together,
  clearcoat, clearcoat plus cheap ones), renders it under both policies and
  compares the frames: identical, 0/255, with and without MSAA.

### What is not measured

- **Per-pixel GPU cost, on any device.** Nothing here measured frame time.
  From the code, a pixel of a material WITHOUT the cheap features now pays:
  an index test and a constant in each extension loader, the UV lookup for
  four texture slots whose fetch is then skipped (specular, specular colour,
  transmission, volume thickness), and a few multiplies by 1. Next to the
  light loop, shadows and IBL that should not show — but it has not been
  timed, and a phone GPU at native resolution is where it would show first.
  If it does, `PbrSpecialization::EveryFeature` removes it.
- **Transmission, volume and dispersion as live opaque features.** A
  material with transmission is drawn by the transparent pass, so the opaque
  kernel only ever passes these three through (volume and dispersion without
  transmission are in the player-tests scene and render the same both ways).
- **Non-MSAA cold time.** The non-MSAA kernels were checked for output and
  kernel counts, not timed.

## API

New, nothing removed or changed:

- `awsm_renderer_materials::pbr::PbrSpecialization` — `ExpensiveFeatures`
  (default) or `EveryFeature`; `always_compiled()`, `kernel_features(material)`.
- `PbrFeatures::cheap()`, `PbrFeatures::union()`.
- `AwsmRendererBuilder::with_pbr_specialization(PbrSpecialization)`.
  Build-time only; `remove_all` keeps it.
- `DynamicMaterials::pbr_specialization()`.

```rust
// One kernel per exact feature set, as in 0.40:
let renderer = AwsmRendererBuilder::new(gpu)
    .with_pbr_specialization(PbrSpecialization::EveryFeature)
    .build()
    .await?;
```

## Upgrading

Nothing to do. If you assert on bucket or pipeline counts, expect fewer on
scenes whose PBR materials differ in cheap features. If you maintain the PBR
templates, read "Specialize-only materials" in `docs/SHADER_GUIDELINES.md`:
code behind a cheap feature's gate now runs for every opaque PBR pixel and
must be neutral for a material without the feature.

## Not in this release

Merging the expensive lobes too (clearcoat, sheen, iridescence, diffuse
transmission) would take DANCE-OFF to about 5.6 s and `pbr-extensions` to
about 5.1 s, and their unused paths rendered pixel-identically when tried.
It waits for a frame-time measurement on phones, because every PBR pixel
would then run those lobes with zero weight. Anisotropy cannot join until its
unused path is made neutral: compiled into a kernel that shades a material
without it, it changed 90 pixels (up to 162/255).
