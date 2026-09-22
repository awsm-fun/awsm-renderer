//! Turning a USD `Mesh` into glTF-shaped triangle meshes, one per material piece.
//!
//! USD meshes are polygonal, with each primvar (normals, UVs) at its own
//! **interpolation**: per point, per face-corner, per face, or constant. glTF has
//! one index per vertex, so every distinct `(point, normal, uv)` becomes one
//! vertex — the same de-indexing the MuJoCo exporter does for OBJ-style meshes.
//! Faces are fan-triangulated.
//!
//! A mesh split into material `GeomSubset`s becomes one piece per subset (plus a
//! remainder for faces no subset claims), because the importer mints one
//! material per geom and the GLB carries geometry only.
//!
//! Each piece carries exactly the UV sets its material samples, in the
//! material's order (set 0 always first), as `TEXCOORD_0..n` — so a texture's
//! sidecar `uv` index means the same thing on every mesh the material is on,
//! whatever the mesh calls its primvars.

use anyhow::{bail, Result};
use awsm_renderer_glb_export::MeshData;
use glam::{DMat3, DMat4, DVec3};
use openusd::usd::{Prim, Stage};

use crate::stage::{
    interpolation, value, value_int_array, value_token, value_vec2_array, value_vec3_array,
};

/// One drawable piece of a mesh: the faces bound to one material.
pub struct Piece {
    /// The prim whose material binding applies — the subset when the mesh is
    /// split, otherwise the mesh itself.
    pub binding_prim: Prim,
    /// Subset name, `None` for a whole mesh or its unclaimed remainder.
    pub subset: Option<String>,
    pub mesh: MeshData,
}

/// Primvar names tried, in order, for the first UV set. `st` is the USD
/// convention; Isaac's CAD-converted assets write `st_0`.
const UV_NAMES: &[&str] = &[
    "primvars:st",
    "primvars:st_0",
    "primvars:st0",
    "primvars:UVMap",
    "primvars:uv",
];

/// A welded vertex's identity: position slot, normal bits, and the bits of
/// every UV set's value.
type WeldKey = (usize, [u64; 3], Vec<[u64; 2]>);

/// A UV set a material samples, as the material names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum UvSet {
    /// By index — OmniPBR's `uv_space_index`. Set `n` is the mesh's
    /// `primvars:st{n}` / `primvars:st_{n}` (set 0: see [`UV_NAMES`]).
    Index(u32),
    /// By primvar name — a `UsdPrimvarReader_float2`'s `varname`.
    Name(String),
}

