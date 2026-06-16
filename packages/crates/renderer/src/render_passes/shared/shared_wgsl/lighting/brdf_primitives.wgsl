// brdf_primitives.wgsl — Tier A (generic) BRDF building blocks.
// -------------------------------------------------------------
// PBR (metal/roughness) BRDF with Image-Based Lighting (WGSL)
// Implements Cook-Torrance specular BRDF with split-sum IBL approximation
// Safe for HDR workflows (no final saturate - tone mapping applied elsewhere)
// Supports: KHR_materials_ior, KHR_materials_transmission, KHR_materials_volume,
//           KHR_materials_clearcoat, KHR_materials_sheen
// -------------------------------------------------------------
//
// These functions take PLAIN parameters (vec3/f32/textures), NOT the PbrMaterialColor
// type — so they're reusable by any material, including Custom (dynamic) ones. The
// PbrMaterialColor orchestrators that call into these live in brdf_pbr.wgsl (Tier B).
// The pbr_features-gated extension lobes (clearcoat/sheen/anisotropy/iridescence) stay
// here: they're generic-signature helpers, compiled only when their feature is on.
// See the module taxonomy in awsm-materials::shader_includes.

// IOR and refraction utilities (effective_ior / ior_to_f0 / refract_direction)
// now live in shared_wgsl/math.wgsl — they're always-included generic helpers
// used by the transparent transmission path even when brdf.wgsl is gated out.

// -------------------------------------------------------------
// Volume Attenuation (Beer's Law)
//
// Strict Beer's law: `T(x) = attenuationColor^(distance / attenuationDistance)`.
// We do NOT clamp this — assets with high `thickness / attenuationDistance`
// ratios will go nearly opaque, which is physically correct but can read
// as "the material isn't transmitting" (see DragonDispersion notes).
// Loosening this would need to be an explicit artistic knob, not a
// silent override of physics.
// -------------------------------------------------------------

// Calculate light attenuation through a medium using Beer's Law
// T(x) = attenuation_color^(distance / attenuation_distance)
fn volume_attenuation(
    distance: f32,
    attenuation_color: vec3<f32>,
    attenuation_distance: f32
) -> vec3<f32> {
    // Early exit: no distance = no attenuation
    if (distance <= 0.0) {
        return vec3<f32>(1.0);
    }
    // Early exit: infinite distance = no attenuation
    if (attenuation_distance <= 0.0 || attenuation_distance > 1e10) {
        return vec3<f32>(1.0);
    }
    // Early exit: white = no color shift
    if (all(attenuation_color >= vec3<f32>(0.999))) {
        return vec3<f32>(1.0);
    }

    // Beer's Law: T(x) = c^(x/d)
    return pow(attenuation_color, vec3<f32>(distance / attenuation_distance));
}

// Check if volume attenuation should be applied (optimization)
fn should_apply_volume_attenuation(
    thickness: f32,
    attenuation_distance: f32,
    attenuation_color: vec3<f32>
) -> bool {
    return thickness > 0.0
        && attenuation_distance < 1e10
        && any(attenuation_color < vec3<f32>(1.0));
}

// -------------------------------------------------------------
// Microfacet BRDF Components
// -------------------------------------------------------------

// Compute half-vector robustly.
// Returns zero when view and light are antiparallel (v + l == 0), which avoids
// injecting an arbitrary fallback direction into the BRDF.
fn safe_half_vector(v: vec3<f32>, l: vec3<f32>) -> vec3<f32> {
    let sum = v + l;
    let len_sq = dot(sum, sum);
    if (len_sq > 1e-8) {
        return sum * inverseSqrt(len_sq);
    }
    return vec3<f32>(0.0);
}

// Fresnel-Schlick approximation: view-dependent reflectance
fn fresnel_schlick(cos_theta: f32, F0: vec3<f32>) -> vec3<f32> {
    let ct = saturate(cos_theta);
    let one_minus = 1.0 - ct;
    return F0 + (1.0 - F0) * pow(one_minus, 5.0);
}

