//! Temporal anti-aliasing + temporal upscaling: configuration, the camera
//! jitter sequence, and the per-renderer runtime state.
//!
//! The resolve pass itself lives in `render_passes/taa`. This module holds
//! what the rest of the renderer (camera, harnesses, the editor) needs to
//! agree on: the config types, the jitter sequence, and the reconstruction
//! kernel that both the resolve shader and the reference-capture harness use.
//!
//! Conventions (the ones every formula in the resolve relies on):
//!
//! - Pixel coordinates: `x` right, `y` down, pixel `p` covers `[p, p + 1)`.
//! - A jitter `j` is in **render pixels**, each component in `[-0.5, 0.5]`.
//! - With jitter `j` applied, a surface point whose unjittered screen
//!   position is `s` rasterises at `s + j`. Equivalently, **render pixel `p`
//!   holds the scene sampled at unjittered position `p + 0.5 - j`**.

use glam::{Mat4, Vec2, Vec3};

use crate::AwsmRenderer;

/// Temporal anti-aliasing configuration (`AntiAliasing::taa`).
#[derive(Clone, Debug, PartialEq)]
pub struct TaaConfig {
    /// Render size / output size, `0.5..=1.0`. `1.0` is TAA at native
    /// resolution; below that the scene is rendered smaller and the resolve
    /// reconstructs the full-resolution image (temporal upscaling).
    pub render_scale: f32,
    /// RCAS sharpening strength after the resolve. `0.0` = off (no pass).
    pub sharpen: f32,
    /// Tuning knobs. `Default` is the shipped tuning.
    pub tuning: TaaTuning,
}

// The config is compared to detect changes and never holds NaN in practice
// (`AntiAliasing::validate` rejects a non-finite scale), so `Eq` is sound
// enough for `AntiAliasing: Eq`.
impl Eq for TaaConfig {}
impl Eq for TaaTuning {}

impl TaaConfig {
    /// Lowest supported render scale.
    pub const MIN_RENDER_SCALE: f32 = 0.5;

    /// TAA at native resolution, no sharpening.
    pub fn native() -> Self {
        Self {
            render_scale: 1.0,
            sharpen: 0.0,
            tuning: TaaTuning::default(),
        }
    }

    /// The config for a preset.
    pub fn preset(preset: TaaPreset) -> Self {
        Self {
            render_scale: preset.render_scale(),
            sharpen: preset.sharpen(),
            tuning: TaaTuning::default(),
        }
    }

    /// The preset this config's render scale corresponds to, if any.
    pub fn matching_preset(&self) -> Option<TaaPreset> {
        TaaPreset::ALL
            .into_iter()
            .find(|p| (p.render_scale() - self.render_scale).abs() < 1e-3)
    }

    /// `render_scale` clamped to the supported range (NaN becomes 1.0).
    pub fn clamped_render_scale(&self) -> f32 {
        if self.render_scale.is_finite() {
            self.render_scale.clamp(Self::MIN_RENDER_SCALE, 1.0)
        } else {
            1.0
        }
    }

    /// Whether this config upscales (render size smaller than output size).
    pub fn upscales(&self) -> bool {
        self.clamped_render_scale() < 1.0 - 1e-4
    }
}

impl Default for TaaConfig {
    fn default() -> Self {
        Self::native()
    }
}

/// Named render-scale presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaaPreset {
    /// Native resolution: anti-aliasing only.
    Native,
    /// 0.67 of the width and height (45% of the pixels).
    Quality,
    /// 0.58 (34% of the pixels).
    Balanced,
    /// 0.5 (25% of the pixels).
    Performance,
}

impl TaaPreset {
    /// Every preset, highest quality first.
    pub const ALL: [TaaPreset; 4] = [
        TaaPreset::Native,
        TaaPreset::Quality,
        TaaPreset::Balanced,
        TaaPreset::Performance,
    ];

    /// Render size / output size.
    pub fn render_scale(self) -> f32 {
        match self {
            TaaPreset::Native => 1.0,
            TaaPreset::Quality => 0.67,
            TaaPreset::Balanced => 0.58,
            TaaPreset::Performance => 0.5,
        }
    }

    /// Default RCAS strength for the preset. Upscaling is softer than a
    /// native resolve, so the upscaling presets sharpen mildly; native does
    /// not (a native resolve that needs sharpening has a bug in it).
    pub fn sharpen(self) -> f32 {
        match self {
            TaaPreset::Native => 0.0,
            TaaPreset::Quality | TaaPreset::Balanced | TaaPreset::Performance => 0.25,
        }
    }
}

