//! Static WGSL validation of the rendered material shaders, via `naga`.
//!
//! `shader_completeness` only checks `<base>_get_material(` calls; it does NOT
//! catch the general "templated code calls a function the gated includes no
//! longer define" hazard that the include-gating work introduces. Those breaks otherwise
//! surface only at pipeline-compile time in a real browser — and the
//! Custom-only benchmark never exercises the first-party PBR/Toon/Unlit/Flipbook
//! kernels at all.
//!
//! These tests render each material template and run it through naga's WGSL
//! front-end + validator (Capabilities::all, so storage textures / texture
//! arrays / etc. are permitted). naga is a proxy for Chrome's Tint, not
//! identical — but it reliably catches undefined-function / undefined-type /
//! type-mismatch errors, which is exactly the Phase-4 regression class. Runs
//! natively under `cargo test`, no GPU.

#![cfg(test)]

use awsm_renderer_materials::MaterialShaderId;

use crate::dynamic_materials::{BucketEntry, ShadingBase};
use crate::render_passes::material_decal::shader::cache_key::ShaderCacheKeyMaterialDecal;
use crate::render_passes::material_decal::shader::template::ShaderTemplateMaterialDecal;
use crate::render_passes::material_opaque::shader::cache_key::{
    DynamicShaderInfo, ShaderCacheKeyMaterialOpaque,
};
use crate::render_passes::material_opaque::shader::template::ShaderTemplateMaterialOpaque;
use crate::render_passes::material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent;
use crate::render_passes::material_transparent::shader::template::ShaderTemplateMaterialTransparent;
use crate::render_passes::shared::material::cache_key::ShaderMaterialVertexAttributes;

/// Parse + validate `src` with naga; panic with a readable error on failure.
fn naga_validate(src: &str, label: &str) {
    let module = match naga::front::wgsl::parse_str(src) {
        Ok(m) => m,
        Err(e) => panic!(
            "{label}: naga WGSL PARSE failed:\n{}",
            e.emit_to_string(src)
        ),
    };
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    if let Err(e) = validator.validate(&module) {
        panic!(
            "{label}: naga WGSL VALIDATION failed:\n{}",
            e.emit_to_string(src)
        );
    }
}

fn first_party_key(
    shader_id: MaterialShaderId,
    base: ShadingBase,
    owns_skybox: bool,
    msaa: Option<u32>,
    mipmaps: bool,
) -> ShaderCacheKeyMaterialOpaque {
    first_party_key_prep(shader_id, base, owns_skybox, msaa, mipmaps)
}

// Prep is unconditional now, so this is identical to `first_party_key`; kept as
// a named alias for the prep-read test sites that document intent.
fn first_party_key_prep(
    shader_id: MaterialShaderId,
    base: ShadingBase,
    owns_skybox: bool,
    msaa: Option<u32>,
    mipmaps: bool,
) -> ShaderCacheKeyMaterialOpaque {
    ShaderCacheKeyMaterialOpaque {
        write_ssr_descriptor: false,
        reverse_z: false,
        texture_pool_arrays_len: 1,
        texture_pool_samplers_len: 1,
        msaa_sample_count: msaa,
        mipmaps,
        max_shadow_casters: 4,
        sscs_enabled: false,
        sscs_step_count: 16,
        shader_id,
        base,
        owns_skybox,
        pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
        dispatch_hash: 0,
        dynamic_shader: None,
        bucket_entries: crate::dynamic_materials::first_party_bucket_entries(),
    }
}

fn custom_key(
    includes: awsm_renderer_materials::ShaderIncludes,
    msaa: Option<u32>,
    mipmaps: bool,
) -> ShaderCacheKeyMaterialOpaque {
    let dyn_id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
    let mut bucket_entries = crate::dynamic_materials::first_party_bucket_entries();
    bucket_entries.push(BucketEntry {
        shader_id: dyn_id,
        base: ShadingBase::Custom,
        pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
        name: "noise".to_string(),
    });
    ShaderCacheKeyMaterialOpaque {
        write_ssr_descriptor: false,
        reverse_z: false,
        texture_pool_arrays_len: 1,
        texture_pool_samplers_len: 1,
        msaa_sample_count: msaa,
        mipmaps,
        max_shadow_casters: 4,
        sscs_enabled: false,
        sscs_step_count: 16,
        shader_id: dyn_id,
        base: ShadingBase::Custom,
        owns_skybox: false,
        pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
        dispatch_hash: 1,
        dynamic_shader: Some(DynamicShaderInfo {
            shader_includes: includes,
            struct_decl: "struct MaterialData { _pad: u32, };".to_string(),
            loader_decl:
                "fn material_data_load(byte_offset: u32) -> MaterialData { return MaterialData(0u); }"
                    .to_string(),
            // Reads only world_normal/position (always provided) — declares no
            // includes, so it exercises the leanest Custom kernel.
            wgsl_fragment: "return OpaqueShadingOutput(input.world_normal * 0.5 + 0.5, 1.0);"
                .to_string(),
        }),
        bucket_entries,
    }
}

fn render(key: &ShaderCacheKeyMaterialOpaque, label: &str) -> String {
    ShaderTemplateMaterialOpaque::try_from(key)
        .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
        .into_source()
        .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"))
}

const CONFIGS: [(Option<u32>, bool); 3] = [(None, true), (None, false), (Some(4), true)];

#[test]
fn sscs_enabled_shaders_validate() {
    // Every other validation test renders the SSCS-OFF variant (sscs_available is
    // false → `apply_sscs` compiles to `return 1.0`). This one exercises the
    // SSCS-ON path: the compile-time `{{ sscs_step_count }}` march bound and the
    // `sscs_params` uniform reads must produce valid WGSL across step counts
    // (incl. the clamped-minimum 1) for both prep and opaque.
    use crate::render_passes::material_prep::shader::cache_key::ShaderCacheKeyMaterialPrep;
    use crate::render_passes::material_prep::shader::template::ShaderTemplateMaterialPrep;

    for step_count in [1u32, 8, 32] {
        for msaa in [None, Some(4u32)] {
            // Prep owns the punctual + directional `apply_sscs` call sites.
            let label = format!("sscs-on prep step={step_count} msaa={msaa:?}");
            let src = ShaderTemplateMaterialPrep::try_from(&ShaderCacheKeyMaterialPrep {
                msaa_sample_count: msaa,
                max_shadow_casters: 4,
                sscs_enabled: true,
                sscs_step_count: step_count,
                reverse_z: false,
            })
            .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
            .into_source()
            .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
            naga_validate(&src, &label);
            assert!(
                src.contains("sscs_params"),
                "{label}: SSCS-on body should read the sscs_params uniform"
            );

            // Opaque (first-party PBR bucket) with SSCS enabled.
            let label = format!("sscs-on opaque step={step_count} msaa={msaa:?}");
            let mut key =
                first_party_key(MaterialShaderId::PBR, ShadingBase::Pbr, false, msaa, false);
            key.sscs_enabled = true;
            key.sscs_step_count = step_count;
            naga_validate(&render(&key, &label), &label);
        }
    }
}

#[test]
fn reverse_z_shadow_shaders_validate() {
    // 003 stage 7: the shared shadow receiver
    // (`shared_wgsl/shadow/bind_groups.wgsl`) grew `{% if reverse_z %}` arms —
    // cube-face NDC.z reconstruction, ref-depth bias directions, PCSS blocker
    // tests + penumbra math, and the EVSM remap. Every other validation test
    // renders `reverse_z: false`, so without this test a syntax/type error in
    // a reverse arm would only surface at pipeline compile in a real browser.
    use crate::render_passes::material_prep::shader::cache_key::ShaderCacheKeyMaterialPrep;
    use crate::render_passes::material_prep::shader::template::ShaderTemplateMaterialPrep;

    // The forward/reverse cube-face NDC.z formulas — exactly one must be
    // compiled in, matching the writer projection the convention builds.
    const CUBE_NDC_FORWARD: &str =
        "(range / (range - near)) * (1.0 - near / max(view_depth, near))";
    const CUBE_NDC_REVERSE: &str =
        "(near / (range - near)) * (range / max(view_depth, near) - 1.0)";

    for msaa in [None, Some(4u32)] {
        // Prep owns the shadow-sampling call sites (SSCS on to compile that
        // branch's reverse-z sentinel too).
        let label = format!("reverse-z prep msaa={msaa:?}");
        let src = ShaderTemplateMaterialPrep::try_from(&ShaderCacheKeyMaterialPrep {
            msaa_sample_count: msaa,
            max_shadow_casters: 4,
            sscs_enabled: true,
            sscs_step_count: 16,
            reverse_z: true,
        })
        .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
        .into_source()
        .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
        naga_validate(&src, &label);
        assert!(
            src.contains(CUBE_NDC_REVERSE) && !src.contains(CUBE_NDC_FORWARD),
            "{label}: reverse-z module must compile the REVERSE cube NDC.z arm only"
        );

        // Opaque + transparent compile the same shared receiver include.
        let mut key = first_party_key(MaterialShaderId::PBR, ShadingBase::Pbr, false, msaa, false);
        key.reverse_z = true;
        let label = format!("reverse-z opaque msaa={msaa:?}");
        naga_validate(&render(&key, &label), &label);

        let mut tkey = transparent_first_party_key(ShadingBase::Pbr, msaa);
        tkey.reverse_z = true;
        let label = format!("reverse-z transparent msaa={msaa:?}");
        let src = ShaderTemplateMaterialTransparent::try_from(&tkey)
            .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
            .into_source()
            .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
        naga_validate(&src, &label);
    }

    // Forward stays forward: the default-off path must keep the forward arm.
    let label = "forward-z prep (control)";
    let src = ShaderTemplateMaterialPrep::try_from(&ShaderCacheKeyMaterialPrep {
        msaa_sample_count: None,
        max_shadow_casters: 4,
        sscs_enabled: false,
        sscs_step_count: 16,
        reverse_z: false,
    })
    .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
    .into_source()
    .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
    assert!(
        src.contains(CUBE_NDC_FORWARD) && !src.contains(CUBE_NDC_REVERSE),
        "{label}: forward module must compile the FORWARD cube NDC.z arm only"
    );
}

