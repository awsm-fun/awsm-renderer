// -------------------------------------------------------------
// PBR (metal/roughness) BRDF with Image-Based Lighting (WGSL)
// Implements Cook-Torrance specular BRDF with split-sum IBL approximation
// Safe for HDR workflows (no final saturate - tone mapping applied elsewhere)
// Supports: KHR_materials_ior, KHR_materials_transmission, KHR_materials_volume,
//           KHR_materials_clearcoat, KHR_materials_sheen
// -------------------------------------------------------------

// -------------------------------------------------------------
// IOR and Refraction Utilities
// -------------------------------------------------------------

// Get effective IOR value, defaulting to 1.5 when invalid (< 1.0)
// IOR = 1.0 is valid (air, no refraction), IOR < 1.0 is physically invalid
// Note: Rust side should default to 1.5 when KHR_materials_ior extension is absent
fn effective_ior(ior: f32) -> f32 {
    return select(ior, 1.5, ior < 1.0);
}

// Convert index of refraction to F0 (reflectance at normal incidence)
// Default IOR of 1.5 yields F0 = 0.04 (standard dielectric)
fn ior_to_f0(ior: f32) -> f32 {
    let ior_val = effective_ior(ior);
    let ratio = (ior_val - 1.0) / (ior_val + 1.0);
    return ratio * ratio;
}

// Calculate refracted direction using Snell's law
// Returns vec3(0) if total internal reflection occurs
fn refract_direction(incident: vec3<f32>, normal: vec3<f32>, eta: f32) -> vec3<f32> {
    // Optimization: no refraction when eta ≈ 1.0 (same medium)
    if (abs(eta - 1.0) < 0.001) {
        return incident;
    }

    // eta = ior_outside / ior_inside (typically 1.0 / ior for entering)
    let cos_i = -dot(incident, normal);
    let sin_t2 = eta * eta * (1.0 - cos_i * cos_i);

    // Total internal reflection check
    if (sin_t2 > 1.0) {
        return vec3<f32>(0.0);  // Signal TIR to caller
    }

    let cos_t = sqrt(1.0 - sin_t2);
    return eta * incident + (eta * cos_i - cos_t) * normal;
}

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

// -------------------------------------------------------------
// Sheen BRDF (KHR_materials_sheen)
// Uses Charlie distribution for cloth-like sheen
// -------------------------------------------------------------

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

