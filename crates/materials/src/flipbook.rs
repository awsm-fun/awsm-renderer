//! Sprite-sheet flipbook material — grid-uniform atlas, sequential cell
//! playback driven by `frame_globals.time`.
//!
//! Covers the common VFX / UI sprite-sheet case (4×4 explosion, 8×8 smoke
//! loop, UI button frames). Irregular-cell atlases (TexturePacker-style)
//! are intentionally out of scope here and live with the
//! dynamic-materials follow-up.
//!
//! The WGSL implementation lives in `wgsl/flipbook_material.wgsl`. The
//! renderer pulls that fragment via the `{{ materials_wgsl }}` askama
//! variable when the `flipbook` feature is on.

use crate::{
    shader::MaterialShader,
    writer::{write, write_material_texture},
    MaterialAlphaMode, MaterialShaderId, MaterialTexture, TextureContext,
};

/// WGSL helper module for this material.
pub const WGSL_FRAGMENT: &str = include_str!("wgsl/flipbook_material.wgsl");

/// Playback mode for a [`FlipBookMaterial`].
///
/// The numeric values are written into the material payload as `u32` and
/// dispatched against by the WGSL `flipbook_apply_mode` function. Keep
/// in lockstep with the `FLIPBOOK_MODE_*` consts there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FlipBookMode {
    /// Wrap on `frame_count`: `frame = floor(t * fps) mod frame_count`.
    #[default]
    Loop = 0,
    /// Forward then reverse: `0,1,2,3,2,1,0,1,...` for a 4-frame sheet
    /// (period `2 * frame_count - 2`).
    PingPong = 1,
    /// Advance to the last frame, then hold there forever.
    Clamp = 2,
    /// Advance once; after the last frame, write `alpha = 0` so the
    /// quad disappears cleanly when used with `alpha_mode = Blend`.
    /// (Pairing `Once` with an opaque alpha mode is undefined — the
    /// shader still freezes on the final frame, which is the more
    /// useful fallback.)
    Once = 3,
}

impl FlipBookMode {
    /// Numeric value packed into the material payload + matched by
    /// `flipbook_apply_mode` in WGSL.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Sprite-sheet flipbook material.
///
/// Samples a single cell from a grid-uniform atlas per frame, based on
/// `frame_globals.time + time_offset`, `fps`, and `mode`. Result is the
/// sampled cell multiplied by `tint`.
///
/// Use `time_offset` to phase per-instance copies of the same material
/// (smoke / sparks scattered across the scene without all advancing in
/// lockstep). Setting `fps = 0.0` freezes the material on cell 0,
/// useful as a static cell-cropper.
#[derive(Clone, Debug)]
pub struct FlipBookMaterial {
    /// Sprite-sheet atlas. `None` renders as `tint` only — useful as
    /// a sanity check during scene authoring, not intended as a
    /// shipping configuration.
    pub atlas_tex: Option<MaterialTexture>,
    /// Multiplier on the sampled atlas color. Default `[1, 1, 1, 1]`.
    pub tint: [f32; 4],
    /// Number of columns in the atlas grid. Must be `>= 1`.
    pub cols: u32,
    /// Number of rows in the atlas grid. Must be `>= 1`.
    pub rows: u32,
    /// Number of cells actually used (typically `<= cols * rows`).
    /// `1` displays only cell 0 regardless of time / mode.
    pub frame_count: u32,
    /// Playback rate in frames per second. `0.0` freezes on cell 0
    /// (useful as a static cell-cropper).
    pub fps: f32,
    /// Per-instance phase offset in seconds. Two instances of the same
    /// material with different `time_offset` show different cells on
    /// the same frame.
    pub time_offset: f32,
    /// Playback mode — see [`FlipBookMode`].
    pub mode: FlipBookMode,
    /// Atlas cell indexing direction. `false` (default) reads
    /// cell 0 at the top-left, growing right-then-down; `true` reads
    /// cell 0 at the bottom-left, growing right-then-up. Use this to
    /// match the convention of your sprite-sheet exporter.
    pub flip_y: bool,
    // Immutable properties — changing them requires recreating the
    // material (same shape as Unlit/PBR/Toon).
    alpha_mode: MaterialAlphaMode,
    double_sided: bool,
}

impl FlipBookMaterial {
    /// Creates a flipbook material with sensible defaults: opaque
    /// fields zeroed, `tint` white, `fps = 24`, `mode = Loop`,
    /// `cols = rows = 1`, `frame_count = 1`. Callers populate
    /// `atlas_tex`, `cols`, `rows`, `frame_count` (and optionally
    /// `tint` / `fps` / `mode` / `time_offset` / `flip_y`) before
    /// inserting into the material storage.
    pub fn new(alpha_mode: MaterialAlphaMode, double_sided: bool) -> Self {
        Self {
            atlas_tex: None,
            tint: [1.0, 1.0, 1.0, 1.0],
            cols: 1,
            rows: 1,
            frame_count: 1,
            fps: 24.0,
            time_offset: 0.0,
            mode: FlipBookMode::Loop,
            flip_y: false,
            alpha_mode,
            double_sided,
        }
    }

