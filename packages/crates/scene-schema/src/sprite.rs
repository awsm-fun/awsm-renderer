//! Authored sprite / billboard nodes.

use super::primitive::TextureRef;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Eq, Hash, Copy, Default)]
pub enum BillboardMode {
    /// No billboarding — the quad sits in 3D space as authored.
    None,
    /// Rotate around the world Y axis to face the camera (signage, name tags).
    YAxis,
    /// Fully face the camera (round particle stand-ins).
    #[default]
    Full,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Copy, Default)]
pub enum SpriteAlphaMode {
    Opaque,
    Mask {
        cutoff_x1000: u32,
    },
    #[default]
    Blend,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SpriteDef {
    pub texture: Option<TextureRef>,
    pub size: [f32; 2],
    pub billboard: BillboardMode,
    pub alpha_mode: SpriteAlphaMode,
    pub tint: [f32; 4],
    /// Optional grid-uniform sprite-sheet animation. When present, the
    /// sprite materializes as a flipbook material sampling `texture` as
    /// an N×M atlas and animating per `frame_globals.time +
    /// time_offset`. Default `None` keeps the existing single-cell
    /// unlit sprite behaviour.
    #[serde(default)]
    pub flipbook: Option<SpriteFlipBookDef>,
}

impl Default for SpriteDef {
    fn default() -> Self {
        Self {
            texture: None,
            size: [1.0, 1.0],
            billboard: BillboardMode::default(),
            alpha_mode: SpriteAlphaMode::default(),
            tint: [1.0, 1.0, 1.0, 1.0],
            flipbook: None,
        }
    }
}

/// Grid-uniform sprite-sheet flipbook configuration attached to a
/// [`SpriteDef`]. Runtime semantics match
/// `awsm_materials::flipbook::FlipBookMaterial`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Copy)]
#[serde(rename_all = "snake_case")]
pub struct SpriteFlipBookDef {
    /// Atlas columns. Must be `>= 1`.
    pub cols: u32,
    /// Atlas rows. Must be `>= 1`.
    pub rows: u32,
    /// Number of cells actually used (typically `<= cols * rows`).
    /// `1` displays only cell 0 regardless of time / mode.
    pub frame_count: u32,
    /// Playback rate in frames per second. `0.0` freezes on cell 0.
    pub fps: f32,
    /// Per-instance phase offset in seconds. Two sprites referencing
    /// the same atlas with different `time_offset` show different
    /// cells on the same frame.
    #[serde(default)]
    pub time_offset: f32,
    /// Playback mode — see [`FlipBookModeDef`].
    #[serde(default)]
    pub mode: FlipBookModeDef,
    /// Atlas cell indexing direction. `false` (default) reads cell 0
    /// at the top-left, growing right-then-down; `true` reads cell 0
    /// at the bottom-left.
    #[serde(default)]
    pub flip_y: bool,
}

/// Playback mode for a [`SpriteFlipBookDef`]. Mirrors
/// `awsm_materials::flipbook::FlipBookMode`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default, Copy)]
#[serde(rename_all = "snake_case")]
pub enum FlipBookModeDef {
    /// Wrap on `frame_count`.
    #[default]
    Loop,
    /// Forward then reverse (`0,1,...,N-1,N-2,...,1,0,1,...`).
    PingPong,
    /// Stop on the last frame.
    Clamp,
    /// Play once; past the end alpha becomes 0.
    Once,
}
