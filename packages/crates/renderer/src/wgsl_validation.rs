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

use awsm_materials::MaterialShaderId;

use crate::dynamic_materials::{BucketEntry, ShadingBase};
use crate::render_passes::material_opaque::shader::cache_key::{
    DynamicShaderInfo, ShaderCacheKeyMaterialOpaque, ShaderCacheKeyMaterialOpaqueEmpty,
};
use crate::render_passes::material_opaque::shader::template::{
    ShaderTemplateMaterialOpaque, ShaderTemplateMaterialOpaqueEmpty,
};
use crate::render_passes::material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent;
use crate::render_passes::material_transparent::shader::template::ShaderTemplateMaterialTransparent;
use crate::render_passes::shared::material::cache_key::ShaderMaterialVertexAttributes;

/// Parse + validate `src` with naga; panic with a readable error on failure.
fn naga_validate(src: &str, label: &str) {
    let module = match naga::front::wgsl::parse_str(src) {
        Ok(m) => m,
        Err(e) => panic!("{label}: naga WGSL PARSE failed:\n{}", e.emit_to_string(src)),
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
    first_party_key_prep(shader_id, base, owns_skybox, msaa, mipmaps, false)
}

fn first_party_key_prep(
    shader_id: MaterialShaderId,
    base: ShadingBase,
    owns_skybox: bool,
    msaa: Option<u32>,
    mipmaps: bool,
    prep_enabled: bool,
) -> ShaderCacheKeyMaterialOpaque {
    ShaderCacheKeyMaterialOpaque {
        texture_pool_arrays_len: 1,
        texture_pool_samplers_len: 1,
        msaa_sample_count: msaa,
        mipmaps,
        prep_enabled,
        max_shadow_casters: 4,
        shader_id,
        base,
        owns_skybox,
        pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(),
        dispatch_hash: 0,
        dynamic_shader: None,
        bucket_entries: crate::dynamic_materials::first_party_bucket_entries(),
    }
}

fn custom_key(
    includes: awsm_materials::ShaderIncludes,
    msaa: Option<u32>,
    mipmaps: bool,
) -> ShaderCacheKeyMaterialOpaque {
    let dyn_id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
    let mut bucket_entries = crate::dynamic_materials::first_party_bucket_entries();
    bucket_entries.push(BucketEntry {
        shader_id: dyn_id,
        base: ShadingBase::Custom,
        pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(),
        name: "noise".to_string(),
    });
    ShaderCacheKeyMaterialOpaque {
        texture_pool_arrays_len: 1,
        texture_pool_samplers_len: 1,
        msaa_sample_count: msaa,
        mipmaps,
        prep_enabled: false,
        max_shadow_casters: 4,
        shader_id: dyn_id,
        base: ShadingBase::Custom,
        owns_skybox: false,
        pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(),
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
fn first_party_opaque_shaders_validate() {
    let bases = [
        (MaterialShaderId::PBR, ShadingBase::Pbr, false, "pbr"),
        (MaterialShaderId::UNLIT, ShadingBase::Unlit, false, "unlit"),
        (MaterialShaderId::TOON, ShadingBase::Toon, false, "toon"),
        (MaterialShaderId::FLIPBOOK, ShadingBase::Flipbook, false, "flipbook"),
        (MaterialShaderId::SKYBOX, ShadingBase::Pbr, true, "skybox"),
    ];
    for (id, base, owns_skybox, name) in bases {
        for (msaa, mips) in CONFIGS {
            let label = format!("opaque/{name} msaa={msaa:?} mips={mips}");
            let src = render(&first_party_key(id, base, owns_skybox, msaa, mips), &label);
            naga_validate(&src, &label);
            // Entry-point existence guard: the launcher creates every opaque
            // bucket's pipeline with `.with_entry_point("cs_opaque")` — so the
            // module MUST define `fn cs_opaque`. naga only checks the module
            // *compiles*, not that the requested entry point exists; a missing
            // one fails at pipeline-create time on a real GPU (it's how the
            // skybox writer's `fn main` slipped through the 1024 module
            // unification until model-tests caught it). The MSAA path also
            // needs `cs_edge` — except the skybox writer (no edge resolve).
            assert!(
                src.contains("fn cs_opaque("),
                "{label}: opaque module missing `fn cs_opaque` entry point \
                 (launcher requests it → pipeline-create would fail on GPU)"
            );
            if msaa.is_some() && !owns_skybox {
                assert!(
                    src.contains("fn cs_edge("),
                    "{label}: MSAA opaque module missing `fn cs_edge` entry point"
                );
            }
        }
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
        true,  // prep_enabled
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
    // Plan B (stage 4): the PBR opaque kernel with prep enabled + MSAA off
    // reads the prep pass's per-pixel shadow-visibility buffer instead of
    // sampling shadow maps inline. Assert it (a) validates, (b) reads
    // `prep_shadow_visibility` via textureLoad, and (c) DROPS the inline
    // `sample_shadow_directional` definition (the ~50 KB win). Also build the
    // prep-OFF and MSAA-on variants and assert they KEEP the inline sampler
    // (byte-identical behavior to today). Mirrors
    // `opaque_prep_read_variant_validates`.
    let prep_key = first_party_key_prep(
        MaterialShaderId::PBR,
        ShadingBase::Pbr,
        false,
        None, // no MSAA → prep_read = true → shadow_from_buffer = true (PBR lights)
        true,
        true, // prep_enabled
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

    // Control 1: prep OFF (no MSAA) keeps inline sampling, no buffer read.
    let off_key = first_party_key(MaterialShaderId::PBR, ShadingBase::Pbr, false, None, true);
    let off_src = render(&off_key, "opaque/pbr prep-off");
    assert!(
        off_src.contains("fn sample_shadow_directional"),
        "prep-off PBR opaque must KEEP inline `fn sample_shadow_directional`"
    );
    assert!(
        !off_src.contains("textureLoad(prep_shadow_visibility"),
        "prep-off PBR opaque must NOT read the prep shadow buffer"
    );

    // Control 2: prep ON but MSAA on ⇒ prep_read false ⇒ inline sampling kept.
    let msaa_key = first_party_key_prep(
        MaterialShaderId::PBR,
        ShadingBase::Pbr,
        false,
        Some(4), // MSAA on → prep_read = false → shadow_from_buffer = false
        true,
        true,
    );
    let msaa_src = render(&msaa_key, "opaque/pbr prep-on msaa4");
    naga_validate(&msaa_src, "opaque/pbr prep-on msaa4");
    assert!(
        msaa_src.contains("fn sample_shadow_directional"),
        "MSAA-on PBR opaque must KEEP inline `fn sample_shadow_directional` (prep_read false under MSAA)"
    );
    assert!(
        !msaa_src.contains("textureLoad(prep_shadow_visibility"),
        "MSAA-on PBR opaque must NOT read the prep shadow buffer"
    );

    // Measurement: report the prep-read (no-MSAA) PBR size vs prep-off.
    eprintln!(
        "[stage4] PBR opaque no-MSAA — prep-read(shadow_from_buffer): {} B, prep-off(inline): {} B (delta {})",
        src.len(),
        off_src.len(),
        off_src.len() as i64 - src.len() as i64,
    );
    // The shadow-from-buffer variant must be SMALLER (the inline sampler drop).
    assert!(
        src.len() < off_src.len(),
        "shadow_from_buffer PBR ({} B) should be smaller than prep-off inline PBR ({} B)",
        src.len(),
        off_src.len()
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
        })
        .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
        .into_source()
        .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
        naga_validate(&src, &label);
        assert!(
            src.contains("fn cs_prep("),
            "{label}: prep module missing `fn cs_prep` entry point"
        );
    }
}

#[test]
fn custom_opaque_shaders_validate() {
    use awsm_materials::ShaderIncludes as S;
    // empty (leanest), all (Tier-A), and an explicit Tier-B declaration (must
    // still validate — Tier-B is masked off on the Custom path).
    let tier_b = S::BRDF.union(S::APPLY_LIGHTING).union(S::MATERIAL_COLOR_CALC);
    for inc in [S::empty(), S::all(), tier_b] {
        for (msaa, mips) in CONFIGS {
            let label = format!("opaque/custom inc={:?} msaa={msaa:?} mips={mips}", inc.bits());
            let src = render(&custom_key(inc, msaa, mips), &label);
            naga_validate(&src, &label);
        }
    }
}

#[test]
fn empty_opaque_shader_validates() {
    // The no-geometry opaque template — also includes light_access etc., so
    // Phase 4 gating must keep it valid.
    for msaa in [None, Some(4)] {
        let key = ShaderCacheKeyMaterialOpaqueEmpty {
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: msaa,
        };
        let label = format!("opaque-empty msaa={msaa:?}");
        let src = ShaderTemplateMaterialOpaqueEmpty::try_from(&key)
            .unwrap_or_else(|e| panic!("{label}: template build failed: {e:?}"))
            .into_source()
            .unwrap_or_else(|e| panic!("{label}: render failed: {e:?}"));
        naga_validate(&src, &label);
    }
}

fn transparent_first_party_key(base: ShadingBase, msaa: Option<u32>) -> ShaderCacheKeyMaterialTransparent {
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
        base,
        pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(),
        dispatch_hash: 0,
        dynamic_shader_id: None,
        dynamic_shader: None,
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
    use awsm_materials::ShaderIncludes as S;
    let dyn_id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
    let tier_b = S::BRDF.union(S::APPLY_LIGHTING).union(S::MATERIAL_COLOR_CALC);
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