// -------------------------------------------------------------
// Direct Lighting BRDF (Cook-Torrance)
// With clearcoat and sheen extensions
// -------------------------------------------------------------
fn brdf_direct(color: PbrMaterialColor, light_brdf: LightBrdf, surface_to_camera: vec3<f32>) -> vec3<f32> {
    let n = safe_normalize(light_brdf.normal);
    let l = safe_normalize(light_brdf.light_dir);

    // Early-out for lights that contribute nothing: out-of-range punctuals
    // have zero radiance (`inverse_square` returns 0 once dist ≥ range) and
    // back-facing surfaces have n·l ≤ 0. Both make the full Cook-Torrance
    // result exactly zero, so we skip the expensive GGX/Fresnel evaluation.
    // This is the dominant win for froxel/clustered shading: a froxel can
    // bin dozens of lights (the cull bounds them to the froxel volume, which
    // is large in the distance), but only a handful actually reach any given
    // pixel — the rest are culled here for the cost of a dot product.
    {% if pbr_features.diffuse_transmission %}
    // EXCEPTION: a diffuse-transmissive surface also receives a back-side
    // (transmitted) contribution from a light BEHIND it — n·l ≤ 0 but
    // dot(-n, l) > 0. Skipping on n·l ≤ 0 alone would drop that entirely
    // (so a firefly behind a leaf, or any back-light, would never show the
    // red transmission). Only skip when the light reaches NEITHER side.
    if ((light_brdf.n_dot_l <= 0.0 && dot(-n, l) <= 0.0)
        || dot(light_brdf.radiance, light_brdf.radiance) <= 0.0) {
        return vec3<f32>(0.0);
    }
    {% else %}
    if (light_brdf.n_dot_l <= 0.0 || dot(light_brdf.radiance, light_brdf.radiance) <= 0.0) {
        return vec3<f32>(0.0);
    }
    {% endif %}
    let v = safe_normalize(surface_to_camera);
    let h = safe_half_vector(v, l);

    // Material properties
    let base_color = color.base.rgb;
    let metallic   = clamp(color.metallic_roughness.x, 0.0, 1.0);
    let roughness  = max(clamp(color.metallic_roughness.y, 0.0, 1.0), 0.04);
    let alpha      = roughness * roughness;

    // Lighting geometry
    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let has_half = dot(h, h) > 0.0;
    let n_dot_h = select(0.0, max(dot(n, h), 0.0), has_half);
    let v_dot_h = select(0.0, max(dot(v, h), 0.0), has_half);

    // F0: base reflectivity at normal incidence
    // KHR_materials_ior: dielectric_f0_base = ((ior - 1) / (ior + 1))^2
    // KHR_materials_specular: dielectric_f0 = min(f0_base * specular_color, 1.0) * specular
    let dielectric_f0_base = ior_to_f0(color.ior);
    let dielectric_f0 = min(vec3<f32>(dielectric_f0_base) * color.specular_color, vec3<f32>(1.0)) * color.specular;
    var F0 = mix(dielectric_f0, base_color, metallic);

    // f90: grazing angle reflectivity (specular for dielectrics, 1.0 for metals per spec)
    let f90 = mix(color.specular, 1.0, metallic);

    // KHR_materials_iridescence: thin-film interference modulates F0.
    // Compile-time gated: stripped entirely from a specialized bucket that
    // lacks the extension (the all-features config keeps it).
    {% if pbr_features.iridescence %}
    let iri_f0 = iridescence_fresnel(n_dot_v, color.iridescence_ior, color.iridescence_thickness, F0);
    F0 = mix(F0, iri_f0, color.iridescence);
    {% endif %}

    // Cook-Torrance specular BRDF: DFG / (4 * N·L * N·V)
    // When V and L are antiparallel, H is undefined. Treat that as zero specular
    // and use view-Fresnel for diffuse energy conservation.
    let F = select(
        fresnel_schlick_f90(n_dot_v, F0, f90),
        fresnel_schlick_f90(v_dot_h, F0, f90),
        has_half
    );

    var specular = vec3<f32>(0.0);
    if (has_half) {
        // Isotropic Cook-Torrance specular — the base path, written once.
        let D = distribution_ggx(n_dot_h, alpha);
        let G = geometry_smith(n, v, l, alpha);
        specular = F * (D * G) / max(4.0 * n_dot_l * n_dot_v, EPSILON);

        // KHR_materials_anisotropy: an anisotropy bucket overrides the
        // isotropic base above with anisotropic GGX (the iso compute is
        // then dead → DCE). Compile-time gated; no runtime strength check
        // (strength 0 → anisotropic == isotropic anyway).
        {% if pbr_features.anisotropy %}
        let a = anisotropic_alpha(roughness, color.anisotropy_strength);
        let t = safe_normalize(color.anisotropy_t);
        let b = safe_normalize(color.anisotropy_b);
        let t_dot_l = dot(t, l);
        let t_dot_v = dot(t, v);
        let b_dot_l = dot(b, l);
        let b_dot_v = dot(b, v);
        let t_dot_h = dot(t, h);
        let b_dot_h = dot(b, h);
        let aD = distribution_ggx_anisotropic(t_dot_h, b_dot_h, n_dot_h, a.x, a.y);
        let aV = visibility_anisotropic(n_dot_l, n_dot_v, t_dot_l, t_dot_v, b_dot_l, b_dot_v, a.x, a.y);
        specular = F * aD * aV;
        {% endif %}
    }

    // Lambertian diffuse (energy-conserving: scaled by (1-F_max) and non-metallic portion)
    let F_max = max(max(F.r, F.g), F.b);
    let k_d = (1.0 - F_max) * (1.0 - metallic);
    let diffuse = k_d * base_color * (1.0 / PI);

    // `result` accumulates the FRONT (camera-side) layer — reflective
    // diffuse + specular — which the sheen albedo-scaling and clearcoat
    // Fresnel attenuate below. The diffuse-transmission BACK lobe is held
    // separately in `transmit_back` and added at the very end: it is on the
    // far side of the surface, so those front-layer energy factors must not
    // touch it.
    var transmit_back = vec3<f32>(0.0);
    {% if pbr_features.diffuse_transmission %}
    // KHR_materials_diffuse_transmission: split the diffuse layer into a
    // reflective lobe (front `n_dot_l`, base color) scaled by (1-dt) and a
    // transmissive lobe (back `n_dot_l_back`, diffuseTransmissionColor) at
    // weight dt. At dt=1 the reflective lobe vanishes and a back-lit
    // surface shows purely transmitted light. Specular is not part of the
    // diffuse split.
    let dt = color.diffuse_transmission;
    let n_dot_l_back = max(dot(-n, l), 0.0);
    let diffuse_reflect = diffuse * n_dot_l * (1.0 - dt);
    transmit_back = (k_d * color.diffuse_transmission_color * (1.0 / PI) * n_dot_l_back) * dt
        * light_brdf.radiance * color.occlusion;
    var result = (diffuse_reflect + specular * n_dot_l) * light_brdf.radiance * color.occlusion;
    {% else %}
    var result = (diffuse + specular) * n_dot_l * light_brdf.radiance * color.occlusion;
    {% endif %}

    // Sheen contribution (cloth-like rim highlight) — compile-time gated.
    // `sheen_scaling` is energy taken from the FRONT diffuse, so it only
    // attenuates `result`, never the back-transmission lobe.
    {% if pbr_features.sheen %}
    let sheen = sheen_brdf_direct(color.sheen_color, color.sheen_roughness, n, v, l);
    let sheen_scaling = sheen_albedo_scaling(color.sheen_color, color.sheen_roughness, n_dot_v);
    result = result * sheen_scaling + sheen * light_brdf.radiance * n_dot_l * color.occlusion;
    {% endif %}

    // Clearcoat contribution (additional specular layer) — compile-time
    // gated. The base is attenuated by the clearcoat Fresnel (evaluated at
    // the half-angle `v_dot_h`, which is normal-independent). The clearcoat
    // specular is added weighted by the CLEARCOAT normal's cosine
    // `cc_n_dot_l` (not the base `n_dot_l`) — these differ when a clearcoat
    // normal map is present.
    {% if pbr_features.clearcoat %}
    let cc_n_dot_l = max(dot(safe_normalize(color.clearcoat_normal), l), 0.0);
    let clearcoat_spec = clearcoat_brdf_direct(
        color.clearcoat,
        color.clearcoat_roughness,
        color.clearcoat_normal,
        v,
        l,
    );
    let cc_fresnel = clearcoat_fresnel(color.clearcoat, v_dot_h);
    result = result * (1.0 - cc_fresnel) + clearcoat_spec * light_brdf.radiance * cc_n_dot_l;
    {% endif %}

    return result + transmit_back;
}