#[test]
fn first_party_opaque_shaders_validate() {
    let bases = [
        (MaterialShaderId::PBR, ShadingBase::Pbr, false, "pbr"),
        (MaterialShaderId::UNLIT, ShadingBase::Unlit, false, "unlit"),
        (MaterialShaderId::TOON, ShadingBase::Toon, false, "toon"),
        (
            MaterialShaderId::FLIPBOOK,
            ShadingBase::Flipbook,
            false,
            "flipbook",
        ),
        (MaterialShaderId::SKYBOX, ShadingBase::Pbr, true, "skybox"),
    ];
    for (id, base, owns_skybox, name) in bases {
        for (msaa, mips) in CONFIGS {
            let label = format!("opaque/{name} msaa={msaa:?} mips={mips}");
            let src = render(&first_party_key(id, base, owns_skybox, msaa, mips), &label);
            naga_validate(&src, &label);
            // Compile invariant (David): a module carries ONLY the entry points
            // its AA config dispatches. Non-MSAA → `cs_opaque` (render() path);
            // MSAA → `cs_shade` (render_shade), NEVER `cs_opaque`. naga only checks
            // the module compiles, not that the requested entry point exists; a
            // missing one fails at pipeline-create on a real GPU (it's how the
            // skybox writer's `fn main` slipped through the 1024 unification until
            // model-tests caught it). So assert the RIGHT kernel per config + the
            // ABSENCE of the cross-AA kernel.
            if msaa.is_some() {
                assert!(
                    src.contains("fn cs_shade(") && !src.contains("fn cs_opaque("),
                    "{label}: MSAA opaque module must define `fn cs_shade` and NOT `fn cs_opaque`"
                );
            } else {
                assert!(
                    src.contains("fn cs_opaque(") && !src.contains("fn cs_shade("),
                    "{label}: non-MSAA opaque module must define `fn cs_opaque` and NOT `fn cs_shade`"
                );
            }
        }
    }
}

#[test]
fn ssr_ibl_suppression_gated_on_descriptor_axis() {
    // Mirror-correctness (SSR feature B): when the opaque module writes the
    // SSR reflection descriptor (`write_ssr_descriptor` — true exactly when
    // SSR is enabled), the shared `brdf_pbr.wgsl` must suppress the IBL
    // SPECULAR term for low-spread surfaces (SSR supplies the reflection —
    // trace hit or skybox env fallback — so adding prefiltered-env specular
    // would double-count the environment). When the axis is OFF, the module
    // must not reference the suppression at all (SSR-off builds stay
    // byte-identical). The `ssr-spread-gate` tag marks every copy of the
    // shared 0.15 constant (resolve / temporal / brdf_pbr).
    const SUPPRESSION_TERM: &str =
        "let ssr_ibl_keep = 1.0 - ssr_mask_factor * (1.0 - smoothstep(0.0, SSR_SPREAD_GATE, ssr_spread));";
    for (msaa, mips) in CONFIGS {
        // ON: PBR opaque with the descriptor axis set.
        let mut key = first_party_key(MaterialShaderId::PBR, ShadingBase::Pbr, false, msaa, mips);
        key.write_ssr_descriptor = true;
        let label = format!("opaque/pbr ssr-descriptor msaa={msaa:?} mips={mips}");
        let src = render(&key, &label);
        naga_validate(&src, &label);
        assert!(
            src.contains("ssr-spread-gate"),
            "{label}: write_ssr_descriptor module must carry the `ssr-spread-gate` \
             IBL-specular suppression"
        );
        assert!(
            src.contains(SUPPRESSION_TERM),
            "{label}: write_ssr_descriptor module must compute the exact suppression term \
             `{SUPPRESSION_TERM}`"
        );
        assert!(
            src.contains("* ssr_ibl_keep"),
            "{label}: the IBL specular term must be scaled by `ssr_ibl_keep`"
        );
        // The descriptor store must be present too (the axis' original job) —
        // and its reflectivity must be scaled by the writing material's MSAA
        // sample coverage (comment-pinned in compute.wgsl): an un-scaled
        // single-sample descriptor makes the SSR composite add all-or-nothing
        // reflection along silhouettes, visibly undoing MSAA.
        assert!(
            src.contains("textureStore(reflection_descriptor_tex"),
            "{label}: write_ssr_descriptor module must store the reflection descriptor"
        );
        assert!(
            src.contains("fn ssr_descriptor_coverage(coords: vec2<i32>, mat_offset: u32) -> f32 {")
                && src.contains("vec4<f32>(ssr_reflectivity * ssr_cov, ssr_spread)"),
            "{label}: descriptor reflectivity must be scaled by the material's \
             MSAA sample coverage (ssr_descriptor_coverage)"
        );

        // OFF: the identical key without the axis must not reference any of it.
        let off_key = first_party_key(MaterialShaderId::PBR, ShadingBase::Pbr, false, msaa, mips);
        let off_label = format!("opaque/pbr no-ssr msaa={msaa:?} mips={mips}");
        let off = render(&off_key, &off_label);
        assert!(
            !off.contains("ssr-spread-gate") && !off.contains("ssr_ibl_keep"),
            "{off_label}: SSR-off module must NOT reference the IBL suppression \
             (byte-identical SSR-off builds)"
        );
    }

    // Transparent surfaces never write the descriptor — their brdf include
    // must never carry the suppression, SSR on or off.
    let tkey = transparent_first_party_key(ShadingBase::Pbr, None);
    let tsrc = ShaderTemplateMaterialTransparent::try_from(&tkey)
        .expect("transparent template builds")
        .into_source()
        .expect("transparent renders");
    assert!(
        !tsrc.contains("ssr-spread-gate") && !tsrc.contains("ssr_ibl_keep"),
        "transparent/pbr must NOT compile the SSR IBL suppression (no descriptor writes)"
    );
}

#[test]
fn toon_shader_is_banded_and_gated() {
    // §19 regression guard: `first_party_opaque_shaders_validate` proves Toon
    // COMPILES, but not that it still cel-SHADES. A refactor could drop the
    // banding and toon would silently render like smooth PBR (and still validate).
    // Assert the assembled Toon module carries the banded shading branch AND its
    // quantizer; and that a non-Toon base (Unlit) does NOT invoke the toon branch.
    let toon = render(
        &first_party_key(
            MaterialShaderId::TOON,
            ShadingBase::Toon,
            false,
            None,
            false,
        ),
        "opaque/toon",
    );
    assert!(
        toon.contains("compute_toon_lit_color"),
        "toon base lost its shading branch (compute_toon_lit_color)"
    );
    assert!(
        toon.contains("fn toon_quantize"),
        "toon lost its cel-shading BANDING (toon_quantize) — would render smooth"
    );
    assert!(
        toon.contains("floor("),
        "toon banding quantizer (floor) missing"
    );

    // The toon shading branch is base-gated: a non-Toon base must not call it.
    let unlit = render(
        &first_party_key(
            MaterialShaderId::UNLIT,
            ShadingBase::Unlit,
            false,
            None,
            false,
        ),
        "opaque/unlit",
    );
    assert!(
        !unlit.contains("compute_toon_lit_color"),
        "non-Toon (Unlit) base wrongly assembled the toon shading branch"
    );
}

