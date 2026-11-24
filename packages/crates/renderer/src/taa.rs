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
    /// How the resolve tells that a pixel's history shows something else
    /// (structural: a resolve variant and, for the id test, a texture). See
    /// [`TaaDisocclusion`] and `docs/TAA.md` §3.5 for what each level fixes
    /// and costs.
    pub disocclusion: TaaDisocclusion,
}

/// The disocclusion tests the resolve compiles (`docs/TAA.md` §3.5, step 5;
/// cost and recommendation per level in §2.4 and §7.6). Each level adds to
/// the one before; a level that is not selected allocates, copies and binds
/// nothing of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TaaDisocclusion {
    /// Depth only (round 1): this frame's surface against last frame's
    /// depth, and the pixel's own surface by depth. Misses the contact line
    /// under a moving object (the two depths touch there). Keeps last
    /// frame's depth (4 B/px at render size).
    Depth,
    /// Depth, plus last frame's motion where depth cannot tell two surfaces
    /// apart (round 2): adds a copy of last frame's motion vectors (4 B/px,
    /// 8 on the fallback path). Catches the contact line under an object
    /// moving over a STATIC surface.
    DepthAndMotion,
    /// Depth and motion, plus last frame's object id: the visibility
    /// target's per-pixel mesh id is kept for one frame (8 B/px at render
    /// size), so "a different object was here last frame" is decided
    /// exactly, also on a moving surface and wherever two objects touch in
    /// depth; and a revealed or motion-boundary pixel is reconstructed from
    /// its own object's samples only, so the other object's colour never
    /// enters its history. The recommended level.
    #[default]
    DepthMotionAndId,
}

impl TaaDisocclusion {
    /// Keeps and reads last frame's motion vectors.
    pub fn motion(self) -> bool {
        !matches!(self, Self::Depth)
    }

    /// Keeps and reads last frame's object ids.
    pub fn id(self) -> bool {
        matches!(self, Self::DepthMotionAndId)
    }
}

// The config is compared to detect changes and never holds NaN in practice
// (`AntiAliasing::validate` rejects a non-finite scale), so `Eq` is sound
// enough for `AntiAliasing: Eq`.
impl Eq for TaaConfig {}
impl Eq for TaaTuning {}

impl TaaConfig {
    /// Lowest supported render scale.
    pub const MIN_RENDER_SCALE: f32 = 0.5;

    /// The render scale's quantum (see [`Self::clamped_render_scale`]).
    pub const RENDER_SCALE_STEP: f32 = 0.01;

    /// TAA at native resolution, no sharpening.
    pub fn native() -> Self {
        Self {
            render_scale: 1.0,
            sharpen: 0.0,
            tuning: TaaTuning::default(),
            disocclusion: TaaDisocclusion::default(),
        }
    }

    /// The config for a preset.
    pub fn preset(preset: TaaPreset) -> Self {
        Self {
            render_scale: preset.render_scale(),
            sharpen: preset.sharpen(),
            tuning: TaaTuning::default(),
            disocclusion: TaaDisocclusion::default(),
        }
    }

    /// The preset this config's render scale corresponds to, if any.
    pub fn matching_preset(&self) -> Option<TaaPreset> {
        TaaPreset::ALL
            .into_iter()
            .find(|p| (p.render_scale() - self.render_scale).abs() < 1e-3)
    }

