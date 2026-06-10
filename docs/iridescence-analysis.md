# Dish iridescence — code-level diagnosis (Phase 8)

Reference target: `/Users/dakom/Downloads/olives.png` (Khronos viewer + a "color
photo studio" IBL): clear refractive glass with a **subtle pink-violet** thin-film
sheen near the crown; clean warm **gold metal** bowl (`goldLeaf`, no iridescence).
The iridescence is understated.

Our symptoms: model-tests (`populate_gltf`) goes **white on the bowl top**; the
editor showed **over-strong green/rainbow** iridescence. Both share the same
renderer shader, so the divergence between them is mostly *environment*
(model-tests PhotoStudio vs the editor's flat IBL); the divergence from the
*Khronos reference* is the shader itself. This is an analysis, not a fix.

## What's confirmed correct (ruled out)

- **Iridescence + thickness textures.** The source genuinely points both
  `iridescenceTexture` and `iridescenceThicknessTexture` at the *same* image
  (index 0 for glassDish, 6 for glassCover). Our extraction collapsing them to one
  asset is therefore correct, **not** a bug.
- **Thickness mapping.** `pbr_iridescence_thickness` = `mix(min, max, tex.g)` with
  min=500/max=550 nm — matches the spec. (`material_color_calc.wgsl:485`.)
- **Transmission routing.** Fixed earlier this session; the glass is correctly in
  the transparent pass.

## Prime suspect: the thin-film model is a 3-wavelength two-beam approximation

`brdf.wgsl:342-425` (`iridescence_fresnel`) is explicitly a **simplified two-beam
Airy/Fabry-Perot** model, *not* the spec-referenced Belcour-Barla 2017 spectral
integration (the code comment says so). Two consequences bear directly on the
"wrong hue" symptom:

1. **3-sample spectral approximation.** It evaluates interference at exactly three
   wavelengths — `685/550/463 nm` (`brdf.wgsl:407`) — as RGB, instead of
   integrating the thin-film reflectance against the full CIE sensitivity curves.
   A 3-point sample readily lands the wrong hue, *especially the green channel*:
   at our thickness the green sample sits near a cosine zero-crossing, so a small
   thickness/IOR error flips green between a trough and a peak — exactly the kind
   of error that turns a pink sheen green. **This is the most likely cause of
   green-instead-of-pink.**

   *Sanity check of the expected hue:* at ~525 nm thickness, IOR 1.3, near-normal,
   OPD ≈ 2·1.3·525 ≈ 1365 nm; phases give cos≈+1 (red 685), cos≈−1 (green 550),
   cos≈+1 (blue 463) → red+blue up, green down → **magenta/pink**, which is what
   the reference shows. So the *math direction* is right; the **3-sample hue
   fidelity** is the weak link, plus anything that perturbs the per-channel phase.

2. **Two-beam only (no higher Fabry-Perot orders).** Fine for low R12·R23 (our
   dielectric case), so probably not the dominant error here, but worth noting for
   the glassCover (which has a metallic-ish base via its MR texture → higher R23 →
   the dropped higher-order terms matter more there).

## Second factor: grazing-angle reflection ("white bowl top")

`iridescence_fresnel` returns `base_f0` unmodified once `sin_t2 >= 1.0` (total
internal reflection, `brdf.wgsl:382-386`). At the bowl's top rim, n·v → 0, so:
(a) interference is bypassed (no tint), and (b) base Fresnel → 1 (full mirror
reflection of the environment). Under a bright IBL that reads **white**. This is
*partly physical* — glass is mirror-like at grazing — but two things make it read
wrong vs the reference:

- The TIR bypass is abrupt: iridescent tint vanishes exactly where the reflection
  gets strongest, so the rim loses its sheen instead of keeping a colored edge.
- It needs verifying that the grazing reflection is **energy-balanced against
  transmission** — i.e. the transmitted (refracted) background is weighted by
  `(1 - specular_reflectance)` using the iridescent F0. If transmission is added on
  top of a full-strength reflection rather than `1-R` of it, grazing pixels are
  over-bright (white). Worth confirming in the transparent compositing
  (`material_transparent .../material_color_calc.wgsl` lines ~100-130 hand off
  `transmission_factor` + iridescence to lighting — trace how the two are summed).

## Third factor: F0-modulation amplitude under a sharp IBL

Iridescence is applied as `F0 = mix(F0, iridescence_fresnel(...), factor)`
(`brdf.wgsl:489-490` opaque, `629-631`; transparent passes the same inputs). For a
pure dielectric (glassDish, F0≈0.04) the oscillation amplitude is small (~0.05),
so the sheen *should* be subtle. With roughness 0.07 the IBL specular is a sharp
mirror, so even a small per-channel F0 oscillation paints crisp colored fringes
off a bright/colorful environment — which is why a flat white IBL produced visible
"rainbow rings" while a soft studio IBL reads as a faint tint. So the editor's
flat-IBL rainbow is largely an **environment artifact amplified by low roughness**,
not necessarily a shader bug — consistent with the much-improved look once we
loaded PhotoStudio.

## Recommended fixes, in priority order

1. **Replace the 3-sample hue with a proper spectral→RGB conversion** (the highest-
   value, most-likely fix for green-vs-pink). Either: (a) the spec's
   `evalSensitivity` Gaussian-fit XYZ approach (a handful of extra ALU ops, no
   LUT, matches the Khronos sample viewer), then XYZ→sRGB; or (b) the full
   Belcour-Barla LUT if we want offline-grade pearlescence (costs a 64³ LUT). Start
   with (a) — it's cheap and is exactly what the reference renderer uses.
2. **Verify transmission↔reflection energy conservation** at grazing (trace the
   transparent compositing); ensure refraction is `(1-R)`-weighted with the
   iridescent R. Likely fixes "white bowl top."
3. **Soften/keep tint through grazing**: instead of hard TIR bypass returning
   `base_f0`, fade toward it so the rim keeps a colored edge.

## Needs render verification (can't do autonomously)

Which of the three dominates is a visual question. The order above is my
confidence ranking; #1 (spectral model) is where I'd put a fix first and compare
to `olives.png` under a matching IBL. Everything here is read from the shader
source — no in-browser confirmation yet.