#[test]
fn unified_shade_opaque_shaders_validate() {
    // U1 (`docs/plans/unified-edge-shading.md`): under MSAA the opaque module
    // emits the merged `cs_shade` entry point (interior sample-0 → opaque_tex +
    // edge per-sample → accumulator) + the `edge_id_tex` group(3) binding it
    // reads. naga-validate it across every base (incl SKYBOX + Custom) × mips
    // on/off — cs_shade is MSAA-only (there are no edges otherwise), so only the
    // MSAA config carries it. Prep is unconditional (the opaque path is
    // prep-only), so there is no prep on/off axis. Asserts the entry point
    // exists (the dispatch selects it by name → pipeline-create would fail on
    // GPU if absent) and that the cs_opaque entry point still coexists (the
    // no-MSAA interior path).
    let bases = [
        (MaterialShaderId::PBR, ShadingBase::Pbr, false, "pbr"),
        (MaterialShaderId::UNLIT, ShadingBase::Unlit, false, "unlit"),
        (MaterialShaderId::TOON, ShadingBase::Toon, false, "toon"),
        (
            MaterialShaderId::FLIPBOOK,
            ShadingBase::Flipbook,
            false,
            "flipbook",
        ),
        (MaterialShaderId::SKYBOX, ShadingBase::Pbr, true, "skybox"),
    ];
    for (id, base, owns_skybox, name) in bases {
        for mips in [false, true] {
            let key = first_party_key_prep(id, base, owns_skybox, Some(4), mips);
            let label = format!("opaque-unified/{name} msaa=4 mips={mips}");
            let src = render(&key, &label);
            naga_validate(&src, &label);
            assert!(
                src.contains("fn cs_shade("),
                "{label}: unified opaque module missing `fn cs_shade` entry point \
                 (dispatch requests it → pipeline-create would fail on GPU)"
            );
            // Invariant (A2): under MSAA the module is cs_shade ONLY — no
            // `cs_opaque` (the no-MSAA interior entry is never compiled here).
            assert!(
                !src.contains("fn cs_opaque("),
                "{label}: MSAA module must NOT carry `fn cs_opaque` (cross-AA code)"
            );
            // The edge-id texture binding cs_shade reads must be declared.
            assert!(
                src.contains("var edge_id_tex: texture_storage_2d<r32uint, read>"),
                "{label}: unified module missing the read-only `edge_id_tex` binding"
            );
        }
    }

    // Custom (dynamic) base under MSAA + unified — exercises the cs_shade
    // dynamic-wrapper arm (custom_shade_dynamic from both interior + edge).
    for mips in [false, true] {
        let key = custom_key(
            awsm_renderer_materials::ShaderIncludes::all(),
            Some(4),
            mips,
        );
        let label = format!("opaque-unified/custom msaa=4 mips={mips}");
        let src = render(&key, &label);
        naga_validate(&src, &label);
        assert!(
            src.contains("fn cs_shade("),
            "{label}: unified Custom module missing `fn cs_shade`"
        );
    }
}

#[test]
fn custom_material_ibl_include_validates() {
    // D1: a custom material that opts into the `ibl` include can call
    // `sample_ibl(...)` and the assembled Custom kernel must validate (the helper
    // references the always-bound IBL cubemaps/LUT + `get_lights_info`). Without
    // the include the symbol is undefined → this guards the wiring.
    use awsm_renderer_materials::ShaderIncludes;
    for (msaa, mips) in CONFIGS {
        let mut key = custom_key(ShaderIncludes::IBL, msaa, mips);
        key.dynamic_shader.as_mut().unwrap().wgsl_fragment =
            "let ibl = sample_ibl(vec3<f32>(0.8, 0.8, 0.8), input.world_normal, \
             input.surface_to_camera, 0.3, 0.0); return OpaqueShadingOutput(ibl, 1.0);"
                .to_string();
        let label = format!("opaque/custom ibl msaa={msaa:?} mips={mips}");
        let src = render(&key, &label);
        naga_validate(&src, &label);
        assert!(
            src.contains("fn sample_ibl("),
            "{label}: `ibl` include did not emit sample_ibl"
        );
    }
}

#[test]
fn custom_material_normal_map_include_validates() {
    // D1-normalmap: a custom material that opts into `normal_map` can call
    // `apply_normal_map(...)` / `material_tbn(...)` over the always-present
    // world_tangent/world_bitangent/world_normal fields, and the assembled Custom
    // kernel must validate. Without the include the symbols are undefined → guards
    // both the include wiring AND that the OpaqueShadingInput tangent fields exist.
    use awsm_renderer_materials::ShaderIncludes;
    for (msaa, mips) in CONFIGS {
        let mut key = custom_key(ShaderIncludes::NORMAL_MAP, msaa, mips);
        key.dynamic_shader.as_mut().unwrap().wgsl_fragment =
            "let n = apply_normal_map(input, vec3<f32>(0.6, 0.5, 0.9)); \
             let _tbn = material_tbn(input); return OpaqueShadingOutput(n * 0.5 + 0.5, 1.0);"
                .to_string();
        let label = format!("opaque/custom normal_map msaa={msaa:?} mips={mips}");
        let src = render(&key, &label);
        naga_validate(&src, &label);
        assert!(
            src.contains("fn apply_normal_map("),
            "{label}: `normal_map` include did not emit apply_normal_map"
        );
    }
}

#[test]
fn opaque_prep_read_variant_validates() {
    // Plan B (stage 2b): the prep-read opaque variant (prep enabled + MSAA
    // off) must compile, and `texture_uv()` / `vertex_color()` must read the
    // prep array textures via `textureLoad` instead of recomputing from the
    // geometry pool. PBR exercises both helpers. Mirrors
    // `first_party_opaque_shaders_validate`'s helper, kept minimal.
    // mips OFF: the gradient mipmap path (`get_uv_derivatives`) is the one
    // remaining first-party caller of `_texture_uv_per_vertex` (UV gradients
    // are recomputed, never materialized — Plan B decision #2), so the
    // recompute-helper drop is observable only on the no-mips PBR variant.
    let key = first_party_key_prep(
        MaterialShaderId::PBR,
        ShadingBase::Pbr,
        false,
        None,  // no MSAA → prep_read = true
        false, // no mips → no get_uv_derivatives caller of _texture_uv_per_vertex
    );
    let src = render(&key, "opaque/pbr prep_read");
    naga_validate(&src, "opaque/pbr prep_read");
    assert!(
        src.contains("fn cs_opaque("),
        "prep_read opaque module missing `fn cs_opaque`"
    );
    // The prep-materialized array textures must be both declared and read.
    assert!(
        src.contains("textureLoad(prep_uv,"),
        "prep_read opaque module should `textureLoad(prep_uv, ...)` in texture_uv()"
    );
    assert!(
        src.contains("textureLoad(prep_vcolor,"),
        "prep_read opaque module should `textureLoad(prep_vcolor, ...)` in vertex_color()"
    );
    // The geometry-pool recompute helpers must NOT be emitted (the size win).
    assert!(
        !src.contains("fn _texture_uv_per_vertex("),
        "prep_read opaque module should drop the `_texture_uv_per_vertex` recompute helper"
    );
    assert!(
        !src.contains("fn _vertex_color_per_vertex("),
        "prep_read opaque module should drop the `_vertex_color_per_vertex` recompute helper"
    );
}