    /// Returns the material alpha mode by reference.
    pub fn alpha_mode(&self) -> &MaterialAlphaMode {
        &self.alpha_mode
    }

    /// Returns whether the material is double sided.
    pub fn double_sided(&self) -> bool {
        self.double_sided
    }

    /// Returns the alpha cutoff for masked materials.
    pub fn alpha_cutoff(&self) -> Option<f32> {
        match self.alpha_mode {
            MaterialAlphaMode::Mask { cutoff } => Some(cutoff),
            _ => None,
        }
    }

    /// Returns true if alpha blending is enabled.
    pub fn has_alpha_blend(&self) -> bool {
        matches!(self.alpha_mode, MaterialAlphaMode::Blend)
    }
}

impl MaterialShader for FlipBookMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        MaterialShaderId::FlipBook
    }

    fn wgsl_fragment(&self) -> &'static str {
        WGSL_FRAGMENT
    }

    fn alpha_mode(&self) -> MaterialAlphaMode {
        self.alpha_mode
    }

    fn is_transparency_pass(&self) -> bool {
        self.has_alpha_blend() || self.alpha_cutoff().is_some()
    }

    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, data: &mut Vec<u8>) {
        // Validation deferred to authoring time: zero-frame materials
        // would divide by zero in WGSL; `frame_count > cols * rows`
        // would index off the end of the atlas grid. Log + clamp so
        // a misauthored material renders harmlessly (cell 0) rather
        // than wedging the renderer.
        if self.frame_count == 0 {
            tracing::warn!(
                "[flipbook] frame_count == 0 is invalid; treating as 1. (cols={}, rows={})",
                self.cols,
                self.rows
            );
        }
        let max_frames = self.cols.saturating_mul(self.rows).max(1);
        if self.frame_count > max_frames {
            tracing::warn!(
                "[flipbook] frame_count={} exceeds cols*rows={}; clamping. \
                 Reduce frame_count or grow the atlas grid.",
                self.frame_count,
                max_frames
            );
        }
        let frame_count_clamped = self.frame_count.max(1).min(max_frames);
        let cols_clamped = self.cols.max(1);
        let rows_clamped = self.rows.max(1);

        write(data, self.shader_id().as_u32().into());

        write(data, self.alpha_mode.variant_as_u32().into());
        write(data, self.alpha_cutoff().unwrap_or(0.0f32).into());

        write_material_texture(data, self.atlas_tex.as_ref(), ctx);

        write(data, self.tint[0].into());
        write(data, self.tint[1].into());
        write(data, self.tint[2].into());
        write(data, self.tint[3].into());

        write(data, cols_clamped.into());
        write(data, rows_clamped.into());
        write(data, frame_count_clamped.into());
        write(data, self.fps.into());
        write(data, self.time_offset.into());
        write(data, self.mode.as_u32().into());
        write(data, u32::from(self.flip_y).into());
    }
}