impl UvSet {
    /// The primvar names this set may go by on a mesh, most specific first.
    fn candidates(&self) -> Vec<String> {
        match self {
            UvSet::Index(0) => UV_NAMES.iter().map(|s| s.to_string()).collect(),
            UvSet::Index(n) => vec![format!("primvars:st{n}"), format!("primvars:st_{n}")],
            UvSet::Name(name) => {
                let mut out = vec![format!("primvars:{name}")];
                // `st`, `st1`, `st_2`, ...: the conventional names ARE indices,
                // so a reader asking for `st` still finds Isaac's `st_0`.
                let digits = name
                    .strip_prefix("st_")
                    .or_else(|| name.strip_prefix("st"))
                    .map(|d| if d.is_empty() { "0" } else { d });
                if let Some(n) = digits.and_then(|d| d.parse::<u32>().ok()) {
                    out.extend(UvSet::Index(n).candidates());
                }
                out
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interp {
    Constant,
    Uniform,
    Vertex,
    FaceVarying,
}

/// A primvar resolved to per-slot values plus how a face-corner picks its slot.
struct Primvar<T> {
    values: Vec<T>,
    interp: Interp,
    /// `primvars:<name>:indices` — an indexed primvar stores unique values once
    /// and maps each slot to one of them.
    indices: Option<Vec<i64>>,
}

impl<T: Copy> Primvar<T> {
    /// The value for face `face`, corner `corner` (global corner index), point
    /// `point`.
    fn at(&self, face: usize, corner: usize, point: usize) -> Option<T> {
        let slot = match self.interp {
            Interp::Constant => 0,
            Interp::Uniform => face,
            Interp::Vertex => point,
            Interp::FaceVarying => corner,
        };
        let index = match &self.indices {
            Some(ix) => usize::try_from(*ix.get(slot)?).ok()?,
            None => slot,
        };
        self.values.get(index).copied()
    }
}

struct Counts {
    points: usize,
    faces: usize,
    corners: usize,
}

fn resolve_interp(declared: Option<&str>, slots: usize, counts: &Counts) -> Option<Interp> {
    let declared = match declared {
        Some("constant") => Some(Interp::Constant),
        Some("uniform") => Some(Interp::Uniform),
        Some("vertex") | Some("varying") => Some(Interp::Vertex),
        Some("faceVarying") => Some(Interp::FaceVarying),
        _ => None,
    };
    let expected = |i: Interp| match i {
        Interp::Constant => 1,
        Interp::Uniform => counts.faces,
        Interp::Vertex => counts.points,
        Interp::FaceVarying => counts.corners,
    };
    if let Some(i) = declared {
        if expected(i) == slots {
            return Some(i);
        }
    }
    // Undeclared (the `normals` attribute defaults to `vertex`) or declared
    // wrongly: infer from the slot count, preferring the finest match.
    [
        Interp::FaceVarying,
        Interp::Vertex,
        Interp::Uniform,
        Interp::Constant,
    ]
    .into_iter()
    .find(|i| expected(*i) == slots)
}

fn read_primvar<T>(
    prim: &Prim,
    name: &str,
    counts: &Counts,
    decode: impl Fn(&openusd::sdf::Value) -> Option<Vec<T>>,
) -> Option<Primvar<T>> {
    let attr = prim.attribute(name);
    let values = decode(&value(&attr)?)?;
    if values.is_empty() {
        return None;
    }
    let indices =
        value(&prim.attribute(format!("{name}:indices"))).and_then(|v| value_int_array(&v));
    let slots = indices.as_ref().map_or(values.len(), Vec::len);
    let interp = resolve_interp(interpolation(&attr).as_deref(), slots, counts)?;
    Some(Primvar {
        values,
        interp,
        indices,
    })
}

/// Split `prim` (a `Mesh`) into material pieces, with every vertex transformed
/// by `xform` — the mesh's transform relative to the frame it will be drawn in.
///
/// `uv_sets_for` answers, for a piece's binding prim, which UV sets its
/// material samples (set 0 first). A set the mesh does not have falls back to
/// set 0, and says so in `notes`.
pub fn pieces(
    stage: &Stage,
    prim: &Prim,
    xform: DMat4,
    uv_sets_for: &mut dyn FnMut(&Prim) -> Vec<UvSet>,
    notes: &mut Vec<String>,
) -> Result<Vec<Piece>> {
    let path = prim.path().clone();
    let Some(points) = value(&prim.attribute("points")).and_then(|v| value_vec3_array(&v)) else {
        return Ok(Vec::new());
    };
    let Some(face_counts) =
        value(&prim.attribute("faceVertexCounts")).and_then(|v| value_int_array(&v))
    else {
        return Ok(Vec::new());
    };
    let Some(face_indices) =
        value(&prim.attribute("faceVertexIndices")).and_then(|v| value_int_array(&v))
    else {
        return Ok(Vec::new());
    };

    // Corner offsets, validated once so every later lookup is in range.
    let mut starts = Vec::with_capacity(face_counts.len());
    let mut corner = 0usize;
    for (f, c) in face_counts.iter().enumerate() {
        if *c < 0 {
            bail!("{path}: face {f} has negative vertex count {c}");
        }
        starts.push(corner);
        corner += *c as usize;
    }
    if corner != face_indices.len() {
        bail!(
            "{path}: faceVertexCounts sum to {corner} but there are {} faceVertexIndices",
            face_indices.len()
        );
    }
    if let Some(bad) = face_indices
        .iter()
        .find(|i| **i < 0 || **i as usize >= points.len())
    {
        bail!(
            "{path}: face vertex index {bad} out of range for {} points",
            points.len()
        );
    }

    let counts = Counts {
        points: points.len(),
        faces: face_counts.len(),
        corners: face_indices.len(),
    };
    // `primvars:normals` overrides the `normals` attribute when both exist.
    let normals = read_primvar(prim, "primvars:normals", &counts, value_vec3_array)
        .or_else(|| read_primvar(prim, "normals", &counts, value_vec3_array));
    // Each requested UV set, read once per mesh; `None` = the mesh has none.
    let mut uv_cache: std::collections::HashMap<UvSet, Option<std::rc::Rc<Primvar<[f64; 2]>>>> =
        std::collections::HashMap::new();
    let mut read_uv = |set: &UvSet| -> Option<std::rc::Rc<Primvar<[f64; 2]>>> {
        uv_cache
            .entry(set.clone())
            .or_insert_with(|| {
                set.candidates()
                    .iter()
                    .find_map(|n| read_primvar(prim, n, &counts, value_vec2_array))
                    .map(std::rc::Rc::new)
            })
            .clone()
    };
    // The primvars for one piece's sets, in order. A missing set falls back to
    // set 0 (a texture that samples SOMETHING beats one that samples nothing);
    // with no set 0 either, the piece has no UVs at all.
    let mut uvs_for =
        |sets: &[UvSet], notes: &mut Vec<String>| -> Vec<std::rc::Rc<Primvar<[f64; 2]>>> {
            let Some(base) = read_uv(&UvSet::Index(0)) else {
                return Vec::new();
            };
            sets.iter()
                .map(|set| match read_uv(set) {
                    Some(p) => p,
                    None => {
                        notes.push(format!(
                            "{path}: UV set {set:?} not found; using the first UV set"
                        ));
                        base.clone()
                    }
                })
                .collect()
        };

    // A left-handed mesh, or a mirroring transform, reverses which way a
    // counter-clockwise face points; flip once to keep glTF's CCW-front rule.
    let left_handed = value(&prim.attribute("orientation"))
        .and_then(|v| value_token(&v))
        .is_some_and(|o| o == "leftHanded");
    let flip = left_handed ^ (xform.determinant() < 0.0);
    let normal_xform = DMat3::from_mat4(xform).inverse().transpose();

    let build = |faces: &mut dyn Iterator<Item = usize>,
                 uvs: &[std::rc::Rc<Primvar<[f64; 2]>>]|
     -> MeshData {
        let mut out = MeshData::default();
        let mut uv_out: Vec<Vec<[f32; 2]>> = vec![Vec::new(); uvs.len()];
        let mut normal_out: Vec<[f32; 3]> = Vec::new();
        // Keyed on the attribute VALUES, not their slots: face-varying normals
        // give every corner its own slot even where neighbouring faces share
        // the exact normal (every flat or smooth-shaded region of a CAD mesh),
        // and keying on slots would triple the vertex count for nothing.
        let mut seen: std::collections::HashMap<WeldKey, u32> = std::collections::HashMap::new();
        for f in faces {
            let n = face_counts[f] as usize;
            if n < 3 {
                continue;
            }
            let start = starts[f];
            let mut vertex = |c: usize| -> u32 {
                let corner = start + c;
                let point = face_indices[corner] as usize;
                let nrm = normals.as_ref().and_then(|p| p.at(f, corner, point));
                let uv: Vec<Option<[f64; 2]>> =
                    uvs.iter().map(|p| p.at(f, corner, point)).collect();
                let key = (
                    point,
                    nrm.map_or([u64::MAX; 3], |v| v.map(f64::to_bits)),
                    uv.iter()
                        .map(|v| v.map_or([u64::MAX; 2], |v| v.map(f64::to_bits)))
                        .collect(),
                );
                let next = out.positions.len() as u32;
                *seen.entry(key).or_insert_with(|| {
                    let p = xform.transform_point3(DVec3::from_array(points[point]));
                    out.positions.push(p.as_vec3().to_array());
                    if let Some(v) = nrm {
                        let v = (normal_xform * DVec3::from_array(v)).normalize_or_zero();
                        normal_out.push(v.as_vec3().to_array());
                    }
                    for (set, v) in uv_out.iter_mut().zip(&uv) {
                        if let Some(v) = v {
                            // USD's V runs bottom-up; glTF's top-down.
                            set.push([v[0] as f32, 1.0 - v[1] as f32]);
                        }
                    }
                    next
                })
            };
            let first = vertex(0);
            for k in 1..n - 1 {
                let (b, c) = (vertex(k), vertex(k + 1));
                if flip {
                    out.indices.extend_from_slice(&[first, c, b]);
                } else {
                    out.indices.extend_from_slice(&[first, b, c]);
                }
            }
        }
        // Normals and UVs are all-or-nothing per mesh: a primvar that resolved
        // for some corners and not others is malformed, and a partial array
        // would misalign every vertex after the gap.
        if normal_out.len() == out.positions.len() && !normal_out.is_empty() {
            out.normals = Some(normal_out);
        } else if !out.positions.is_empty() {
            out.compute_vertex_normals();
        }
        if !out.positions.is_empty() && uv_out.iter().all(|s| s.len() == out.positions.len()) {
            out.uvs = uv_out;
        }
        out
    };

    let subsets = material_subsets(stage, prim, face_counts.len());
    if subsets.is_empty() {
        let uvs = uvs_for(&uv_sets_for(prim), notes);
        let mesh = build(&mut (0..face_counts.len()), &uvs);
        if mesh.indices.is_empty() {
            return Ok(Vec::new());
        }
        return Ok(vec![Piece {
            binding_prim: prim.clone(),
            subset: None,
            mesh,
        }]);
    }

    let mut claimed = vec![false; face_counts.len()];
    let mut out = Vec::new();
    for (subset, faces) in &subsets {
        for f in faces {
            claimed[*f] = true;
        }
        let uvs = uvs_for(&uv_sets_for(subset), notes);
        let mesh = build(&mut faces.iter().copied(), &uvs);
        if !mesh.indices.is_empty() {
            out.push(Piece {
                binding_prim: subset.clone(),
                subset: subset.path().name().map(str::to_string),
                mesh,
            });
        }
    }
    let uvs = uvs_for(&uv_sets_for(prim), notes);
    let rest = build(&mut (0..face_counts.len()).filter(|f| !claimed[*f]), &uvs);
    if !rest.indices.is_empty() {
        out.push(Piece {
            binding_prim: prim.clone(),
            subset: None,
            mesh: rest,
        });
    }
    Ok(out)
}

/// The mesh's `materialBind` face subsets with their (in-range, deduplicated)
/// face lists. Subsets of other families (physics, user tags) are ignored.
fn material_subsets(_stage: &Stage, prim: &Prim, faces: usize) -> Vec<(Prim, Vec<usize>)> {
    let Ok(children) = prim.children() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for child in children {
        let is_subset = child
            .type_name()
            .ok()
            .flatten()
            .is_some_and(|t| t.as_str() == "GeomSubset");
        if !is_subset {
            continue;
        }
        let element = value(&child.attribute("elementType")).and_then(|v| value_token(&v));
        if element.as_deref().is_some_and(|e| e != "face") {
            continue;
        }
        let family = value(&child.attribute("familyName")).and_then(|v| value_token(&v));
        if family.as_deref().is_some_and(|f| f != "materialBind") {
            continue;
        }
        let Some(indices) = value(&child.attribute("indices")).and_then(|v| value_int_array(&v))
        else {
            continue;
        };
        let mut list: Vec<usize> = indices
            .into_iter()
            .filter(|i| *i >= 0 && (*i as usize) < faces)
            .map(|i| i as usize)
            .collect();
        list.sort_unstable();
        list.dedup();
        out.push((child, list));
    }
    out
}
