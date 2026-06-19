//! Pure-data glTF → canonical AWSM-format conversion.
//!
//! This is the single import/convert path (see `docs/buffers.md`). It runs entirely on bytes — no GPU,
//! no browser, no renderer — so it's exhaustively property-testable. Both the
//! editor and the player call [`convert`] before any population:
//!
//! ```text
//! foreign glTF ──convert──▶ self-contained canonical glb (geometry + materials
//!                           + textures, AWSM_format-stamped)
//!                           + extracted material / animation specs (for the bridge)
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

pub mod animations;
pub mod images;
pub mod materials;
pub use animations::{extract_animations, AnimChannel, AnimProperty, AnimationSpec, Interpolation};
pub use images::{extract_images, ImageData};
pub use materials::{
    extract_extensions, extract_materials, AlphaMode, Clearcoat, ExtTextureSlot, Iridescence,
    MaterialExtensions, MaterialSpec, Sheen, TexRef, Volume,
};

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
    /// Materials lifted from the source glTF, in neutral form (empty for an
    /// already-canonical/geometry-only glb — its materials live in the bundle).
    pub materials: Vec<MaterialSpec>,
    /// Animations lifted from the source glTF, in neutral form (raw sampler data
    /// keyed by glTF node index). Empty for an already-canonical glb.
    pub animations: Vec<AnimationSpec>,
    /// Texture images' raw encoded bytes, index-aligned with the source glTF's
    /// images (what `MaterialSpec`'s `TexRef.image` points at).
    pub images: Vec<ImageData>,
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
    // Parse WITHOUT decoding images (`Gltf::from_slice` + `import_buffers`, not
    // `import_slice`): a pure-data converter ships RAW image bytes, so it must
    // not depend on the image decoder accepting them — and it skips the decode
    // cost entirely.
    let mut g = gltf::Gltf::from_slice(bytes).map_err(|e| ConvertError::Parse(e.to_string()))?;

    if is_canonical(&g) {
        // Our own export — already canonical. Pass the bytes through untouched.
        // (Materials/animations live alongside it in the bundle, not in the glb.)
        return Ok(CanonicalImport {
            glb: bytes.to_vec(),
            is_already_canonical: true,
            format_version: awsm_format_version(&g).or(Some(AWSM_FORMAT_VERSION)),
            ..Default::default()
        });
    }

    // Resolve buffers (GLB blob + data-URI; no external files — pure data).
    let blob = g.blob.take();
    let buffers = gltf::import_buffers(&g, None, blob)
        .map_err(|e| ConvertError::Parse(format!("buffers: {e}")))?;
    let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    // Lift the source materials + animations + image bytes into neutral specs
    // BEFORE reexport strips them from the geometry-only canonical glb.
    let materials = extract_materials(&g);
    let animations = extract_animations(&g, &buffers);
    let images = extract_images(&g, &buffers);
    let scene = reexport_clean_scene(&g, &buffers).ok_or(ConvertError::NoScene)?;
    let glb = stamp_awsm_format(write_glb(&scene))?;

    Ok(CanonicalImport {
        glb,
        is_already_canonical: false,
        format_version: None,
        materials,
        animations,
        images,
    })
}

/// Inject the [`AWSM_FORMAT`] marker into a GLB's JSON chunk — adds the name to
/// `extensionsUsed` and a document-level `extensions.AWSM_format = { version }`.
/// Pure byte/JSON surgery on the JSON chunk (the BIN chunk is untouched), so the
/// result re-parses as a normal glTF that [`is_canonical`] now recognizes.
pub fn stamp_awsm_format(glb_bytes: Vec<u8>) -> Result<Vec<u8>, ConvertError> {
    let mut glb = gltf::binary::Glb::from_slice(&glb_bytes)
        .map_err(|e| ConvertError::Parse(format!("glb: {e}")))?;
    let mut root: serde_json::Value = serde_json::from_slice(&glb.json)
        .map_err(|e| ConvertError::Parse(format!("glb json: {e}")))?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| ConvertError::Parse("glTF root is not an object".into()))?;

    // extensionsUsed: append AWSM_format if not present.
    let used = obj
        .entry("extensionsUsed")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if let Some(arr) = used.as_array_mut() {
        if !arr.iter().any(|v| v.as_str() == Some(AWSM_FORMAT)) {
            arr.push(serde_json::Value::String(AWSM_FORMAT.to_string()));
        }
    }
    // extensions.AWSM_format = { version }.
    let exts = obj
        .entry("extensions")
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    if let Some(map) = exts.as_object_mut() {
        map.insert(
            AWSM_FORMAT.to_string(),
            serde_json::json!({ "version": AWSM_FORMAT_VERSION }),
        );
    }

    glb.json = std::borrow::Cow::Owned(serde_json::to_vec(&root).expect("reserialize gltf json"));
    glb.to_vec()
        .map_err(|e| ConvertError::Parse(format!("glb write: {e}")))
}