#[test]
fn opaque_shadow_from_buffer_variant_validates() {
    // Plan B (stage 4): the PBR opaque kernel (prep is unconditional) + MSAA off
    // reads the prep pass's per-pixel shadow-visibility buffer instead of
    // sampling shadow maps inline. Assert it (a) validates, (b) reads
    // `prep_shadow_visibility` via textureLoad, and (c) DROPS the inline
    // `sample_shadow_directional` definition (the ~50 KB win). The prep-OFF
    // controls are gone — the opaque path is prep-only now. Mirrors
    // `opaque_prep_read_variant_validates`.
    let prep_key = first_party_key_prep(
        MaterialShaderId::PBR,
        ShadingBase::Pbr,
        false,
        None, // no MSAA → prep_read = true → shadow_from_buffer = true (PBR lights)
        true,
    );
    let src = render(&prep_key, "opaque/pbr shadow_from_buffer");
    naga_validate(&src, "opaque/pbr shadow_from_buffer");
    assert!(
        src.contains("fn cs_opaque("),
        "shadow_from_buffer opaque module missing `fn cs_opaque`"
    );
    // (b) reads the prep shadow buffer.
    assert!(
        src.contains("textureLoad(prep_shadow_visibility"),
        "shadow_from_buffer opaque module should `textureLoad(prep_shadow_visibility, ...)`"
    );
    assert!(
        src.contains("var prep_shadow_visibility: texture_2d_array<f32>")
            || src.contains("prep_shadow_visibility: texture_2d_array<f32>"),
        "shadow_from_buffer opaque module should declare `prep_shadow_visibility` (binding 26)"
    );
    // (c) the inline shadow sampler is dropped (the size win).
    assert!(
        !src.contains("fn sample_shadow_directional"),
        "shadow_from_buffer opaque module should DROP `fn sample_shadow_directional` (the inline sampler)"
    );
    // (d) the cascade-debug overlay SURVIVES the drop: it's a shading-time
    // colour op reading only the always-bound shadow uniforms, emitted
    // ungated (outside the `needs_shadow_sampling` block). Regression guard
    // for `debug_cascade_colors` producing zero pixel change — the overlay
    // used to sit inside the sampling block, so prep-only opaque (which is
    // ALL opaque) compiled it out.
    assert!(
        src.contains("fn debug_cascade_tint("),
        "shadow_from_buffer opaque module must KEEP `fn debug_cascade_tint` (cascade-debug overlay)"
    );
    assert!(
        src.contains("color = debug_cascade_tint("),
        "shadow_from_buffer opaque module must CALL the cascade-debug overlay from apply_lighting"
    );

    // Control 2 (stage 5b-shadow): MSAA on ⇒ cs_opaque (PRIMARY) reads
    // the full-screen prep buffer AND cs_edge (EDGE) reads the compact
    // per-edge-sample buffer — so NOTHING inline-samples shadows, and the inline
    // `sample_shadow_directional` DROPS from the MSAA module (the MSAA analog of
    // stage 4's no-MSAA win). The recompute UV/vcolor helpers STAY (cs_edge still
    // recomputes attributes — 5b-attrs deferred).
    let msaa_key = first_party_key_prep(
        MaterialShaderId::PBR,
        ShadingBase::Pbr,
        false,
        Some(4), // MSAA on → prep_present = true, needs_shadow_sampling = false (5b)
        true,
    );
    let msaa_src = render(&msaa_key, "opaque/pbr prep-on msaa4");
    naga_validate(&msaa_src, "opaque/pbr prep-on msaa4");
    // (5b-shadow) The inline sampler is DROPPED under MSAA+prep — the ~50 KB win.
    assert!(
        !msaa_src.contains("fn sample_shadow_directional"),
        "MSAA+prep PBR opaque must DROP inline `fn sample_shadow_directional` (5b: cs_edge reads the compact edge-shadow buffer)"
    );
    // The cascade-debug overlay survives the 5b drop too (same guard as the
    // no-MSAA (d) assert above).
    assert!(
        msaa_src.contains("fn debug_cascade_tint(")
            && msaa_src.contains("color = debug_cascade_tint("),
        "MSAA+prep PBR opaque must KEEP + CALL `debug_cascade_tint` (cascade-debug overlay)"
    );
    // cs_opaque (PRIMARY) reads the full-screen buffer; cs_edge (EDGE) reads the
    // compact per-edge-sample buffer. Both reads must be present.
    assert!(
        msaa_src.contains("textureLoad(prep_shadow_visibility"),
        "MSAA+prep PBR opaque cs_opaque (PRIMARY) must READ the full-screen prep shadow buffer"
    );
    assert!(
        msaa_src.contains("textureLoad(prep_edge_shadow"),
        "MSAA+prep PBR opaque cs_edge (EDGE) must READ the compact per-edge-sample shadow buffer"
    );
    assert!(
        msaa_src.contains("var prep_edge_shadow: texture_2d_array<f32>")
            || msaa_src.contains("prep_edge_shadow: texture_2d_array<f32>"),
        "MSAA+prep PBR opaque must declare `prep_edge_shadow` (binding 27)"
    );
    // The shared PrepReadContext mode-select must carry the EDGE arm (the
    // abstraction that lets cs_opaque read PRIMARY while cs_edge reads EDGE — no
    // forked copies).
    assert!(
        msaa_src.contains("g_prep_ctx.mode == PREP_MODE_EDGE"),
        "MSAA+prep PBR opaque must branch the shared shadow read on the EDGE mode"
    );
    assert!(
        msaa_src.contains("textureLoad(prep_uv,") && msaa_src.contains("textureLoad(prep_vcolor,"),
        "MSAA+prep PBR opaque cs_opaque (PRIMARY) must read the prep UV/vcolor arrays"
    );
    // The recompute helpers STAY under MSAA+prep (cs_edge recomputes UV/vcolor —
    // 5b-attrs deferred).
    assert!(
        msaa_src.contains("fn _texture_uv_per_vertex(")
            && msaa_src.contains("fn _vertex_color_per_vertex("),
        "MSAA+prep PBR opaque must KEEP the recompute helpers (cs_edge recomputes attrs; 5b-attrs deferred)"
    );

    // Control 3 (stage 5b-shadow): MSAA OFF still keeps the inline sampler
    // DROPPED (stage 4) and reads only the full-screen buffer (no edges → no
    // compact edge buffer / no EDGE read).
    let no_msaa_src = render(&prep_key, "opaque/pbr prep-on no-msaa");
    assert!(
        !no_msaa_src.contains("textureLoad(prep_edge_shadow"),
        "no-MSAA PBR opaque must NOT read the compact edge buffer (no edges)"
    );

    // Measurement: report the prep (no-MSAA shadow-from-buffer) PBR size and the
    // MSAA module size. The prep-OFF baselines are gone (opaque is prep-only).
    eprintln!(
        "[stage4] PBR opaque no-MSAA — prep-read(shadow_from_buffer): {} B",
        src.len(),
    );
    eprintln!(
        "[stage5b] PBR opaque MSAA4 — prep-on(edge-shadow buffer): {} B",
        msaa_src.len(),
    );
}

/// Render the material-classify shader (bind groups + compute concatenated)
/// for a given config. Mirrors the renderer's `ShaderTemplateMaterialClassify`
/// build path so the gating is exercised exactly as the pipeline cache does.
fn render_classify(msaa: Option<u32>, emit_edge_data: bool, label: &str) -> String {
    use crate::render_passes::material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify;
    use crate::render_passes::material_classify::shader::template::ShaderTemplateMaterialClassify;
    ShaderTemplateMaterialClassify::try_from(&ShaderCacheKeyMaterialClassify {
        msaa_sample_count: msaa,
        bucket_count: crate::dynamic_materials::first_party_bucket_entries().len() as u32,
        emit_edge_data,
    })
    .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
    .into_source()
    .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"))
}

#[test]
fn material_classify_shader_validates() {
    // U0 (`docs/plans/unified-edge-shading.md`): the classify shader must
    // naga-validate per (msaa, emit) config, including the MSAA edge path.
    for (msaa, emit) in [(None, false), (Some(4u32), true)] {
        let label = format!("classify msaa={msaa:?} emit={emit}");
        let src = render_classify(msaa, emit, &label);
        naga_validate(&src, &label);
        assert!(
            src.contains("fn cs_main("),
            "{label}: classify module missing `fn cs_main` entry point"
        );
    }

    // On the MSAA edge path, both edge scaffolds must render: the edge_id_tex
    // storage texture (declared + written) and the ANY-sample tile_mask branch.
    let on = render_classify(Some(4), true, "classify on");
    assert!(
        on.contains("var edge_id_tex: texture_storage_2d<r32uint, write>"),
        "MSAA classify must declare `edge_id_tex` storage texture (binding 11)"
    );
    assert!(
        on.contains("textureStore(edge_id_tex,"),
        "MSAA classify must write `edge_id_tex`"
    );
    assert!(
        on.contains("ubucket1"),
        "MSAA classify must build the ANY-sample tile_mask (4-sample OR)"
    );
    // Unified-edge U2b-3: the per-bucket + skybox edge-SAMPLE-LIST machinery
    // (`append_edge_sample`) is REMOVED — it fed only the deleted cs_edge /
    // skybox_edge_resolve pipelines. cs_shade drives edge shading from the
    // edge-id texture + the packed slot map, so the slot-map pack (edge_data
    // store of `slot_map` / the 16-bit `slot_base` form) must remain.
    assert!(
        !on.contains("fn append_edge_sample("),
        "U2b-3: append_edge_sample (edge-sample lists) must be REMOVED"
    );
    assert!(
        on.contains("edge_slot_map_base"),
        "cs_shade still needs the slot_map pack — edge_slot_map_base must remain"
    );
}

#[test]
fn material_prep_shader_validates() {
    use crate::render_passes::material_prep::shader::cache_key::ShaderCacheKeyMaterialPrep;
    use crate::render_passes::material_prep::shader::template::ShaderTemplateMaterialPrep;
    for msaa in [None, Some(4u32)] {
        let label = format!("material_prep msaa={msaa:?}");
        let src = ShaderTemplateMaterialPrep::try_from(&ShaderCacheKeyMaterialPrep {
            msaa_sample_count: msaa,
            max_shadow_casters: 4,
            sscs_enabled: false,
            sscs_step_count: 16,
            reverse_z: false,
        })
        .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
        .into_source()
        .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
        naga_validate(&src, &label);
        assert!(
            src.contains("fn cs_prep("),
            "{label}: prep module missing `fn cs_prep` entry point"
        );
        // Stage 5b-shadow: the MSAA prep module ALSO carries `cs_prep_edge`
        // (per-edge-sample shadow → compact edge buffer); the no-MSAA module does
        // NOT (no edges). Both must validate via naga above. The shared
        // shadow-visibility helper is the single source for both kernels.
        assert!(
            src.contains("fn compute_shadow_visibility_packed("),
            "{label}: prep module missing shared `compute_shadow_visibility_packed` helper"
        );
        if msaa.is_some() {
            assert!(
                src.contains("fn cs_prep_edge("),
                "{label}: MSAA prep module missing `fn cs_prep_edge` entry point"
            );
            assert!(
                src.contains("textureStore(edge_shadow_out"),
                "{label}: cs_prep_edge must write the compact edge-shadow texture"
            );
        } else {
            assert!(
                !src.contains("fn cs_prep_edge("),
                "{label}: no-MSAA prep module must NOT emit `cs_prep_edge` (no edges)"
            );
        }
    }
}

#[test]
fn custom_opaque_shaders_validate() {
    use awsm_renderer_materials::ShaderIncludes as S;
    // empty (leanest), all (Tier-A), and an explicit Tier-B declaration (must
    // still validate — Tier-B is masked off on the Custom path).
    let tier_b = S::BRDF
        .union(S::APPLY_LIGHTING)
        .union(S::MATERIAL_COLOR_CALC);
    for inc in [S::empty(), S::all(), tier_b] {
        for (msaa, mips) in CONFIGS {
            let label = format!(
                "opaque/custom inc={:?} msaa={msaa:?} mips={mips}",
                inc.bits()
            );
            let src = render(&custom_key(inc, msaa, mips), &label);
            naga_validate(&src, &label);
        }
    }
}

