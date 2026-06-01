// (intentionally near-empty)
//
// The legacy `fn unlit(color: PbrMaterialColor)` lived here but was dead — the
// actual unlit shading path uses `compute_unlit_output` + the unlit material's
// own `compute_unlit_material_color`. It referenced `PbrMaterialColor`, which
// (after the skinny-materials materials_wgsl filtering) is no longer present in
// non-PBR pipelines, so a dead reference to it broke unlit/toon compilation.
// Removed. See docs/SHADER_GUIDELINES.md.
