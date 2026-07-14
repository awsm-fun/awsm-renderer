//! `KHR_mesh_quantization` + `EXT_meshopt_compression` encode — a post-pass
//! over a finished (uncompressed) GLB from [`crate::write_glb`].
//!
//! Quantization: POSITION → i16-normalized (stride 8), NORMAL → oct-filtered
//! i8 (stride 4), TANGENT → oct-filtered i8 vec4 (stride 4, w = handedness),
//! TEXCOORD in [0,1] → u16-normalized (stride 4). The dequantization
//! transform (uniform scale + translation, so normals never skew) folds into:
//! - a fresh WRAPPER child node for static meshes (the original node may be an
//!   animation target — its TRS can't be touched), or
//! - the skin's inverse bind matrices for skinned meshes (skinned vertices
//!   ignore node TRS; per-skin the transform is the UNION of its meshes'
//!   bounds since IBMs are shared across them).
//!
//! meshopt: every rewritten attribute stream and index stream gets its own
//! `EXT_meshopt_compression` bufferView (ATTRIBUTES / TRIANGLES) pointing at
//! the real BIN, with the parent view aimed at a `fallback: true` buffer —
//! exactly the shape the Phase-4 import decode pass consumes.
//!
//! Meshes with morph targets skip QUANTIZATION (deltas would need their own
//! treatment) but their streams still meshopt-encode. Non-[0,1] UVs stay f32
//! (a per-primitive `KHR_texture_transform` remap would collide with authored
//! transforms) — also still meshopt-encoded.
//!
//! The two halves are independent ([`CompressOptions`]): quantization without
//! meshopt emits the quantized accessors into PLAIN bufferViews
//! (`KHR_mesh_quantization` alone) — except normals/tangents, whose octahedral
//! packing is a meshopt decode filter, so the plain path quantizes them
//! per-component to i16-normalized instead. meshopt without quantization
//! encodes the raw f32 streams. The structural eligibility guards (morph
//! targets, multi-skin / mixed-use meshes, IBM-less skins, out-of-[0,1] UVs)
//! are correctness, not policy — they apply under every mode.

use std::collections::{HashMap, HashSet};

use awsm_renderer_codec_meshopt::meshopt::ffi;
use glam::{Mat4, Vec3};
use gltf::json::validation::{Checked, USize64};
use gltf::json::{self, accessor};

const EXT: &str = "EXT_meshopt_compression";
const QUANT: &str = "KHR_mesh_quantization";

/// Encode knobs for [`compress_glb_with`]. The two halves are independent —
/// any combination is a valid wire format (both off = passthrough).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CompressOptions {
    /// meshopt-encode every mesh stream (`EXT_meshopt_compression`).
    pub meshopt: bool,
    pub quantization: Quantization,
}

/// Quantization policy (`KHR_mesh_quantization`). Structural guards apply
/// even under `Always` — they are correctness, not policy.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Quantization {
    Off,
    Always,
    /// Quantize only when the position grid step (max half-extent / 32767)
    /// stays at or under `threshold_mm`.
    Smart {
        threshold_mm: f32,
    },
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            meshopt: true,
            quantization: Quantization::Smart { threshold_mm: 0.1 },
        }
    }
}

/// How a compressed logical stream is encoded on the wire.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Attributes,
    Triangles,
}

/// Per-accessor rewrite plan.
struct Stream {
    /// Logical bytes (what decode reconstructs), `stride × count`.
    logical: Vec<u8>,
    stride: usize,
    count: usize,
    mode: Mode,
    filter: Option<&'static str>,
    /// glTF forbids `byteStride` on index views; attributes require it.
    parent_stride: Option<usize>,
}

