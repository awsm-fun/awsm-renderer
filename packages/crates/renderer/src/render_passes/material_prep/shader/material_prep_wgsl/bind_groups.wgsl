// Bind group declarations for the material prep compute pass (Plan B,
// docs/plans/deferred-shared-prep-pass.md). Layout must stay in lockstep with
// material_prep/bind_group.rs (added in the buffer-wiring sub-stage).
//
// STAGE-1 SCAFFOLD: only the inputs/outputs the placeholder compute body needs
// are declared. Subsequent sub-stages add the geometry pool + mesh-meta inputs
// (for world-pos/UV/vcolor interpolation), and the froxel light list + shadow
// maps + K-layer shadow_visibility output (deferred shadows).

// Visibility buffer (triangle id + meta offset), written by the geometry pass.
@group(0) @binding(0) var visibility_data_tex: {% if multisampled_geometry %}texture_multisampled_2d<u32>{% else %}texture_2d<u32>{% endif %};

// Materialized world position (fp32). Storage-write; per-material kernels read it
// instead of recomputing (when PrepPassConfig.enabled).
@group(0) @binding(1) var world_pos_out: texture_storage_2d<rgba32float, write>;
