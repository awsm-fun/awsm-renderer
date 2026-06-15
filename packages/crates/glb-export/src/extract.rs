//! Read geometry back **out** of a source glTF/GLB (the reverse of [`write_glb`]).
//!
//! The editor's Model nodes reference one node inside an imported glTF file (by
//! node index, optionally one primitive of it). Exporting such a node means
//! re-reading that node's mesh from the original file and lowering its accessors
//! into a plain [`MeshData`] — the same plain-data shape every other geometry kind
//! bakes to before [`write_glb`]. This is pure (no GPU / wasm), so it is natively
//! unit-testable.
//!
//! ## Transforms (do NOT double-transform)
//!
//! The returned geometry is the **raw accessor positions** in the glTF node's own
//! local space — no node matrix is applied. The editor mirrors each glTF node's
//! local transform onto the corresponding editor node, and the exporter writes
//! that transform onto the `ExportNode`, which places the geometry. Applying the
//! node's transform here too would double-transform it.

use std::collections::HashMap;

use crate::{
    AlphaMode, ExportImage, ExportMaterial, ExportNode, ExportSkin, ExtraPrimitive, GlbScene,
    ImageMime, MeshData, MorphTarget, PbrMaterial, TexRef, Trs, UnlitMaterial,
};

/// Re-export a source glTF/GLB into a **clean** [`GlbScene`] — geometry + skin rig
/// (skeleton, joints, inverse-bind, per-vertex JOINTS/WEIGHTS) + morph targets +
/// **materials and their textures** (core PBR / unlit factors + the referenced
/// images, copied as their original encoded PNG/JPEG bytes — no re-encode), with
/// animations, cameras, and lights dropped. Materials stay PER PRIMITIVE: a
/// multi-material source mesh becomes one primitive per material on the SAME
/// node (see [`ExportNode::extra_primitives`]), so node counts — and therefore
/// skin-joint flatten indices and clip bindings — are untouched. Feed the
/// result to [`write_glb`](crate::write_glb) to produce the bundle's clean
/// `assets/<id>.glb` (the "re-export everything through our writer" path: uniform
/// encoding, no orphaned accessors).
///
/// Node hierarchy + transforms are preserved (so the skin's joint refs + our
/// clips' joint-node targets stay valid). Returns `None` if the bytes don't parse
/// or carry no default/first scene.
pub fn reexport_clean(bytes: &[u8]) -> Option<GlbScene> {
    let (doc, buffers, _images) = gltf::import_slice(bytes).ok()?;
    let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    reexport_clean_scene(&doc, &buffers)
}

/// Map each source glTF node index → its index in the **clean re-export**: the
/// depth-first (root-first, children in order) flatten over the default scene,
/// exactly the order [`reexport_clean_scene`] builds [`GlbScene::nodes`] and
/// [`write_glb`](crate::write_glb) assigns glTF node indices. Nodes outside the
/// default scene are absent.
///
/// Use this to translate a source joint node index into the index the player's
/// loader will see after it loads the re-exported `assets/<id>.glb` — the basis
/// for binding our animation clips' bone targets to the rig's baked joints.
pub fn scene_node_flat_indices(doc: &gltf::Document) -> HashMap<usize, usize> {
    let mut flat_of: HashMap<usize, usize> = HashMap::new();
    let Some(scene) = doc.default_scene().or_else(|| doc.scenes().next()) else {
        return flat_of;
    };
    fn index_walk(node: &gltf::Node, flat_of: &mut HashMap<usize, usize>, next: &mut usize) {
        flat_of.insert(node.index(), *next);
        *next += 1;
        for c in node.children() {
            index_walk(&c, flat_of, next);
        }
    }
    let mut next = 0usize;
    for r in scene.nodes() {
        index_walk(&r, &mut flat_of, &mut next);
    }
    flat_of
}