// Fresnel-Schlick with explicit f90 for KHR_materials_specular
fn fresnel_schlick_f90(cos_theta: f32, F0: vec3<f32>, f90: f32) -> vec3<f32> {
    let ct = saturate(cos_theta);
    let one_minus = 1.0 - ct;
    return F0 + (vec3<f32>(f90) - F0) * pow(one_minus, 5.0);
}

// GGX/Trowbridge-Reitz normal distribution function
fn distribution_ggx(n_dot_h: f32, alpha: f32) -> f32 {
    let a  = max(alpha, 0.001);
    let a2 = a * a;
    let ndh = saturate(n_dot_h);
    let d  = ndh * ndh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d + EPSILON);
}

// Schlick-GGX geometry function (single direction)
fn geometry_schlick_ggx(n_dot_x: f32, alpha: f32) -> f32 {
    let a = max(alpha, 0.001);
    let k = ((a + 1.0) * (a + 1.0)) * 0.125; // Direct lighting: k = (α+1)²/8
    let ndx = saturate(n_dot_x);
    return ndx / (ndx * (1.0 - k) + k);
}

// Smith geometry function (combines view and light directions)
fn geometry_smith(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, alpha: f32) -> f32 {
    let n_dot_v = saturate(dot(n, v));
    let n_dot_l = saturate(dot(n, l));
    return geometry_schlick_ggx(n_dot_v, alpha) * geometry_schlick_ggx(n_dot_l, alpha);
}

// -------------------------------------------------------------
// Clearcoat BRDF (KHR_materials_clearcoat)
// -------------------------------------------------------------

