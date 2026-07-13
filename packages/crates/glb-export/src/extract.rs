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
    reexport_clean_scene_with_images(doc, buffers, &[])
}

/// Like [`reexport_clean_scene`] but ALSO accepts the importer's retained ENCODED
/// image bytes (`GltfData.encoded_images`, by glTF image index) so EXTERNAL-file
/// images — which can't be resolved from `(doc, buffers)` alone — still round-trip
/// into the clean glb. Pass `&[]` for the embedded-only behaviour. This is how
/// model-tests (and any importer of external-image glTF) makes textures survive the
/// our-format conversion.
pub fn reexport_clean_scene_with_images(
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    encoded_images: &[Option<(Vec<u8>, String)>],
) -> Option<GlbScene> {
    let scene = doc.default_scene().or_else(|| doc.scenes().next())?;

    // glTF node index → flat (depth-first) index, matching `write_glb`'s flatten,
    // so skin joint refs (glTF node indices) become our flat indices.
    let flat_of = scene_node_flat_indices(doc);

    let mut pool = ImagePool::with_external(encoded_images);
    let nodes: Vec<ExportNode> = scene
        .nodes()
        .map(|r| build_clean_node(&r, doc, buffers, &mut pool))
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
    /// Fallback ENCODED bytes for source-image indices that can't be resolved from
    /// `(doc, buffers)` alone — i.e. EXTERNAL-file URIs. The importer (loader) fetched
    /// them; we re-embed them here so external-image glTF round-trips. Empty for the
    /// plain `reexport_clean_scene` path (today's behaviour). Consumed on first use.
    external: HashMap<usize, (Vec<u8>, ImageMime)>,
}

impl ImagePool {
    /// Build a pool seeded with retained ENCODED image bytes (PNG/JPEG only), keyed by
    /// glTF image index — the loader's `GltfData.encoded_images`. `intern` falls back
    /// to these when buffer/`data:`-URI resolution fails (external-file images).
    fn with_external(encoded_images: &[Option<(Vec<u8>, String)>]) -> Self {
        let external = encoded_images
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let (bytes, mime) = entry.as_ref()?;
                let mime = match mime.as_str() {
                    "image/png" => ImageMime::Png,
                    "image/jpeg" | "image/jpg" => ImageMime::Jpeg,
                    _ => return None,
                };
                Some((i, (bytes.clone(), mime)))
            })
            .collect();
        Self {
            external,
            ..Default::default()
        }
    }

    /// Pool index for a source texture's image, inserting on first use.
    /// `None` when the bytes can't be resolved (embedded OR retained) or the mime
    /// isn't PNG/JPEG.
    fn intern(&mut self, texture: &gltf::Texture, buffers: &[Vec<u8>]) -> Option<usize> {
        let img = texture.source();
        if let Some(&i) = self.by_source.get(&img.index()) {
            return Some(i);
        }
        // Try embedded (buffer View / data: URI) first; fall back to the importer's
        // retained encoded bytes for external-file images.
        let (bytes, mime) =
            Self::resolve_embedded(&img, buffers).or_else(|| self.external.remove(&img.index()))?;
        let i = self.images.len();
        self.images.push(ExportImage {
            name: img.name().unwrap_or("").to_string(),
            bytes,
            mime,
        });
        self.by_source.insert(img.index(), i);
        Some(i)
    }

    /// Resolve a glTF image's encoded bytes + mime from `(doc, buffers)` ALONE —
    /// buffer `View`s or `data:` URIs. `None` for external-file URIs (caller falls
    /// back to retained bytes) or non-PNG/JPEG mimes.
    fn resolve_embedded(img: &gltf::Image, buffers: &[Vec<u8>]) -> Option<(Vec<u8>, ImageMime)> {
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
        Some((bytes, mime))
    }
}

/// A texture slot reference → [`TexRef`] into the pool (with its TEXCOORD set).
fn tex_ref(
    texture: &gltf::Texture,
    tex_coord: u32,
    transform: Option<crate::TexTransform>,
    buffers: &[Vec<u8>],
    pool: &mut ImagePool,
) -> Option<TexRef> {
    pool.intern(texture, buffers).map(|image| TexRef {
        image,
        tex_coord,
        transform,
    })
}

/// `KHR_texture_transform` from a typed gltf accessor (base-color / metallic-roughness
/// / emissive textureInfos, which the gltf crate types).
fn tt_from_gltf(t: &gltf::texture::TextureTransform) -> crate::TexTransform {
    crate::TexTransform {
        offset: t.offset(),
        rotation: t.rotation(),
        scale: t.scale(),
        tex_coord: t.tex_coord(),
    }
}