/// Resolve tuning. Every value is a uniform; changing one never recompiles.
#[derive(Clone, Debug, PartialEq)]
pub struct TaaTuning {
    /// Variance-clip box half-width in standard deviations when the pixel is
    /// still. Looser when still, where flicker is the visible failure.
    pub gamma_still: f32,
    /// Variance-clip box half-width when the pixel moves (tighter: ghosting
    /// is the visible failure).
    pub gamma_moving: f32,
    /// Relative depth slack of the disocclusion test.
    pub disocclusion_threshold: f32,
    /// History-length cap at native resolution.
    pub max_history_native: f32,
    /// History-length cap when upscaling.
    pub max_history_upscaling: f32,
    /// Minimum current-frame weight at `motion_alpha_speed` output pixels per
    /// frame of motion (ramps linearly from 0).
    pub motion_alpha: f32,
    /// The speed (output px / frame) at which `motion_alpha` is reached.
    pub motion_alpha_speed: f32,
    /// Reactive value written by the transparent pass per unit of alpha.
    pub reactive_transparent: f32,
    /// Reactive value written for materials flagged `temporal: reactive`.
    pub reactive_material: f32,
    /// Extra reactivity from SSR reflectivity. 0 unless mirrors trail.
    pub reactive_ssr: f32,
    /// Added to `log2(render_scale)` for the texture LOD bias.
    pub mip_bias_extra: f32,
    /// The speed (output px / frame) at which the clip box has narrowed
    /// from `gamma_still` to `gamma_moving`.
    pub gamma_moving_speed: f32,
    /// History-length cap for a pixel moving at `max_history_moving_speed`
    /// or faster. History resampled every frame under motion softens a
    /// little each time; a short memory keeps that from building up, and
    /// lets the image sharpen again within a few frames of stopping.
    pub max_history_moving: f32,
    /// The speed (output px / frame) at which the cap has dropped from the
    /// still value to `max_history_moving`.
    pub max_history_moving_speed: f32,
    /// Minimum current-frame weight on a motion boundary: a pixel whose 3×3
    /// holds surfaces moving differently (a moving silhouette). The coverage
    /// of such a pixel changes every frame, so its history is the average
    /// coverage of the last frames, not this frame's.
    pub motion_edge_alpha: f32,
    /// The relative speed (output px / frame) between the nearest and the
    /// farthest surface of the 3×3 at which `motion_edge_alpha` is reached.
    pub motion_edge_speed: f32,
}

impl Default for TaaTuning {
    fn default() -> Self {
        Self {
            gamma_still: 1.25,
            gamma_moving: 0.75,
            disocclusion_threshold: 0.05,
            max_history_native: 16.0,
            max_history_upscaling: 24.0,
            motion_alpha: 0.2,
            motion_alpha_speed: 8.0,
            reactive_transparent: 0.8,
            reactive_material: 0.6,
            reactive_ssr: 0.0,
            mip_bias_extra: 0.0,
            gamma_moving_speed: 2.0,
            max_history_moving: 4.0,
            max_history_moving_speed: 1.0,
            motion_edge_alpha: 0.6,
            motion_edge_speed: 2.0,
        }
    }
}

/// Debug views of the TAA pipeline (`AwsmRenderer::set_taa_debug_view`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TaaDebugView {
    /// Normal output.
    #[default]
    None,
    /// Motion vectors: hue = direction, brightness = length (1 px / frame =
    /// full). Sky in grey.
    Velocity,
    /// |current − previous frame warped by v|, as a heatmap.
    ReprojectionError,
    /// History length `n / N_max`, black to white.
    HistoryLength,
    /// R = off-screen reset, G = disocclusion, B = clip amount.
    Rejection,
    /// The reactive mask.
    Reactive,
    /// Upscaling only: the weight of the nearest current-frame sample.
    Confidence,
    /// Normal output with the jitter held at zero. Separates jitter bugs
    /// from rejection bugs.
    FreezeJitter,
    /// Only the (clipped) history side of the blend.
    HistoryOnly,
    /// Only the current frame's reconstruction (no history).
    CurrentOnly,
}