// -------------------------------------------------------------
// Image-Based Lighting (IBL) - Split-sum Approximation
// -------------------------------------------------------------

// IBL with transmission background provided by caller
// transmission_background: pre-sampled color from behind the surface (screen-space or IBL)
fn brdf_ibl_with_transmission(
    color: PbrMaterialColor,
    normal: vec3<f32>,
    surface_to_camera: vec3<f32>,
    ibl_filtered_env_tex: texture_cube<f32>,
    ibl_filtered_env_sampler: sampler,
    ibl_irradiance_tex: texture_cube<f32>,
    ibl_irradiance_sampler: sampler,
    brdf_lut_tex: texture_2d<f32>,
    brdf_lut_sampler: sampler,
    ibl_info: IblInfo,
    transmission_background: vec3<f32>,
) -> vec3<f32> {
    let n = safe_normalize(normal);
    let v = safe_normalize(surface_to_camera);

    // Material properties
    let base_color = color.base.rgb;
    let metallic   = clamp(color.metallic_roughness.x, 0.0, 1.0);
    let roughness  = max(clamp(color.metallic_roughness.y, 0.0, 1.0), 0.04);

    let n_dot_v = saturate(dot(n, v));

    // F0: base reflectivity at normal incidence
    // KHR_materials_ior: dielectric_f0_base = ((ior - 1) / (ior + 1))^2
    // KHR_materials_specular: dielectric_f0 = min(f0_base * specular_color, 1.0) * specular
    let dielectric_f0_base = ior_to_f0(color.ior);
    let dielectric_f0 = min(vec3<f32>(dielectric_f0_base) * color.specular_color, vec3<f32>(1.0)) * color.specular;
    var F0 = mix(dielectric_f0, base_color, metallic);

    // f90: grazing angle reflectivity (specular for dielectrics, 1.0 for metals per spec)
    let f90 = mix(color.specular, 1.0, metallic);

    // KHR_materials_iridescence: thin-film modulates F0 before Fresnel.
    {% if pbr_features.iridescence %}
    let iri_f0 = iridescence_fresnel(n_dot_v, color.iridescence_ior, color.iridescence_thickness, F0);
    F0 = mix(F0, iri_f0, color.iridescence);
    {% endif %}

    // Fresnel at view direction
    let F_view = fresnel_schlick_f90(n_dot_v, F0, f90);
    let F_view_max = max(max(F_view.r, F_view.g), F_view.b);

    // Effective transmission: metals don't transmit
    let effective_transmission = color.transmission * (1.0 - metallic);

    // Calculate base layer (diffuse or transmission)
    var base_layer = vec3<f32>(0.0);

    if (effective_transmission > 0.0) {
        // Diffuse IBL contribution
        let irradiance = sampleIrradiance(n, ibl_irradiance_tex, ibl_irradiance_sampler);
        let diffuse_brdf = base_color * (1.0 / PI) * irradiance;

        // Transmission BTDF contribution
        // Apply volume attenuation if thickness > 0
        var attenuation = vec3<f32>(1.0);
        if (should_apply_volume_attenuation(
            color.volume_thickness,
            color.volume_attenuation_distance,
            color.volume_attenuation_color
        )) {
            attenuation = volume_attenuation(
                color.volume_thickness,
                color.volume_attenuation_color,
                color.volume_attenuation_distance
            );
        }

        // BTDF: transmitted background * base_color * attenuation
        let transmission_btdf = transmission_background * base_color * attenuation;

        // Mix diffuse and transmission based on transmission factor
        // Per spec: base = mix(diffuse_brdf, specular_btdf * baseColor, transmission)
        base_layer = mix(diffuse_brdf, transmission_btdf, effective_transmission);
    } else {
        // No transmission - standard diffuse
        let irradiance = sampleIrradiance(n, ibl_irradiance_tex, ibl_irradiance_sampler);
        base_layer = base_color * (1.0 / PI) * irradiance;
    }

    // Apply diffuse/transmission energy conservation
    let k_d = (1.0 - F_view_max) * (1.0 - metallic);
    var base_contribution = k_d * base_layer * color.occlusion;

    // KHR_materials_diffuse_transmission back-side lobe, kept SEPARATE from
    // `base_contribution` so the front-layer sheen/clearcoat factors below
    // don't attenuate it (it's on the far side of the surface). The diffuse
    // layer is a mix of a reflective lobe (front, base color) and a
    // transmissive lobe (back, *diffuseTransmissionColor* only — NOT base
    // color); at factor=1 the reflection vanishes and the surface shows
    // purely the transmitted environment in the transmission tint.
    var transmit_back = vec3<f32>(0.0);
    {% if pbr_features.diffuse_transmission %}
    let back_irradiance = sampleIrradiance(-n, ibl_irradiance_tex, ibl_irradiance_sampler);
    let dt_transmitted = (1.0 - F_view_max) * (1.0 - metallic)
        * color.diffuse_transmission_color
        * (1.0 / PI) * back_irradiance;
    let dt = color.diffuse_transmission;
    base_contribution = base_contribution * (1.0 - dt);
    transmit_back = dt * dt_transmitted * color.occlusion;
    {% endif %}

    // Specular IBL: prefiltered environment * (F0 * scale + f90 * bias) from BRDF LUT
    // KHR_materials_anisotropy: bend the reflection direction and stretch the
    // mip level toward the rough axis. This is the glTF Sample Viewer's
    // empirical fit, not a derived integral — anisotropic IBL with a
    // single split-sum LUT is an open problem. The fit produces the
    // expected stretched highlights (brushed metal, disc grooves) and
    // is what the reference renderer ships, but a physically-correct
    // result would need either a 2D anisotropic BRDF LUT or per-pixel
    // importance sampling. Either upgrade is large enough to warrant
    // its own work item.
    var R = reflect(-v, n);
    var ibl_roughness = roughness;
    {% if pbr_features.anisotropy %}
    {
        let t = safe_normalize(color.anisotropy_t);
        let b = safe_normalize(color.anisotropy_b);
        let aniso_strength = clamp(abs(color.anisotropy_strength), 0.0, 1.0);
        let aniso_dir = select(b, t, color.anisotropy_strength >= 0.0);
        // Tangent perpendicular to the view in the surface plane.
        let aniso_tangent = cross(aniso_dir, v);
        let aniso_normal = cross(aniso_tangent, aniso_dir);
        // Bend the reflected normal toward the anisotropy direction; smoother
        // along the rough axis, sharper across.
        let bend_factor = 1.0 - aniso_strength * (1.0 - roughness);
        let bent_normal = normalize(mix(aniso_normal, n, bend_factor * bend_factor));
        R = reflect(-v, bent_normal);
        ibl_roughness = mix(roughness, 1.0, aniso_strength * aniso_strength * (1.0 - n_dot_v));
    }
    {% endif %}
    let prefiltered = samplePrefilteredEnv(R, ibl_roughness, ibl_filtered_env_tex, ibl_filtered_env_sampler, ibl_info);
    let brdf_lut = sampleBRDFLUT(n_dot_v, roughness, brdf_lut_tex, brdf_lut_sampler);
    // Apply occlusion to specular with reduced strength to avoid over-darkening reflections
    let specular = prefiltered * (F0 * brdf_lut.x + vec3<f32>(f90) * brdf_lut.y) * mix(1.0, color.occlusion, 0.5);

    // Sheen contribution for IBL (approximate) — compile-time gated; the
    // else keeps the unscaled base (sheen-absent scaling == 1).
    {% if pbr_features.sheen %}
    let sheen_scaling = sheen_albedo_scaling(color.sheen_color, color.sheen_roughness, n_dot_v);
    var base_with_sheen = base_contribution * sheen_scaling;
    let irradiance_sheen = sampleIrradiance(n, ibl_irradiance_tex, ibl_irradiance_sampler);
    let sheen_alpha = color.sheen_roughness * color.sheen_roughness;
    let fresnel_sheen = pow(1.0 - n_dot_v, 3.0); // Softer falloff
    let sheen_contrib = color.sheen_color * irradiance_sheen * sheen_alpha * fresnel_sheen * color.occlusion;
    base_with_sheen += sheen_contrib;
    {% else %}
    let base_with_sheen = base_contribution;
    {% endif %}

    var result = base_with_sheen + specular + color.emissive;

    // Clearcoat IBL layer — compile-time gated.
    {% if pbr_features.clearcoat %}
    let cc_n = safe_normalize(color.clearcoat_normal);
    let cc_n_dot_v = saturate(dot(cc_n, v));
    let cc_R = reflect(-v, cc_n);
    let cc_roughness = max(color.clearcoat_roughness, 0.04);
    // Sample prefiltered environment for clearcoat reflection
    let cc_prefiltered = samplePrefilteredEnv(cc_R, cc_roughness, ibl_filtered_env_tex, ibl_filtered_env_sampler, ibl_info);
    let cc_brdf_lut = sampleBRDFLUT(cc_n_dot_v, cc_roughness, brdf_lut_tex, brdf_lut_sampler);
    // Clearcoat specular (F0 = 0.04 for dielectric)
    let cc_specular = cc_prefiltered * (CLEARCOAT_F0 * cc_brdf_lut.x + cc_brdf_lut.y);
    // Clearcoat Fresnel attenuation — evaluated at the CLEARCOAT normal's
    // view cosine `cc_n_dot_v` (differs from base `n_dot_v` when a clearcoat
    // normal map is present).
    let cc_fresnel = clearcoat_fresnel(color.clearcoat, cc_n_dot_v);
    // Final: attenuated base + clearcoat
    result = result * (1.0 - cc_fresnel) + color.clearcoat * cc_specular;
    {% endif %}

    // Back-side diffuse transmission, added last so neither the sheen
    // albedo-scaling nor the clearcoat Fresnel attenuated it.
    return result + transmit_back;
}