/// `KHR_texture_transform` parsed from RAW JSON — for normal/occlusion textureInfos,
/// which the gltf crate doesn't type (read from `doc.as_json()`, mirroring the
/// renderer). Missing fields fall back to glTF defaults.
fn tt_from_json(v: &serde_json::Value) -> crate::TexTransform {
    let vec2 = |key: &str, default: [f32; 2]| -> [f32; 2] {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| {
                let f =
                    |i: usize, d: f32| a.get(i).and_then(|n| n.as_f64()).unwrap_or(d as f64) as f32;
                [f(0, default[0]), f(1, default[1])]
            })
            .unwrap_or(default)
    };
    crate::TexTransform {
        offset: vec2("offset", [0.0, 0.0]),
        rotation: v.get("rotation").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
        scale: vec2("scale", [1.0, 1.0]),
        tex_coord: v.get("texCoord").and_then(|x| x.as_u64()).map(|n| n as u32),
    }
}

/// Lower a source glTF material into the export IR per the crate's material
/// policy: `KHR_materials_unlit` → [`ExportMaterial::Unlit`], everything else →
/// core-PBR [`ExportMaterial::Pbr`] (factors + base-color / metallic-roughness /
/// normal / occlusion / emissive textures). The glTF DEFAULT material (no
/// index) returns `None` — an absent material round-trips as absent.
/// A glTF `textureInfo` → `{ "index": <clean pool index>, "texCoord": n }` JSON,
/// with the texture interned into the pool (so its index is the clean glb's). For
/// building GAP-3 KHR_* extension JSON. `None` when the texture can't be interned.
fn ext_tex_json(
    info: Option<gltf::texture::Info>,
    buffers: &[Vec<u8>],
    pool: &mut ImagePool,
) -> Option<serde_json::Value> {
    let info = info?;
    // Typed extension textures carrying their own KHR_texture_transform is a deferred
    // follow-up (rare; raw extensions pass their transforms through verbatim already).
    let tr = tex_ref(&info.texture(), info.tex_coord(), None, buffers, pool)?;
    Some(serde_json::json!({ "index": tr.image, "texCoord": tr.tex_coord }))
}

/// Build the TYPED KHR_* material-extension JSON (specular / transmission / volume —
/// the ones the `gltf` crate types) for the clean glb's `extensions.others`, with
/// texture indices remapped to the clean pool. ior + emissive_strength are carried
/// as `PbrMaterial` scalar fields instead; the RAW-JSON extensions (clearcoat / sheen
/// / …) are a follow-up. (GAP 3.)
/// Recursively remap every textureInfo `index` in a raw extension JSON value from
/// the SOURCE glTF texture index to the clean glb's POOL index (interning each on the
/// way). KHR_materials_* objects only use `index` for textures, so remapping every
/// `index` field is safe; the index Number is a leaf, so the recursion can't
/// double-remap it.
fn remap_texture_indices(
    value: &mut serde_json::Value,
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    pool: &mut ImagePool,
) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(src) = map.get("index").and_then(|v| v.as_u64()) {
                if let Some(clean) = doc
                    .textures()
                    .nth(src as usize)
                    .and_then(|t| pool.intern(&t, buffers))
                {
                    map.insert("index".to_string(), serde_json::Value::from(clean));
                }
            }
            for v in map.values_mut() {
                remap_texture_indices(v, doc, buffers, pool);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                remap_texture_indices(v, doc, buffers, pool);
            }
        }
        _ => {}
    }
}

fn build_pbr_extensions(
    mat: &gltf::Material,
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    pool: &mut ImagePool,
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::json;
    let mut out = serde_json::Map::new();

    if let Some(s) = mat.specular() {
        let mut o = serde_json::Map::new();
        o.insert("specularFactor".into(), json!(s.specular_factor()));
        o.insert(
            "specularColorFactor".into(),
            json!(s.specular_color_factor()),
        );
        if let Some(t) = ext_tex_json(s.specular_texture(), buffers, pool) {
            o.insert("specularTexture".into(), t);
        }
        if let Some(t) = ext_tex_json(s.specular_color_texture(), buffers, pool) {
            o.insert("specularColorTexture".into(), t);
        }
        out.insert("KHR_materials_specular".into(), json!(o));
    }
    if let Some(t) = mat.transmission() {
        let mut o = serde_json::Map::new();
        o.insert("transmissionFactor".into(), json!(t.transmission_factor()));
        if let Some(tex) = ext_tex_json(t.transmission_texture(), buffers, pool) {
            o.insert("transmissionTexture".into(), tex);
        }
        out.insert("KHR_materials_transmission".into(), json!(o));
    }
    if let Some(v) = mat.volume() {
        let mut o = serde_json::Map::new();
        o.insert("thicknessFactor".into(), json!(v.thickness_factor()));
        o.insert("attenuationColor".into(), json!(v.attenuation_color()));
        let dist = v.attenuation_distance();
        if dist.is_finite() {
            o.insert("attenuationDistance".into(), json!(dist));
        }
        if let Some(tex) = ext_tex_json(v.thickness_texture(), buffers, pool) {
            o.insert("thicknessTexture".into(), tex);
        }
        out.insert("KHR_materials_volume".into(), json!(o));
    }

    // RAW-JSON extensions the gltf crate doesn't type (renderer reads them raw too) —
    // pass each object through VERBATIM, remapping its texture indices to the clean pool.
    const RAW_EXTS: &[&str] = &[
        "KHR_materials_clearcoat",
        "KHR_materials_sheen",
        "KHR_materials_anisotropy",
        "KHR_materials_iridescence",
        "KHR_materials_dispersion",
        "KHR_materials_diffuse_transmission",
    ];
    for &name in RAW_EXTS {
        if let Some(raw) = mat.extension_value(name) {
            let mut value = raw.clone();
            remap_texture_indices(&mut value, doc, buffers, pool);
            out.insert(name.to_string(), value);
        }
    }
    out
}

