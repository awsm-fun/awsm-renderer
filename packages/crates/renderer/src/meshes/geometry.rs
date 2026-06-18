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

use awsm_renderer_core::pipeline::primitive::FrontFace;
use slotmap::new_key_type;

use crate::bounds::Aabb;
use crate::materials::Material;
use crate::meshes::buffer_info::{
    MeshBufferGeometryMorphInfo, MeshBufferMaterialMorphInfo, MeshBufferSkinInfo,
    MeshBufferVertexAttributeInfo,
};
use crate::meshes::morphs::{GeometryMorphKey, MaterialMorphKey};
use crate::meshes::skins::SkinKey;

new_key_type! {
    /// Handle to a registered [`GeometrySource`]. Multiple meshes (different
    /// materials / transforms) bind to one `GeometryKey` and share its GPU
    /// representations — the dedup unit (§1 ③).
    pub struct GeometryKey;
}

/// The retained CPU source for one piece of geometry — enough to pack EITHER
/// pass representation (visibility / transparency) at commit, without re-supply.
///
/// Held only from `register_geometry` until its first `commit_load` consumes it
/// (then dropped — §1 ②). The custom-attribute side (UVs/colors + their layout +
/// the per-triangle index bytes) is pass-INDEPENDENT and pre-built once; the
/// per-pass vertex streams are derived at commit from `positions` / `normals` /
/// `uvs0` / `indices` via [`crate::mesh_pack::pack_visibility_bytes`] /
/// [`crate::mesh_pack::pack_transparency_bytes`]. Both the raw-mesh path and the
/// glTF decoder produce this same struct (source-agnostic, §5).
pub struct GeometrySource {
    /// Per-vertex positions (original, non-exploded).
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex normals — area-weighted face normals computed at register if
    /// the caller didn't supply them. Material-independent, so safe to compute
    /// up front (unlike tangents, which are derived at commit — see §6 step 2).
    pub normals: Vec<[f32; 3]>,
    /// UV set 0, retained so commit can run MikkTSpace tangent generation when a
    /// bound material samples a normal map. `None` ⇒ no tangents (the packer's
    /// synthetic `[0,0,0,1]` fallback, same as today).
    pub uvs0: Option<Vec<[f32; 2]>>,
    /// AUTHORED tangents (e.g. a glTF `TANGENT` attribute). When present, commit
    /// uses these verbatim; when `None` and a bound material samples a normal map,
    /// commit generates them via MikkTSpace from positions/normals/uv0/indices.
    pub tangents: Option<Vec<[f32; 4]>>,
    /// Triangle indices into the per-vertex streams.
    pub indices: Vec<u32>,
    /// Winding for the packed visibility stream.
    pub front_face: FrontFace,
    /// Pass-independent custom-attribute layout (UVs / colors / …), in
    /// declaration order. Becomes the mesh's `buffer_info.triangles.vertex_attributes`.
    pub vertex_attributes: Vec<MeshBufferVertexAttributeInfo>,
    /// Pass-independent custom-attribute bytes (AoS, one record per original
    /// vertex), pre-packed once and shared by both representations.
    pub custom_attribute_bytes: Vec<u8>,
    /// Pass-independent per-triangle attribute-index bytes (3×`u32` per triangle).
    pub attribute_index_bytes: Vec<u8>,
    /// World-independent local-space AABB (computed from positions).
    pub aabb: Option<Aabb>,
    /// Optional geometry morph-target data (already inserted; shared per geometry)
    /// plus its buffer-layout. Morph deltas are kind-independent (they delta the
    /// base attributes), so they travel with the source and are reattached to the
    /// rebuilt `buffer_info` at resolve. `None` for the raw path.
    pub geometry_morph_key: Option<GeometryMorphKey>,
    pub geometry_morph_info: Option<MeshBufferGeometryMorphInfo>,
    /// Optional material morph-target data plus layout.
    pub material_morph_key: Option<MaterialMorphKey>,
    pub material_morph_info: Option<MeshBufferMaterialMorphInfo>,
    /// Optional skin (rig) for skinned geometry (already inserted; per geometry)
    /// plus its buffer-layout.
    pub skin_key: Option<SkinKey>,
    pub skin_info: Option<MeshBufferSkinInfo>,
}

impl GeometrySource {
    /// Number of original (non-exploded) vertices.
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }
    /// Number of triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

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