impl TaaDebugView {
    /// Every view, in menu order.
    pub const ALL: [TaaDebugView; 10] = [
        TaaDebugView::None,
        TaaDebugView::Velocity,
        TaaDebugView::ReprojectionError,
        TaaDebugView::HistoryLength,
        TaaDebugView::Rejection,
        TaaDebugView::Reactive,
        TaaDebugView::Confidence,
        TaaDebugView::FreezeJitter,
        TaaDebugView::HistoryOnly,
        TaaDebugView::CurrentOnly,
    ];

    /// The value the resolve shader's `debug_view` uniform takes.
    pub fn shader_id(self) -> u32 {
        match self {
            TaaDebugView::None => 0,
            TaaDebugView::Velocity => 1,
            TaaDebugView::ReprojectionError => 2,
            TaaDebugView::HistoryLength => 3,
            TaaDebugView::Rejection => 4,
            TaaDebugView::Reactive => 5,
            TaaDebugView::Confidence => 6,
            // FreezeJitter is normal output; the freeze happens on the CPU.
            TaaDebugView::FreezeJitter => 0,
            TaaDebugView::HistoryOnly => 8,
            TaaDebugView::CurrentOnly => 9,
        }
    }

    /// Whether the camera jitter is held at zero under this view.
    pub fn freezes_jitter(self) -> bool {
        matches!(self, TaaDebugView::FreezeJitter)
    }
}

/// Test / reference hook: force a camera jitter regardless of the AA mode.
///
/// The reference-capture harness uses it to render the 64 jittered frames a
/// ground-truth image is accumulated from, with TAA off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JitterOverride {
    /// A fixed offset in render pixels, each component in `[-0.5, 0.5]`.
    Fixed(Vec2),
}

/// Per-renderer TAA runtime state. Plain CPU values: it owns no GPU
/// resources, so it costs nothing when TAA is off.
#[derive(Clone, Debug, Default)]
pub struct TaaRuntime {
    /// Frames rendered since the last history reset. Drives the jitter
    /// phase, so a reset never starts mid-cycle.
    pub(crate) frame_index: u32,
    /// Set by a reset; the next resolve ignores history, then clears it.
    pub(crate) reset_pending: bool,
    pub(crate) debug_view: TaaDebugView,
    pub(crate) jitter_override: Option<JitterOverride>,
    /// Diagnostic views beyond [`TaaDebugView`] (ids 10+ in the resolve
    /// shader), for the test harness. Overrides `debug_view` when set.
    pub(crate) debug_view_raw: Option<u32>,
    /// Harness overrides of [`CLIP_ALPHA`] and [`REACTIVE_GAMMA_SCALE`].
    pub(crate) experimental: Option<[f32; 2]>,
    /// Resolve dispatches per frame (`AwsmRenderer::set_taa_debug_repeat`,
    /// cost measurement). 0 (the default) and 1 both mean one dispatch.
    pub(crate) debug_repeat: u32,
}

/// Number of jitter phases for a render width and an output width:
/// `ceil(8 · (O.w / R.w)²)`. 8 at native, 18 at 0.67, 32 at 0.5, so each
/// output pixel gets about 8 render samples per cycle (FSR2's rule).
pub fn jitter_phase_count(render_width: u32, post_width: u32) -> u32 {
    let ratio = post_width.max(1) as f32 / render_width.max(1) as f32;
    // The epsilon keeps an exact ratio (1.0, 2.0) from rounding up a phase
    // on float noise.
    ((8.0 * ratio * ratio) - 1e-3).ceil().max(1.0) as u32
}

/// The jitter for `frame_index` in a cycle of `phase_count`: Halton base 2
/// (x) and base 3 (y), minus 0.5, in render pixels.
///
/// The Halton index is `(frame_index % phase_count) + 1`. Index 0 is
/// `(-0.5, -0.5)`, a pixel corner, and would be hit once every cycle.
///
/// The amplitude is constant. It must never depend on camera motion: the
/// history is only valid if every frame samples the same pattern.
pub fn jitter_offset(frame_index: u32, phase_count: u32) -> Vec2 {
    let i = (frame_index % phase_count.max(1)) + 1;
    Vec2::new(
        crate::camera::halton(i, 2) - 0.5,
        crate::camera::halton(i, 3) - 0.5,
    )
}