/// Strip embedded materials, textures, images, and samplers from a GLB —
/// for artifacts whose consumer applies its OWN materials (the player bundle
/// applies scene.toml materials to every rig primitive via
/// `GltfMaterialSource::Single`, so the rig's embedded content is dead
/// weight: its images were being fetched, transcoded, and pooled at load,
/// then never referenced). Orphaned image bufferViews are dropped by the
/// next [`compress_glb`] pass (it only carries views something references),
/// so call this BEFORE compressing.
pub fn strip_materials_and_images(glb: &[u8]) -> anyhow::Result<Vec<u8>> {
    let parsed = gltf::Gltf::from_slice(glb)?;
    let bin = parsed.blob.clone().unwrap_or_default();
    let mut root = parsed.document.into_json();

    for mesh in &mut root.meshes {
        for prim in &mut mesh.primitives {
            // Optional per spec — absent means the default material, which
            // the player replaces anyway.
            prim.material = None;
        }
    }
    root.materials.clear();
    root.textures.clear();
    root.images.clear();
    root.samplers.clear();
    // Nothing textured remains; drop the now-unused declarations (material
    // extensions died with `materials`).
    let stale = |e: &String| e == "KHR_texture_basisu" || e.starts_with("KHR_materials_");
    root.extensions_used.retain(|e| !stale(e));
    root.extensions_required.retain(|e| !stale(e));

    Ok(crate::write::glb_from_parts(
        serde_json::to_vec(&root)?,
        bin,
    ))
}

/// Compress a GLB produced by [`crate::write_glb`] with default options:
/// quantize eligible meshes (Smart threshold) and meshopt-encode every mesh
/// stream. Non-mesh bufferViews (embedded images) pass through untouched.
/// Returns a new GLB.
pub fn compress_glb(glb: &[u8]) -> anyhow::Result<Vec<u8>> {
    compress_glb_with(glb, &CompressOptions::default())
}