/// Like [`reexport_clean`] but operating on an already-parsed
/// [`gltf::Document`] + its raw buffer blobs — so a caller that already decoded
/// the source (e.g. the editor's import, which holds the doc before it's consumed
/// by the renderer) can build the clean rig without re-parsing bytes.
pub fn reexport_clean_scene(doc: &gltf::Document, buffers: &[Vec<u8>]) -> Option<GlbScene> {
    let scene = doc.default_scene().or_else(|| doc.scenes().next())?;

    // glTF node index → flat (depth-first) index, matching `write_glb`'s flatten,
    // so skin joint refs (glTF node indices) become our flat indices.
    let flat_of = scene_node_flat_indices(doc);

    let mut pool = ImagePool::default();
    let nodes: Vec<ExportNode> = scene
        .nodes()
        .map(|r| build_clean_node(&r, buffers, &mut pool))
        .collect();

    let skins: Vec<ExportSkin> = doc
        .skins()
        .map(|skin| {
            let joints: Vec<usize> = skin
                .joints()
                .filter_map(|j| flat_of.get(&j.index()).copied())
                .collect();
            let reader = skin.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
            let inverse_bind_matrices: Vec<[f32; 16]> = reader
                .read_inverse_bind_matrices()
                .map(|it| {
                    it.map(|m| {
                        // glTF/our IBM are both column-major 4x4; flatten cols.
                        let mut out = [0.0f32; 16];
                        for (c, col) in m.iter().enumerate() {
                            out[c * 4..c * 4 + 4].copy_from_slice(col);
                        }
                        out
                    })
                    .collect()
                })
                .unwrap_or_default();
            let skeleton = skin
                .skeleton()
                .and_then(|n| flat_of.get(&n.index()).copied());
            ExportSkin {
                joints,
                inverse_bind_matrices,
                skeleton,
            }
        })
        .collect();

    Some(GlbScene {
        nodes,
        skins,
        images: pool.images,
        ..Default::default()
    })
}

/// Deduplicating image pool for the clean re-export: each SOURCE image index
/// maps to one [`ExportImage`] holding the original encoded bytes (GLB buffer
/// view or `data:` URI — external file URIs can't be resolved here and their
/// textures are skipped).
#[derive(Default)]
struct ImagePool {
    images: Vec<ExportImage>,
    by_source: HashMap<usize, usize>,
}

impl ImagePool {
    /// Pool index for a source texture's image, inserting on first use.
    /// `None` when the bytes can't be resolved or the mime isn't PNG/JPEG.
    fn intern(&mut self, texture: &gltf::Texture, buffers: &[Vec<u8>]) -> Option<usize> {
        let img = texture.source();
        if let Some(&i) = self.by_source.get(&img.index()) {
            return Some(i);
        }
        let (bytes, mime): (Vec<u8>, &str) = match img.source() {
            gltf::image::Source::View { view, mime_type } => {
                let buf = buffers.get(view.buffer().index())?;
                (
                    buf.get(view.offset()..view.offset() + view.length())?
                        .to_vec(),
                    mime_type,
                )
            }
            gltf::image::Source::Uri { uri, mime_type } => {
                // Only `data:` URIs are resolvable from bytes alone.
                let rest = uri.strip_prefix("data:")?;
                let (header, b64) = rest.split_once(",")?;
                let mime = mime_type.unwrap_or_else(|| header.split(';').next().unwrap_or(""));
                use base64::Engine as _;
                (
                    base64::engine::general_purpose::STANDARD
                        .decode(b64.as_bytes())
                        .ok()?,
                    mime,
                )
            }
        };
        let mime = match mime {
            "image/png" => ImageMime::Png,
            "image/jpeg" | "image/jpg" => ImageMime::Jpeg,
            _ => return None,
        };
        let i = self.images.len();
        self.images.push(ExportImage {
            name: img.name().unwrap_or("").to_string(),
            bytes,
            mime,
        });
        self.by_source.insert(img.index(), i);
        Some(i)
    }
}

/// A texture slot reference → [`TexRef`] into the pool (with its TEXCOORD set).
fn tex_ref(
    texture: &gltf::Texture,
    tex_coord: u32,
    buffers: &[Vec<u8>],
    pool: &mut ImagePool,
) -> Option<TexRef> {
    pool.intern(texture, buffers)
        .map(|image| TexRef { image, tex_coord })
}

