// brdf_pbr.wgsl — Tier B (PBR-internal) BRDF orchestrators.
// -------------------------------------------------------------
// These take the `PbrMaterialColor` type and drive the lobes in brdf_primitives.wgsl
// (which MUST be included before this file). NEVER reachable by a Custom material —
// emitted only for the PBR base. See the taxonomy in awsm-materials::shader_includes.
// -------------------------------------------------------------
{% if write_ssr_descriptor %}
// ssr-spread-gate: spread at-or-above this keeps FULL IBL specular; below it
// SSR owns the reflection (trace hit OR skybox env fallback on miss) and the
// IBL specular is suppressed in proportion to the reflectivity handed to SSR,
// so environment reflections aren't counted twice. Keep in sync with the
// same-named constant in ssr_wgsl/resolve.wgsl (travel-blur ramp) and
// ssr_wgsl/temporal.wgsl (history-blend ramp) — grep "ssr-spread-gate".
// Compiled ONLY when the material writes the SSR descriptor
// (write_ssr_descriptor), so SSR-off builds are byte-identical.
const SSR_SPREAD_GATE: f32 = 0.15;
{% endif %}
// Direct Lighting BRDF (Cook-Torrance)
// With clearcoat and sheen extensions
// -------------------------------------------------------------
fn brdf_direct(color: PbrMaterialColor, light_brdf: LightSample, surface_to_camera: vec3<f32>) -> vec3<f32> {
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
    {% if write_ssr_descriptor %}
    // ssr-spread-gate (wgsl_validation pins this term): SSR is on and this
    // surface writes the reflection descriptor, so for low-spread (near-
    // mirror) pixels SSR supplies the reflection — scene geometry on a hit,
    // the skybox env fallback on a miss. Adding the prefiltered-env IBL
    // specular on top would double-count the environment (washed-out mirror
    // images), so scale it down by the reflectivity actually handed to SSR.
    // `ssr_f0`/`ssr_spread` mirror `ssr_pbr_descriptor` in compute.wgsl (the
    // exact values the descriptor stores): F0 = mix(0.04, base, metallic),
    // spread = raw GGX roughness (NOT the 0.04-floored `roughness` above —
    // a mirror's spread is exactly 0). Mirrors (spread 0, mask→1) fully
    // suppress; by spread ≥ SSR_SPREAD_GATE the IBL specular is fully back
    // (matching the resolve/temporal ramps); diffuse IBL is untouched.
    let ssr_f0 = mix(vec3<f32>(0.04), base_color, metallic);
    let ssr_mask_factor = max(ssr_f0.r, max(ssr_f0.g, ssr_f0.b));
    let ssr_spread = saturate(color.metallic_roughness.y);
    let ssr_ibl_keep = 1.0 - ssr_mask_factor * (1.0 - smoothstep(0.0, SSR_SPREAD_GATE, ssr_spread));
    {% endif %}
    // Apply occlusion to specular with reduced strength to avoid over-darkening reflections
    let specular = prefiltered * (F0 * brdf_lut.x + vec3<f32>(f90) * brdf_lut.y) * mix(1.0, color.occlusion, 0.5){% if write_ssr_descriptor %} * ssr_ibl_keep{% endif %};

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
