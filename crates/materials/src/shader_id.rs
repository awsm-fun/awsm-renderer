//! Material shader identifiers.
//!
//! The id is written as the first word of the material's storage-buffer slot
//! and dispatched against in the visibility-buffer compute pass + transparent
//! fragment shader.
//!
//! First-party materials hold associated-constant ids in the low range
//! ([`MaterialShaderId::PBR`] / [`UNLIT`](Self::UNLIT) / [`TOON`](Self::TOON) /
//! [`FLIPBOOK`](Self::FLIPBOOK)). Runtime-registered dynamic materials get ids
//! at or above [`MaterialShaderId::DYNAMIC_START`] assigned by the registry —
//! see [`is_dynamic`](Self::is_dynamic).
//!
//! The on-disk numeric representation is unchanged from the pre-1.0 enum form
//! — a `u32`. The shape is a `repr(transparent)` newtype so the dynamic range
//! is open-ended; first-party ids stay stable per build.

/// Stable per-build identifier for a material shader.
///
/// Constructed via associated constants for first-party materials or
/// internally by the dynamic-material [`registry`](crate::registry) for
/// runtime-registered ones. Game code never builds a [`MaterialShaderId`]
/// from a raw u32.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct MaterialShaderId(u32);

impl MaterialShaderId {
    /// Physically based rendering. See `awsm-materials::pbr`.
    pub const PBR: Self = Self(1);
    /// Unlit (constant emissive surface). See `awsm-materials::unlit`.
    pub const UNLIT: Self = Self(2);
    /// Toon / cel-shading. See `awsm-materials::toon`.
    pub const TOON: Self = Self(3);
    /// Sprite-sheet flipbook. See `awsm-materials::flipbook`.
    pub const FLIPBOOK: Self = Self(4);
    /// Scanline overlay (promoted from the dynamic-material worked
    /// example). See `awsm-materials::scanline`. Gated by the
    /// `scanline` Cargo feature on this crate.
    pub const SCANLINE: Self = Self(5);

    /// Reserved boundary: ids `6..DYNAMIC_START` are held for future
    /// first-party materials.
    ///
    /// The first dynamic-material registration receives this value; each
    /// subsequent registration receives the next-higher u32. Materials
    /// removed from the registry leave their id holes in place rather than
    /// reusing them.
    pub const DYNAMIC_START: u32 = 10_000;

    /// Returns true if this id was assigned by the dynamic-material
    /// registry (i.e. is `>= DYNAMIC_START`).
    pub fn is_dynamic(self) -> bool {
        self.0 >= Self::DYNAMIC_START
    }

    /// Returns the numeric id as written into the material payload.
    pub fn as_u32(self) -> u32 {
        self.0
    }

    /// Constructs a [`MaterialShaderId`] from a raw u32.
    ///
    /// Internal to the dynamic-material registry. First-party ids must be
    /// referred to via the associated constants ([`Self::PBR`] etc.) and
    /// dynamic ids must be reached through
    /// [`AwsmRenderer::register_material`](../../../awsm_renderer/struct.AwsmRenderer.html#method.register_material)
    /// — there is no game-code-facing path to mint an id with this
    /// constructor.
    /// Constructs a dynamic [`MaterialShaderId`] from its registry-allocated
    /// raw value.
    ///
    /// **Internal to the renderer's dynamic-material registry.** Panics if
    /// `raw < DYNAMIC_START` — first-party ids must be referred to via the
    /// associated constants ([`Self::PBR`] etc.) and game code never builds
    /// a [`MaterialShaderId`] directly; the dynamic id is the return value
    /// of `AwsmRenderer::register_material`.
    pub fn from_dynamic_raw(raw: u32) -> Self {
        assert!(
            raw >= Self::DYNAMIC_START,
            "MaterialShaderId::from_dynamic_raw called with non-dynamic id {raw}; \
             first-party ids must be referred to via the associated constants"
        );
        Self(raw)
    }

    /// Returns the canonical WGSL constant name (`SHADER_ID_PBR`, etc.) for
    /// the four first-party ids.
    ///
    /// Used by the dispatch-table generator on the renderer side. Returns
    /// `None` for dynamic ids — those carry the registered material's name
    /// instead, formatted by the renderer's WGSL emitter.
    pub fn wgsl_const_name(self) -> Option<&'static str> {
        if self == Self::PBR {
            Some("SHADER_ID_PBR")
        } else if self == Self::UNLIT {
            Some("SHADER_ID_UNLIT")
        } else if self == Self::TOON {
            Some("SHADER_ID_TOON")
        } else if self == Self::FLIPBOOK {
            Some("SHADER_ID_FLIPBOOK")
        } else if self == Self::SCANLINE {
            Some("SHADER_ID_SCANLINE")
        } else {
            None
        }
    }
}
