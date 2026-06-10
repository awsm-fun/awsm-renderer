//! Pure-data glTF → canonical AWSM-format conversion.
//!
//! This is the single import/convert path (see `docs/buffers.md` +
//! `docs/plans/mesh-pipeline-overhaul.md`). It runs entirely on bytes — no GPU,
//! no browser, no renderer — so it's exhaustively property-testable. Both the
//! editor and the player call [`convert`] before any population:
//!
//! ```text
//! foreign glTF ──convert──▶ canonical glb (geometry-only, AWSM_format-stamped)
//!                           + extracted materials + animations
//! our own glb (AWSM_format) ──convert──▶ passed through unchanged
//! ```
//!
//! The [`AWSM_FORMAT`] extension marker makes the round-trip idempotent:
//! converting our own export is a no-op (`convert(convert(x)) == convert(x)`).
//!
//! ## Implementation status (overnight Phase 3 — see the plan doc)
//! Increment 1 (this file): crate + `AWSM_FORMAT` + detection + the geometry
//! conversion (clean glb via `glb-export`'s `reexport_clean_scene`/`write_glb`).
//! FOLLOW-ON increments, each its own commit:
//!   - stamp `AWSM_FORMAT` onto the output glb (needs a `glb-export` writer hook)
//!     → unlocks the idempotency pass-through + its proptest;
//!   - bake tangents + ensure normals into the canonical glb (needs
//!     `MeshData.tangents` + a `TANGENT` accessor in `glb-export::write_glb`);
//!   - extract materials + animations (move the pure logic out of the editor
//!     bridge: `extract_material_specs`/`extract_extensions`/`extract_animations`).

use awsm_glb_export::{reexport_clean_scene, write_glb};

/// Document-level glTF extension stamped onto a canonical AWSM glb. Its presence
/// means "this glb was produced by our exporter / converter and is already in
/// canonical form" — re-converting it is a no-op. Carries a version so a future
/// canonical-form change is detectable rather than silently mis-read.
pub const AWSM_FORMAT: &str = "AWSM_format";

/// Current canonical-form version stamped under [`AWSM_FORMAT`]'s `version` field.
pub const AWSM_FORMAT_VERSION: u32 = 1;

/// The result of [`convert`]: a canonical, geometry-only glb plus the
/// material/animation data lifted out of the source (ours, in editor/player form).
#[derive(Debug, Clone, Default)]
pub struct CanonicalImport {
    /// Canonical glb bytes — geometry only, multi-primitive nodes preserved,
    /// AWSM_format-stamped (stamping lands in a follow-on increment).
    pub glb: Vec<u8>,
    /// `true` when the input already carried [`AWSM_FORMAT`] and was passed
    /// through untouched (no re-conversion).
    pub is_already_canonical: bool,
    /// The canonical-form version read from the input's `AWSM_format` (when
    /// `is_already_canonical`), else `None`.
    pub format_version: Option<u32>,
    // FOLLOW-ON: extracted materials (our `MaterialDefinition`s) + animation clips.
    // Empty until the extraction increment moves the editor's pure logic here.
}

/// Conversion failures (all pure-data; no I/O).
#[derive(Debug)]
pub enum ConvertError {
    /// The bytes didn't parse as glTF/GLB.
    Parse(String),
    /// The document has no usable scene to convert.
    NoScene,
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::Parse(e) => write!(f, "glTF parse: {e}"),
            ConvertError::NoScene => write!(f, "glTF has no convertible scene"),
        }
    }
}
impl std::error::Error for ConvertError {}

/// Does this parsed document carry the [`AWSM_FORMAT`] marker (i.e. it's already
/// canonical)?
pub fn is_canonical(doc: &gltf::Document) -> bool {
    doc.extensions_used().any(|e| e == AWSM_FORMAT)
}

/// Normalize arbitrary glTF/GLB bytes to the canonical AWSM form.
///
/// - Already-canonical input (`AWSM_format` present) → passed through unchanged.
/// - Otherwise → geometry re-exported clean (materials/animations/cruft stripped,
///   multi-primitive nodes preserved, skins kept) via `glb-export`.
pub fn convert(bytes: &[u8]) -> Result<CanonicalImport, ConvertError> {
    let (doc, buffers, _images) =
        gltf::import_slice(bytes).map_err(|e| ConvertError::Parse(e.to_string()))?;

    if is_canonical(&doc) {
        // Our own export — already canonical. Pass the bytes through untouched.
        // (Materials/animations live alongside it in the bundle, not in the glb.)
        return Ok(CanonicalImport {
            glb: bytes.to_vec(),
            is_already_canonical: true,
            // FOLLOW-ON: read the concrete version from the extension value.
            format_version: Some(AWSM_FORMAT_VERSION),
            ..Default::default()
        });
    }

    let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    let scene = reexport_clean_scene(&doc, &buffers).ok_or(ConvertError::NoScene)?;
    let glb = write_glb(&scene);

    Ok(CanonicalImport {
        glb,
        is_already_canonical: false,
        format_version: None,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use awsm_glb_export::{write_glb as export_glb, ExportNode, GlbScene};
    use awsm_meshgen::box_mesh;
    use glam::Vec3;

    /// A plain (non-AWSM) glb converts: geometry survives, and the result is not
    /// flagged already-canonical (since stamping isn't wired yet, a freshly
    /// converted glb is correctly seen as "not yet canonical").
    #[test]
    fn converts_foreign_glb_preserving_geometry() {
        let src = box_mesh(Vec3::splat(2.0));
        let node = ExportNode::new("Cube").with_mesh(src.clone());
        let source_glb = export_glb(&GlbScene {
            nodes: vec![node],
            ..Default::default()
        });

        let out = convert(&source_glb).expect("convert");
        assert!(!out.is_already_canonical);
        assert!(!out.glb.is_empty());

        // The canonical glb re-reads with the same vertex/index counts.
        let mesh = awsm_glb_export::extract_node_mesh_from_bytes(&out.glb, 0, None)
            .expect("canonical glb yields geometry");
        assert_eq!(mesh.positions.len(), src.positions.len());
        assert_eq!(mesh.indices.len(), src.indices.len());
    }

    /// A plain glb is correctly detected as NOT canonical.
    #[test]
    fn plain_glb_is_not_canonical() {
        let glb = export_glb(&GlbScene {
            nodes: vec![ExportNode::new("n").with_mesh(box_mesh(Vec3::ONE))],
            ..Default::default()
        });
        let (doc, _, _) = gltf::import_slice(&glb).unwrap();
        assert!(!is_canonical(&doc));
    }
}