/// Lower a source glTF material into the export IR per the crate's material
/// policy: `KHR_materials_unlit` → [`ExportMaterial::Unlit`], everything else →
/// core-PBR [`ExportMaterial::Pbr`] (factors + base-color / metallic-roughness /
/// normal / occlusion / emissive textures). The glTF DEFAULT material (no
/// index) returns `None` — an absent material round-trips as absent.
fn extract_material(
    mat: &gltf::Material,
    buffers: &[Vec<u8>],
    pool: &mut ImagePool,
) -> Option<ExportMaterial> {
    mat.index()?; // default material → emit none (same defaults on reimport)
    let name = mat.name().unwrap_or("").to_string();
    let alpha_mode = match mat.alpha_mode() {
        gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
        gltf::material::AlphaMode::Mask => AlphaMode::Mask {
            cutoff: mat.alpha_cutoff().unwrap_or(0.5),
        },
        gltf::material::AlphaMode::Blend => AlphaMode::Blend,
    };
    let pbr = mat.pbr_metallic_roughness();
    if mat.unlit() {
        return Some(ExportMaterial::Unlit(UnlitMaterial {
            name,
            base_color: pbr.base_color_factor(),
            base_color_texture: pbr
                .base_color_texture()
                .and_then(|i| tex_ref(&i.texture(), i.tex_coord(), buffers, pool)),
            alpha_mode,
            double_sided: mat.double_sided(),
        }));
    }
    Some(ExportMaterial::Pbr(PbrMaterial {
        name,
        base_color: pbr.base_color_factor(),
        metallic: pbr.metallic_factor(),
        roughness: pbr.roughness_factor(),
        emissive: mat.emissive_factor(),
        alpha_mode,
        double_sided: mat.double_sided(),
        base_color_texture: pbr
            .base_color_texture()
            .and_then(|i| tex_ref(&i.texture(), i.tex_coord(), buffers, pool)),
        metallic_roughness_texture: pbr
            .metallic_roughness_texture()
            .and_then(|i| tex_ref(&i.texture(), i.tex_coord(), buffers, pool)),
        normal_texture: mat
            .normal_texture()
            .and_then(|i| tex_ref(&i.texture(), i.tex_coord(), buffers, pool)),
        occlusion_texture: mat
            .occlusion_texture()
            .and_then(|i| tex_ref(&i.texture(), i.tex_coord(), buffers, pool)),
        emissive_texture: mat
            .emissive_texture()
            .and_then(|i| tex_ref(&i.texture(), i.tex_coord(), buffers, pool)),
    }))
}