    /// `render_scale` clamped to the supported range (NaN becomes 1.0) and
    /// quantised to [`Self::RENDER_SCALE_STEP`]. Every render-size target is
    /// rebuilt when the render size changes, so a resolution governor nudging
    /// the scale by a hair must not rebuild them each frame: only a change of
    /// at least one step reaches the frame sizes. The presets (0.67, 0.58,
    /// 0.5) are exact steps.
    pub fn clamped_render_scale(&self) -> f32 {
        if !self.render_scale.is_finite() {
            return 1.0;
        }
        let steps = 1.0 / Self::RENDER_SCALE_STEP;
        let clamped = self.render_scale.clamp(Self::MIN_RENDER_SCALE, 1.0);
        ((clamped * steps).round() / steps).clamp(Self::MIN_RENDER_SCALE, 1.0)
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
    /// Upper bound of the history-length cap when upscaling (the cap itself
    /// is the native one scaled by the pixel ratio, see [`history_cap`]).
    pub max_history_upscaling: f32,
    /// Minimum current-frame weight at `motion_alpha_speed` output pixels per
    /// frame of motion (ramps linearly from 0).
    pub motion_alpha: f32,
    /// The speed (output px / frame) at which `motion_alpha` is reached.
    pub motion_alpha_speed: f32,
    /// Reactive value written by the transparent pass per unit of alpha
    /// (glass, water, lines and other transparent surfaces; not particles).
    pub reactive_transparent: f32,
    /// Reactive value of particle meshes (`AwsmRenderer::set_mesh_taa_particles`,
    /// set by the scene loader for every emitter). Nothing reprojects a
    /// particle, so a covered pixel takes this weight whatever the sprite's
    /// alpha (a soft sprite's low-alpha skirt would otherwise keep its stale
    /// colour for many frames). `docs/TAA.md` §3.6.
    pub reactive_particles: f32,
    /// Reactive value per unit of alpha written by lines (`docs/TAA.md`
    /// §3.6). Lines have no motion vectors; an app that animates lines
    /// raises it (1.0 = a moving line keeps no history, at the cost of
    /// aliased line edges).
    pub reactive_lines: f32,
    /// At `TaaDisocclusion::DepthMotionAndId`: the current-frame weight of a
    /// pixel whose history came from a 2x2 that held an object now absent
    /// from its 3x3 (a resting edge released by a sub-pixel move). 0 = off
    /// (default); 1.0 removes the last measured trail event but raises
    /// still-image flicker on thin wires and small instances a sample missed
    /// (`taa-thin` 0.301 -> 0.323, `instancing-stress` 0.105 -> 0.151).
    /// `docs/TAA.md` §2.4.
    pub edge_release: f32,
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
    /// lets the image sharpen again within a few frames of stopping. 1.0
    /// (round 3; was 2.0): closer to the reference in motion on every
    /// measured scene and steadier frame to frame (`docs/TAA.md` §2.4).
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
    /// 0.5 (round 3; was 2.0): a silhouette moving slower than that kept
    /// part of its history and ghosted for 15-45 frames (skinned legs, a
    /// deforming edge; `docs/TAA.md` §6, §7.5).
    pub motion_edge_speed: f32,
}

impl Default for TaaTuning {
    fn default() -> Self {
        Self {
            gamma_still: 1.25,
            gamma_moving: 0.75,
            disocclusion_threshold: 0.05,
            max_history_native: 16.0,
            max_history_upscaling: 64.0,
            motion_alpha: 0.2,
            motion_alpha_speed: 8.0,
            reactive_transparent: 0.3,
            reactive_particles: 1.0,
            reactive_lines: 0.3,
            edge_release: 0.0,
            reactive_material: 0.2,
            reactive_ssr: 0.0,
            mip_bias_extra: 0.0,
            gamma_moving_speed: 2.0,
            max_history_moving: 1.0,
            max_history_moving_speed: 1.0,
            motion_edge_alpha: 1.0,
            motion_edge_speed: 0.5,
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
    /// Harness override of [`CLIP_ALPHA`] (the second value is no longer
    /// used: see `set_taa_experimental_tuning`).
    pub(crate) experimental: Option<[f32; 2]>,
    /// Harness diagnostics: particles write `alpha * reactive_particles`
    /// (alpha-scaled) instead of the weight on every covered pixel.
    pub(crate) particles_alpha_scaled: bool,
    /// Resolve dispatches per frame (`AwsmRenderer::set_taa_debug_repeat`,
    /// cost measurement). 0 (the default) and 1 both mean one dispatch.
    pub(crate) debug_repeat: u32,
    /// Why TAA was replaced by MSAA 4x on this renderer, if it was (see
    /// [`AwsmRenderer::taa_fallback_reason`]).
    pub(crate) fallback_reason: Option<String>,
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

/// The history-length cap (N_max) for a render and an output size.
///
/// Native: `max_history_native`. Upscaling: the native cap divided by the
/// pixel ratio, so the history holds as many render samples per output
/// pixel as it does at native resolution (16 frames at native is 36 at 0.67
/// and 64 at 0.5), up to `max_history_upscaling`. A fixed 24 left the fine
/// detail of an upscaled still image flickering: each frame brings fewer
/// samples per output pixel, so the same memory averages fewer of them.
pub fn history_cap(tuning: &TaaTuning, render_size: (u32, u32), post_size: (u32, u32)) -> f32 {
    if render_size == post_size {
        return tuning.max_history_native;
    }
    let ratio = (post_size.0.max(1) as f32 * post_size.1.max(1) as f32)
        / (render_size.0.max(1) as f32 * render_size.1.max(1) as f32);
    (tuning.max_history_native * ratio.max(1.0)).min(tuning.max_history_upscaling)
}

/// The still clip-box half-width (γ) for a render and an output size.
///
/// Native: `gamma_still`. Upscaling widens it by 0.8 per unit of
/// (output / render − 1): 1.25 becomes 1.74 at 0.67 and 2.25 at 0.5. A
/// feature smaller than a render pixel is seen by only some jitter phases,
/// and the native box clips it back out on the frames that miss it: at 0.67
/// one isolated feature in six was gone from a still image, one in twenty
/// with the wider box, and still-image flicker dropped too. The price is on
/// the strip a moving object leaves (about 12% more error at 0.67).
pub fn still_gamma(tuning: &TaaTuning, render_size: (u32, u32), post_size: (u32, u32)) -> f32 {
    let ratio = post_size.0.max(1) as f32 / render_size.0.max(1) as f32;
    tuning.gamma_still * (1.0 + STILL_GAMMA_UPSCALE * (ratio - 1.0).max(0.0))
}

/// How much [`still_gamma`] widens per unit of (output / render − 1).
pub const STILL_GAMMA_UPSCALE: f32 = 0.8;

/// Current-frame weight per unit of clip amount: a heavily clipped history
/// is suspect, so the more of it the clip removed, the less it is trusted.
pub const CLIP_ALPHA: f32 = 1.0;

/// The exponent of [`reconstruction_weight`].
pub const RECONSTRUCTION_SHARPNESS: f32 = 2.29;

/// The texture LOD bias under TAA (03 §7): `log2(render / post)` (negative
/// when upscaling: textures sampled at render resolution pick the mip the
/// OUTPUT resolution needs) plus the tuning's `mip_bias_extra`. Uses the
/// width ratio (both axes scale by the same render scale). Non-finite input
/// gives 0.
///
/// Under TAA the value is in the camera uniform's `prev_position.w` (0
/// without TAA). Built-in and custom materials get it without doing
/// anything: the opaque path scales the stored UV gradients by `2^bias` in
/// the geometry pass, the transparent pass samples with `textureSampleBias`
/// in the texture-pool helpers. Custom WGSL that computes its own LOD (an
/// explicit `textureSampleLevel`) should add `camera_raw.prev_position.w`.
/// Alpha-test cutouts sample LOD 0 and need no bias.
pub fn texture_lod_bias(render: (u32, u32), post: (u32, u32), extra: f32) -> f32 {
    let bias = (render.0.max(1) as f32 / post.0.max(1) as f32).log2() + extra;
    if bias.is_finite() {
        bias
    } else {
        0.0
    }
}

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
    /// factor, 14 = R dilated / G disocclusion test 1 / B test 2, 15 = R
    /// alpha >= 0.99 / G motion-boundary term / B clip term, 16 = test 2's
    /// parts: R depth / G MIXED / B COVERED. Like every view, they need the
    /// debug variant ([`Self::taa_debug_variant_ready`]). `None` returns to
    /// [`Self::set_taa_debug_view`]'s view.
    #[doc(hidden)]
    pub fn set_taa_debug_view_raw(&mut self, id: Option<u32>) {
        self.taa.debug_view_raw = id;
        self.start_taa_debug_variant();
    }

    /// Whether the resolve variant with the debug views is ready for the
    /// next frame (or failed to compile, then the normal image shows). The
    /// variant compiles on first selection of a view; until it is ready the
    /// production resolve runs and the view shows the normal image. Tests
    /// that read a debug view wait for this after selecting one. True when
    /// no view is selected or TAA is off.
    pub fn taa_debug_variant_ready(&self) -> bool {
        if self.taa.debug_shader_id() == 0 {
            return true;
        }
        self.render_passes
            .taa
            .as_ref()
            .is_none_or(|pass| pass.pipelines.debug_settled())
    }

    /// Start compiling the debug-view resolve variant if a view is selected
    /// (it compiles on first need; see [`Self::taa_debug_variant_ready`]).
    fn start_taa_debug_variant(&mut self) {
        if self.taa.debug_shader_id() == 0 {
            return;
        }
        if let Some(pass) = self.render_passes.taa.as_mut() {
            pass.pipelines.ensure_debug(
                &self.gpu,
                &mut self.shaders,
                &self.pipeline_layouts,
                &mut self.pipelines.compute,
            );
        }
    }

    /// Harness diagnostics: overrides [`CLIP_ALPHA`] for tuning sweeps.
    /// `None` restores it. The second value was the reactive clip-box scale,
    /// removed once tuning settled on 1.0 (no narrowing); it is ignored, and
    /// stays in the signature only until the harness drops the knob.
    #[doc(hidden)]
    pub fn set_taa_experimental_tuning(&mut self, values: Option<[f32; 2]>) {
        self.taa.experimental = values;
    }

    /// Harness diagnostics: particles write an alpha-scaled reactive value
    /// (`alpha * reactive_particles`) instead of the weight on every covered
    /// pixel, to compare the two.
    #[doc(hidden)]
    pub fn set_taa_particles_alpha_scaled(&mut self, on: bool) {
        self.taa.particles_alpha_scaled = on;
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
        self.start_taa_debug_variant();
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

    #[test]
    fn texture_lod_bias_follows_the_scale() {
        assert_eq!(texture_lod_bias((1920, 1080), (1920, 1080), 0.0), 0.0);
        assert_eq!(texture_lod_bias((960, 540), (1920, 1080), 0.0), -1.0);
        assert!((texture_lod_bias((1286, 724), (1920, 1080), 0.0) - (-0.578)).abs() < 1e-2);
        assert_eq!(texture_lod_bias((960, 540), (1920, 1080), 0.25), -0.75);
        assert_eq!(texture_lod_bias((960, 540), (1920, 1080), f32::NAN), 0.0);
    }

    #[test]
    fn render_scale_is_quantised_with_exact_presets() {
        let at = |render_scale: f32| {
            TaaConfig {
                render_scale,
                ..TaaConfig::native()
            }
            .clamped_render_scale()
        };
        for preset in TaaPreset::ALL {
            assert_eq!(at(preset.render_scale()), preset.render_scale());
        }
        assert_eq!(at(1.0), 1.0);
        // A governor's small nudges land on the same step.
        assert_eq!(at(0.6712), 0.67);
        assert_eq!(at(0.6688), 0.67);
        assert_eq!(at(0.6741), 0.67);
        assert_eq!(at(0.6762), 0.68);
        // Clamped first, and the step never leaves the range.
        assert_eq!(at(0.1), TaaConfig::MIN_RENDER_SCALE);
        assert_eq!(at(1.7), 1.0);
        assert_eq!(at(0.996), 1.0);
        assert_eq!(at(f32::INFINITY), 1.0);
    }
}