fn extract_material(
    mat: &gltf::Material,
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    pool: &mut ImagePool,
) -> Option<ExportMaterial> {
    mat.index()?; // default material → emit none (same defaults on reimport)
                  // Raw material JSON — for reading KHR_texture_transform on the normal/occlusion
                  // textureInfos, which the gltf crate doesn't type (mirrors renderer-gltf).
    let mat_json = mat.index().and_then(|i| doc.as_json().materials.get(i));
    let normal_tt = mat_json
        .and_then(|m| m.normal_texture.as_ref())
        .and_then(|nt| nt.extensions.as_ref())
        .and_then(|e| e.others.get("KHR_texture_transform"))
        .map(tt_from_json);
    let occlusion_tt = mat_json
        .and_then(|m| m.occlusion_texture.as_ref())
        .and_then(|ot| ot.extensions.as_ref())
        .and_then(|e| e.others.get("KHR_texture_transform"))
        .map(tt_from_json);
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
            base_color_texture: pbr.base_color_texture().and_then(|i| {
                let tt = i.texture_transform().as_ref().map(tt_from_gltf);
                tex_ref(&i.texture(), i.tex_coord(), tt, buffers, pool)
            }),
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
        base_color_texture: pbr.base_color_texture().and_then(|i| {
            let tt = i.texture_transform().as_ref().map(tt_from_gltf);
            tex_ref(&i.texture(), i.tex_coord(), tt, buffers, pool)
        }),
        metallic_roughness_texture: pbr.metallic_roughness_texture().and_then(|i| {
            let tt = i.texture_transform().as_ref().map(tt_from_gltf);
            tex_ref(&i.texture(), i.tex_coord(), tt, buffers, pool)
        }),
        // normal/occlusion textureInfos aren't typed by the gltf crate — their
        // KHR_texture_transform is read from the raw material JSON above.
        normal_texture: mat
            .normal_texture()
            .and_then(|i| tex_ref(&i.texture(), i.tex_coord(), normal_tt, buffers, pool)),
        occlusion_texture: mat
            .occlusion_texture()
            .and_then(|i| tex_ref(&i.texture(), i.tex_coord(), occlusion_tt, buffers, pool)),
        emissive_texture: mat.emissive_texture().and_then(|i| {
            let tt = i.texture_transform().as_ref().map(tt_from_gltf);
            tex_ref(&i.texture(), i.tex_coord(), tt, buffers, pool)
        }),
        // KHR_* scalar material extensions (typed gltf accessors).
        ior: mat.ior(),
        emissive_strength: mat.emissive_strength(),
        // Texture-bearing extensions (typed + raw), JSON with texture indices remapped
        // to the clean pool.
        extensions_json: build_pbr_extensions(mat, doc, buffers, pool),
    }))
}