// Standard IBL without explicit transmission background (uses IBL for transmission)
fn brdf_ibl(
    color: PbrMaterialColor,
    normal: vec3<f32>,
    surface_to_camera: vec3<f32>,
    ibl_filtered_env_tex: texture_cube<f32>,
    ibl_filtered_env_sampler: sampler,
    ibl_irradiance_tex: texture_cube<f32>,
    ibl_irradiance_sampler: sampler,
    brdf_lut_tex: texture_2d<f32>,
    brdf_lut_sampler: sampler,
    ibl_info: IblInfo
) -> vec3<f32> {
    // For IBL-only transmission, sample the environment in the refracted direction
    var transmission_background = vec3<f32>(0.0);

    let effective_transmission = color.transmission * (1.0 - clamp(color.metallic_roughness.x, 0.0, 1.0));

    if (effective_transmission > 0.0) {
        let n = safe_normalize(normal);
        let v = safe_normalize(surface_to_camera);
        let roughness = max(clamp(color.metallic_roughness.y, 0.0, 1.0), 0.04);

        // Determine sample direction for transmission
        var sample_dir = -v;  // Default: straight through (thin-walled)

        // If volumetric (thickness > 0), apply refraction
        let ior_val = effective_ior(color.ior);
        if (color.volume_thickness > 0.0 && ior_val != 1.0) {
            // KHR_materials_dispersion: when dispersion is non-zero, refract
            // per RGB channel so the transmitted background separates into
            // chromatic fringes. Half-spread matches glTF Sample Renderer
            // (`(ior - 1) * 0.025 * dispersion`), which keeps the offset
            // well-behaved across the typical Abbe range while still showing
            // through at the test asset's exaggerated `dispersion = 25`.
            //
            // Trade-off: the visible fringe strength is honest-to-physics
            // quiet at typical glass values (dispersion ≈ 0.3-0.7). Some
            // engines amplify this for artistic effect; we don't. If a game
            // wants showier chromatic aberration, the place to scale it is
            // here, not in the asset.
            if (color.dispersion > 0.0) {
                let dstrength = (ior_val - 1.0) * 0.025 * color.dispersion;
                let ior_r = max(ior_val - dstrength, 1.0001);
                let ior_b = ior_val + dstrength;
                let refracted_r = refract_direction(v, n, 1.0 / ior_r);
                let refracted_g = refract_direction(v, n, 1.0 / ior_val);
                let refracted_b = refract_direction(v, n, 1.0 / ior_b);
                let dir_r = select(-v, refracted_r, dot(refracted_r, refracted_r) > 1e-6);
                let dir_g = select(-v, refracted_g, dot(refracted_g, refracted_g) > 1e-6);
                let dir_b = select(-v, refracted_b, dot(refracted_b, refracted_b) > 1e-6);
                let s_r = samplePrefilteredEnv(dir_r, roughness, ibl_filtered_env_tex, ibl_filtered_env_sampler, ibl_info);
                let s_g = samplePrefilteredEnv(dir_g, roughness, ibl_filtered_env_tex, ibl_filtered_env_sampler, ibl_info);
                let s_b = samplePrefilteredEnv(dir_b, roughness, ibl_filtered_env_tex, ibl_filtered_env_sampler, ibl_info);
                transmission_background = vec3<f32>(s_r.r, s_g.g, s_b.b);
            } else {
                let refracted = refract_direction(v, n, 1.0 / ior_val);
                if (dot(refracted, refracted) > 1e-6) {
                    sample_dir = refracted;
                }
                transmission_background = samplePrefilteredEnv(
                    sample_dir,
                    roughness,
                    ibl_filtered_env_tex,
                    ibl_filtered_env_sampler,
                    ibl_info
                );
            }
        } else {
            // Sample environment with roughness-based blur
            transmission_background = samplePrefilteredEnv(
                sample_dir,
                roughness,
                ibl_filtered_env_tex,
                ibl_filtered_env_sampler,
                ibl_info
            );
        }
    }

    return brdf_ibl_with_transmission(
        color,
        normal,
        surface_to_camera,
        ibl_filtered_env_tex,
        ibl_filtered_env_sampler,
        ibl_irradiance_tex,
        ibl_irradiance_sampler,
        brdf_lut_tex,
        brdf_lut_sampler,
        ibl_info,
        transmission_background
    );
}