{# Skinny: clearcoat lobe defs gated by the feature (the call sites already are),
   so a PBR variant without clearcoat doesn't compile them. #}
{% if pbr_features.clearcoat %}
// Clearcoat uses a fixed F0 of 0.04 (standard dielectric)
const CLEARCOAT_F0: f32 = 0.04;

// Compute clearcoat specular contribution for direct lighting
fn clearcoat_brdf_direct(
    clearcoat: f32,
    clearcoat_roughness: f32,
    clearcoat_normal: vec3<f32>,
    v: vec3<f32>,
    l: vec3<f32>,
) -> f32 {
    // Early exit if no clearcoat
    if (clearcoat <= 0.0) {
        return 0.0;
    }

    let cc_n = safe_normalize(clearcoat_normal);
    let h = safe_half_vector(v, l);
    if (dot(h, h) == 0.0) {
        return 0.0;
    }

    let cc_n_dot_l = max(dot(cc_n, l), 0.0);
    let cc_n_dot_v = max(dot(cc_n, v), 1e-4);
    let cc_n_dot_h = max(dot(cc_n, h), 0.0);
    let cc_v_dot_h = max(dot(v, h), 0.0);

    // Clearcoat uses squared roughness (alpha)
    let cc_alpha = max(clearcoat_roughness * clearcoat_roughness, 0.001);

    // GGX specular BRDF for clearcoat
    let Fc = fresnel_schlick(cc_v_dot_h, vec3<f32>(CLEARCOAT_F0)).r;
    let Dc = distribution_ggx(cc_n_dot_h, cc_alpha);
    let Gc = geometry_smith(cc_n, v, l, cc_alpha);

    return clearcoat * Fc * Dc * Gc / max(4.0 * cc_n_dot_l * cc_n_dot_v, EPSILON);
}

// Compute clearcoat Fresnel for energy conservation (attenuates base layer)
fn clearcoat_fresnel(clearcoat: f32, v_dot_h: f32) -> f32 {
    if (clearcoat <= 0.0) {
        return 0.0;
    }
    return clearcoat * fresnel_schlick(v_dot_h, vec3<f32>(CLEARCOAT_F0)).r;
}
{% endif %}{# end pbr_features.clearcoat (lobe defs) #}

// -------------------------------------------------------------
// Sheen BRDF (KHR_materials_sheen)
// Uses Charlie distribution for cloth-like sheen
// -------------------------------------------------------------
{# Skinny: sheen lobe defs gated by the feature. #}
{% if pbr_features.sheen %}

// Charlie distribution function for sheen (inverted Gaussian)
// This creates a soft, cloth-like highlight at grazing angles
fn distribution_charlie(n_dot_h: f32, roughness: f32) -> f32 {
    let alpha = roughness * roughness;
    let inv_alpha = 1.0 / alpha;
    let cos2h = n_dot_h * n_dot_h;
    let sin2h = 1.0 - cos2h;
    // Charlie distribution: (2 + 1/alpha) * sin^(1/alpha) / (2*PI)
    return (2.0 + inv_alpha) * pow(sin2h, inv_alpha * 0.5) / (2.0 * PI);
}

// Visibility function for sheen (Ashikhmin)
fn visibility_ashikhmin(n_dot_v: f32, n_dot_l: f32) -> f32 {
    // Guard the denominator: it → 0 when both cosines → 0, which would
    // produce inf/NaN. EPSILON floor keeps it finite at grazing angles.
    return 1.0 / max(4.0 * (n_dot_l + n_dot_v - n_dot_l * n_dot_v), EPSILON);
}

// Compute sheen contribution for direct lighting
fn sheen_brdf_direct(
    sheen_color: vec3<f32>,
    sheen_roughness: f32,
    n: vec3<f32>,
    v: vec3<f32>,
    l: vec3<f32>,
) -> vec3<f32> {
    // Early exit if no sheen
    if (all(sheen_color <= vec3<f32>(0.0))) {
        return vec3<f32>(0.0);
    }

    let h = safe_half_vector(v, l);
    if (dot(h, h) == 0.0) {
        return vec3<f32>(0.0);
    }

    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let n_dot_h = max(dot(n, h), 0.0);

    // Use minimum roughness to avoid division issues
    let roughness = max(sheen_roughness, 0.07);

    let D = distribution_charlie(n_dot_h, roughness);
    let V = visibility_ashikhmin(n_dot_v, n_dot_l);

    return sheen_color * D * V;
}

// Estimate sheen albedo scaling for energy conservation
// Based on KHR_materials_sheen spec: sheen_albedo_scaling = 1.0 - max3(sheenColor) * E(VdotN)
// E(x) is the directional albedo of the sheen BRDF, approximated here without an LUT
fn sheen_albedo_scaling(sheen_color: vec3<f32>, sheen_roughness: f32, n_dot_v: f32) -> f32 {
    // Use max component as per spec (not luminance)
    let sheen_max = max(max(sheen_color.r, sheen_color.g), sheen_color.b);
    if (sheen_max <= 0.0) {
        return 1.0;  // No sheen = no scaling
    }

    // Approximate E(n_dot_v) - the directional albedo of the Charlie sheen BRDF
    // E increases with roughness and at grazing angles (lower n_dot_v)
    // This approximation is based on fitting to reference LUT values
    let alpha = sheen_roughness * sheen_roughness;
    // E ranges from ~0.0 at roughness=0 to ~0.2 at roughness=1 for normal incidence
    // And increases at grazing angles
    let E = alpha * (0.18 + 0.06 * (1.0 - n_dot_v));

    return 1.0 - sheen_max * E;
}
{% endif %}{# end pbr_features.sheen (lobe defs) #}

// -------------------------------------------------------------
// IBL Sampling Functions
// -------------------------------------------------------------

// Sample the irradiance map for diffuse IBL.
//
// The irradiance cubemaps store (sampled/blurred) environment *radiance* L,
// not the cosine-integrated irradiance E = ∫ L cosθ dω (for a uniform
// environment, E = π·L). The diffuse BRDF applies `base_color/PI *
// irradiance`, which assumes the latter — so without the π the diffuse IBL
// comes out π× too dim (a Lambertian surface under a uniform white env
// should read albedo·L but read albedo·L/π). Restore the missing factor
// here so every diffuse-IBL consumer is corrected in one place; specular
// IBL samples the prefiltered env separately and is unaffected.
fn sampleIrradiance(
    n: vec3<f32>,
    irradiance_tex: texture_cube<f32>,
    irradiance_sampler: sampler
) -> vec3<f32> {
    return textureSampleLevel(irradiance_tex, irradiance_sampler, n, 0.0).rgb * PI;
}

// Sample prefiltered environment map for specular IBL (split-sum approximation)
// Roughness selects mip level: 0 = sharp reflections, max = fully diffuse
fn samplePrefilteredEnv(
    R: vec3<f32>,
    roughness: f32,
    filtered_env_tex: texture_cube<f32>,
    filtered_env_sampler: sampler,
    ibl_info: IblInfo
) -> vec3<f32> {
    let max_mip = f32(ibl_info.prefiltered_env_mip_count - 1u);
    let mip_level = roughness * max_mip;
    return textureSampleLevel(filtered_env_tex, filtered_env_sampler, R, mip_level).rgb;
}

// Sample BRDF integration LUT (2D texture indexed by N·V and roughness)
// Returns (scale, bias) for computing F0 * scale + bias
fn sampleBRDFLUT(
    n_dot_v: f32,
    roughness: f32,
    brdf_lut_tex: texture_2d<f32>,
    brdf_lut_sampler: sampler
) -> vec2<f32> {
    let uv = vec2<f32>(saturate(n_dot_v), saturate(roughness));
    return textureSampleLevel(brdf_lut_tex, brdf_lut_sampler, uv, 0.0).rg;
}

// -------------------------------------------------------------
// Anisotropy (KHR_materials_anisotropy)
// -------------------------------------------------------------
{# Skinny: anisotropy lobe defs gated by the feature. #}
{% if pbr_features.anisotropy %}

// Returns a per-direction anisotropic roughness pair `(alpha_t, alpha_b)`.
// `strength` is the (signed) anisotropy factor — sign flips orient the lobe.
fn anisotropic_alpha(roughness: f32, strength: f32) -> vec2<f32> {
    let alpha = max(roughness * roughness, 0.0016);
    let s = clamp(abs(strength), 0.0, 1.0);
    // Spec: roughness_t = mix(roughness, 1, anisotropy^2) (the "rough" axis)
    //       roughness_b = roughness                          (the "smooth" axis)
    let alpha_t = mix(alpha, 1.0, s * s);
    let alpha_b = alpha;
    return vec2<f32>(alpha_t, alpha_b);
}

// Anisotropic GGX distribution (Burley/Disney form).
fn distribution_ggx_anisotropic(
    t_dot_h: f32,
    b_dot_h: f32,
    n_dot_h: f32,
    alpha_t: f32,
    alpha_b: f32
) -> f32 {
    let a2 = alpha_t * alpha_b;
    let f = vec3<f32>(alpha_b * t_dot_h, alpha_t * b_dot_h, a2 * n_dot_h);
    let denom = a2 / max(dot(f, f), EPSILON);
    return a2 * denom * denom / PI;
}

fn visibility_anisotropic(
    n_dot_l: f32,
    n_dot_v: f32,
    t_dot_l: f32,
    t_dot_v: f32,
    b_dot_l: f32,
    b_dot_v: f32,
    alpha_t: f32,
    alpha_b: f32
) -> f32 {
    let lambda_v = n_dot_l * length(vec3<f32>(alpha_t * t_dot_v, alpha_b * b_dot_v, n_dot_v));
    let lambda_l = n_dot_v * length(vec3<f32>(alpha_t * t_dot_l, alpha_b * b_dot_l, n_dot_l));
    return 0.5 / max(lambda_v + lambda_l, EPSILON);
}
{% endif %}{# end pbr_features.anisotropy (lobe defs) #}

// -------------------------------------------------------------
// Iridescence (KHR_materials_iridescence)
//
// Trade-off: this is a simplified two-beam Fabry-Perot model — not the
// full Belcour-Barla 2017 thin-film integration the spec references.
// We get:
//   * The right qualitative behavior (rainbow fringes that shift with
//     view angle and film thickness)
//   * The right peak colors at typical thicknesses (100-400 nm)
// We do NOT get:
//   * Physically accurate spectral integration. At thick films (>1µm)
//     or very high IOR ratios, hue progression drifts from a true
//     Belcour-Barla evaluation.
//   * Higher-order Fabry-Perot terms (`(amp1*amp2)^n` for n>1). The
//     two-beam term dominates for the typical (small) R12 and R23
//     values we'll see; the extra terms would matter for highly
//     reflective film/base stacks (e.g. metallic underlayers).
//
// The simpler form runs in a handful of ALU ops per fragment and pulls
// no extra LUTs. If we ever need the full physical answer (real-time
// pearlescent paint comparable to offline renderers), the upgrade path
// is the LUT-based Belcour-Barla — but it costs a 64x64x64 RGB LUT and
// noticeably more shader cost.
// -------------------------------------------------------------
{# Skinny: iridescence lobe defs gated by the feature. #}
{% if pbr_features.iridescence %}
fn iridescence_fresnel(
    cos_theta_v: f32,
    eta_thin: f32,
    thickness_nm: f32,
    base_f0: vec3<f32>
) -> vec3<f32> {
    // Force the film IOR back toward the outside medium when the layer
    // is too thin for coherent interference — keeps the result smooth as
    // thickness → 0.
    let outside_ior = 1.0;
    let scaled_ior = mix(outside_ior, max(eta_thin, 1.0), smoothstep(0.0, 0.03, thickness_nm));

    // Snell's law inside the film.
    let sin_t2 = (outside_ior / scaled_ior) * (outside_ior / scaled_ior)
        * (1.0 - cos_theta_v * cos_theta_v);
    if (sin_t2 >= 1.0) {
        // Total internal reflection: bypass interference, the surface
        // already reflects everything.
        return base_f0;
    }
    let cos_t2 = sqrt(1.0 - sin_t2);

    // Reflectance at the outside/film interface (averaged over polarization).
    let r_par = (scaled_ior * cos_theta_v - outside_ior * cos_t2)
        / (scaled_ior * cos_theta_v + outside_ior * cos_t2);
    let r_perp = (outside_ior * cos_theta_v - scaled_ior * cos_t2)
        / (outside_ior * cos_theta_v + scaled_ior * cos_t2);
    let r12 = clamp(0.5 * (r_par * r_par + r_perp * r_perp), 0.0, 1.0);

    // The base/film interface reflectance is the base F0 — that already
    // encodes the metallic/dielectric weighting from upstream.
    let r23 = clamp(base_f0, vec3<f32>(0.0), vec3<f32>(1.0));

    // Amplitude reflectances (square roots of the intensity reflectances).
    let amp1 = sqrt(r12);
    let amp2 = sqrt(r23);

    // OPD round trip and per-wavelength phase. Wavelengths centred on the
    // peaks of the CIE RGB sensitivity curves.
    let opd = 2.0 * scaled_ior * thickness_nm * cos_t2;
    let wavelengths = vec3<f32>(685.0, 550.0, 463.0);
    let phase = 2.0 * PI * opd / wavelengths;
    let cos_phase = cos(phase);

    // Two-beam Airy reflectance (Fabry-Perot, ignoring higher orders), in
    // terms of the amplitude coefficients ρ12=amp1, ρ23=amp2:
    //   R = |ρ12 + ρ23·e^{iφ}|² / |1 + ρ12·ρ23·e^{iφ}|²
    //     = (r12 + r23 + 2·amp1·amp2·cosφ) / (1 + r12·r23 + 2·amp1·amp2·cosφ)
    // The previous code used only the numerator, which peaks at
    // (√r12+√r23)² > max(r12,r23) — energy non-conserving, just clamped to
    // 1. The denominator (Airy normalization) keeps R ≤ 1 and physical. Its
    // minimum is (1-amp1·amp2)² ≥ 0 (=0 only at total reflection, already
    // returned above), so a tiny floor guards the division.
    let cross = 2.0 * vec3<f32>(amp1) * amp2 * cos_phase;
    let numerator = vec3<f32>(r12) + r23 + cross;
    let denominator = vec3<f32>(1.0) + vec3<f32>(r12) * r23 + cross;
    let interference = numerator / max(denominator, vec3<f32>(1e-4));
    return clamp(interference, vec3<f32>(0.0), vec3<f32>(1.0));
}
{% endif %}{# end pbr_features.iridescence (lobe defs) #}