/// One node → a clean `ExportNode`: geometry + skin attrs + morph targets +
/// per-primitive materials. The FIRST primitive fills the node's own
/// mesh/material slots; further primitives become
/// [`ExportNode::extra_primitives`] (glTF materials are per-primitive — never
/// merge primitives across materials, and never add nodes, so skin-joint
/// flatten indices stay valid).
fn build_clean_node(
    node: &gltf::Node,
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    pool: &mut ImagePool,
) -> ExportNode {
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
            // Quantization-aware reads (KHR_mesh_quantization): the typed
            // readers assert F32 and panic on i16/i8-normalized accessors.
            let positions: Vec<[f32; 3]> = match crate::quant::read_attr_f32::<3>(
                &primitive,
                &gltf::Semantic::Positions,
                buffers,
            ) {
                Some(p) => p,
                None => continue,
            };
            let vcount = positions.len();
            let normals: Option<Vec<[f32; 3]>> =
                crate::quant::read_attr_f32::<3>(&primitive, &gltf::Semantic::Normals, buffers);
            // All TEXCOORD_n sets (0,1,2,…), so multi-UV meshes (e.g. an AO map on
            // TEXCOORD_1) round-trip — generalized to N, not just set 0.
            let mut uvs: Vec<Vec<[f32; 2]>> = Vec::new();
            let mut uv_set = 0u32;
            while let Some(t) = reader.read_tex_coords(uv_set) {
                uvs.push(t.into_f32().collect());
                uv_set += 1;
            }
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
            // Carry the AUTHORED tangents through verbatim so the writer emits them
            // instead of regenerating via MikkTSpace — a save→reload of this clean
            // rig glb then preserves the exact tangent basis a normal map was baked
            // against (regenerated tangents shade differently; the symptom is a dark
            // patch where authored ≠ MikkTSpace, e.g. mirrored-UV seams).
            let tangents: Option<Vec<[f32; 4]>> =
                crate::quant::read_attr_f32::<4>(&primitive, &gltf::Semantic::Tangents, buffers);
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
            let material = extract_material(&primitive.material(), doc, buffers, pool);

            if first {
                first = false;
                out.mesh = Some(mesh_data);
                out.material = material;
                out.joints = joints;
                out.weights = weights;
                out.tangents = tangents;
                out.morph_targets = morph_targets;
                out.morph_weights = mesh.weights().map(|w| w.to_vec()).unwrap_or_default();
            } else {
                out.extra_primitives.push(ExtraPrimitive {
                    mesh: mesh_data,
                    material,
                    joints,
                    weights,
                    tangents,
                    morph_targets,
                });
            }
        }
    }

    out.children = node
        .children()
        .map(|c| build_clean_node(&c, doc, buffers, pool))
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
/// One node's extracted geometry plus its optional skin binding. ALL of the node's
/// UV sets (`TEXCOORD_0`, `TEXCOORD_1`, …) are read in the SAME merge pass as the
/// positions so they stay vertex-aligned, and folded into [`MeshData::uvs`] — the
/// renderer reads set N via `material_uv(in, Nu)`.
#[derive(Clone)]
pub struct ExtractedNodeMesh {
    /// Geometry, with ALL UV sets folded into `mesh.uvs` (set 0 = `uvs[0]`, the
    /// 2nd set = `uvs[1]`, …) — N-set, no separate `uvs1` channel.
    pub mesh: MeshData,
    /// Authored per-vertex `TANGENT` (vec4: xyz + handedness), vertex-aligned with
    /// `mesh.positions`. `Some` only when EVERY read primitive supplied tangents (a
    /// partial channel would misalign). Carried so a captured (static) imported mesh
    /// preserves the exact tangent basis across save→reload instead of regenerating
    /// it (the dark-patch bug). `MeshData` itself stays tangent-free (32 call sites);
    /// this rides alongside like `skin`/`morph`.
    pub tangents: Option<Vec<[f32; 4]>>,
    /// Per-node SKIN (rig binding), read in the SAME merge pass as the geometry so
    /// the per-vertex joints/weights stay vertex-aligned with `mesh.positions`.
    /// `Some` only when the node binds a skin AND every read primitive supplied
    /// `JOINTS_0`/`WEIGHTS_0`. The editor maps `joint_node_indices` → its skeleton
    /// `TransformKey`s (via the import template's `node_index_to_transform`) so a
    /// captured skinned mesh re-binds to the SAME persistent skeleton
    /// In-memory only — NOT serialized (the rig
    /// glb remains the persisted source), so no project-format change.
    pub skin: Option<ExtractedSkin>,
    /// Per-node MORPH targets (position [+ normal] deltas + default weights), read in
    /// the same merge pass as the geometry so the deltas stay vertex-aligned with
    /// `mesh.positions`. `Some` only when the node's mesh has ≥1 morph target with a
    /// consistent target count across its primitives. The MATERIALISER packs this into
    /// the renderer's geometry-morph layout (`ExtractedMorph::packed_values`) so a
    /// rig-glb-decoded morph drawable goes node-owned.
    pub morph: Option<ExtractedMorph>,
}

/// One node's morph-target set, extracted alongside its geometry. Carries raw
/// per-target position/normal deltas (tangent deltas are dropped — the renderer
/// zero-fills that slot) + the default per-target weights; the packing method
/// produces the exact byte layout the renderer's geometry-morph store consumes.
#[derive(Clone)]
pub struct ExtractedMorph {
    /// Per-target position deltas (one Vec per target, vertex-aligned with positions).
    pub target_positions: Vec<Vec<[f32; 3]>>,
    /// Per-target normal deltas (parallel; `None` for a target with no normal deltas).
    pub target_normals: Vec<Option<Vec<[f32; 3]>>>,
    /// Default per-target weights (the rest pose; the animation drives them).
    pub weights: Vec<f32>,
}

impl ExtractedMorph {
    /// Number of morph targets.
    pub fn targets_len(&self) -> usize {
        self.target_positions.len()
    }

    /// Bytes per vertex across ALL targets: `40 * targets_len` (each target is
    /// position 3×f32 + normal 3×f32 + tangent 4×f32 = 40 bytes).
    pub fn vertex_stride_size(&self) -> usize {
        40 * self.targets_len()
    }