/// The projection with a render-pixel jitter `jitter_px` applied, for a
/// render target of `render_size` pixels.
///
/// ```text
/// ndc_offset = ( 2·j.x / R.w , −2·j.y / R.h )     // y flips: pixels down, NDC up
/// projection_jittered = translate(ndc_offset) · projection
/// ```
///
/// Pre-multiplying a translation adds `offset · w` to clip x/y, a constant
/// NDC shift after the divide, so it is right for perspective and
/// orthographic projections and for either depth convention.
pub fn jittered_projection(projection: Mat4, jitter_px: Vec2, render_size: Vec2) -> Mat4 {
    let ndc = Vec2::new(
        2.0 * jitter_px.x / render_size.x.max(1.0),
        -2.0 * jitter_px.y / render_size.y.max(1.0),
    );
    Mat4::from_translation(Vec3::new(ndc.x, ndc.y, 0.0)) * projection
}

/// The reconstruction kernel: weight of a current-frame sample at squared
/// distance `d2` (render pixels²) from the output pixel's centre. A Gaussian
/// approximation of a Blackman-Harris window about one render pixel wide.
///
/// The resolve shader and the reference-capture harness must use the same
/// kernel, or the reference is not what the resolve converges to.
/// `RECONSTRUCTION_SHARPNESS` is the single source for both.
pub fn reconstruction_weight(d2: f32) -> f32 {
    (-RECONSTRUCTION_SHARPNESS * d2).exp()
}

/// Current-frame weight per unit of clip amount: a heavily clipped history
/// is suspect, so the more of it the clip removed, the less it is trusted.
pub const CLIP_ALPHA: f32 = 0.5;

/// How far the variance-clip box narrows on a fully reactive pixel.
pub const REACTIVE_GAMMA_SCALE: f32 = 0.5;

/// The exponent of [`reconstruction_weight`].
pub const RECONSTRUCTION_SHARPNESS: f32 = 2.29;

impl TaaRuntime {
    /// The resolve shader's `debug_view` value.
    pub(crate) fn debug_shader_id(&self) -> u32 {
        self.debug_view_raw
            .unwrap_or_else(|| self.debug_view.shader_id())
    }
}

impl AwsmRenderer {
    /// Harness diagnostics: selects a resolve debug view by its raw shader
    /// id. Ids 10 and up are internal views with no [`TaaDebugView`] name:
    /// 10 = previous depth / expected previous depth (0.5 = equal, darker =
    /// something nearer was there), 11 = history length before this frame's
    /// update, 12 = current-frame weight (alpha), 13 = motion-boundary
    /// factor. `None` returns to [`Self::set_taa_debug_view`]'s view.
    #[doc(hidden)]
    pub fn set_taa_debug_view_raw(&mut self, id: Option<u32>) {
        self.taa.debug_view_raw = id;
    }

    /// Harness diagnostics: overrides [`CLIP_ALPHA`] and
    /// [`REACTIVE_GAMMA_SCALE`] for tuning sweeps. `None` restores them.
    #[doc(hidden)]
    pub fn set_taa_experimental_tuning(&mut self, values: Option<[f32; 2]>) {
        self.taa.experimental = values;
    }

    /// Forget all temporal history (TAA, and the jitter cycle restarts).
    /// Apps must call it on camera cuts and teleports. The renderer calls it
    /// itself when the AA mode or the output size changes and on scene load /
    /// teardown; its camera-jump guard is only a backstop: it cannot catch a
    /// teleport inside a world larger than the camera's distance to its
    /// centre.
    ///
    /// Costs nothing when TAA is off.
    pub fn reset_temporal_history(&mut self) {
        self.taa.frame_index = 0;
        self.taa.reset_pending = true;
    }

    /// Selects a TAA debug view. Views that need TAA data show the normal
    /// image when TAA is off.
    pub fn set_taa_debug_view(&mut self, view: TaaDebugView) {
        if self.taa.debug_view.freezes_jitter() != view.freezes_jitter() {
            // Freezing or unfreezing changes the sample pattern, and history
            // accumulated under the other pattern is not valid for it.
            self.reset_temporal_history();
        }
        self.taa.debug_view = view;
    }

    /// The selected TAA debug view.
    pub fn taa_debug_view(&self) -> TaaDebugView {
        self.taa.debug_view
    }

    /// Test / reference hook: force a camera jitter even when TAA is off.
    /// `None` restores normal behaviour. See [`JitterOverride`].
    pub fn set_jitter_override(&mut self, jitter: Option<JitterOverride>) {
        self.taa.jitter_override = jitter;
    }