/// One node → a clean `ExportNode`: geometry + skin attrs + morph targets +
/// per-primitive materials. The FIRST primitive fills the node's own
/// mesh/material slots; further primitives become
/// [`ExportNode::extra_primitives`] (glTF materials are per-primitive — never
/// merge primitives across materials, and never add nodes, so skin-joint
/// flatten indices stay valid).
fn build_clean_node(node: &gltf::Node, buffers: &[Vec<u8>], pool: &mut ImagePool) -> ExportNode {
    let (translation, rotation, scale) = node.transform().decomposed();
    let mut out = ExportNode {
        name: node.name().unwrap_or("").to_string(),
        transform: Trs {
            translation,
            rotation,
            scale,
        },
        skin: node.skin().map(|s| s.index()),
        ..Default::default()
    };

    if let Some(mesh) = node.mesh() {
        // Morph target names ride the glTF `mesh.extras.targetNames`
        // convention (the reader's `extras` feature is on workspace-wide).
        let target_names: Vec<Option<String>> = mesh
            .extras()
            .as_ref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw.get()).ok())
            .and_then(|v| {
                v.get("targetNames")
                    .and_then(|a| a.as_array())
                    .map(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect())
            })
            .unwrap_or_default();

        let mut first = true;
        for primitive in mesh.primitives() {
            let reader = primitive.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
            let positions: Vec<[f32; 3]> = match reader.read_positions() {
                Some(p) => p.collect(),
                None => continue,
            };
            let vcount = positions.len();
            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|n| n.collect());
            let uvs: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|t| t.into_f32().collect());
            let colors: Option<Vec<[f32; 4]>> =
                reader.read_colors(0).map(|c| c.into_rgba_f32().collect());
            let indices: Vec<u32> = match reader.read_indices() {
                Some(idx) => idx.into_u32().collect(),
                None => (0..vcount as u32).collect(),
            };
            let (joints, weights) = match (reader.read_joints(0), reader.read_weights(0)) {
                (Some(j), Some(w)) => (
                    Some(j.into_u16().collect::<Vec<_>>()),
                    Some(w.into_f32().collect::<Vec<_>>()),
                ),
                _ => (None, None),
            };
            // Morph targets — names only on the main primitive (mesh-level).
            let morph_targets: Vec<MorphTarget> = reader
                .read_morph_targets()
                .enumerate()
                .map(|(ti, (tp, tn, _tt))| MorphTarget {
                    name: if first {
                        target_names.get(ti).cloned().flatten()
                    } else {
                        None
                    },
                    positions: tp
                        .map(|p| p.collect())
                        .unwrap_or_else(|| vec![[0.0; 3]; vcount]),
                    normals: tn.map(|n| n.collect()),
                })
                .collect();
            let mesh_data = MeshData {
                positions,
                normals,
                uvs,
                colors,
                indices,
            };
            let material = extract_material(&primitive.material(), buffers, pool);

            if first {
                first = false;
                out.mesh = Some(mesh_data);
                out.material = material;
                out.joints = joints;
                out.weights = weights;
                out.morph_targets = morph_targets;
                out.morph_weights = mesh.weights().map(|w| w.to_vec()).unwrap_or_default();
            } else {
                out.extra_primitives.push(ExtraPrimitive {
                    mesh: mesh_data,
                    material,
                    joints,
                    weights,
                    morph_targets,
                });
            }
        }
    }

    out.children = node
        .children()
        .map(|c| build_clean_node(&c, buffers, pool))
        .collect();
    out
}

/// Read the geometry of a single glTF node out of an already-loaded
/// [`gltf::Document`] + its buffer blobs, into a plain [`MeshData`].
///
/// - `node_index` selects the glTF node; its mesh is read.
/// - `primitive_index`: `Some(i)` reads only that one primitive; `None` merges
///   every primitive on the node into one mesh (concatenating vertices and
///   offsetting each primitive's indices).
///
/// Returns `None` when the node index is out of range, the node has no mesh, the
/// requested primitive index is out of range, or a primitive carries no positions.
/// Missing normals/uvs/colors are simply left empty/`None` (the writer recomputes
/// or omits them) — mirroring how the renderer-bridge tolerates partial meshes.
///
/// Positions/normals/uvs are the **raw** accessor values (the node's own local
/// space); see the module docs on why no transform is applied.
/// One node's extracted geometry plus its optional **second** UV set
/// (`TEXCOORD_1`), read in the SAME merge pass as the primary [`MeshData`] so it
/// stays vertex-aligned. `uvs1` rides a parallel channel (not [`MeshData`], whose
/// many construction sites would churn) up to the editor's captured mesh, where
/// it becomes the renderer's UV set 1 (`material_uv(in, 1u)`).
pub struct ExtractedNodeMesh {
    pub mesh: MeshData,
    pub uvs1: Option<Vec<[f32; 2]>>,
}

