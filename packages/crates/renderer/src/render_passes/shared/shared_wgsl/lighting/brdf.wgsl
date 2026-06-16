// brdf.wgsl — aggregator (Phase 2 of docs/plans/material-optimizations.md).
//
// Split into two tiers so the generic half can be offered to Custom materials
// without dragging in the PBR-only orchestrators:
//   - brdf_primitives.wgsl (Tier A): generic fresnel/ggx/smith/IBL samplers +
//     the plain-param extension lobes. Reusable by ANY material.
//   - brdf_pbr.wgsl       (Tier B): the PbrMaterialColor orchestrators
//     (brdf_direct / brdf_ibl / brdf_ibl_with_transmission). PBR only.
//
// This aggregator preserves the original single-include behavior: every existing
// includer of brdf.wgsl (opaque kernel, transparent pass, the gate tests) still
// gets BOTH halves, in dependency order (primitives first — pbr calls into them).
// Phase 4 includes the halves directly so a non-PBR / Custom pipeline can pull
// only the primitives. See the taxonomy in awsm-materials::shader_includes.

{% include "shared_wgsl/lighting/brdf_primitives.wgsl" %}
{% include "shared_wgsl/lighting/brdf_pbr.wgsl" %}
