//! Material shader identifiers.
//!
//! Each variant's `repr(u32)` value is written as the first word of the
//! material's storage-buffer slot and dispatched against in the
//! visibility-buffer compute pass + transparent fragment shader.
//!
//! Ids are assigned at compile time and are stable per build. They are not
//! intended to round-trip through any persisted format — the runtime always
//! re-renders the dispatch table from the registry.

/// Stable per-build identifier for a material shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum MaterialShaderId {
    /// Physically based rendering. See `awsm-materials::pbr`.
    Pbr = 1,
    /// Unlit (constant emissive surface). See `awsm-materials::unlit`.
    Unlit = 2,
    /// Toon / cel-shading. See `awsm-materials::toon`.
    Toon = 3,
    /// Sprite-sheet flipbook. See `awsm-materials::flipbook`.
    FlipBook = 4,
}

impl MaterialShaderId {
    /// Returns the numeric id as written into the material payload.
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    /// Returns the canonical WGSL constant name (`SHADER_ID_PBR`, etc.).
    /// Used by the dispatch-table generator on the renderer side.
    pub fn wgsl_const_name(self) -> &'static str {
        match self {
            Self::Pbr => "SHADER_ID_PBR",
            Self::Unlit => "SHADER_ID_UNLIT",
            Self::Toon => "SHADER_ID_TOON",
            Self::FlipBook => "SHADER_ID_FLIPBOOK",
        }
    }
}