    /// The jitter (render pixels) the next rendered frame uses, or `None`
    /// when no jitter applies (TAA off and no override).
    pub(crate) fn frame_jitter(&self, render_width: u32, post_width: u32) -> Option<Vec2> {
        if let Some(JitterOverride::Fixed(j)) = self.taa.jitter_override {
            return Some(j.clamp(Vec2::splat(-0.5), Vec2::splat(0.5)));
        }
        if !self.taa_jitter_enabled() {
            return None;
        }
        if self.taa.debug_view.freezes_jitter() {
            return Some(Vec2::ZERO);
        }
        Some(jitter_offset(
            self.taa.frame_index,
            jitter_phase_count(render_width, post_width),
        ))
    }

    /// Whether TAA is on: the camera jitters and the camera uniform tracks
    /// the last rendered frame.
    pub(crate) fn taa_jitter_enabled(&self) -> bool {
        self.anti_aliasing.taa.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

    #[test]
    fn phase_counts_follow_the_scale() {
        assert_eq!(jitter_phase_count(1920, 1920), 8);
        // 0.67: 8 / 0.67² = 17.8 → 18
        assert_eq!(jitter_phase_count(1286, 1920), 18);
        assert_eq!(jitter_phase_count(960, 1920), 32);
        // 0.58: 8 / 0.58² = 23.8 → 24
        assert_eq!(jitter_phase_count(1114, 1920), 24);
        assert_eq!(jitter_phase_count(0, 0), 8);
    }

    #[test]
    fn jitter_is_halton_2_3_and_never_a_corner() {
        // Halton(2): 1/2, 1/4, 3/4, 1/8 … ; Halton(3): 1/3, 2/3, 1/9, 4/9 …
        let expect = [
            (0.5 - 0.5, 1.0 / 3.0 - 0.5),
            (0.25 - 0.5, 2.0 / 3.0 - 0.5),
            (0.75 - 0.5, 1.0 / 9.0 - 0.5),
            (0.125 - 0.5, 4.0 / 9.0 - 0.5),
        ];
        for (frame, (x, y)) in expect.into_iter().enumerate() {
            let j = jitter_offset(frame as u32, 8);
            assert!(
                (j.x - x).abs() < 1e-6 && (j.y - y).abs() < 1e-6,
                "{frame}: {j:?}"
            );
        }
        for n in [8u32, 18, 24, 32, 64] {
            for frame in 0..(n * 2) {
                let j = jitter_offset(frame, n);
                assert!(j.x > -0.5 && j.x < 0.5 && j.y > -0.5 && j.y < 0.5);
                // The cycle repeats exactly.
                assert_eq!(j, jitter_offset(frame + n, n));
            }
            // The phases of one cycle are all distinct and centred.
            let phases: Vec<Vec2> = (0..n).map(|f| jitter_offset(f, n)).collect();
            for a in 0..phases.len() {
                for b in (a + 1)..phases.len() {
                    assert!((phases[a] - phases[b]).length() > 1e-4);
                }
            }
            let mean = phases.iter().copied().sum::<Vec2>() / n as f32;
            assert!(mean.length() < 0.12, "n={n} mean={mean:?}");
        }
    }

    /// Screen position (pixels, y down) of a world point.
    fn screen_px(view_proj: Mat4, p: Vec3, size: Vec2) -> Vec2 {
        let c = view_proj * p.extend(1.0);
        let ndc = Vec2::new(c.x / c.w, c.y / c.w);
        Vec2::new((ndc.x * 0.5 + 0.5) * size.x, (-ndc.y * 0.5 + 0.5) * size.y)
    }

    fn projections() -> Vec<(&'static str, Mat4)> {
        let aspect = 1286.0 / 961.0;
        vec![
            (
                "perspective forward-z",
                Mat4::perspective_rh(0.9, aspect, 0.1, 500.0),
            ),
            (
                "perspective reverse-z infinite",
                Mat4::perspective_infinite_reverse_rh(0.9, aspect, 0.1),
            ),
            (
                "orthographic forward-z",
                Mat4::orthographic_rh(-4.0 * aspect, 4.0 * aspect, -4.0, 4.0, 0.1, 500.0),
            ),
            (
                "orthographic reverse-z",
                Mat4::orthographic_rh(-4.0 * aspect, 4.0 * aspect, -4.0, 4.0, 500.0, 0.1),
            ),
        ]
    }

    #[test]
    fn jitter_moves_a_point_by_exactly_j_render_pixels() {
        let size = Vec2::new(1286.0, 961.0);
        let view = Mat4::look_at_rh(Vec3::new(1.0, 2.0, 6.0), Vec3::ZERO, Vec3::Y);
        let points = [
            Vec3::new(0.3, -0.2, 0.5),
            Vec3::new(-2.0, 1.0, -3.0),
            Vec3::new(1.5, 0.1, 2.0),
        ];
        for (name, projection) in projections() {
            for j in [
                Vec2::new(0.25, -0.375),
                Vec2::new(-0.5, 0.5),
                Vec2::new(0.0, 0.1),
            ] {
                let jittered = jittered_projection(projection, j, size);
                for p in points {
                    let a = screen_px(projection * view, p, size);
                    let b = screen_px(jittered * view, p, size);
                    let moved = b - a;
                    assert!(
                        (moved - j).length() < 2e-3,
                        "{name}: j={j:?} moved={moved:?}"
                    );
                    // Depth is untouched: the depth buffer stays comparable
                    // across frames.
                    let za = (projection * view * p.extend(1.0)).z;
                    let zb = (jittered * view * p.extend(1.0)).z;
                    assert!((za - zb).abs() < 1e-6, "{name}: depth moved");
                }
            }
        }
    }

    #[test]
    fn unjittered_matrix_round_trips() {
        // Removing the jitter from the jittered projection gives the
        // original back: the unjittered matrix the motion vectors use is the
        // same matrix the frame would have rendered with, not an
        // approximation of it.
        let size = Vec2::new(1920.0, 1435.0);
        for (name, projection) in projections() {
            let j = Vec2::new(0.31, -0.44);
            let jittered = jittered_projection(projection, j, size);
            let back = jittered_projection(jittered, -j, size);
            let (a, b) = (projection.to_cols_array(), back.to_cols_array());
            for i in 0..16 {
                assert!((a[i] - b[i]).abs() < 1e-5, "{name}: element {i}");
            }
            // And zero jitter is the identity.
            assert_eq!(
                jittered_projection(projection, Vec2::ZERO, size).to_cols_array(),
                projection.to_cols_array()
            );
        }
    }

    #[test]
    fn a_pixel_holds_the_scene_at_p_plus_half_minus_j() {
        // The sentence every resolve formula uses: with jitter j, the point
        // at unjittered screen position `p + 0.5 − j` rasterises at the
        // centre of render pixel p.
        let size = Vec2::new(640.0, 480.0);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        for (name, projection) in projections() {
            let j = Vec2::new(0.2, -0.3);
            let view_proj = projection * view;
            let inv = view_proj.inverse();
            let p = Vec2::new(200.0, 120.0);
            let want = p + Vec2::splat(0.5) - j;
            // Unproject that unjittered screen position to a world point.
            let ndc = Vec2::new(want.x / size.x * 2.0 - 1.0, 1.0 - want.y / size.y * 2.0);
            let w = inv * Vec4::new(ndc.x, ndc.y, 0.5, 1.0);
            let world = w.truncate() / w.w;
            let jittered = jittered_projection(projection, j, size) * view;
            let at = screen_px(jittered, world, size);
            assert!(
                (at - (p + Vec2::splat(0.5))).length() < 2e-3,
                "{name}: landed at {at:?}"
            );
        }
    }

    #[test]
    fn kernel_matches_the_design() {
        assert_eq!(reconstruction_weight(0.0), 1.0);
        // 0.7 render px away (the worst case at 0.5 scale) is about 0.3.
        assert!((reconstruction_weight(0.7 * 0.7) - 0.3256).abs() < 1e-3);
    }

    #[test]
    fn presets() {
        assert_eq!(TaaPreset::Quality.render_scale(), 0.67);
        assert_eq!(TaaPreset::Balanced.render_scale(), 0.58);
        assert_eq!(TaaPreset::Performance.render_scale(), 0.5);
        assert_eq!(
            TaaConfig::preset(TaaPreset::Quality).matching_preset(),
            Some(TaaPreset::Quality)
        );
        assert!(!TaaConfig::native().upscales());
        assert!(TaaConfig::preset(TaaPreset::Performance).upscales());
        let odd = TaaConfig {
            render_scale: f32::NAN,
            ..TaaConfig::native()
        };
        assert_eq!(odd.clamped_render_scale(), 1.0);
    }
}
