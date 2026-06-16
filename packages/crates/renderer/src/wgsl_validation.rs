//! Static WGSL validation of the rendered material shaders, via `naga`.
//!
//! `shader_completeness` only checks `<base>_get_material(` calls; it does NOT
//! catch the general "templated code calls a function the gated includes no
//! longer define" hazard that the include-gating work (Phase 4 of
//! docs/plans/material-optimizations.md) introduces. Those breaks otherwise
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
    ShaderCacheKeyMaterialOpaque {
        texture_pool_arrays_len: 1,
        texture_pool_samplers_len: 1,
        msaa_sample_count: msaa,
        mipmaps,
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