#[test]
fn custom_froxel_lights_accessors_validate() {
    // Stage 7: a custom material with LIGHT_ACCESS can iterate the per-pixel
    // froxel-culled lights via material_pixel_light_count / material_pixel_light
    // (which wrap the froxel_walk SSOT). Assert it validates AND that froxel_walk
    // got pulled into the Custom kernel (custom never has APPLY_LIGHTING, so the
    // `light_access && !apply_lighting` include path must supply it).
    use awsm_renderer_materials::ShaderIncludes as S;
    let dyn_id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
    let mut bucket_entries = crate::dynamic_materials::first_party_bucket_entries();
    bucket_entries.push(BucketEntry {
        shader_id: dyn_id,
        base: ShadingBase::Custom,
        pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
        name: "froxel_lit".to_string(),
    });
    let fragment = "var c = vec3<f32>(0.0);\n\
        let n = material_pixel_light_count(input);\n\
        for (var i = 0u; i < n; i = i + 1u) {\n\
            let l = material_pixel_light(input, i);\n\
            let s = light_sample(l, input.world_normal, input.world_position);\n\
            c = c + s.radiance * s.n_dot_l;\n\
        }\n\
        return OpaqueShadingOutput(c, 1.0);";
    for (msaa, mips) in CONFIGS {
        let key = ShaderCacheKeyMaterialOpaque {
            write_ssr_descriptor: false,
            reverse_z: false,
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: msaa,
            mipmaps: mips,
            max_shadow_casters: 4,
            sscs_enabled: false,
            sscs_step_count: 16,
            shader_id: dyn_id,
            base: ShadingBase::Custom,
            owns_skybox: false,
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 1,
            dynamic_shader: Some(DynamicShaderInfo {
                shader_includes: S::LIGHT_ACCESS,
                struct_decl: "struct MaterialData { _pad: u32, };".to_string(),
                loader_decl:
                    "fn material_data_load(byte_offset: u32) -> MaterialData { return MaterialData(0u); }"
                        .to_string(),
                wgsl_fragment: fragment.to_string(),
            }),
            bucket_entries: bucket_entries.clone(),
        };
        let label = format!("opaque/custom-froxel-lit msaa={msaa:?} mips={mips}");
        let src = render(&key, &label);
        naga_validate(&src, &label);
        assert!(
            src.contains("fn froxel_base_for_pixel("),
            "{label}: froxel_walk SSOT not pulled into the custom LIGHT_ACCESS kernel"
        );
        assert!(
            src.contains("fn material_pixel_light("),
            "{label}: custom froxel-light accessor missing"
        );
    }
}

fn transparent_first_party_key(
    base: ShadingBase,
    msaa: Option<u32>,
) -> ShaderCacheKeyMaterialTransparent {
    ShaderCacheKeyMaterialTransparent {
        instancing_transforms: false,
        attributes: ShaderMaterialVertexAttributes {
            normals: true,
            tangents: true,
            color_sets: None,
            uv_sets: Some(1),
        },
        texture_pool_arrays_len: 1,
        texture_pool_samplers_len: 1,
        msaa_sample_count: msaa,
        mipmaps: true,
        reverse_z: false,
        base,
        pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
        dispatch_hash: 0,
        dynamic_shader_id: None,
        dynamic_shader: None,
        dynamic_vertex_shader: None,
        froxel_slice_count: crate::render_passes::light_culling::buffers::DEFAULT_SLICE_COUNT,
    }
}

#[test]
fn first_party_transparent_shaders_validate() {
    for (base, name) in [
        (ShadingBase::Pbr, "pbr"),
        (ShadingBase::Unlit, "unlit"),
        (ShadingBase::Toon, "toon"),
        (ShadingBase::Flipbook, "flipbook"),
    ] {
        for msaa in [None, Some(4)] {
            let label = format!("transparent/{name} msaa={msaa:?}");
            let key = transparent_first_party_key(base, msaa);
            let src = ShaderTemplateMaterialTransparent::try_from(&key)
                .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
                .into_source()
                .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
            naga_validate(&src, &label);
        }
    }
}

#[test]
fn custom_transparent_shaders_validate() {
    use awsm_renderer_materials::ShaderIncludes as S;
    let dyn_id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
    let tier_b = S::BRDF
        .union(S::APPLY_LIGHTING)
        .union(S::MATERIAL_COLOR_CALC);
    for inc in [S::empty(), S::all(), tier_b] {
        for msaa in [None, Some(4)] {
            let mut key = transparent_first_party_key(ShadingBase::Custom, msaa);
            key.dispatch_hash = 1;
            key.dynamic_shader_id = Some(dyn_id);
            key.dynamic_shader = Some(DynamicShaderInfo {
                shader_includes: inc,
                struct_decl: "struct MaterialData { _pad: u32, };".to_string(),
                loader_decl:
                    "fn material_data_load(byte_offset: u32) -> MaterialData { return MaterialData(0u); }"
                        .to_string(),
                wgsl_fragment:
                    "return TransparentShadingOutput(vec4<f32>(input.world_normal * 0.5 + 0.5, 0.5));"
                        .to_string(),
            });
            let label = format!("transparent/custom inc={:?} msaa={msaa:?}", inc.bits());
            let src = ShaderTemplateMaterialTransparent::try_from(&key)
                .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
                .into_source()
                .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
            naga_validate(&src, &label);
        }
    }
}

#[test]
fn material_final_blend_shader_validates() {
    // The MSAA final_blend compositor must naga-validate for both values of
    // the write_ssr_descriptor axis — ON additionally resolves the per-pixel
    // SSR reflection descriptor from the edge arms' per-sample sums
    // (comment-pinned in final_blend.wgsl / compute.wgsl: a single-sample
    // descriptor makes the SSR composite add all-or-nothing reflection along
    // silhouettes, visibly undoing MSAA).
    use crate::render_passes::material_opaque::shader::edge_cache_key::ShaderCacheKeyMaterialFinalBlend;
    use crate::render_passes::material_opaque::shader::edge_template::ShaderTemplateMaterialFinalBlend;
    for write_ssr_descriptor in [false, true] {
        let key = ShaderCacheKeyMaterialFinalBlend {
            bucket_entries: crate::dynamic_materials::first_party_bucket_entries(),
            color_format: "rgba16float".to_string(),
            write_ssr_descriptor,
        };
        let label = format!("final_blend ssr={write_ssr_descriptor}");
        let src = ShaderTemplateMaterialFinalBlend::try_from(&key)
            .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
            .into_source()
            .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
        naga_validate(&src, &label);
        // 8-word slot stride (see ACCUMULATOR_SLOT_BYTES) in every variant.
        assert!(
            src.contains("(edge_pixel_id * 4u + slot) * 8u"),
            "{label}: final_blend must read the 8-word accumulator stride"
        );
        if write_ssr_descriptor {
            assert!(
                src.contains("let desc_rgb = desc_rgb_sum / 4.0;")
                    && src.contains("textureStore(reflection_descriptor_tex, coords,"),
                "{label}: SSR final_blend must resolve + store the per-pixel \
                 reflection descriptor"
            );
        } else {
            assert!(
                !src.contains("reflection_descriptor_tex"),
                "{label}: SSR-off final_blend must not reference the descriptor"
            );
        }
    }
}