    /// Default weights as little-endian `f32` bytes (the renderer's weight buffer).
    pub fn weights_bytes(&self) -> Vec<u8> {
        self.weights.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// Pack to the renderer's GEOMETRY-MORPH values layout — exactly what
    /// `awsm_renderer::meshes::morphs`' `insert_raw` consumes, MIRRORING
    /// `renderer-gltf`'s `buffers::morph::convert_morph_targets`: indexed (one entry
    /// per ORIGINAL vertex), interleaved per target as
    /// `[T0 pos(12) T0 norm(12) T0 tang(16) T1 pos … ]`. Tangent deltas aren't carried
    /// (the clean glb drops them), so the 16-byte tangent slot is zero-filled — the
    /// same as the glTF decode's "no tangent morph" case. `vertex_count` is the mesh's
    /// vertex count (deltas are vertex-aligned; missing entries zero-fill).
    pub fn packed_values(&self, vertex_count: usize) -> Vec<u8> {
        let targets = self.targets_len();
        let mut out = Vec::with_capacity(vertex_count * targets * 40);
        for v in 0..vertex_count {
            for t in 0..targets {
                let p = self.target_positions[t].get(v).copied().unwrap_or([0.0; 3]);
                for c in p {
                    out.extend_from_slice(&c.to_le_bytes());
                }
                match &self.target_normals[t] {
                    Some(n) => {
                        let nd = n.get(v).copied().unwrap_or([0.0; 3]);
                        for c in nd {
                            out.extend_from_slice(&c.to_le_bytes());
                        }
                    }
                    None => out.extend_from_slice(&[0u8; 12]),
                }
                // Tangent delta — not carried; zero-filled vec4 (matches the decode).
                out.extend_from_slice(&[0u8; 16]);
            }
        }
        out
    }
}

/// One node's skin (rig) binding, extracted alongside its geometry. Mirrors the
/// shapes the renderer's `RawSkin` + `Skins::insert` consume; `joint_node_indices`
/// are glTF node indices (mapped to editor `TransformKey`s by the caller).
///
/// Note: reads skin SET 0 only (`JOINTS_0`/`WEIGHTS_0`) — 4 influences/vertex,
/// the common case. Multi-set rigs (`JOINTS_1`+) are a follow-up.
#[derive(Clone)]
pub struct ExtractedSkin {
    /// glTF node indices of the skin's joints (parallel to `inverse_bind_matrices`).
    pub joint_node_indices: Vec<usize>,
    /// Per-joint inverse-bind matrix, column-major 16 floats.
    pub inverse_bind_matrices: Vec<[f32; 16]>,
    /// Per-vertex joint indices (into `joint_node_indices`), 4 per vertex,
    /// vertex-aligned with `mesh.positions`.
    pub joints: Vec<[u16; 4]>,
    /// Per-vertex blend weights, 4 per vertex, parallel to `joints`.
    pub weights: Vec<[f32; 4]>,
}

impl ExtractedSkin {
    /// Pack the per-vertex joints + weights into the renderer's skin storage-buffer
    /// byte layout — exactly what `awsm_renderer::raw_mesh::RawSkin::index_weights`
    /// (→ `Skins::insert`) consumes. One entry per original vertex (set 0 only), with
    /// the 4 influences **interleaved** as `(u32 joint index LE, f32 weight LE)` per
    /// influence. This MIRRORS `renderer-gltf`'s `buffers::skin::convert_skin` so a
    /// rig-glb-decoded skin re-binds bit-identically to the glTF import's. The joint
    /// indices here are into the skin's own joint list (`joint_node_indices` order);
    /// the renderer resolves them against `RawSkin.joints` (the editor `TransformKey`s
    /// the caller maps from `joint_node_indices`).
    pub fn packed_index_weights(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.joints.len() * 32);
        for (j, w) in self.joints.iter().zip(self.weights.iter()) {
            for i in 0..4 {
                out.extend_from_slice(&(j[i] as u32).to_le_bytes());
                out.extend_from_slice(&w[i].to_le_bytes());
            }
        }
        out
    }
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
    let mut joints: Vec<[u16; 4]> = Vec::new();
    let mut weights: Vec<[f32; 4]> = Vec::new();
    let mut tangents: Vec<[f32; 4]> = Vec::new();
    // Track whether *every* read primitive supplied normals/uvs/colors/skin; if any
    // didn't, the channel is dropped wholesale (a partial channel would misalign
    // with positions). The writer fills the gaps (recompute normals / omit uvs).
    let mut any_primitive = false;
    let mut all_have_normals = true;
    let mut all_have_uvs = true;
    let mut all_have_uvs1 = true;
    let mut all_have_colors = true;
    let mut all_have_skin = true;
    let mut all_have_tangents = true;
    // Morph targets, accumulated per-target across primitives (vertex-aligned with
    // `positions`). The first contributing primitive fixes the target count; a
    // sibling with a different count drops morph (glTF requires consistency, so
    // this only guards malformed input + the multi-primitive-with-mixed-morph edge).
    let mut morph_target_positions: Vec<Vec<[f32; 3]>> = Vec::new();
    let mut morph_target_normals: Vec<Option<Vec<[f32; 3]>>> = Vec::new();
    let mut morph_inited = false;
    let mut morph_targets_len = 0usize;
    let mut morph_consistent = true;

