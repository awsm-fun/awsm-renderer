// Bind groups + material-load helpers for the CUSTOM-VERTEX geometry variant.
//
// Same augmented group 0 + reused groups 1-3 as the MASKED variant. The
// custom-vertex hook's material_data_load reads the materials storage buffer
// those bind groups declare (the plain geometry bind groups lack it). We reuse
// the masked bind-group declarations verbatim via an include.
//
// The masked variant gets its type definitions + the minimal material-load
// helpers from its FRAGMENT includes (shared_wgsl/masked_alpha.wgsl); this
// variant pairs the masked bind groups with the PLAIN geometry fragment (which
// includes none of that), so we provide them here. We deliberately do NOT
// include shared_wgsl/material.wgsl: it emits the full materials_wgsl blob,
// whose dynamic-material color fragments reference opaque-only contract types.
// Instead, mirror masked_alpha's minimal helper set (the same the generated
// material_data_load / material_sample_<name> reference). WGSL resolves
// module-scope identifiers order-independently, so declaring these alongside
// the bindings is fine.
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
{% include "shared_wgsl/textures.wgsl" %}
{% include "masked_wgsl/bind_groups.wgsl" %}

// Minimal material-buffer load helpers + the LOD-0 `texture_pool_sample` the
// generated `material_sample_<name>` helpers reference. Shared verbatim with the
// masked fragment (`shared_wgsl/masked_alpha.wgsl`) so the COMBINED masked +
// custom-vertex module can include both without redefining them.
{% include "shared_wgsl/material_load_helpers.wgsl" %}