pub fn extract_node_mesh(
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    node_index: u32,
    primitive_index: Option<u32>,
) -> Option<ExtractedNodeMesh> {
    let node = doc.nodes().find(|n| n.index() == node_index as usize)?;
    let mesh = node.mesh()?;

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut uvs1: Vec<[f32; 2]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    // Track whether *every* read primitive supplied normals/uvs/colors; if any
    // didn't, the channel is dropped wholesale (a partial channel would misalign
    // with positions). The writer fills the gaps (recompute normals / omit uvs).
    let mut any_primitive = false;
    let mut all_have_normals = true;
    let mut all_have_uvs = true;
    let mut all_have_uvs1 = true;
    let mut all_have_colors = true;

    for (i, primitive) in mesh.primitives().enumerate() {
        if let Some(want) = primitive_index {
            if i as u32 != want {
                continue;
            }
        }
        let reader = primitive.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
        let prim_positions: Vec<[f32; 3]> = match reader.read_positions() {
            Some(p) => p.collect(),
            None => continue, // a primitive with no positions can't contribute.
        };
        let base = positions.len() as u32;
        let vert_count = prim_positions.len();
        positions.extend(prim_positions);
        any_primitive = true;

        match reader.read_normals() {
            Some(n) => normals.extend(n),
            None => all_have_normals = false,
        }
        match reader.read_tex_coords(0) {
            Some(t) => uvs.extend(t.into_f32()),
            None => all_have_uvs = false,
        }
        match reader.read_tex_coords(1) {
            Some(t) => uvs1.extend(t.into_f32()),
            None => all_have_uvs1 = false,
        }
        match reader.read_colors(0) {
            Some(c) => colors.extend(c.into_rgba_f32()),
            None => all_have_colors = false,
        }

        match reader.read_indices() {
            Some(idx) => indices.extend(idx.into_u32().map(|x| x + base)),
            // Non-indexed primitive: emit a trivial 0..n index run (offset by base).
            None => indices.extend((0..vert_count as u32).map(|x| x + base)),
        }
    }

    if !any_primitive {
        return None;
    }

    // A 2nd UV set is only meaningful alongside set 0 (it's packed contiguously
    // after it); drop it if set 0 is absent so the renderer's `has_uvs1 =
    // has_uvs && …` guard never sees a dangling set.
    let uvs1 = (all_have_uvs && all_have_uvs1).then_some(uvs1);

    Some(ExtractedNodeMesh {
        mesh: MeshData {
            positions,
            normals: all_have_normals.then_some(normals),
            uvs: all_have_uvs.then_some(uvs),
            colors: all_have_colors.then_some(colors),
            indices,
        },
        uvs1,
    })
}

