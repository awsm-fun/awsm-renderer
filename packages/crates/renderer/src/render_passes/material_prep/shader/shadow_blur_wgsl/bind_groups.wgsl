// Bind group for the OPTIONAL shadow-visibility denoise blur (separable,
// edge-aware). Reads the packed per-pixel shadow-visibility array + the
// geometry depth, writes a blurred copy. Two MSAA variants (the depth binding
// type follows the geometry pass). Layout must stay in lockstep with
// `material_prep/bind_group.rs` (`create_blur_bind_group_layout_key`).
//
// The pass is separable: H (src = prep_shadow_visibility → dst = tmp) then V
// (src = tmp → dst = prep_shadow_visibility), so the opaque reader's binding
// never changes and the whole thing is skipped when the toggle is off.

// CameraRaw + camera_from_raw — depth → linear view-z for the edge-stopping
// weight (reject silhouette / sky jumps; smooth on continuous surfaces).
{% include "shared_wgsl/camera.wgsl" %}

{% if multisampled_geometry %}
    @group(0) @binding(0) var blur_depth_tex: texture_depth_multisampled_2d;
{% else %}
    @group(0) @binding(0) var blur_depth_tex: texture_depth_2d;
{% endif %}
@group(0) @binding(1) var<uniform> blur_camera_raw: CameraRaw;
// Source visibility (sampled) + blurred destination (storage write). Both are
// the Rgba8unorm `ceil(K/4)`-layer packed visibility format.
@group(0) @binding(2) var blur_src: texture_2d_array<f32>;
@group(0) @binding(3) var blur_dst: texture_storage_2d_array<rgba8unorm, write>;
