//! Native guard against the "templated base-branch calls a material function
//! the filtered `materials_wgsl` doesn't define" bug class.
//!
//! Every material-bearing shader template injects `materials_wgsl` — the
//! concatenation of the *enabled* material fragments, filtered to this
//! pipeline's `base` for first-party (skinny) pipelines. A template's
//! per-base branch (`{% if base == ShadingBase::Flipbook %}` …) then calls
//! that base's entry point, `<base>_get_material(...)`. If the filtered
//! fragment is missing the function — because the fragment was refactored and
//! a function slid out of the emitted range, or a new base's fragment isn't
//! wired into the registry — the template still renders, but the WGSL fails
//! validation at **pipeline compile time**, i.e. only on first render / editor
//! boot. No CPU-side materials test sees it.
//!
//! This bit us once: the day-3 flipbook cell-math extraction sliced
//! `flipbook_get_material` out of the emitted fragment range, and every shader
//! whose base-branch called it (opaque compute, edge-resolve — newly reachable
//! at boot once FLIPBOOK joined the masked variant set) failed validation and
//! took the whole editor down at init.
//!
//! These tests render each material-bearing template for every first-party
//! base and assert every `<x>_get_material(` CALL has a matching
//! `fn <x>_get_material` DEFINITION in the same module — the precise, low-
//! false-positive signature of the bug (each base defines exactly one
//! `_get_material` entry point; a shared include defines none). They run
//! natively (no GPU), so the break is caught by `cargo test` instead of a
//! browser boot.

#![cfg(test)]

use awsm_renderer_materials::MaterialShaderId;

use crate::dynamic_materials::{BucketEntry, ShadingBase};

/// The four first-party shading bases + their canonical bucket name. Custom
/// (dynamic) materials carry their own author fragment and are exercised
/// separately by the dynamic-material tests.
fn first_party_bases() -> [(MaterialShaderId, ShadingBase, &'static str); 4] {
    [
        (MaterialShaderId::PBR, ShadingBase::Pbr, "pbr"),
        (MaterialShaderId::UNLIT, ShadingBase::Unlit, "unlit"),
        (MaterialShaderId::TOON, ShadingBase::Toon, "toon"),
        (
            MaterialShaderId::FLIPBOOK,
            ShadingBase::Flipbook,
            "flipbook",
        ),
    ]
}

/// A single-entry bucket list for `base` — enough to template the bucket
/// dispatch / `ClassifyBuckets` view for a one-material pipeline.
fn single_bucket(shader_id: MaterialShaderId, base: ShadingBase, name: &str) -> Vec<BucketEntry> {
    vec![BucketEntry {
        shader_id,
        base,
        pbr_features: 0,
        name: name.to_string(),
    }]
}

/// Assert every `<ident>_get_material(` call in `src` has a matching
/// `fn <ident>_get_material` definition. `label` identifies the template +
/// base in the failure message.
fn assert_get_material_calls_defined(src: &str, label: &str) {
    const NEEDLE: &str = "_get_material(";
    for line in src.lines() {
        // Skip the definition lines themselves (and comments) so a `fn
        // foo_get_material(` doesn't read as an unsatisfied call.
        let trimmed = line.trim_start();
        if trimmed.starts_with("fn ") || trimmed.starts_with("//") {
            continue;
        }
        let mut rest = line;
        while let Some(pos) = rest.find(NEEDLE) {
            let head = &rest[..pos];
            let ident_start = head
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map(|i| i + 1)
                .unwrap_or(0);
            let ident = &head[ident_start..];
            // Only the `<base>_get_material` entry points; ignore a bare
            // `_get_material(` with no identifier (can't happen in practice).
            if !ident.is_empty() {
                let def = format!("fn {ident}_get_material");
                assert!(
                    src.contains(&def),
                    "{label}: WGSL CALLS `{ident}_get_material(` but never defines `{def}` \
                     — the filtered materials_wgsl is missing this base's fragment entry point \
                     (would fail pipeline compile at first render / editor boot)",
                );
            }
            rest = &rest[pos + NEEDLE.len()..];
        }
    }
}

#[test]
fn scanner_catches_missing_definition_and_accepts_present_one() {
    // Present definition → passes.
    assert_get_material_calls_defined(
        "fn flipbook_get_material(o: u32) -> X { }\nlet m = flipbook_get_material(off);",
        "self-test/good",
    );
    // Call with no matching `fn …` → must panic (the bug we guard against).
    let missing = std::panic::catch_unwind(|| {
        assert_get_material_calls_defined(
            "let m = flipbook_get_material(off);", // no `fn flipbook_get_material`
            "self-test/bad",
        )
    });
    assert!(
        missing.is_err(),
        "the completeness scanner FAILED to flag a called-but-undefined loader \
         — the guard is inert",
    );
}

#[test]
fn opaque_compute_defines_every_called_loader_per_base() {
    use crate::render_passes::material_opaque::shader::cache_key::ShaderCacheKeyMaterialOpaque;
    use crate::render_passes::material_opaque::shader::template::ShaderTemplateMaterialOpaque;
    for (shader_id, base, name) in first_party_bases() {
        let key = ShaderCacheKeyMaterialOpaque {
            write_ssr_descriptor: false,
            reverse_z: false,
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: Some(4),
            mipmaps: true,
            max_shadow_casters: 4,
            sscs_enabled: false,
            sscs_step_count: 16,
            shader_id,
            base,
            owns_skybox: false,
            pbr_features: 0,
            dispatch_hash: 0,
            dynamic_shader: None,
            bucket_entries: single_bucket(shader_id, base, name),
        };
        let src = ShaderTemplateMaterialOpaque::try_from(&key)
            .unwrap_or_else(|e| panic!("opaque/{name}: template build failed: {e:?}"))
            .into_source()
            .unwrap_or_else(|e| panic!("opaque/{name}: render failed: {e:?}"));
        assert_get_material_calls_defined(&src, &format!("opaque-compute/{name}"));
    }
}

#[test]
fn transparent_defines_every_called_loader_per_base() {
    use crate::render_passes::light_culling::buffers::DEFAULT_SLICE_COUNT;
    use crate::render_passes::material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent;
    use crate::render_passes::material_transparent::shader::template::ShaderTemplateMaterialTransparent;
    use crate::render_passes::shared::material::cache_key::ShaderMaterialVertexAttributes;
    for (shader_id, base, name) in first_party_bases() {
        let _ = shader_id; // transparent keys on `base`, not the id
        let key = ShaderCacheKeyMaterialTransparent {
            instancing_transforms: false,
            attributes: ShaderMaterialVertexAttributes {
                normals: true,
                tangents: true,
                color_sets: None,
                uv_sets: Some(1),
            },
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: None,
            mipmaps: true,
            reverse_z: false,
            base,
            pbr_features: 0,
            dispatch_hash: 0,
            dynamic_shader_id: None,
            dynamic_shader: None,
            dynamic_vertex_shader: None,
            froxel_slice_count: DEFAULT_SLICE_COUNT,
        };
        let src = ShaderTemplateMaterialTransparent::try_from(&key)
            .unwrap_or_else(|e| panic!("transparent/{name}: template build failed: {e:?}"))
            .into_source()
            .unwrap_or_else(|e| panic!("transparent/{name}: render failed: {e:?}"));
        assert_get_material_calls_defined(&src, &format!("transparent/{name}"));
    }
}