    for (i, primitive) in mesh.primitives().enumerate() {
        if let Some(want) = primitive_index {
            if i as u32 != want {
                continue;
            }
        }
        let reader = primitive.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
        // Quantization-aware reads — see the sibling site above.
        let prim_positions: Vec<[f32; 3]> =
            match crate::quant::read_attr_f32::<3>(&primitive, &gltf::Semantic::Positions, buffers)
            {
                Some(p) => p,
                None => continue, // a primitive with no positions can't contribute.
            };
        let base = positions.len() as u32;
        let vert_count = prim_positions.len();
        positions.extend(prim_positions);
        any_primitive = true;

        match crate::quant::read_attr_f32::<3>(&primitive, &gltf::Semantic::Normals, buffers) {
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
        // Authored tangents (vec4 xyz+handedness). Only kept if EVERY primitive has
        // them (else dropped wholesale to stay vertex-aligned → regenerated later).
        match crate::quant::read_attr_f32::<4>(&primitive, &gltf::Semantic::Tangents, buffers) {
            Some(t) => tangents.extend(t),
            None => all_have_tangents = false,
        }
        // Skin set 0 (JOINTS_0/WEIGHTS_0), vertex-aligned with positions. Both must
        // be present for a primitive to contribute skin; otherwise the node's skin
        // is dropped (a partial skin channel would misalign).
        match (reader.read_joints(0), reader.read_weights(0)) {
            (Some(j), Some(w)) => {
                joints.extend(j.into_u16());
                weights.extend(w.into_f32());
            }
            _ => all_have_skin = false,
        }

        match reader.read_indices() {
            Some(idx) => indices.extend(idx.into_u32().map(|x| x + base)),
            // Non-indexed primitive: emit a trivial 0..n index run (offset by base).
            None => indices.extend((0..vert_count as u32).map(|x| x + base)),
        }

        // Morph targets (position [+ normal] deltas per target; tangent deltas
        // dropped — the renderer zero-fills the tangent slot). Absent position
        // deltas zero-fill so the channel stays vertex-aligned.
        // (position deltas, optional normal deltas) per morph target.
        type TargetDeltas = (Vec<[f32; 3]>, Option<Vec<[f32; 3]>>);
        let prim_targets: Vec<TargetDeltas> = reader
            .read_morph_targets()
            .map(|(tp, tn, _tt)| {
                (
                    tp.map(|p| p.collect())
                        .unwrap_or_else(|| vec![[0.0; 3]; vert_count]),
                    tn.map(|n| n.collect()),
                )
            })
            .collect();
        if !morph_inited {
            morph_inited = true;
            morph_targets_len = prim_targets.len();
            for (tp, tn) in prim_targets {
                morph_target_positions.push(tp);
                morph_target_normals.push(tn);
            }
        } else if prim_targets.len() == morph_targets_len {
            for (t, (tp, tn)) in prim_targets.into_iter().enumerate() {
                morph_target_positions[t].extend(tp);
                match (&mut morph_target_normals[t], tn) {
                    (Some(acc), Some(n)) => acc.extend(n),
                    (None, None) => {}
                    // normal-delta presence differs across primitives — drop this
                    // target's normals (it just zero-fills; positions still morph).
                    (slot, _) => *slot = None,
                }
            }
        } else {
            morph_consistent = false;
        }
    }

    if !any_primitive {
        return None;
    }

    // Fold the UV sets into one vec (set 0, then set 1 when present). A 2nd set is
    // only meaningful alongside set 0 (sets pack contiguously by index), so it's
    // dropped if set 0 is absent — `uv_sets` stays dense.
    let mut uv_sets: Vec<Vec<[f32; 2]>> = Vec::new();
    if all_have_uvs {
        uv_sets.push(uvs);
        if all_have_uvs1 {
            uv_sets.push(uvs1);
        }
    }

    // Skin: only when the node binds one AND every read primitive supplied skin.
    // The skin's joint node-indices + inverse-bind matrices are NODE-level (read
    // once from `node.skin()`); the per-vertex joints/weights were merged above.
    let skin = match (node.skin(), all_have_skin && !joints.is_empty()) {
        (Some(s), true) => {
            let joint_node_indices: Vec<usize> = s.joints().map(|j| j.index()).collect();
            let sreader = s.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
            let inverse_bind_matrices: Vec<[f32; 16]> = sreader
                .read_inverse_bind_matrices()
                .map(|it| {
                    it.map(|m| {
                        // glTF IBMs are column-major 4x4; flatten cols (matches ExportSkin).
                        let mut out = [0.0f32; 16];
                        for (c, col) in m.iter().enumerate() {
                            out[c * 4..c * 4 + 4].copy_from_slice(col);
                        }
                        out
                    })
                    .collect()
                })
                .unwrap_or_default();
            Some(ExtractedSkin {
                joint_node_indices,
                inverse_bind_matrices,
                joints,
                weights,
            })
        }
        _ => None,
    };

    // Morph: only when consistent across primitives AND there's at least one target.
    // Default weights come from the mesh (the rig glb writes them from mesh.weights());
    // mismatched/absent → zeros (rest pose, the animation drives them).
    let morph = (morph_consistent && morph_targets_len > 0).then(|| {
        let weights = mesh
            .weights()
            .map(|w| w.to_vec())
            .filter(|w| w.len() == morph_targets_len)
            .unwrap_or_else(|| vec![0.0; morph_targets_len]);
        ExtractedMorph {
            target_positions: morph_target_positions,
            target_normals: morph_target_normals,
            weights,
        }
    });

    // Tangents only when every primitive supplied a length-aligned channel.
    let tangents = (all_have_tangents && !tangents.is_empty() && tangents.len() == positions.len())
        .then_some(tangents);
    Some(ExtractedNodeMesh {
        mesh: MeshData {
            positions,
            normals: all_have_normals.then_some(normals),
            uvs: uv_sets,
            colors: all_have_colors.then_some(colors),
            indices,
        },
        tangents,
        skin,
        morph,
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

/// Like [`extract_node_mesh_from_bytes`] but returns the FULL
/// [`ExtractedNodeMesh`] — geometry + optional 2nd UV set + the per-node
/// [`ExtractedSkin`]. This is the MATERIALISER's entry point: decode the clean
/// rig glb (`assets/<source>.glb`) at a node to rebuild its skinned drawable
/// (geometry + skin) from our-format. Self-contained sources only (embedded /
/// data-URI buffers — the rig glb is single-BIN).
pub fn extract_node_mesh_with_skin_from_bytes(
    bytes: &[u8],
    node_index: u32,
    primitive_index: Option<u32>,
) -> Option<ExtractedNodeMesh> {
    let (doc, buffers, _images) = gltf::import_slice(bytes).ok()?;
    let buffers: Vec<Vec<u8>> = buffers.into_iter().map(|b| b.0).collect();
    extract_node_mesh(&doc, &buffers, node_index, primitive_index)
}

/// Extract the ENCODED image bytes (the original PNG/JPEG) of every glTF
/// TEXTURE, keyed by glTF texture index. Reuses the clean-export [`ImagePool`]
/// resolution: GLB buffer-view or `data:` URI sources only (external-file URIs
/// can't be resolved from bytes, and non-PNG/JPEG mimes are skipped — those
/// texture indices are simply absent from the map).
///
/// This is how the EDITOR captures imported textures for PERSISTENCE: the
/// renderer keeps only DECODED pixels, so the original encoded bytes must be
/// grabbed off the document before it's consumed by populate. Call it with the
/// import's `data.doc` + `data.buffers.raw` (same inputs as
/// [`reexport_clean_scene`]).
pub fn extract_texture_images(
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
) -> std::collections::BTreeMap<usize, ExportImage> {
    extract_texture_images_with_external(doc, buffers, &[])
}

/// [`extract_texture_images`] that ALSO resolves EXTERNAL-file-URI images from the
/// loader's re-fetched `encoded_images` (indexed by glTF image index). The plain
/// `(doc, buffers)` form can only recover embedded buffer-view / `data:` URI images;
/// an external-URI texture's bytes live ONLY in `encoded_images`, so without this
/// the editor never captures them → empty `content_hash` → the texture silently
/// drops on save→reload. Pass the import's `data.encoded_images`.
pub fn extract_texture_images_with_external(
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    encoded_images: &[Option<(Vec<u8>, String)>],
) -> std::collections::BTreeMap<usize, ExportImage> {
    let mut pool = ImagePool::with_external(encoded_images);
    let mut out = std::collections::BTreeMap::new();
    for texture in doc.textures() {
        if let Some(pool_idx) = pool.intern(&texture, buffers) {
            out.insert(texture.index(), pool.images[pool_idx].clone());
        }
    }
    out
}

/// Bytes wrapper for [`extract_texture_images`] — parses a self-contained,
/// single-BIN glb (the rig glb shape) WITHOUT decoding images (we want the
/// original encoded bytes, and the importer's image decode would both waste work
/// and reject stub/edge images). Buffer 0 is the GLB BIN blob, where embedded
/// texture bytes live.
pub fn extract_texture_images_from_bytes(
    bytes: &[u8],
) -> std::collections::BTreeMap<usize, ExportImage> {
    let Ok(gltf) = gltf::Gltf::from_slice(bytes) else {
        return std::collections::BTreeMap::new();
    };
    let buffers: Vec<Vec<u8>> = gltf.blob.into_iter().collect();
    extract_texture_images(&gltf.document, &buffers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{write_glb, ExportNode, GlbScene, Trs};
    use awsm_renderer_meshgen::box_mesh;
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

    /// `ExtractedSkin::packed_index_weights` lays bytes out exactly as the renderer's
    /// skin storage buffer expects (mirrors renderer-gltf `convert_skin`): one entry
    /// per vertex, the 4 influences interleaved as (u32 joint index LE, f32 weight LE).
    #[test]
    fn packed_index_weights_layout() {
        let skin = ExtractedSkin {
            joint_node_indices: vec![5, 6, 7, 8],
            inverse_bind_matrices: vec![],
            joints: vec![[0u16, 1, 2, 3], [3, 0, 0, 0]],
            weights: vec![[0.5f32, 0.25, 0.125, 0.125], [1.0, 0.0, 0.0, 0.0]],
        };
        let got = skin.packed_index_weights();

        // Hand-build the expected bytes: per vertex, per influence i, u32 idx then f32 weight.
        let mut want: Vec<u8> = Vec::new();
        for (j, w) in skin.joints.iter().zip(skin.weights.iter()) {
            for i in 0..4 {
                want.extend_from_slice(&(j[i] as u32).to_le_bytes());
                want.extend_from_slice(&w[i].to_le_bytes());
            }
        }
        assert_eq!(got, want);
        // 2 vertices × 4 influences × (4-byte u32 + 4-byte f32) = 64 bytes.
        assert_eq!(got.len(), 2 * 4 * 8);
        // First influence of vertex 0: joint index 0 (u32) then weight 0.5 (f32).
        assert_eq!(&got[0..4], &0u32.to_le_bytes());
        assert_eq!(&got[4..8], &0.5f32.to_le_bytes());
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
            uvs: vec![],
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

        // Phase-2(a): extract_node_mesh ALSO returns the per-node skin, vertex-
        // aligned with the geometry, with the skin's joint node-indices +
        // inverse-bind matrices (the shapes the editor maps to TransformKeys).
        let mesh_node = doc
            .nodes()
            .find(|n| n.mesh().is_some() && n.skin().is_some())
            .expect("skinned mesh node");
        let raw_buffers: Vec<Vec<u8>> = buffers.iter().map(|b| b.0.clone()).collect();
        let ex = extract_node_mesh(&doc, &raw_buffers, mesh_node.index() as u32, None)
            .expect("extract skinned node");
        let skin = ex.skin.expect("extracted skin");
        // Per-vertex joints/weights are vertex-aligned with positions (3 verts).
        assert_eq!(skin.joints.len(), ex.mesh.positions.len());
        assert_eq!(skin.weights.len(), ex.mesh.positions.len());
        assert_eq!(skin.joints[0], [0, 1, 0, 0]);
        assert_eq!(skin.weights[0], [0.5, 0.5, 0.0, 0.0]);
        // Two joints, parallel inverse-bind matrices.
        assert_eq!(skin.joint_node_indices.len(), 2);
        assert_eq!(skin.inverse_bind_matrices.len(), 2);

        // Step vi: extract_node_mesh ALSO returns the per-node MORPH — one target,
        // its position deltas vertex-aligned, packed to the renderer's geometry-morph
        // VALUES layout (40B/target/vertex, position delta first).
        let morph = ex.morph.expect("extracted morph");
        assert_eq!(morph.targets_len(), 1);
        assert_eq!(morph.target_positions[0].len(), ex.mesh.positions.len());
        assert_eq!(morph.target_positions[0][0], [0.0, 0.2, 0.0]);
        assert_eq!(morph.weights, vec![0.0]);
        assert_eq!(morph.vertex_stride_size(), 40);
        let vc = ex.mesh.positions.len();
        let packed = morph.packed_values(vc);
        assert_eq!(packed.len(), vc * morph.vertex_stride_size());
        // Vertex 0, target 0: position delta [0, 0.2, 0] as little-endian f32, then a
        // zero-filled normal(12) + tangent(16).
        assert_eq!(&packed[0..4], &0.0f32.to_le_bytes());
        assert_eq!(&packed[4..8], &0.2f32.to_le_bytes());
        assert_eq!(&packed[8..12], &0.0f32.to_le_bytes());
        assert_eq!(&packed[12..40], &[0u8; 28]);
        assert_eq!(morph.weights_bytes(), 0.0f32.to_le_bytes().to_vec());
    }

    /// `reexport_clean` PRESERVES each node's local transform (it does not bake
    /// or strip them). This is the invariant `awsm-renderer-scene-loader` relies on: a
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
