//! Geometry kind — the SINGLE source of truth for "which pass-geometry a material needs".
//!
//! A mesh's geometry kind is a pure function of its material (alpha mode + transmission) plus
//! whether it's a HUD overlay. Historically this decision was duplicated across three places that
//! could disagree (`mesh_buffer_geometry_kind` in the glTF decoder, the
//! `add_raw_mesh` / `add_raw_mesh_transparent` split, and `Materials::is_transparency_pass`), which
//! is what let a mesh's built geometry drift from its routing — the frame-killing
//! `VisibilityGeometryBufferNotFound` class. [`geometry_kind`] is the one function everything funnels
//! through; the load transaction calls it at commit, against each geometry's final bound materials.
//!
//! See `docs/plans/todo.md` §4.

use crate::materials::Material;

/// Which pass-geometry representation(s) a mesh needs, derived from its material.
///
/// The two representations are genuinely different byte streams built from the same source: the
/// VISIBILITY stream (exploded, `triangle_count * 3` vertices, the geometry/opaque + shadow passes)
/// and the TRANSPARENCY stream (original `vertex_count`, the forward transparent pass).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryKind {
    /// Opaque / alpha-mask materials → visibility geometry only.
    Visibility,
    /// Alpha-blend / transmission materials → transparency geometry only.
    Transparency,
    /// HUD overlays draw in both the opaque and transparent passes.
    Both,
}

/// THE decision: which geometry kind does `material` need, given whether the mesh is a HUD overlay?
///
/// - HUD ⇒ [`GeometryKind::Both`] (HUD meshes are drawn in both passes).
/// - otherwise the transparency classification ([`Material::is_transparency_pass`] — Blend or
///   Opaque-with-transmission ⇒ transparency; Opaque / Mask ⇒ visibility) picks the single kind.
///
/// This is the *only* place the kind is decided. The glTF decoder and the raw mesh path both produce
/// a `GeometrySource` (added in step 2); `commit_load` calls this over the union of materials bound
/// to each geometry to decide what to build.
pub fn geometry_kind(material: &Material, is_hud: bool) -> GeometryKind {
    if is_hud {
        GeometryKind::Both
    } else if material.is_transparency_pass() {
        GeometryKind::Transparency
    } else {
        GeometryKind::Visibility
    }
}

#[cfg(test)]
mod tests {
    use super::{geometry_kind, GeometryKind};
    use crate::materials::Material;
    use awsm_materials::{pbr::PbrMaterial, MaterialAlphaMode};

    fn pbr(alpha: MaterialAlphaMode) -> Material {
        Material::Pbr(Box::new(PbrMaterial::new(alpha, false)))
    }

    #[test]
    fn opaque_and_mask_are_visibility() {
        // Mask is alpha-tested OPAQUE per glTF — visibility geometry, not transparency.
        assert_eq!(
            geometry_kind(&pbr(MaterialAlphaMode::Opaque), false),
            GeometryKind::Visibility
        );
        assert_eq!(
            geometry_kind(&pbr(MaterialAlphaMode::Mask { cutoff: 0.5 }), false),
            GeometryKind::Visibility
        );
    }

    #[test]
    fn blend_is_transparency() {
        assert_eq!(
            geometry_kind(&pbr(MaterialAlphaMode::Blend), false),
            GeometryKind::Transparency
        );
    }

    #[test]
    fn hud_overrides_to_both_regardless_of_material() {
        // HUD wins over the material classification, both opaque and blend.
        assert_eq!(
            geometry_kind(&pbr(MaterialAlphaMode::Opaque), true),
            GeometryKind::Both
        );
        assert_eq!(
            geometry_kind(&pbr(MaterialAlphaMode::Blend), true),
            GeometryKind::Both
        );
    }

    #[test]
    fn matches_is_transparency_pass_for_non_hud() {
        // geometry_kind's non-HUD branch must agree with the material classifier exactly —
        // that agreement is what makes the kind/buffer/flag chain impossible to desync.
        // (Transmission → transparency is covered by is_transparency_pass's own tests in the
        // materials crate; here we assert the bridge for the alpha-mode cases.)
        for alpha in [
            MaterialAlphaMode::Opaque,
            MaterialAlphaMode::Mask { cutoff: 0.5 },
            MaterialAlphaMode::Blend,
        ] {
            let m = pbr(alpha);
            let expected = if m.is_transparency_pass() {
                GeometryKind::Transparency
            } else {
                GeometryKind::Visibility
            };
            assert_eq!(geometry_kind(&m, false), expected);
        }
    }
}
