// Image-based lighting primitive for CUSTOM (dynamic-WGSL) materials.
//
// Opt-in via the `ibl` shader-include. The single biggest "make a custom
// material first-class in a real (IBL-lit) scene" helper: the generic punctual
// light API (`light_access`) iterates only punctual lights, so a dynamic
// material in an environment-lit scene with no punctual lights renders ~black
// while built-in PBR meshes beside it are lit correctly. `sample_ibl` gives the
// environment (ambient) term using the SAME prefiltered-env / irradiance
// cubemaps + BRDF LUT the built-in PBR path uses (split-sum approximation).
//
// This is a general primitive, NOT a PBR re-implementation: it returns just the
// image-based ambient radiance for a surface; the caller composes it with any
// punctual lighting / emissive / custom shading it wants.
//
// Bindings (`ibl_irradiance_tex` / `ibl_filtered_env_tex` / `brdf_lut_tex` + their
// samplers) are part of the always-declared kernel ABI, so this helper costs
// nothing until a material's WGSL actually calls it. `get_lights_info()` (from
// `light_access`, a declared dependency of `ibl`) supplies the cubemap mip counts.

/// Diffuse irradiance for normal `n` (already π-corrected for a Lambertian BRDF).
fn sample_ibl_diffuse(n: vec3<f32>) -> vec3<f32> {
    return textureSampleLevel(ibl_irradiance_tex, ibl_irradiance_sampler, normalize(n), 0.0).rgb * PI;
}

/// Prefiltered specular radiance along reflection vector `r` at `roughness`
/// (selects the prefiltered-env mip from the scene's IBL info).
fn sample_ibl_specular(r: vec3<f32>, roughness: f32) -> vec3<f32> {
    let info = get_lights_info();
    let max_mip = f32(max(info.ibl.prefiltered_env_mip_count, 1u) - 1u);
    return textureSampleLevel(
        ibl_filtered_env_tex,
        ibl_filtered_env_sampler,
        normalize(r),
        saturate(roughness) * max_mip,
    ).rgb;
}

/// Split-sum image-based lighting for a surface: diffuse irradiance × albedo +
/// prefiltered specular × (F0·scale + bias) from the BRDF LUT. `metallic` blends
/// the Fresnel F0 toward `albedo` and removes the diffuse term (metals have no
/// diffuse). The full ambient/environment contribution; add your punctual /
/// emissive terms on top.
fn sample_ibl(
    albedo: vec3<f32>,
    normal: vec3<f32>,
    surface_to_camera: vec3<f32>,
    roughness: f32,
    metallic: f32,
) -> vec3<f32> {
    let n = normalize(normal);
    let v = normalize(surface_to_camera);
    let n_dot_v = max(dot(n, v), 1e-4);
    let r = reflect(-v, n);

    let irradiance = sample_ibl_diffuse(n);
    let prefiltered = sample_ibl_specular(r, roughness);
    let lut = textureSampleLevel(
        brdf_lut_tex,
        brdf_lut_sampler,
        vec2<f32>(saturate(n_dot_v), saturate(roughness)),
        0.0,
    ).rg;

    let f0 = mix(vec3<f32>(0.04), albedo, metallic);
    let diffuse = irradiance * albedo * (1.0 - metallic);
    let specular = prefiltered * (f0 * lut.x + vec3<f32>(lut.y));
    return diffuse + specular;
}
