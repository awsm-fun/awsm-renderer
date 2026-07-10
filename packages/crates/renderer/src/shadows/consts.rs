//! Shadow-subsystem constants + the cube-resolution clamp.
//!
//! Anything in here is shape / capacity metadata read by both the
//! Rust-side packing code and (in the case of layout sizes) the
//! WGSL receiver shader via the `ShadowDescriptor` / `ShadowGlobals`
//! UBOs. Keep these in lockstep with `shared_wgsl/shadow/bind_groups.wgsl`.

/// Maximum number of shadow descriptors stored in the per-frame
/// uniform array. 32 entries × 96 B = 3 KB — well under the
/// `maxUniformBufferBindingSize` ceiling (default 64 KB).
pub const MAX_SHADOW_DESCRIPTORS: u32 = 32;

/// Maximum number of shadow VIEWS per frame (one render pass each).
/// Point lights have 6 views per descriptor (cube faces); directional
/// lights have one per cascade. 96 covers a worst case of 8 point +
/// 4 directional × 4 cascades + 32 spots.
pub const MAX_SHADOW_VIEWS: u32 = 96;

/// Size in bytes of a single packed `ShadowDescriptor` (see
/// `shared_wgsl/shadow/bind_groups.wgsl`):
/// - `view_projection: mat4x4<f32>` (64 B)
/// - `atlas_rect: vec4<f32>` (16 B)
/// - `bias_params: vec4<f32>` (16 B)
/// - `cascade_info: vec4<f32>` (16 B)
/// - `extra_params: vec4<f32>` (16 B) — `(shadow_samples, _, _, _)`
pub const SHADOW_DESCRIPTOR_BYTES: usize = 128;

/// Size in bytes of the `ShadowGlobals` uniform block. Layout:
/// - `atlas_sizes: vec4<f32>` (16 B) — `(pcf.w, pcf.h, evsm.w, evsm.h)`
/// - `evsm_sscs: vec4<f32>` (16 B) — `(evsm_exponent, evsm_blur_radius, _, _)`;
///   SSCS step_count + enabled are now compile-time template constants, so
///   `.z` / `.w` are reserved padding.
/// - `flags: vec4<u32>` (16 B)
/// - `cascade_array: vec4<f32>` (16 B) — `(layer.w, layer.h, max_layers, _)`
/// - `sscs_params: vec4<f32>` (16 B) — `(step_world, thickness,
///   directional_darkening, punctual_darkening)`, live-tunable SSCS scalars.
pub const SHADOW_GLOBALS_BYTES: usize = 80;

/// Logical size of a single per-view shadow uniform entry: a
/// `mat4x4` view-projection (64 B) and a `vec4` of bias parameters
/// (16 B). The actual buffer is laid out with stride
/// `SHADOW_VIEW_STRIDE` so dynamic uniform offsets stay aligned.
pub const SHADOW_VIEW_BYTES: usize = 80;

/// Stride between shadow-view buffer slots — aligned to
/// `minUniformBufferOffsetAlignment` (256 B on every adapter we
/// target) so each slot is a valid dynamic-offset target.
pub const SHADOW_VIEW_STRIDE: usize = 256;

/// Default per-face cube shadow map resolution. The runtime value is
/// `ShadowsConfig::point_shadow_resolution` (held on `Shadows` as
/// `cube_resolution`); this constant is the default the config falls
/// back to. 1024² × 6 × Depth32f × N_lights of VRAM (24 MB for 8
/// lights) — industry standard for medium-quality point shadows.
/// Drop to 512 / 256 for mobile-class browsers; bump to 2048 for
/// ultra-quality.
pub const POINT_SHADOW_RESOLUTION: u32 = 1024;

/// Minimum legal per-face cube resolution. Anything smaller than this
/// produces extreme stair-step aliasing well before saving meaningful
/// memory (a 32² face is 24 KB, vs 256 KB at 256²) — so we clamp.
pub const MIN_POINT_SHADOW_RESOLUTION: u32 = 64;

/// Clamps a user-supplied cube-face resolution to the legal range. The
/// upper bound matches `SHADOW_ATLAS_MAX_SIZE` so a single cube face
/// can't out-size the 2D atlas (`Shadows::new` already saturates VRAM
/// for the 8-light × 6-face pool when we approach that limit).
pub fn clamp_point_shadow_resolution(res: u32) -> u32 {
    res.clamp(MIN_POINT_SHADOW_RESOLUTION, SHADOW_ATLAS_MAX_SIZE)
}

/// Near plane used when generating each point-light cube face. The
/// receiver-side WGSL constant `POINT_SHADOW_NEAR` MUST match this —
/// the shadow VS writes perspective NDC.z with this near, and the
/// receiver remaps its linear distance to the same NDC.z curve for
/// the comparison. Diverging values cause silent failure (no shadow
/// or all shadow). This is the SEMANTIC near distance — under
/// reverse-Z (003 stage 7) `DepthConvention::perspective` consumes it
/// unchanged on the writer side, and the receiver's reverse formula
/// arm uses the same value; no flip needed here.
pub const POINT_SHADOW_NEAR: f32 = 0.05;

/// Sentinel meaning "this light has no shadow descriptor allocated"
/// in the packed `LightPacked` row 4. The shading shader uses this to
/// short-circuit shadow sampling.
pub const SHADOW_INDEX_NONE: u32 = u32::MAX;

/// Upper bound for `atlas_size` when dynamic resizing kicks in. Caps
/// the atlas at 8K to match the plan's "Shadow atlas size dropdown:
/// 1024 / 2048 / 4096 / 8192" ceiling.
pub const SHADOW_ATLAS_MAX_SIZE: u32 = 8192;