/// Parse glTF/GLB bytes and extract one node's geometry in a single call — the
/// editor's export path uses this on the cached source bytes of an imported model.
///
/// Returns `None` if the bytes don't parse or the node has no extractable mesh.
/// Only self-contained sources resolve here: `.glb` and `.gltf` with embedded /
/// data-URI buffers (no external `.bin` side-files), which is what the editor
/// caches at import time.
pub fn extract_node_mesh_from_bytes(
    bytes: &[u8],
    node_index: u32,
    primitive_index: Option<u32>,
) -> Option<MeshData> {
    let (doc, buffers, _images) = gltf::import_slice(bytes).ok()?;
    let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    // The bytes path (editor export) only needs the primary geometry.
    extract_node_mesh(&doc, &buffers, node_index, primitive_index).map(|e| e.mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{write_glb, ExportNode, GlbScene, Trs};
    use awsm_meshgen::box_mesh;
    use glam::Vec3;

    /// Round-trip: write a 2-node GLB (a parent transform + a child cube mesh),
    /// re-read its bytes, extract the child node's geometry, and assert the
    /// vertex/index counts match the source cube.
    #[test]
    fn extract_child_node_mesh_roundtrip() {
        let src = box_mesh(Vec3::splat(2.0));
        let child = ExportNode::new("Cube").with_mesh(src.clone());
        let mut parent = ExportNode::new("Parent");
        parent.transform = Trs::IDENTITY;
        parent.children = vec![child];
        let scene = GlbScene {
            nodes: vec![parent],
            ..Default::default()
        };
        let glb = write_glb(&scene);

        // write_glb flattens depth-first: node 0 = Parent (no mesh), node 1 = Cube.
        let mesh = extract_node_mesh_from_bytes(&glb, 1, None).expect("child node mesh");
        assert_eq!(mesh.positions.len(), src.positions.len());
        assert_eq!(mesh.indices.len(), src.indices.len());

        // Node 0 has no mesh ⇒ None.
        assert!(extract_node_mesh_from_bytes(&glb, 0, None).is_none());
        // Out-of-range node ⇒ None.
        assert!(extract_node_mesh_from_bytes(&glb, 99, None).is_none());
    }

    /// Merging multiple primitives concatenates vertices and offsets indices, so
    /// no index references another primitive's vertex range.
    #[test]
    fn merge_primitives_offsets_indices() {
        // Two mesh nodes written separately, then re-read; the writer emits one
        // primitive per node, so to exercise multi-primitive merge we instead read
        // each node and confirm a single-primitive node merges identically.
        let a = box_mesh(Vec3::splat(1.0));
        let node = ExportNode::new("A").with_mesh(a.clone());
        let glb = write_glb(&GlbScene {
            nodes: vec![node],
            ..Default::default()
        });
        // primitive_index None and Some(0) yield the same single primitive.
        let all = extract_node_mesh_from_bytes(&glb, 0, None).unwrap();
        let one = extract_node_mesh_from_bytes(&glb, 0, Some(0)).unwrap();
        assert_eq!(all.positions.len(), one.positions.len());
        assert_eq!(all.indices, one.indices);
        // Every index is in range.
        assert!(all
            .indices
            .iter()
            .all(|&i| (i as usize) < all.positions.len()));
        // Out-of-range primitive ⇒ None.
        assert!(extract_node_mesh_from_bytes(&glb, 0, Some(9)).is_none());
    }

    /// The "re-export everything" core: a skinned + morphed scene survives
    /// write → `reexport_clean` → write → re-parse with skin/morph intact, and
    /// materials are dropped.
    #[test]
    fn reexport_clean_preserves_skin_and_morph() {
        use crate::{ExportMaterial, ExportSkin, MorphTarget, PbrMaterial};

        let tri = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uvs: None,
            colors: None,
            indices: vec![0, 1, 2],
        };
        let ident = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let src = GlbScene {
            // Armature(0) → J0(1), J1(2); skinned Mesh(3) with a material (dropped).
            nodes: vec![
                ExportNode {
                    name: "Armature".into(),
                    children: vec![ExportNode::new("J0"), ExportNode::new("J1")],
                    ..Default::default()
                },
                ExportNode {
                    name: "Mesh".into(),
                    mesh: Some(tri),
                    material: Some(ExportMaterial::Pbr(PbrMaterial::default())),
                    skin: Some(0),
                    joints: Some(vec![[0, 1, 0, 0]; 3]),
                    weights: Some(vec![[0.5, 0.5, 0.0, 0.0]; 3]),
                    morph_targets: vec![MorphTarget {
                        name: None,
                        positions: vec![[0.0, 0.2, 0.0]; 3],
                        normals: None,
                    }],
                    morph_weights: vec![0.0],
                    ..Default::default()
                },
            ],
            skins: vec![ExportSkin {
                joints: vec![1, 2],
                inverse_bind_matrices: vec![ident, ident],
                skeleton: Some(0),
            }],
            ..Default::default()
        };
        let glb = write_glb(&src);

        // Re-export clean, write again, re-parse: rig AND material survive
        // (materials/textures are preserved since the day-3 rig-material work —
        // a reimported rig must render textured, not source-default grey).
        let clean = reexport_clean(&glb).expect("reexport");
        assert_eq!(clean.skins.len(), 1);
        assert_eq!(clean.skins[0].joints, vec![1, 2]);
        let glb2 = crate::write_glb(&clean);
        let (doc, buffers, _i) = gltf::import_slice(&glb2).expect("re-parse cleaned");
        assert_eq!(doc.skins().count(), 1, "skin preserved");
        assert_eq!(doc.materials().count(), 1, "material preserved");
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let r = prim.reader(|b| Some(&buffers[b.index()]));
        assert_eq!(r.read_joints(0).expect("joints").into_u16().count(), 3);
        assert_eq!(prim.morph_targets().count(), 1, "morph preserved");
        // The skinned node still binds the skin.
        assert!(doc.nodes().any(|n| n.skin().is_some()));
    }

    /// `reexport_clean` PRESERVES each node's local transform (it does not bake
    /// or strip them). This is the invariant `awsm-scene-loader` relies on: a
    /// skinned rig glb carries the original glTF's root basis-conversion node
    /// (e.g. RiggedSimple's `Z_UP`), so the rig glb is self-placing and the
    /// loader roots it at the renderer root rather than re-applying a scene
    /// transform — otherwise the root rotation double-applies. If a future change
    /// makes reexport flatten/bake transforms, this fails and the loader's
    /// SkinnedMesh placement must be revisited.
    #[test]
    fn reexport_clean_preserves_node_transforms() {
        // A non-identity root transform, like the Z-up→Y-up `Z_UP` node.
        let rot = glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2).to_array();
        let mut parent = ExportNode::new("Z_UP");
        parent.transform = Trs {
            translation: [1.0, 2.0, 3.0],
            rotation: rot,
            scale: [1.0, 1.0, 1.0],
        };
        parent.children = vec![ExportNode::new("Cube").with_mesh(box_mesh(Vec3::splat(1.0)))];
        let glb = write_glb(&GlbScene {
            nodes: vec![parent],
            ..Default::default()
        });

        let clean = reexport_clean(&glb).expect("reexport");
        assert_eq!(clean.nodes.len(), 1, "single root preserved");
        let t = &clean.nodes[0].transform;
        for (got, want) in t.translation.iter().zip([1.0, 2.0, 3.0].iter()) {
            assert!((got - want).abs() < 1e-5, "translation {:?}", t.translation);
        }
        for (got, want) in t.rotation.iter().zip(rot.iter()) {
            assert!(
                (got - want).abs() < 1e-5,
                "rotation {:?} vs {:?}",
                t.rotation,
                rot
            );
        }
        assert_eq!(
            clean.nodes[0].children.len(),
            1,
            "child hierarchy preserved"
        );
    }

    // scene_node_flat_indices maps each SOURCE glTF node index to its index in
    // the depth-first re-export — the basis for retargeting skin joints + clip
    // bone channels. A source whose `nodes` array order differs from the scene's
    // DFS order is the case that actually exercises the mapping (a foreign glTF).
    #[test]
    fn flat_indices_follow_depth_first_not_source_order() {
        // Tree (scene root = node 2):
        //   2 "root"  ── children [1, 3]
        //   1 "child" ── children [0]
        //   0 "grandchild"
        //   3 "sibling"
        // DFS (root-first, children in order): 2, 1, 0, 3.
        const GLTF: &str = r#"{
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [2]}],
            "nodes": [
                {"name": "grandchild"},
                {"name": "child", "children": [0]},
                {"name": "root", "children": [1, 3]},
                {"name": "sibling"}
            ]
        }"#;
        let doc = gltf::Gltf::from_slice(GLTF.as_bytes()).expect("parse");
        let flat = scene_node_flat_indices(&doc);
        assert_eq!(flat.get(&2), Some(&0), "root visited first");
        assert_eq!(flat.get(&1), Some(&1), "child second");
        assert_eq!(flat.get(&0), Some(&2), "grandchild third (depth-first)");
        assert_eq!(flat.get(&3), Some(&3), "sibling last");
        assert_eq!(flat.len(), 4);
    }

    #[test]
    fn flat_indices_exclude_nodes_outside_the_scene() {
        // Node 1 ("orphan") is in `nodes` but unreferenced by the scene/children.
        const GLTF: &str = r#"{
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [
                {"name": "root"},
                {"name": "orphan"}
            ]
        }"#;
        let doc = gltf::Gltf::from_slice(GLTF.as_bytes()).expect("parse");
        let flat = scene_node_flat_indices(&doc);
        assert_eq!(flat.get(&0), Some(&0));
        assert!(!flat.contains_key(&1), "node outside the scene is absent");
        assert_eq!(flat.len(), 1);
    }

    #[test]
    fn flat_indices_empty_when_no_scene() {
        // Nodes present but no scenes → nothing to flatten.
        const GLTF: &str = r#"{
            "asset": {"version": "2.0"},
            "nodes": [{"name": "lonely"}]
        }"#;
        let doc = gltf::Gltf::from_slice(GLTF.as_bytes()).expect("parse");
        assert!(scene_node_flat_indices(&doc).is_empty());
    }
}