/// [`compress_glb`] with explicit [`CompressOptions`]. Both knobs off returns
/// the input unchanged.
pub fn compress_glb_with(glb: &[u8], options: &CompressOptions) -> anyhow::Result<Vec<u8>> {
    if !options.meshopt && options.quantization == Quantization::Off {
        return Ok(glb.to_vec());
    }
    let parsed = gltf::Gltf::from_slice(glb)?;
    let bin = parsed.blob.clone().unwrap_or_default();
    let mut root = parsed.document.into_json();

    // ── classify accessors ────────────────────────────────────────────────
    #[derive(Clone, Copy, PartialEq)]
    enum Role {
        Position,
        Normal,
        Tangent,
        TexCoord,
        Indices,
        OtherVertex, // colors / joints / weights — meshopt yes, quantize no
        Raw,         // IBMs, animation samplers, morph deltas, unknown
    }
    let acc_count = root.accessors.len();
    let mut roles = vec![Role::Raw; acc_count];
    // meshes eligible for QUANTIZATION (all-f32 positions, no morph targets)
    let quantize_requested = options.quantization != Quantization::Off;
    let mut quantize_mesh = vec![quantize_requested; root.meshes.len()];

    for (mesh_index, mesh) in root.meshes.iter().enumerate() {
        for prim in &mesh.primitives {
            if prim.targets.is_some() && !prim.targets.as_ref().unwrap().is_empty() {
                quantize_mesh[mesh_index] = false;
            }
            for (semantic, acc) in &prim.attributes {
                let role = match semantic {
                    Checked::Valid(json::mesh::Semantic::Positions) => Role::Position,
                    Checked::Valid(json::mesh::Semantic::Normals) => Role::Normal,
                    Checked::Valid(json::mesh::Semantic::Tangents) => Role::Tangent,
                    Checked::Valid(json::mesh::Semantic::TexCoords(_)) => Role::TexCoord,
                    _ => Role::OtherVertex,
                };
                roles[acc.value()] = role;
            }
            if let Some(indices) = prim.indices {
                roles[indices.value()] = Role::Indices;
            }
            if let Some(targets) = &prim.targets {
                for target in targets {
                    for acc in [target.positions, target.normals, target.tangents]
                        .into_iter()
                        .flatten()
                    {
                        roles[acc.value()] = Role::Raw;
                    }
                }
            }
        }
    }

    // ── group meshes by dequant carrier ───────────────────────────────────
    // users: mesh → (static nodes, skins)
    let mut static_users: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut skin_users: HashMap<usize, HashSet<usize>> = HashMap::new();
    for (node_index, node) in root.nodes.iter().enumerate() {
        if let Some(mesh) = node.mesh {
            match node.skin {
                Some(skin) => {
                    skin_users
                        .entry(mesh.value())
                        .or_default()
                        .insert(skin.value());
                }
                None => static_users
                    .entry(mesh.value())
                    .or_default()
                    .push(node_index),
            }
        }
    }
    // A mesh used by >1 skin, or both skinned and static, can't carry a single
    // dequant transform through shared IBMs — skip quantizing it. Same when
    // the skin has NO inverseBindMatrices accessor (all-identity IBMs): there
    // is nowhere to fold the dequant, and quantizing anyway would corrupt the
    // geometry.
    for (mesh, skins) in &skin_users {
        let ibm_less_skin = skins
            .iter()
            .any(|&s| root.skins[s].inverse_bind_matrices.is_none());
        if skins.len() > 1 || static_users.contains_key(mesh) || ibm_less_skin {
            quantize_mesh[*mesh] = false;
        }
    }

    let read_accessor = |acc: &json::Accessor| -> anyhow::Result<&[u8]> {
        let view_index = acc
            .buffer_view
            .ok_or_else(|| anyhow::anyhow!("accessor without bufferView"))?
            .value();
        let view = &root.buffer_views[view_index];
        let start = view.byte_offset.unwrap_or_default().0 as usize
            + acc.byte_offset.unwrap_or_default().0 as usize;
        let size =
            component_size(acc.component_type.unwrap().0) * type_multiplicity(acc.type_.unwrap());
        let len = size * acc.count.0 as usize;
        bin.get(start..start + len)
            .ok_or_else(|| anyhow::anyhow!("accessor range out of BIN bounds"))
    };

    // Per-mesh position bounds → per-mesh dequant (center, uniform half-extent).
    let mut mesh_bounds: HashMap<usize, (Vec3, Vec3)> = HashMap::new();
    for (mesh_index, mesh) in root.meshes.iter().enumerate() {
        if !quantize_mesh[mesh_index] {
            continue;
        }
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let mut ok = true;
        for prim in &mesh.primitives {
            let Some(pos) = prim
                .attributes
                .get(&Checked::Valid(json::mesh::Semantic::Positions))
            else {
                ok = false;
                break;
            };
            let acc = &root.accessors[pos.value()];
            if acc.component_type.unwrap().0 != accessor::ComponentType::F32 {
                ok = false;
                break;
            }
            for chunk in read_accessor(acc)?.chunks_exact(12) {
                let v = Vec3::new(
                    f32::from_le_bytes(chunk[0..4].try_into().unwrap()),
                    f32::from_le_bytes(chunk[4..8].try_into().unwrap()),
                    f32::from_le_bytes(chunk[8..12].try_into().unwrap()),
                );
                min = min.min(v);
                max = max.max(v);
            }
        }
        if ok && min.x.is_finite() {
            mesh_bounds.insert(mesh_index, (min, max));
        } else {
            quantize_mesh[mesh_index] = false;
        }
    }

    // Per-skin transform = union of its meshes' bounds; per-mesh transform for
    // static. transform = (center, uniform half-extent s).
    let mut mesh_transform: HashMap<usize, (Vec3, f32)> = HashMap::new();
    let mut skin_transform: HashMap<usize, (Vec3, f32)> = HashMap::new();
    {
        let mut skin_bounds: HashMap<usize, (Vec3, Vec3)> = HashMap::new();
        for (mesh, skins) in &skin_users {
            if !quantize_mesh[*mesh] {
                continue;
            }
            let Some(&(bmin, bmax)) = mesh_bounds.get(mesh) else {
                continue;
            };
            let skin = *skins.iter().next().unwrap();
            let entry = skin_bounds
                .entry(skin)
                .or_insert((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)));
            entry.0 = entry.0.min(bmin);
            entry.1 = entry.1.max(bmax);
        }
        for (skin, (bmin, bmax)) in skin_bounds {
            skin_transform.insert(skin, center_extent(bmin, bmax));
        }
        for (mesh, &(bmin, bmax)) in &mesh_bounds {
            if let Some(skins) = skin_users.get(mesh) {
                let skin = *skins.iter().next().unwrap();
                mesh_transform.insert(*mesh, skin_transform[&skin]);
            } else {
                mesh_transform.insert(*mesh, center_extent(bmin, bmax));
            }
        }
    }

    // Smart mode: demote meshes whose position grid step (half-extent /
    // 32767) exceeds the threshold. Skinned meshes carry their skin-union
    // extent, so a too-large skin demotes all of its meshes together.
    if let Quantization::Smart { threshold_mm } = options.quantization {
        let max_extent = threshold_mm * 1e-3 * 32767.0;
        mesh_transform.retain(|&mesh, &mut (_, s)| {
            let keep = s <= max_extent;
            if !keep {
                quantize_mesh[mesh] = false;
            }
            keep
        });
        skin_transform.retain(|_, &mut (_, s)| s <= max_extent);
    }

    // accessor → quantize transform (positions only need it; per accessor via
    // owning mesh).
    let mut acc_transform: HashMap<usize, (Vec3, f32)> = HashMap::new();
    let mut acc_quantize = vec![false; acc_count];
    for (mesh_index, mesh) in root.meshes.iter().enumerate() {
        if !quantize_mesh[mesh_index] {
            continue;
        }
        let Some(&t) = mesh_transform.get(&mesh_index) else {
            continue;
        };
        for prim in &mesh.primitives {
            for (semantic, acc) in &prim.attributes {
                match semantic {
                    Checked::Valid(json::mesh::Semantic::Positions) => {
                        acc_transform.insert(acc.value(), t);
                        acc_quantize[acc.value()] = true;
                    }
                    Checked::Valid(json::mesh::Semantic::Normals)
                    | Checked::Valid(json::mesh::Semantic::Tangents)
                    | Checked::Valid(json::mesh::Semantic::TexCoords(_)) => {
                        acc_quantize[acc.value()] = true;
                    }
                    _ => {}
                }
            }
        }
    }

    // ── rewrite skin IBMs (IBM' = IBM * dequant) in place ─────────────────
    let mut ibm_patched: Vec<(usize, Vec<u8>)> = Vec::new();
    for (skin_index, skin) in root.skins.iter().enumerate() {
        let Some(&(center, s)) = skin_transform.get(&skin_index) else {
            continue;
        };
        let Some(ibm_acc_index) = skin.inverse_bind_matrices.map(|i| i.value()) else {
            continue;
        };
        let dequant = Mat4::from_translation(center) * Mat4::from_scale(Vec3::splat(s));
        let data = read_accessor(&root.accessors[ibm_acc_index])?;
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks_exact(64) {
            let mut cols = [0f32; 16];
            for (i, c) in chunk.chunks_exact(4).enumerate() {
                cols[i] = f32::from_le_bytes(c.try_into().unwrap());
            }
            let ibm = Mat4::from_cols_array(&cols) * dequant;
            for v in ibm.to_cols_array() {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        ibm_patched.push((ibm_acc_index, out));
    }
    let ibm_patched: HashMap<usize, Vec<u8>> = ibm_patched.into_iter().collect();

    // ── build per-accessor streams (quantized or raw) ─────────────────────
    let mut streams: Vec<Option<Stream>> = Vec::with_capacity(acc_count);
    let mut any_meshopt = false;
    let mut any_quantized = false;
    for index in 0..acc_count {
        let acc = &root.accessors[index];
        if acc.buffer_view.is_none() || acc.sparse.is_some() {
            streams.push(None);
            continue;
        }
        let data: Vec<u8> = match ibm_patched.get(&index) {
            Some(patched) => patched.clone(),
            None => read_accessor(acc)?.to_vec(),
        };
        let count = acc.count.0 as usize;
        let comp = acc.component_type.unwrap().0;
        let multiplicity = type_multiplicity(acc.type_.unwrap());
        let elem = component_size(comp) * multiplicity;

        let stream = match roles[index] {
            Role::Indices
                if options.meshopt
                    && count % 3 == 0
                    && matches!(
                        comp,
                        accessor::ComponentType::U16 | accessor::ComponentType::U32
                    ) =>
            {
                Some(Stream {
                    logical: data,
                    stride: component_size(comp),
                    count,
                    mode: Mode::Triangles,
                    filter: None,
                    parent_stride: None,
                })
            }
            Role::Position if acc_quantize[index] && comp == accessor::ComponentType::F32 => {
                let (center, s) = acc_transform[&index];
                let (logical, qmin, qmax) = quantize_positions(&data, count, center, s);
                let acc = &mut root.accessors[index];
                acc.component_type =
                    Checked::Valid(accessor::GenericComponentType(accessor::ComponentType::I16));
                acc.normalized = true;
                acc.min = Some(serde_json::json!(qmin));
                acc.max = Some(serde_json::json!(qmax));
                any_quantized = true;
                Some(Stream {
                    logical,
                    stride: 8,
                    count,
                    mode: Mode::Attributes,
                    filter: None,
                    parent_stride: Some(8),
                })
            }
            // Octahedral packing is a meshopt DECODE filter — without meshopt,
            // normals/tangents quantize per-component to i16-normalized
            // (KHR_mesh_quantization-legal, comparable precision).
            Role::Normal if acc_quantize[index] && comp == accessor::ComponentType::F32 => {
                let acc_comp = if options.meshopt {
                    accessor::ComponentType::I8
                } else {
                    accessor::ComponentType::I16
                };
                let logical = if options.meshopt {
                    oct_encode(&data, count, 3)
                } else {
                    quantize_snorm16(&data, count, 3)
                };
                let stride = if options.meshopt { 4 } else { 8 };
                let acc = &mut root.accessors[index];
                acc.component_type = Checked::Valid(accessor::GenericComponentType(acc_comp));
                acc.normalized = true;
                any_quantized = true;
                Some(Stream {
                    logical,
                    stride,
                    count,
                    mode: Mode::Attributes,
                    filter: options.meshopt.then_some("OCTAHEDRAL"),
                    parent_stride: Some(stride),
                })
            }
            Role::Tangent if acc_quantize[index] && comp == accessor::ComponentType::F32 => {
                let acc_comp = if options.meshopt {
                    accessor::ComponentType::I8
                } else {
                    accessor::ComponentType::I16
                };
                let logical = if options.meshopt {
                    oct_encode(&data, count, 4)
                } else {
                    quantize_snorm16(&data, count, 4)
                };
                let stride = if options.meshopt { 4 } else { 8 };
                let acc = &mut root.accessors[index];
                acc.component_type = Checked::Valid(accessor::GenericComponentType(acc_comp));
                acc.normalized = true;
                any_quantized = true;
                Some(Stream {
                    logical,
                    stride,
                    count,
                    mode: Mode::Attributes,
                    filter: options.meshopt.then_some("OCTAHEDRAL"),
                    parent_stride: Some(stride),
                })
            }
            Role::TexCoord
                if acc_quantize[index]
                    && comp == accessor::ComponentType::F32
                    && uvs_fit_unorm(&data) =>
            {
                let logical = quantize_uvs(&data, count);
                let acc = &mut root.accessors[index];
                acc.component_type =
                    Checked::Valid(accessor::GenericComponentType(accessor::ComponentType::U16));
                acc.normalized = true;
                any_quantized = true;
                Some(Stream {
                    logical,
                    stride: 4,
                    count,
                    mode: Mode::Attributes,
                    filter: None,
                    parent_stride: Some(4),
                })
            }
            // Everything else vertex-shaped still meshopt-encodes when its
            // element size is stride-legal (4..=256, multiple of 4).
            Role::Normal | Role::Tangent | Role::TexCoord | Role::Position | Role::OtherVertex
                if options.meshopt && elem % 4 == 0 && (4..=256).contains(&elem) =>
            {
                Some(Stream {
                    logical: data,
                    stride: elem,
                    count,
                    mode: Mode::Attributes,
                    filter: None,
                    parent_stride: Some(elem),
                })
            }
            _ => None,
        };
        if stream.is_some() && options.meshopt {
            any_meshopt = true;
        }
        streams.push(stream);
    }

    // ── wrapper nodes for static users of quantized meshes ────────────────
    let mut new_nodes: Vec<json::Node> = Vec::new();
    for (mesh, nodes) in &static_users {
        if !quantize_mesh[*mesh] {
            continue;
        }
        let Some(&(center, s)) = mesh_transform.get(mesh) else {
            continue;
        };
        for &node_index in nodes {
            let wrapper_index = root.nodes.len() + new_nodes.len();
            let node = &mut root.nodes[node_index];
            let mesh_ref = node.mesh.take().unwrap();
            new_nodes.push(json::Node {
                mesh: Some(mesh_ref),
                translation: Some([center.x, center.y, center.z]),
                scale: Some([s, s, s]),
                name: Some("dequant".to_string()),
                ..default_node()
            });
            node.children
                .get_or_insert_with(Vec::new)
                .push(json::Index::new(wrapper_index as u32));
        }
    }
    root.nodes.extend(new_nodes);

    // ── rebuild buffers + views ───────────────────────────────────────────
    // Views owned by accessors are replaced; views NOT referenced by any
    // accessor (embedded images) copy through into the new real BIN.
    let mut accessor_views: HashSet<usize> = HashSet::new();
    for acc in &root.accessors {
        if let Some(view) = acc.buffer_view {
            accessor_views.insert(view.value());
        }
    }

    let mut real: Vec<u8> = Vec::new();
    let mut logical_len: usize = 0;
    let mut new_views: Vec<json::buffer::View> = Vec::new();
    let mut old_view_remap: HashMap<usize, usize> = HashMap::new();

    // pass-through views first (images etc.). Views referenced by NOTHING
    // (e.g. image bytes orphaned by `strip_materials_and_images`) are
    // dropped — their bytes never enter the new BIN.
    let image_views: HashSet<usize> = root
        .images
        .iter()
        .filter_map(|img| img.buffer_view.map(|v| v.value()))
        .collect();
    for (view_index, view) in root.buffer_views.iter().enumerate() {
        if accessor_views.contains(&view_index) {
            continue;
        }
        if !image_views.contains(&view_index) {
            continue; // orphan — drop
        }
        let start = view.byte_offset.unwrap_or_default().0 as usize;
        let len = view.byte_length.0 as usize;
        align4(&mut real);
        let offset = real.len();
        real.extend_from_slice(&bin[start..start + len]);
        let mut v = view.clone();
        v.buffer = json::Index::new(0);
        v.byte_offset = Some(USize64(offset as u64));
        old_view_remap.insert(view_index, new_views.len());
        new_views.push(v);
    }
    for image in &mut root.images {
        if let Some(view) = image.buffer_view {
            image.buffer_view = Some(json::Index::new(old_view_remap[&view.value()] as u32));
        }
    }

    // accessor views: compressed (ext) or plain re-append
    for (index, stream_slot) in streams.iter().enumerate() {
        let acc_view = root.accessors[index].buffer_view.map(|v| v.value());
        let Some(_old_view) = acc_view else { continue };
        let new_view_index = match stream_slot {
            Some(stream) if !options.meshopt => {
                // plain-view quantized stream (KHR_mesh_quantization alone):
                // the logical bytes land directly in the real BIN.
                align4(&mut real);
                let offset = real.len();
                real.extend_from_slice(&stream.logical);
                let view = json::buffer::View {
                    buffer: json::Index::new(0),
                    byte_length: USize64(stream.logical.len() as u64),
                    byte_offset: Some(USize64(offset as u64)),
                    byte_stride: stream.parent_stride.map(json::buffer::Stride),
                    name: None,
                    target: None,
                    extensions: Default::default(),
                    extras: Default::default(),
                };
                let idx = new_views.len();
                new_views.push(view);
                idx
            }
            Some(stream) => {
                // encode
                let encoded = match stream.mode {
                    Mode::Attributes => {
                        encode_attributes(&stream.logical, stream.count, stream.stride)
                    }
                    Mode::Triangles => encode_indices(&stream.logical, stream.count, stream.stride),
                };
                align4(&mut real);
                let real_offset = real.len();
                real.extend_from_slice(&encoded);
                // logical placement in the fallback buffer
                logical_len = logical_len.div_ceil(4) * 4;
                let logical_offset = logical_len;
                logical_len += stream.logical.len();

                let mut ext_obj = serde_json::Map::new();
                ext_obj.insert("buffer".into(), serde_json::json!(0));
                ext_obj.insert("byteOffset".into(), serde_json::json!(real_offset));
                ext_obj.insert("byteLength".into(), serde_json::json!(encoded.len()));
                ext_obj.insert("count".into(), serde_json::json!(stream.count));
                ext_obj.insert("byteStride".into(), serde_json::json!(stream.stride));
                ext_obj.insert(
                    "mode".into(),
                    serde_json::json!(match stream.mode {
                        Mode::Attributes => "ATTRIBUTES",
                        Mode::Triangles => "TRIANGLES",
                    }),
                );
                if let Some(filter) = stream.filter {
                    ext_obj.insert("filter".into(), serde_json::json!(filter));
                }
                let mut others = serde_json::Map::new();
                others.insert(EXT.into(), serde_json::Value::Object(ext_obj));

                let view = json::buffer::View {
                    buffer: json::Index::new(1),
                    byte_length: USize64(stream.logical.len() as u64),
                    byte_offset: Some(USize64(logical_offset as u64)),
                    byte_stride: stream.parent_stride.map(json::buffer::Stride),
                    name: None,
                    target: None,
                    extensions: Some(json::extensions::buffer::View { others }),
                    extras: Default::default(),
                };
                let idx = new_views.len();
                new_views.push(view);
                idx
            }
            None => {
                // raw re-append (IBMs use patched bytes)
                let data: Vec<u8> = match ibm_patched.get(&index) {
                    Some(patched) => patched.clone(),
                    None => read_accessor(&root.accessors[index])?.to_vec(),
                };
                align4(&mut real);
                let offset = real.len();
                real.extend_from_slice(&data);
                let view = json::buffer::View {
                    buffer: json::Index::new(0),
                    byte_length: USize64(data.len() as u64),
                    byte_offset: Some(USize64(offset as u64)),
                    byte_stride: None,
                    name: None,
                    target: None,
                    extensions: Default::default(),
                    extras: Default::default(),
                };
                let idx = new_views.len();
                new_views.push(view);
                idx
            }
        };
        let acc = &mut root.accessors[index];
        acc.buffer_view = Some(json::Index::new(new_view_index as u32));
        acc.byte_offset = Some(USize64(0));
    }

    root.buffer_views = new_views;

    // buffers: 0 = real BIN; 1 = fallback (only when anything compressed)
    let mut buffers = vec![json::Buffer {
        byte_length: USize64(real.len() as u64),
        name: None,
        uri: None,
        extensions: Default::default(),
        extras: Default::default(),
    }];
    if any_meshopt {
        let mut fallback_ext = serde_json::Map::new();
        let mut fb = serde_json::Map::new();
        fb.insert("fallback".into(), serde_json::json!(true));
        fallback_ext.insert(EXT.into(), serde_json::Value::Object(fb));
        buffers.push(json::Buffer {
            byte_length: USize64(logical_len as u64),
            name: None,
            uri: None,
            extensions: Some(json::extensions::buffer::Buffer {
                others: fallback_ext,
            }),
            extras: Default::default(),
        });
    }
    let mut declare = |ext: &str| {
        if !root.extensions_used.iter().any(|e| e == ext) {
            root.extensions_used.push(ext.to_string());
        }
        if !root.extensions_required.iter().any(|e| e == ext) {
            root.extensions_required.push(ext.to_string());
        }
    };
    if any_meshopt {
        declare(EXT);
    }
    if any_quantized {
        declare(QUANT);
    }
    root.buffers = buffers;

    // ── container ─────────────────────────────────────────────────────────
    Ok(crate::write::glb_from_parts(
        serde_json::to_vec(&root)?,
        real,
    ))
}

fn default_node() -> json::Node {
    json::Node {
        camera: None,
        children: None,
        extensions: Default::default(),
        extras: Default::default(),
        matrix: None,
        mesh: None,
        name: None,
        rotation: None,
        scale: None,
        translation: None,
        skin: None,
        weights: None,
    }
}

fn align4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

fn center_extent(min: Vec3, max: Vec3) -> (Vec3, f32) {
    let center = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let s = half.x.max(half.y).max(half.z).max(1e-6);
    (center, s)
}

fn component_size(c: accessor::ComponentType) -> usize {
    match c {
        accessor::ComponentType::I8 | accessor::ComponentType::U8 => 1,
        accessor::ComponentType::I16 | accessor::ComponentType::U16 => 2,
        accessor::ComponentType::U32 | accessor::ComponentType::F32 => 4,
    }
}

fn type_multiplicity(t: accessor::Type) -> usize {
    match t {
        accessor::Type::Scalar => 1,
        accessor::Type::Vec2 => 2,
        accessor::Type::Vec3 => 3,
        accessor::Type::Vec4 => 4,
        accessor::Type::Mat2 => 4,
        accessor::Type::Mat3 => 9,
        accessor::Type::Mat4 => 16,
    }
}

/// f32 VEC3 → i16-normalized VEC3, stride 8 (2 pad bytes), values mapped by
/// the (center, s) dequant. Returns (bytes, per-component int min, max).
fn quantize_positions(
    data: &[u8],
    count: usize,
    center: Vec3,
    s: f32,
) -> (Vec<u8>, [i32; 3], [i32; 3]) {
    let mut out = Vec::with_capacity(count * 8);
    let mut qmin = [i32::MAX; 3];
    let mut qmax = [i32::MIN; 3];
    for i in 0..count {
        let at = i * 12;
        let v = [
            f32::from_le_bytes(data[at..at + 4].try_into().unwrap()),
            f32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap()),
            f32::from_le_bytes(data[at + 8..at + 12].try_into().unwrap()),
        ];
        let c = [center.x, center.y, center.z];
        for k in 0..3 {
            let q = (((v[k] - c[k]) / s) * 32767.0)
                .round()
                .clamp(-32767.0, 32767.0) as i16;
            qmin[k] = qmin[k].min(q as i32);
            qmax[k] = qmax[k].max(q as i32);
            out.extend_from_slice(&q.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]); // pad to stride 8
    }
    (out, qmin, qmax)
}

