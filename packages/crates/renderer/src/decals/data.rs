//! Decal data types — the CPU-side authoring surface.

use glam::Mat4;

use crate::bounds::Aabb;

slotmap::new_key_type! {
    /// Opaque handle into the renderer's decal table. Stable across
    /// frames; passed to `AwsmRenderer::update_decal` / `remove_decal`
    /// to mutate or release the decal.
    pub struct DecalKey;
}

/// Blend mode for a decal's accumulation onto `opaque_tex`.
///
/// v1 ships only [`DecalBlendMode::AlphaBlend`] — the canonical
/// Decima/D3-style overlay. Additional modes (e.g.
/// `Additive: final = opaque + decal.rgb * decal.a * alpha`,
/// `Multiply: final = opaque * (1.0 - decal.a) + opaque * decal.rgb *
/// decal.a`) would join this enum and route through the same
/// `decal_blend_mode` u32 the per-decal GPU layout already carries —
/// the WGSL match on `blend_mode` is the single per-pixel branch to
/// extend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum DecalBlendMode {
    #[default]
    AlphaBlend = 0,
    // Additive = 1,
    // Multiply = 2,
}

/// One projection decal.
///
/// A decal is an *oriented unit cube* in world space. The cube
/// projects its texture down its local -Z axis onto whatever geometry
/// sits inside the volume; local-space `xy` maps to the texture's
/// `uv` after a 0.5×+0.5 remap.
///
/// The unit-cube convention means `transform`'s scale columns directly
/// encode the decal's world-space size; rotation columns encode its
/// projection direction. Non-uniform scales are supported (a decal can
/// be a thin slab or a tall pillar).
#[derive(Debug, Clone)]
pub struct Decal {
    /// World-space transform of the decal volume (oriented unit cube
    /// from `-1..1` along each local axis).
    pub transform: Mat4,
    /// Cached inverse of `transform` — refreshed on every
    /// [`crate::AwsmRenderer::update_decal`] call so the per-pixel
    /// "is this world point inside the cube?" test is a single
    /// `inverse_transform * world_pos`.
    pub inverse_transform: Mat4,
    /// Texture-pool index of the decal's RGBA texture. Stored as an
    /// index because the pool's array layer set may grow / shuffle
    /// at runtime (matches the convention used by
    /// `MaterialTexture::Index`).
    pub texture_index: u32,
    /// Multiplier on the texture's sampled alpha. `1.0` uses the
    /// texture's authored alpha verbatim; lower values fade the
    /// decal globally.
    pub alpha: f32,
    /// Blend accumulation mode. v1 ships alpha-blend only.
    pub blend_mode: DecalBlendMode,
    /// World-space AABB of the transformed unit cube. Recomputed on
    /// `update_decal`. Used by the decal classify pass
    /// (`render_passes::material_decal::classify`) to bucket each decal
    /// into the screen tiles its AABB touches, optionally HZB-gated
    /// when `features.gpu_culling` is on; the shading compute then
    /// reads each tile's per-bucket list rather than testing every
    /// decal at every pixel.
    pub world_aabb: Aabb,
}

impl Decal {
    /// Builds a decal from a world-space transform + texture +
    /// authoring alpha. Computes the inverse transform and the
    /// world-space AABB up-front so per-frame work is just a
    /// re-upload.
    pub fn new(transform: Mat4, texture_index: u32, alpha: f32) -> Self {
        let inverse_transform = transform.inverse();
        let world_aabb = aabb_of_unit_cube(&transform);
        Self {
            transform,
            inverse_transform,
            texture_index,
            alpha,
            blend_mode: DecalBlendMode::AlphaBlend,
            world_aabb,
        }
    }
}

/// World-space AABB containing the eight corners of a unit cube
/// (local `(-1..1)^3`) transformed by `m`. Used by the HZB-based
/// classification follow-up to find which tiles a decal may overlap.
fn aabb_of_unit_cube(m: &Mat4) -> Aabb {
    let mut min = glam::Vec3::splat(f32::INFINITY);
    let mut max = glam::Vec3::splat(f32::NEG_INFINITY);
    let signs: [f32; 2] = [-1.0, 1.0];
    for &sx in &signs {
        for &sy in &signs {
            for &sz in &signs {
                let corner = m.transform_point3(glam::Vec3::new(sx, sy, sz));
                min = min.min(corner);
                max = max.max(corner);
            }
        }
    }
    Aabb { min, max }
}