/// Read the `AWSM_format` version from an already-canonical document, if present.
pub fn awsm_format_version(doc: &gltf::Document) -> Option<u32> {
    // The gltf crate doesn't surface unknown document-level extension *values*, so
    // re-read it from the raw extensions json.
    doc.extensions()
        .and_then(|e| e.get(AWSM_FORMAT))
        .and_then(|v| v.get("version"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use awsm_glb_export::{write_glb as export_glb, ExportNode, GlbScene};
    use awsm_meshgen::box_mesh;
    use glam::Vec3;

    fn cube_glb() -> (awsm_meshgen::MeshData, Vec<u8>) {
        let src = box_mesh(Vec3::splat(2.0));
        let glb = export_glb(&GlbScene {
            nodes: vec![ExportNode::new("Cube").with_mesh(src.clone())],
            ..Default::default()
        });
        (src, glb)
    }

    /// A plain (non-AWSM) glb converts: geometry survives AND the output is now
    /// stamped canonical (re-reads with AWSM_format present).
    #[test]
    fn converts_foreign_glb_preserving_geometry() {
        let (src, source_glb) = cube_glb();
        // The source isn't canonical yet.
        let (src_doc, _, _) = gltf::import_slice(&source_glb).unwrap();
        assert!(!is_canonical(&src_doc));

        let out = convert(&source_glb).expect("convert");
        assert!(!out.is_already_canonical);
        assert!(!out.glb.is_empty());

        // The canonical glb re-reads with the same vertex/index counts...
        let mesh = awsm_glb_export::extract_node_mesh_from_bytes(&out.glb, 0, None)
            .expect("canonical glb yields geometry");
        assert_eq!(mesh.positions.len(), src.positions.len());
        assert_eq!(mesh.indices.len(), src.indices.len());

        // ...and is now stamped canonical (version 1).
        let (out_doc, _, _) = gltf::import_slice(&out.glb).unwrap();
        assert!(is_canonical(&out_doc));
        assert_eq!(awsm_format_version(&out_doc), Some(AWSM_FORMAT_VERSION));
    }

    /// A source PBR material is lifted into a neutral MaterialSpec (factors +
    /// alpha + double-sided) while the canonical glb stays geometry-only.
    #[test]
    fn extracts_source_material_factors() {
        use awsm_glb_export::{AlphaMode as ExAlpha, ExportMaterial, PbrMaterial};
        let (_src, _) = cube_glb();
        let mut node = ExportNode::new("Cube").with_mesh(box_mesh(Vec3::splat(2.0)));
        node.material = Some(ExportMaterial::Pbr(PbrMaterial {
            name: "brass".into(),
            base_color: [0.1, 0.2, 0.3, 1.0],
            metallic: 0.25,
            roughness: 0.75,
            emissive: [0.0, 0.0, 0.0],
            alpha_mode: ExAlpha::Mask { cutoff: 0.4 },
            double_sided: true,
            ..Default::default()
        }));
        let source = export_glb(&GlbScene {
            nodes: vec![node],
            ..Default::default()
        });

        let out = convert(&source).expect("convert");
        assert_eq!(out.materials.len(), 1);
        let m = &out.materials[0];
        assert_eq!(m.label, "brass");
        assert_eq!(m.base_color, [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(m.metallic, 0.25);
        assert_eq!(m.roughness, 0.75);
        assert!(m.double_sided);
        assert_eq!(m.alpha_mode, crate::AlphaMode::Mask { cutoff: 0.4 });

        // The canonical glb is now SELF-CONTAINED: per `d4ffbb8c` the re-export
        // carries per-primitive materials through (in addition to the neutral
        // specs asserted above), so the glb renders standalone. Pin the round-trip.
        let (doc, _, _) = gltf::import_slice(&out.glb).unwrap();
        assert_eq!(
            doc.materials().count(),
            1,
            "source material is carried into the canonical glb"
        );
        let carried = doc.materials().next().unwrap();
        assert_eq!(carried.name(), Some("brass"));
        assert!(carried.double_sided());
        assert_eq!(carried.alpha_mode(), gltf::material::AlphaMode::Mask);
        assert_eq!(carried.alpha_cutoff(), Some(0.4));
        let pbr = carried.pbr_metallic_roughness();
        assert_eq!(pbr.base_color_factor(), [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(pbr.metallic_factor(), 0.25);
        assert_eq!(pbr.roughness_factor(), 0.75);
    }

    /// A source glTF animation is lifted into a neutral AnimationSpec (name,
    /// per-channel node index + property + interpolation + raw sampler data),
    /// while the canonical glb is animation-free.
    #[test]
    fn extracts_source_animation() {
        use awsm_glb_export::{AnimInterp, AnimPath, ExportAnimChannel, ExportAnimation};
        let node = ExportNode::new("Cube").with_mesh(box_mesh(Vec3::ONE));
        let anim = ExportAnimation {
            name: "spin".into(),
            channels: vec![ExportAnimChannel {
                node_index: 0,
                path: AnimPath::Rotation,
                interpolation: AnimInterp::Linear,
                times: vec![0.0, 0.5, 1.0],
                // 4/key quaternion xyzw × 3 keys.
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.5, 0.5, 0.0, 0.0, 0.0, 1.0],
            }],
        };
        let source = export_glb(&GlbScene {
            nodes: vec![node],
            animations: vec![anim],
            ..Default::default()
        });

        let out = convert(&source).expect("convert");
        assert_eq!(out.animations.len(), 1);
        let a = &out.animations[0];
        assert_eq!(a.name.as_deref(), Some("spin"));
        assert_eq!(a.channels.len(), 1);
        let ch = &a.channels[0];
        assert_eq!(ch.node_index, 0);
        assert_eq!(ch.property, crate::AnimProperty::Rotation);
        assert_eq!(ch.interpolation, crate::Interpolation::Linear);
        assert_eq!(ch.times, vec![0.0, 0.5, 1.0]);
        assert_eq!(ch.values.len(), 12); // 4/key × 3 keys
        assert_eq!(&ch.values[0..4], &[0.0, 0.0, 0.0, 1.0]);

        // The canonical glb itself carries no animation.
        let (doc, _, _) = gltf::import_slice(&out.glb).unwrap();
        assert_eq!(doc.animations().count(), 0);
    }

    // A valid 1x1 RGBA PNG (so gltf::import_slice's eager image decode succeeds).
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// A source texture image's raw bytes survive convert (and the material's
    /// texture slot points at the right image index).
    #[test]
    fn extracts_image_bytes() {
        use awsm_glb_export::{
            ExportImage, ExportMaterial, ImageMime, PbrMaterial, TexRef as ExTexRef,
        };
        let mut node = ExportNode::new("Cube").with_mesh(box_mesh(Vec3::ONE));
        node.material = Some(ExportMaterial::Pbr(PbrMaterial {
            name: "m".into(),
            base_color_texture: Some(ExTexRef {
                image: 0,
                tex_coord: 0,
                transform: None,
            }),
            ..Default::default()
        }));
        let source = export_glb(&GlbScene {
            nodes: vec![node],
            images: vec![ExportImage {
                name: "tex".into(),
                bytes: PNG_1X1.to_vec(),
                mime: ImageMime::Png,
            }],
            ..Default::default()
        });

        let out = convert(&source).expect("convert");
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].bytes, PNG_1X1);
        assert_eq!(out.images[0].mime_type.as_deref(), Some("image/png"));
        assert_eq!(out.materials[0].base_color_tex.map(|t| t.image), Some(0));
    }

    /// Idempotency: converting an already-canonical glb passes it through
    /// untouched, and convert∘convert == convert on the geometry.
    #[test]
    fn convert_is_idempotent() {
        let (_src, source_glb) = cube_glb();
        let once = convert(&source_glb).expect("convert 1");
        let twice = convert(&once.glb).expect("convert 2");
        assert!(
            twice.is_already_canonical,
            "second pass must detect canonical"
        );
        assert_eq!(twice.format_version, Some(AWSM_FORMAT_VERSION));
        // Pass-through returns the same bytes.
        assert_eq!(twice.glb, once.glb);
    }

    #[test]
    fn foreign_glb_is_not_canonical_and_has_no_version() {
        // A plain (unstamped) glb must read as non-canonical with no version —
        // the precondition that makes `convert` re-export it instead of passing
        // it through.
        let (_m, glb) = cube_glb();
        let g = gltf::Gltf::from_slice(&glb).expect("parse");
        assert!(!is_canonical(&g));
        assert_eq!(awsm_format_version(&g), None);
    }

    #[test]
    fn stamp_is_idempotent_no_duplicate_extension() {
        // Stamping an already-stamped glb must NOT append a second AWSM_format to
        // extensionsUsed (the `if !arr.contains` guard) — otherwise repeated
        // convert/export cycles would grow the array unboundedly.
        let (_m, glb) = cube_glb();
        let once = stamp_awsm_format(glb).expect("stamp 1");
        let twice = stamp_awsm_format(once).expect("stamp 2 (idempotent)");
        let g = gltf::Gltf::from_slice(&twice).expect("parse");
        assert!(is_canonical(&g));
        assert_eq!(awsm_format_version(&g), Some(AWSM_FORMAT_VERSION));
        let count = g.extensions_used().filter(|e| *e == AWSM_FORMAT).count();
        assert_eq!(
            count, 1,
            "re-stamping duplicated AWSM_format in extensionsUsed"
        );
    }
}