/// f32 VEC3/VEC4 unit vectors → octahedral-filtered signed bytes, stride 4.
/// (`meshopt_encodeFilterOct`, 8-bit; W passes through for tangents.)
fn oct_encode(data: &[u8], count: usize, components: usize) -> Vec<u8> {
    let mut input = Vec::with_capacity(count * 4);
    for i in 0..count {
        let at = i * components * 4;
        for k in 0..4 {
            let v = if k < components {
                f32::from_le_bytes(data[at + k * 4..at + k * 4 + 4].try_into().unwrap())
            } else {
                0.0
            };
            input.push(v);
        }
    }
    let mut out = vec![0u8; count * 4];
    unsafe {
        ffi::meshopt_encodeFilterOct(out.as_mut_ptr().cast(), count, 4, 8, input.as_ptr());
    }
    out
}

/// f32 VEC3/VEC4 unit-range values → i16-normalized, stride 8 (VEC3 pads 2
/// bytes; VEC4 is exactly 8). The plain-view stand-in for the octahedral
/// meshopt filter, which needs a meshopt decode pass to reverse.
fn quantize_snorm16(data: &[u8], count: usize, components: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(count * 8);
    for i in 0..count {
        let at = i * components * 4;
        for k in 0..components {
            let v = f32::from_le_bytes(data[at + k * 4..at + k * 4 + 4].try_into().unwrap());
            let q = (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            out.extend_from_slice(&q.to_le_bytes());
        }
        if components == 3 {
            out.extend_from_slice(&[0, 0]); // pad to stride 8
        }
    }
    out
}

fn uvs_fit_unorm(data: &[u8]) -> bool {
    data.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .all(|v| (0.0..=1.0).contains(&v))
}

/// f32 VEC2 in [0,1] → u16-normalized VEC2, stride 4.
fn quantize_uvs(data: &[u8], count: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(count * 4);
    for i in 0..count {
        let at = i * 8;
        for k in 0..2 {
            let v = f32::from_le_bytes(data[at + k * 4..at + k * 4 + 4].try_into().unwrap());
            let q = (v * 65535.0).round().clamp(0.0, 65535.0) as u16;
            out.extend_from_slice(&q.to_le_bytes());
        }
    }
    out
}

fn encode_attributes(logical: &[u8], count: usize, stride: usize) -> Vec<u8> {
    unsafe {
        let bound = ffi::meshopt_encodeVertexBufferBound(count, stride);
        let mut out = vec![0u8; bound];
        let written = ffi::meshopt_encodeVertexBuffer(
            out.as_mut_ptr(),
            bound,
            logical.as_ptr().cast(),
            count,
            stride,
        );
        out.truncate(written);
        out
    }
}

fn encode_indices(logical: &[u8], count: usize, index_size: usize) -> Vec<u8> {
    // The encoder consumes u32 indices regardless of on-wire size.
    let indices: Vec<u32> = match index_size {
        2 => logical
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes(c.try_into().unwrap()) as u32)
            .collect(),
        _ => logical
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect(),
    };
    let vertex_count = indices
        .iter()
        .copied()
        .max()
        .map(|m| m as usize + 1)
        .unwrap_or(0);
    unsafe {
        let bound = ffi::meshopt_encodeIndexBufferBound(count, vertex_count);
        let mut out = vec![0u8; bound];
        let written =
            ffi::meshopt_encodeIndexBuffer(out.as_mut_ptr(), bound, indices.as_ptr(), count);
        out.truncate(written);
        out
    }
}