#[test]
fn ssr_shaders_validate() {
    // SSR trace shader must naga-validate for EVERY permutation
    // (mode × trace-strategy × half_res × msaa × reverse_z) — proving the §5a
    // zero-cost templating emits valid WGSL for each variant, and that the
    // shared `camera.wgsl` / `math.wgsl` includes resolve. Also asserts the
    // compute entry point exists (the dispatch selects it by name).
    // `SsrTrace::HiZ` additionally must reference the pyramid binding.
    // Temporal is NOT a trace axis anymore: history accumulation moved to the
    // dedicated temporal pass (see `ssr_temporal_shader_validates`), so the
    // trace must carry NO history bindings in any variant.
    use crate::render_passes::ssr::shader::cache_key::{
        ShaderCacheKeySsr, ShaderCacheKeySsrTrace, SsrMode, SsrTrace,
    };
    use crate::render_passes::ssr::shader::template::ShaderTemplateSsr;
    for mode in [SsrMode::Mirror, SsrMode::Glossy] {
        for trace in [SsrTrace::LinearDda, SsrTrace::HiZ] {
            for reverse_z in [false, true] {
                for half_res in [false, true] {
                    for multisampled_geometry in [false, true] {
                        let key = ShaderCacheKeySsr::Trace(ShaderCacheKeySsrTrace {
                            mode,
                            trace,
                            half_res,
                            multisampled_geometry,
                            reverse_z,
                        });
                        let label = format!(
                            "ssr mode={mode:?} trace={trace:?} \
                         half_res={half_res} msaa={multisampled_geometry} reverse_z={reverse_z}"
                        );
                        let src = ShaderTemplateSsr::try_from(&key)
                            .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
                            .into_source()
                            .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
                        naga_validate(&src, &label);
                        assert!(
                            src.contains("fn cs_main("),
                            "{label}: SSR module missing `fn cs_main` entry point"
                        );
                        // Temporal accumulation lives in the dedicated pass:
                        // the trace must never reference history.
                        assert!(
                            !src.contains("history_prev"),
                            "{label}: trace must not reference history_prev \
                             (temporal accumulation moved to temporal.wgsl)"
                        );
                        // Deterministic mirror (comment-pinned in trace.wgsl):
                        // mirror pixels take the FIXED 0.5 march phase — no
                        // per-pixel IGN, no per-frame dither. Bilinear scene
                        // depth makes the intersection continuous, so a
                        // perfect mirror needs no stochastic supersampling.
                        assert!(
                            src.contains("select(glossy_jitter, 0.5, spread < MIRROR_SPREAD_EPS)"),
                            "{label}: trace must select the deterministic 0.5 \
                             phase for mirror pixels (spread < MIRROR_SPREAD_EPS)"
                        );
                        // Bilinear scene depth (comment-pinned in trace.wgsl):
                        // THE fundamental fix for the quantization family —
                        // hit tests interpolate the 2x2 raw-depth neighborhood
                        // (sky-guarded) so intersections are continuous.
                        assert!(
                            src.contains(
                                "fn scene_depth_at(pix: vec2<f32>, fdims: vec2<f32>) -> f32 {"
                            ) && src.contains(
                                "return mix(mix(d00, d10, f.x), mix(d01, d11, f.x), f.y);"
                            ),
                            "{label}: trace must sample scene depth bilinearly \
                             (sky-guarded scene_depth_at) in its hit tests"
                        );
                        // Glossy depth-phase (comment-pinned): in the Hi-Z
                        // path the per-pixel IGN jitter can only act through
                        // the in-cell evaluation phase (crossings are
                        // geometric); mirror pixels evaluate mid-cell (0.5),
                        // deterministically.
                        if trace == SsrTrace::HiZ {
                            assert!(
                                src.contains(
                                    "let s_eval = mix(s_cur, min(s_next, screen_len), eval_phase);"
                                ) && src.contains(
                                    "let eval_phase = select(glossy_jitter, 0.5, spread < MIRROR_SPREAD_EPS);"
                                ),
                                "{label}: Hi-Z trace must phase its ray-depth \
                                 evaluation point within the cell (glossy IGN; \
                                 mirror mid-cell deterministic)"
                            );
                        }
                        // Bilinear hit-color reconstruction for the mirror
                        // mode (comment-pinned in trace.wgsl): nearest
                        // sampling dots thin magnified source features.
                        assert!(
                            src.contains("hit_color = mix(mix(h00, h10, hfrac.x), mix(h01, h11, hfrac.x), hfrac.y);"),
                            "{label}: the sharp hit path must reconstruct the \
                             hit color bilinearly at the refined sub-texel \
                             hit_uv (both mode templates carry it — the live \
                             pipeline always compiles Glossy, mirror is its \
                             runtime spread~0 branch)"
                        );
                        // Travel-scaled glossy cone (comment-pinned in
                        // trace.wgsl): rough-reflection blur must grow with
                        // travel like a cone footprint — contact reflections
                        // sharpen like contact shadows.
                        if mode == SsrMode::Glossy {
                            assert!(
                                src.contains("let cone = clamp(screen_travel / (0.12 * f32(full_dims.y)), 0.3, 1.0);")
                                    && src.contains("* cone;"),
                                "{label}: glossy trace must scale its disk-blur \
                                 radius with screen-space travel (cone growth)"
                            );
                        }
                        // Glossy HDR clamp (comment-pinned in trace.wgsl):
                        // bloom-hot reflections keep view-dependent banding
                        // through any blur width — clamp luminance before
                        // filtering (glossy template only; mirror exact).
                        if mode == SsrMode::Glossy {
                            assert!(
                                src.contains(
                                    "hit_color = hit_color * min(1.0, 3.0 / max(hit_lum, 1e-4));"
                                ),
                                "{label}: glossy trace must clamp hit luminance \
                                 before filtering"
                            );
                        }
                        // Mirror-on-mirror fallback (comment-pinned in
                        // trace.wgsl): a hit on a reflective surface samples
                        // a pre-composite color missing its own reflection —
                        // substitute the environment in proportion to the
                        // hit surface's reflectivity (descriptor at the hit).
                        assert!(
                            src.contains("let hit_missing = hit_reflectivity")
                                && src.contains(
                                    "* (1.0 - smoothstep(0.0, SSR_SPREAD_GATE, hit_desc.a));"
                                )
                                && src.contains("hit_color = mix(hit_color, env, hit_missing);"),
                            "{label}: trace must env-substitute hits on \
                             reflective surfaces ONLY where the pre-composite \
                             lacks energy (near-mirror spreads under the \
                             ssr-spread-gate ramp) — reflectivity-only \
                             substitution erases rough metals' reflections"
                        );
                        // Contact-first probe (comment-pinned): the march's
                        // first sample sits at ~1 px, never mid-stride, so
                        // contact geometry cannot be skipped.
                        assert!(
                            src.contains("var s_cur = 1.0;"),
                            "{label}: trace march must start its first probe at ~1 px"
                        );
                        // CROSSING acceptance + post-refine validation
                        // (comment-pinned): a hit is a SIGN CHANGE of
                        // (ray_z - scene_z) across the step (in FRONT at the
                        // previous sample, BEHIND now); the binary refine
                        // converges to the root and the refined penetration
                        // is validated against ONE absolute leak threshold —
                        // angle-robust by construction (the old clause pair
                        // dissolved curved reflections into stipple at steep
                        // cameras).
                        assert!(
                            src.contains("front_prev = ray_z_prev < pscene_z * (1.0 + 1e-4);"),
                            "{label}: trace must detect crossings by the \
                             front-at-previous-sample sign test"
                        );
                        assert!(
                            src.contains("let pen_r = rray_z - rscene_z;")
                                && src.contains("let max_pen = params.thickness + 1e-3 * rscene_z;")
                                && src.contains("hit_conf = 1.0 - smoothstep(0.5, 1.0, max(pen_r, 0.0) / max_pen);"),
                            "{label}: trace must validate the REFINED penetration \
                             against the leak threshold and derive confidence from it"
                        );
                        // The old absolute front bias (+0.01) killed exact
                        // contacts; only the tiny RELATIVE epsilon remains.
                        assert!(
                            !src.contains("scene_z + 0.01") && src.contains("1e-4 * scene_z"),
                            "{label}: trace must use the relative self-intersection \
                             epsilon, not the absolute +0.01 contact-killing bias"
                        );
                        // 8-iteration binary refine (the refined texel is
                        // what hit_uv — and thus the color fetch — uses).
                        assert!(
                            src.contains("for (var b = 0; b < 8; b = b + 1)"),
                            "{label}: trace binary refine must run 8 iterations"
                        );
                        // ENVIRONMENT FALLBACK (comment-pinned in trace.wgsl):
                        // every variant binds the skybox cubemap + sampler and
                        // samples it on a MISS along the world reflected
                        // direction — off-screen reflections return the
                        // environment, never black.
                        assert!(
                            src.contains("var skybox_tex: texture_cube<f32>")
                                && src.contains("var skybox_sampler: sampler")
                                && src.contains(
                                    "let env_mip = spread * f32(textureNumLevels(skybox_tex) - 1u);"
                                ),
                            "{label}: trace must bind the skybox cubemap + filtering sampler \
                             (miss-path env fallback)"
                        );
                        assert!(
                            src.contains("textureSampleLevel(skybox_tex"),
                            "{label}: trace must sample the skybox on a miss \
                             (`textureSampleLevel(skybox_tex`)"
                        );
                        // Edge/travel fades AND hit confidence must mix INTO
                        // the env fallback, not into black; confidence turns
                        // grazing-tangency hit/miss flips into a smooth
                        // transition (comment-pinned in trace.wgsl).
                        assert!(
                            src.contains(
                                "mix(env_reflection, hit_reflection, fade * travel_fade * hit_conf)"
                            ),
                            "{label}: trace must blend hits toward the env fallback by \
                             fade * travel_fade * hit_conf"
                        );
                        // The multisampled variant must bind the MSAA depth type.
                        if multisampled_geometry {
                            assert!(
                                src.contains("texture_depth_multisampled_2d"),
                                "{label}: MSAA SSR must bind texture_depth_multisampled_2d"
                            );
                        }
                        // Hi-Z variant must bind + traverse the pyramid; linear
                        // must not reference it at all.
                        if trace == SsrTrace::HiZ {
                            assert!(
                                src.contains("var hzb_tex")
                                    && src.contains("textureLoad(hzb_tex, cell, mip).g"),
                                "{label}: Hi-Z SSR must bind hzb_tex and read the closest channel"
                            );
                        } else {
                            assert!(
                                !src.contains("hzb_tex"),
                                "{label}: linear SSR must not reference hzb_tex"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn ssr_temporal_shader_validates() {
    // The SSR temporal accumulation (history reproject + neighborhood clamp
    // AFTER the spatial resolve) must naga-validate for every permutation
    // (msaa × reverse_z). Beyond validity, pin the anti-ghosting core: the
    // history sample must be CLAMPED to the 3×3 neighborhood AABB before the
    // blend (comment-pinned in temporal.wgsl — this is what kills
    // camera-motion trails in 1-2 frames), and the reprojection must go
    // through the previous frame's view-projection.
    use crate::render_passes::ssr::shader::cache_key::{
        ShaderCacheKeySsr, ShaderCacheKeySsrTemporal,
    };
    use crate::render_passes::ssr::shader::template::ShaderTemplateSsr;
    for multisampled_geometry in [false, true] {
        for reverse_z in [false, true] {
            let key = ShaderCacheKeySsr::Temporal(ShaderCacheKeySsrTemporal {
                multisampled_geometry,
                reverse_z,
            });
            let label = format!("ssr temporal msaa={multisampled_geometry} reverse_z={reverse_z}");
            let src = ShaderTemplateSsr::try_from(&key)
                .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
                .into_source()
                .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
            naga_validate(&src, &label);
            assert!(
                src.contains("fn cs_main("),
                "{label}: temporal module missing `fn cs_main` entry point"
            );
            // The neighborhood clamp — the anti-ghosting core (comment-pinned
            // in the wgsl): history pulled to the current 3×3 AABB.
            assert!(
                src.contains("clamp(hist, nb_min, nb_max)"),
                "{label}: temporal must clamp history to the neighborhood AABB \
                 (`clamp(hist, nb_min, nb_max)`)"
            );
            // Depth reprojection through the previous frame's view-projection.
            assert!(
                src.contains("prev_view_proj"),
                "{label}: temporal must reproject via cam.prev_view_proj"
            );
            // UNGATED blend (comment-pinned in the wgsl): mirror pixels
            // accumulate exactly like glossy ones — the trace cycles the
            // mirror march phase per frame (temporal supersampling of
            // sub-texel silhouettes) and the history blend converges it. A
            // spread gate here would silently discard that information; the
            // motion safety is the neighborhood clamp, not a gate. The
            // descriptor binding stays declared for layout parity.
            assert!(
                src.contains("var reflection_descriptor_tex"),
                "{label}: temporal must keep the reflection descriptor binding \
                 (bind-group-layout parity)"
            );
            assert!(
                src.contains("params.temporal_weight,")
                    && !src.contains("smoothstep(0.0, SSR_SPREAD_GATE, spread)"),
                "{label}: temporal blend must be spread-UNGATED \
                 (mirror pixels accumulate their per-frame phase cycle)"
            );
            // Unconditional clamp (comment-pinned in the wgsl): ghosting
            // dies in 1-2 frames whatever moved. The trace is deterministic
            // for mirrors (bilinear depth), so nothing needs a relaxed clamp
            // to converge.
            assert!(
                src.contains("let hist_clamped = clamp(hist, nb_min, nb_max);")
                    && !src.contains("reproj_px"),
                "{label}: temporal must clamp history unconditionally \
                 (no reprojection gating)"
            );
            // The multisampled variant must bind the MSAA depth type; the
            // single-sample variant must not.
            if multisampled_geometry {
                assert!(
                    src.contains("texture_depth_multisampled_2d"),
                    "{label}: MSAA temporal must bind texture_depth_multisampled_2d"
                );
            } else {
                assert!(
                    src.contains("var depth_tex: texture_depth_2d"),
                    "{label}: non-MSAA temporal must bind texture_depth_2d"
                );
            }
            // Sky early-out matches the depth convention (reverse-Z far = 0).
            let expect = if reverse_z {
                "depth <= 0.0"
            } else {
                "depth >= 1.0"
            };
            assert!(
                src.contains(expect),
                "{label}: temporal sky test must be `{expect}`"
            );
        }
    }
}

#[test]
fn ssr_resolve_shader_validates() {
    // The SSR spatial resolve (the edge-aware denoise between trace and
    // composite) must naga-validate for every permutation (msaa × reverse_z).
    // Beyond validity, pin the filter's load-bearing structure: the
    // depth-similarity edge weight (same exp(-|Δz|/sigma) form the composite
    // upsample uses) and the coverage-weighted divide (rgb AND coverage share
    // one weight sum so premultiplied color + fractional coverage stay
    // consistent through the additive blend). Both lines carry comments in
    // resolve.wgsl noting this test pins them.
    use crate::render_passes::ssr::shader::cache_key::{
        ShaderCacheKeySsr, ShaderCacheKeySsrResolve,
    };
    use crate::render_passes::ssr::shader::template::ShaderTemplateSsr;
    for multisampled_geometry in [false, true] {
        for reverse_z in [false, true] {
            let key = ShaderCacheKeySsr::Resolve(ShaderCacheKeySsrResolve {
                multisampled_geometry,
                reverse_z,
            });
            let label = format!("ssr resolve msaa={multisampled_geometry} reverse_z={reverse_z}");
            let src = ShaderTemplateSsr::try_from(&key)
                .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
                .into_source()
                .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
            naga_validate(&src, &label);
            assert!(
                src.contains("fn cs_main("),
                "{label}: resolve module missing `fn cs_main` entry point"
            );
            // Depth-similarity edge-stopping term (comment-pinned in the wgsl).
            assert!(
                src.contains("exp(-abs(z_tap - z_center) / sigma)"),
                "{label}: resolve must weight taps by the depth-similarity term \
                 `exp(-abs(z_tap - z_center) / sigma)`"
            );
            // Coverage-weighted divide: rgb + coverage divided by the same
            // weight sum (comment-pinned in the wgsl).
            assert!(
                src.contains("sum / sum_w"),
                "{label}: resolve must divide the accumulated rgb+coverage by \
                 the shared weight sum (`sum / sum_w`)"
            );
            // Mirror passthrough (comment-pinned in the wgsl): the resolve
            // reads the reflection descriptor's spread; mirror pixels
            // (spread < eps) write the CENTER tap unfiltered — a perfect
            // mirror must be pixel-exact through this pass.
            assert!(
                src.contains("var reflection_descriptor_tex"),
                "{label}: resolve must bind the reflection descriptor (spread source)"
            );
            // Mirror pixels get a TIGHT (~1px, scale 0.6) antialiasing
            // kernel — never a passthrough (tangency rows quantize) and
            // NEVER a travel-widened radius (trace alpha rides to ~1 along
            // reflection silhouettes, so travel-widening frays every mirror
            // edge); glossy pixels widen with travel, gated by spread
            // (comment-pinned).
            assert!(
                src.contains("let radius_scale = mix(")
                    && src.contains("0.6,")
                    && src.contains("1.0 + travel * 2.2,")
                    && src.contains("smoothstep(0.0, SSR_SPREAD_GATE, spread),"),
                "{label}: resolve radius must lerp mirror-AA -> travel blur by spread \
                 with NO unconditional travel widening on mirror pixels"
            );
            // Quantization-comb fusing is DIRECTIONAL, two-axis and
            // multi-scale: columns (contacts) stretch taps in x, rows
            // (curved-silhouette apex) stretch in y, proportional to the
            // detected scale — and the periodicity discriminator
            // (`- dev(±2d)`) must exempt isolated THIN features (a reflected
            // thin ring) from fusing. Nothing else widens a mirror pixel
            // (comment-pinned in the wgsl).
            assert!(
                src.contains("var amt_x = 0.0;")
                    && src.contains("var amt_y = 0.0;")
                    && src.contains("- length(center.rgb - (xm2 + xp2) * 0.5)")
                    && src.contains("- length(center.rgb - (ym2 + yp2) * 0.5)")
                    && src.contains("1.0 + amt_x * 3.0")
                    && src.contains("1.0 + amt_y * 3.0")
                    && src.contains("tap_offsets[i] * radius_scale * stretch"),
                "{label}: resolve must fuse quantization combs via the two-axis \
                 multi-scale comb detector with the thin-feature periodicity guard"
            );
            // The gate constant is shared prose-tagged across shader files
            // (resolve / temporal / brdf_pbr) — grep "ssr-spread-gate".
            assert!(
                src.contains("ssr-spread-gate"),
                "{label}: resolve must define its SSR_SPREAD_GATE with the \
                 `ssr-spread-gate` sync tag"
            );
            // The multisampled variant must bind the MSAA depth type; the
            // single-sample variant must not.
            if multisampled_geometry {
                assert!(
                    src.contains("texture_depth_multisampled_2d"),
                    "{label}: MSAA resolve must bind texture_depth_multisampled_2d"
                );
            } else {
                assert!(
                    src.contains("var depth_tex: texture_depth_2d"),
                    "{label}: non-MSAA resolve must bind texture_depth_2d"
                );
            }
            // Sky early-out matches the depth convention (reverse-Z far = 0).
            let expect = if reverse_z {
                "center_depth <= 0.0"
            } else {
                "center_depth >= 1.0"
            };
            assert!(
                src.contains(expect),
                "{label}: resolve sky test must be `{expect}`"
            );
        }
    }
}

#[test]
fn ssr_composite_shader_validates() {
    // M4b: the SSR composite pass is a plain const/function-built WGSL string
    // (NOT part of the `ShaderTemplateSsr` naga suite above), so validate it
    // here. The joint-bilateral upsample reads the full-res depth binding, whose
    // WGSL type differs under MSAA (`texture_depth_multisampled_2d`); both
    // variants must parse + validate and carry the fragment entry point.
    use crate::render_passes::ssr::composite::shader_source;
    for multisampled in [false, true] {
        for reverse_z in [false, true] {
            let label = format!("ssr composite multisampled={multisampled} reverse_z={reverse_z}");
            let src = shader_source(multisampled, reverse_z);
            naga_validate(&src, &label);
            // The sky early-out must match the depth convention: reverse-Z
            // clears to 0 at the far plane, forward-Z to 1.
            let expect = if reverse_z {
                "center_depth <= 0.0"
            } else {
                "center_depth >= 1.0"
            };
            assert!(
                src.contains(expect),
                "{label}: composite sky test must be `{expect}`"
            );
        }
        let label = format!("ssr composite multisampled={multisampled}");
        let src = shader_source(multisampled, true);
        assert!(
            src.contains("fn frag_main("),
            "{label}: composite module missing `fn frag_main` entry point"
        );
        if multisampled {
            assert!(
                src.contains("texture_depth_multisampled_2d"),
                "{label}: MSAA composite must bind texture_depth_multisampled_2d"
            );
        } else {
            assert!(
                src.contains("var depth_tex: texture_depth_2d"),
                "{label}: non-MSAA composite must bind texture_depth_2d"
            );
        }
    }
}

/// A.4: the decal compute pass unpacks a flat `texture_index` with a templated
/// stride (`% {{ texture_pool_layers_per_array }}u`); validate the shader compiles
/// for a non-64 stride (256 = a real device `max_texture_array_layers`) so the
/// substitution can never regress to invalid WGSL.
#[test]
fn decal_shader_validates_with_templated_layer_stride() {
    for msaa in [None, Some(4)] {
        for stride in [256u32, 2048u32] {
            for reverse_z in [false, true] {
                let key = ShaderCacheKeyMaterialDecal {
                    msaa_sample_count: msaa,
                    texture_pool_arrays_len: 1,
                    texture_pool_samplers_len: 1,
                    texture_pool_layers_per_array: stride,
                    reverse_z,
                };
                let label = format!("decal msaa={msaa:?} stride={stride} reverse_z={reverse_z}");
                let src = ShaderTemplateMaterialDecal::try_from(&key)
                    .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
                    .into_source()
                    .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
                assert!(
                    src.contains(&format!("% {stride}u")) && src.contains(&format!("/ {stride}u")),
                    "{label}: expected the templated stride in the unpacking math"
                );
                // 003: the sky skip must match the depth clear value of the
                // convention (reverse-Z clears to 0.0, forward-Z to 1.0).
                if reverse_z {
                    assert!(
                        src.contains("depth <= 0.0"),
                        "{label}: reverse-Z sky skip must test depth <= 0.0"
                    );
                } else {
                    assert!(
                        src.contains("depth >= 1.0"),
                        "{label}: forward-Z sky skip must test depth >= 1.0"
                    );
                }
                naga_validate(&src, &label);
            }
        }
    }
}

/// The decal classify shader's HZB occlusion gate is depth-convention-aware
/// (003): under reverse-Z "closest" is the numerical MAX corner depth and the
/// HZB stores the min-reduced (farthest) occluder bound, so the drop test
/// inverts. Before this axis existed the forward-Z gate ran under reverse-Z
/// and dropped EVERY decal whose screen footprint touched the sky (hzb min =
/// 0.0 clear) — i.e. all of them: the editor rendered no decals at all.
/// Validate all four template variants and pin the gate's comparison
/// direction + the mip-selection formula (the old `31u - firstLeadingBit`
/// computed count-leading-zeros and always picked the coarsest mip).
#[test]
fn decal_classify_shader_validates_for_both_depth_conventions() {
    use crate::render_passes::material_decal::classify::shader::{
        cache_key::ShaderCacheKeyDecalClassify, template::ShaderTemplateDecalClassify,
    };
    for hzb_enabled in [false, true] {
        for reverse_z in [false, true] {
            let key = ShaderCacheKeyDecalClassify {
                hzb_enabled,
                reverse_z,
            };
            let label = format!("decal classify hzb={hzb_enabled} reverse_z={reverse_z}");
            let src = ShaderTemplateDecalClassify::try_from(&key)
                .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
                .into_source()
                .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
            naga_validate(&src, &label);
            if hzb_enabled {
                assert!(
                    !src.contains("31u - firstLeadingBit"),
                    "{label}: mip selection must use firstLeadingBit (floor log2), \
                     not its count-leading-zeros dual"
                );
                if reverse_z {
                    assert!(
                        src.contains("closest_depth = max(closest_depth"),
                        "{label}: reverse-Z closest corner depth is the numerical max"
                    );
                    assert!(
                        src.contains("closest_depth < hzb_bound"),
                        "{label}: reverse-Z occlusion gate must drop when closest \
                         depth is numerically SMALLER than the HZB min-bound"
                    );
                } else {
                    assert!(
                        src.contains("closest_depth = min(closest_depth"),
                        "{label}: forward-Z closest corner depth is the numerical min"
                    );
                    assert!(
                        src.contains("closest_depth > hzb_bound"),
                        "{label}: forward-Z occlusion gate must drop when closest \
                         depth is numerically GREATER than the HZB max-bound"
                    );
                }
            } else {
                assert!(
                    !src.contains("hzb_texture"),
                    "{label}: HZB binding must be absent when the gate is off"
                );
            }
        }
    }
}

/// Axis 1 (docs/plans/006): bloom went content/config-lazy — its 4 compute
/// shaders no longer compile at boot (only when `post_processing.bloom` turns
/// on), so on-device boot validation no longer covers them. Keep them
/// natively validated here: every pyramid step (prefilter / downsample /
/// tent upsample, all routed through the `BloomDownsample` cache key) and
/// the combine must parse + validate and carry the compute entry point.
#[test]
fn bloom_shaders_validate() {
    use crate::render_passes::bloom::shader::{
        cache_key::{BloomPyramidStep, ShaderCacheKeyBloomDownsample},
        template::{ShaderTemplateBloomCombine, ShaderTemplateBloomDownsample},
    };

    // The 9-tap tent kernel in upsample.wgsl + combine.wgsl uses weights
    // (1 2 1 / 2 4 2 / 1 2 1) with a `(1.0 / 16.0)` normalization — pin that
    // the taps sum to exactly the divisor so the filter stays
    // energy-preserving if the kernel is ever edited (edit both together).
    const TENT9_WEIGHTS: [f32; 9] = [1.0, 2.0, 1.0, 2.0, 4.0, 2.0, 1.0, 2.0, 1.0];
    const TENT9_DIVISOR: f32 = 16.0;
    assert_eq!(
        TENT9_WEIGHTS.iter().sum::<f32>(),
        TENT9_DIVISOR,
        "tent9 kernel weights must sum to the shader's normalization divisor"
    );

    for step in [
        BloomPyramidStep::Prefilter,
        BloomPyramidStep::Downsample,
        BloomPyramidStep::Upsample,
    ] {
        let label = format!("bloom pyramid step={step:?}");
        let src = ShaderTemplateBloomDownsample::try_from(&ShaderCacheKeyBloomDownsample { step })
            .unwrap_or_else(|e| panic!("{label}: template dispatch failed: {e:?}"))
            .into_source()
            .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
        naga_validate(&src, &label);
        assert!(
            src.contains("fn cs_main("),
            "{label}: bloom module missing `fn cs_main` entry point"
        );
        if matches!(step, BloomPyramidStep::Upsample) {
            assert!(
                src.contains("(1.0 / 16.0)"),
                "{label}: tent9 normalization (1/16, matching TENT9_DIVISOR) missing"
            );
            assert!(
                src.contains("textureLoad(src_prev"),
                "{label}: upsample must accumulate onto the down-pyramid base"
            );
        }
    }

    let src = ShaderTemplateBloomCombine
        .into_source()
        .expect("bloom combine: render failed");
    naga_validate(&src, "bloom combine");
    assert!(
        src.contains("fn cs_main("),
        "bloom combine: module missing `fn cs_main` entry point"
    );
    assert!(
        src.contains("(1.0 / 16.0)"),
        "bloom combine: tent9 normalization (1/16, matching TENT9_DIVISOR) missing"
    );
    assert!(
        src.contains("textureNumLevels(pyramid)"),
        "bloom combine: scatter-weight normalization must span every pyramid level"
    );
}

/// Axis 1 (docs/plans/006): the cluster-LOD cut + compaction went
/// content-lazy — they no longer compile at boot (only at the first commit
/// with a resident cluster mesh), so the "creating the pipeline validates
/// `cluster_cut.wgsl` on-device" boot checkpoint moved to that commit. Keep
/// every variant natively validated here: cut with paging off/on plus the
/// compaction.
#[cfg(feature = "lod")]
#[test]
fn cluster_lod_shaders_validate() {
    use crate::render_passes::cluster_lod::shader::template::{
        ShaderTemplateClusterCompaction, ShaderTemplateClusterCut,
    };
    for paging in [false, true] {
        let label = format!("cluster cut paging={paging}");
        let src = ShaderTemplateClusterCut { paging }
            .into_source()
            .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
        naga_validate(&src, &label);
        assert!(
            src.contains("fn cs_main("),
            "{label}: cluster cut missing `fn cs_main` entry point"
        );
    }
    let src = ShaderTemplateClusterCompaction
        .into_source()
        .expect("cluster compaction: render failed");
    naga_validate(&src, "cluster compaction");
    assert!(
        src.contains("fn cs_main("),
        "cluster compaction: module missing `fn cs_main` entry point"
    );
}
