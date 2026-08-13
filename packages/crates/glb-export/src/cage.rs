//! The compact **cage** encoding for interpolated-flex skins.
//!
//! A MuJoCo interpolated flex blends every vertex from a regular node lattice —
//! `2×2×2` (trilinear) or `3×3×3` (triquadratic) — with weights that are a pure
//! function of the vertex's normalized cage coordinate. Shipping the expanded
//! form costs 8 or 27 `(joint, weight)` pairs per vertex (and the quadratic
//! Lagrange weights go negative, so they cannot even be quantized); shipping
//! the coordinate costs 12 bytes and reconstructs all of them exactly.
//!
//! On the wire the encoding is one custom vertex attribute plus one primitive
//! extension:
//!
//! - [`CAGE_COORDS_ATTRIBUTE`] (`_AWSM_CAGE_COORDS`): per-vertex `VEC3` `f32`
//!   normalized cage coordinates in `[0, 1]³`. Kept f32 on purpose — the
//!   bundle compressor meshopt-encodes unknown vertex attributes losslessly
//!   and never quantizes them, so the reconstruction is deterministic.
//! - [`AWSM_CAGE_INFLUENCES`]: `{ "order": 2|3, "joints": […] }` — the
//!   interpolation order per axis and the skin-joint index at each lattice
//!   position, in the tensor-product order [`CageInfluences::weights_f64`]
//!   emits (x outermost, z innermost).
//!
//! Every consumer expands through THIS module, so the exporter, the editor
//! materialiser, and the player produce bit-identical skin buffers. The basis
//! is evaluated in f64 (like the exporter always did) and cast to f32 at the
//! end.

use serde_json::json;

use crate::SkinInfluenceSet;

/// The primitive-level glTF extension marking a skin carried in cage form:
/// `{ "order": 2|3, "joints": [<skin joint index per lattice position>] }`.
/// Declared in `extensionsUsed` only (our own artifacts, our own loaders —
/// same policy as `AWSM_materials_none`).
pub const AWSM_CAGE_INFLUENCES: &str = "AWSM_cage_influences";

/// The custom vertex attribute holding the normalized cage coordinates,
/// WITHOUT the leading underscore (that is how `gltf::Semantic::Extras` names
/// it; the serialized attribute is `_AWSM_CAGE_COORDS`).
pub const CAGE_COORDS_ATTRIBUTE: &str = "AWSM_CAGE_COORDS";

/// A skin's influences in cage form: the three numbers per vertex that
/// reconstruct all 8 (trilinear) or 27 (quadratic) blend weights exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct CageInfluences {
    /// Nodes per axis: 2 (trilinear) or 3 (triquadratic Lagrange).
    pub order: usize,
    /// Skin-joint index at each of the `order³` lattice positions, indexed
    /// `(i*order + j)*order + k` over axis bins x/y/z — the order
    /// [`Self::weights_f64`] emits.
    pub joints: Vec<u16>,
    /// Per-vertex normalized cage coordinates, in `[0, 1]³`.
    pub coords: Vec<[f32; 3]>,
}

impl CageInfluences {
    /// Influences per vertex: `order³` (8 or 27).
    pub fn influence_count(&self) -> usize {
        self.order.pow(3)
    }

    /// Four-influence sets needed to hold [`Self::influence_count`] (2 or 7 —
    /// the quadratic form leaves one spare slot carrying weight 0).
    pub fn set_count(&self) -> usize {
        self.influence_count().div_ceil(4)
    }

    /// Number of vertices (parallel to the mesh's positions).
    pub fn vertex_count(&self) -> usize {
        self.coords.len()
    }

    /// `order` and `joints` are mutually consistent and expandable.
    pub fn is_valid(&self) -> bool {
        matches!(self.order, 2 | 3) && self.joints.len() == self.influence_count()
    }

    /// The blend weights for one vertex, in lattice order — the tensor product
    /// of the 1D basis along each axis, evaluated in f64:
    ///
    /// - `order` 2 — linear: `[1-t, t]`.
    /// - `order` 3 — quadratic **Lagrange** on nodes at `t = 0, ½, 1` (NOT
    ///   Bernstein — they agree only while the cage stays affine). Lagrange
    ///   goes negative (to −⅛) outside the middle third, which is why the
    ///   expanded weights can never be unorm-quantized.
    pub fn weights_f64(&self, vertex: usize) -> Vec<f64> {
        let t = self.coords[vertex];
        let basis = |t: f64| -> Vec<f64> {
            match self.order {
                2 => vec![1.0 - t, t],
                _ => vec![
                    (1.0 - t) * (1.0 - 2.0 * t),
                    4.0 * t * (1.0 - t),
                    t * (2.0 * t - 1.0),
                ],
            }
        };
        let (bx, by, bz) = (basis(t[0] as f64), basis(t[1] as f64), basis(t[2] as f64));
        let mut w = Vec::with_capacity(self.influence_count());
        for wx in &bx {
            for wy in &by {
                for wz in &bz {
                    w.push(wx * wy * wz);
                }
            }
        }
        w
    }

    /// Expand to the renderer's skin storage-buffer byte layout — one entry per
    /// vertex, all of a vertex's sets contiguous, each influence as
    /// `(u32 joint index LE, f32 weight LE)`. Byte-compatible with
    /// [`crate::ExtractedSkin::packed_index_weights`]; the spare slot of the
    /// last quadratic set pads with `(0, 0.0)` exactly like the expanded
    /// exporter used to.
    pub fn packed_index_weights(&self) -> Vec<u8> {
        let slots = self.set_count() * 4;
        let mut out = Vec::with_capacity(self.vertex_count() * slots * 8);
        for v in 0..self.vertex_count() {
            let w = self.weights_f64(v);
            for slot in 0..slots {
                let (j, wf) = match (self.joints.get(slot), w.get(slot)) {
                    (Some(&j), Some(&wf)) => (j as u32, wf as f32),
                    _ => (0u32, 0.0f32),
                };
                out.extend_from_slice(&j.to_le_bytes());
                out.extend_from_slice(&wf.to_le_bytes());
            }
        }
        out
    }

    /// Expand to the four-influence-set shape (`JOINTS_0`-style set plus
    /// extras) — what the expanded GLB used to carry per vertex. For tests and
    /// any consumer that wants the classic representation.
    #[allow(clippy::type_complexity)]
    pub fn expand_sets(&self) -> (Vec<[u16; 4]>, Vec<[f32; 4]>, Vec<SkinInfluenceSet>) {
        let influences = self.influence_count();
        let nsets = self.set_count();
        let vertnum = self.vertex_count();
        let mut sets = vec![
            SkinInfluenceSet {
                joints: Vec::with_capacity(vertnum),
                weights: Vec::with_capacity(vertnum),
            };
            nsets
        ];
        for v in 0..vertnum {
            let w = self.weights_f64(v);
            for (s, set) in sets.iter_mut().enumerate() {
                let mut joints = [0u16; 4];
                let mut weights = [0.0f32; 4];
                for i in 0..4 {
                    let idx = s * 4 + i;
                    if idx < influences {
                        joints[i] = self.joints[idx];
                        weights[i] = w[idx] as f32;
                    }
                }
                set.joints.push(joints);
                set.weights.push(weights);
            }
        }
        let first = sets.remove(0);
        (first.joints, first.weights, sets)
    }

    /// The [`AWSM_CAGE_INFLUENCES`] extension payload (order + joint list; the
    /// coords ride the [`CAGE_COORDS_ATTRIBUTE`] accessor).
    pub fn marker_json(&self) -> serde_json::Value {
        json!({ "order": self.order, "joints": self.joints })
    }

    /// Rebuild from the extension payload + the coords attribute. `None` when
    /// the marker is malformed or inconsistent with a 2/3 lattice — a silently
    /// wrong cage is exactly the failure mode worth refusing.
    pub fn from_marker(marker: &serde_json::Value, coords: Vec<[f32; 3]>) -> Option<Self> {
        let order = marker.get("order")?.as_u64()? as usize;
        let joints: Vec<u16> = marker
            .get("joints")?
            .as_array()?
            .iter()
            .map(|j| u16::try_from(j.as_u64()?).ok())
            .collect::<Option<_>>()?;
        let cage = Self {
            order,
            joints,
            coords,
        };
        cage.is_valid().then_some(cage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quadratic(coords: Vec<[f32; 3]>) -> CageInfluences {
        CageInfluences {
            order: 3,
            joints: (0..27).collect(),
            coords,
        }
    }

    #[test]
    fn weights_partition_unity_and_go_negative_for_quadratic() {
        let cage = quadratic(vec![[0.1, 0.35, 0.8], [0.9, 0.05, 0.5]]);
        for v in 0..2 {
            let w = cage.weights_f64(v);
            assert_eq!(w.len(), 27);
            let sum: f64 = w.iter().sum();
            assert!((sum - 1.0).abs() < 1e-12, "sum {sum}");
        }
        // Lagrange must go negative — the property the unorm8 quantizer guard
        // (`weights_fit_unorm8`) exists for.
        let min = cage
            .weights_f64(0)
            .into_iter()
            .fold(f64::MAX, f64::min);
        assert!(min < -0.01, "expected a negative lobe, got min {min}");
    }

    #[test]
    fn quadratic_lagrange_interpolates_lattice_nodes_exactly() {
        // At a lattice node's own coordinate, its weight is exactly 1 and all
        // others exactly 0 — the interpolating property that distinguishes
        // Lagrange from Bernstein.
        let nodes = [0.0f32, 0.5, 1.0];
        for (i, &x) in nodes.iter().enumerate() {
            for (j, &y) in nodes.iter().enumerate() {
                for (k, &z) in nodes.iter().enumerate() {
                    let cage = quadratic(vec![[x, y, z]]);
                    let w = cage.weights_f64(0);
                    let expect = (i * 3 + j) * 3 + k;
                    for (n, &wn) in w.iter().enumerate() {
                        let want = if n == expect { 1.0 } else { 0.0 };
                        assert!(
                            (wn - want).abs() < 1e-12,
                            "node ({i},{j},{k}): weight[{n}] = {wn}, want {want}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn trilinear_matches_the_corner_product() {
        let cage = CageInfluences {
            order: 2,
            joints: (0..8).collect(),
            coords: vec![[0.25, 0.5, 0.75]],
        };
        let w = cage.weights_f64(0);
        let t = [0.25f64, 0.5, 0.75];
        // Lattice order: x outermost, z innermost; bin 0 carries (1-t).
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..2 {
                    let want = [(i, 0), (j, 1), (k, 2)]
                        .iter()
                        .map(|&(bin, ax)| if bin == 1 { t[ax] } else { 1.0 - t[ax] })
                        .product::<f64>();
                    let got = w[(i * 2 + j) * 2 + k];
                    assert!((got - want).abs() < 1e-12);
                }
            }
        }
        assert_eq!(cage.set_count(), 2);
    }

    #[test]
    fn packed_bytes_match_the_expanded_sets() {
        // The packed layout must be byte-identical to expanding first and then
        // packing via `ExtractedSkin::packed_index_weights` — that is the
        // contract that lets every consumer skip the expanded form.
        let cage = quadratic(vec![[0.2, 0.4, 0.9], [0.0, 1.0, 0.5]]);
        let (joints, weights, extra_sets) = cage.expand_sets();
        let skin = crate::ExtractedSkin {
            joint_node_indices: vec![],
            inverse_bind_matrices: vec![],
            joints,
            weights,
            extra_sets,
            cage: None,
        };
        assert_eq!(skin.set_count(), 7);
        assert_eq!(cage.packed_index_weights(), skin.packed_index_weights());
    }

    #[test]
    fn marker_roundtrip_and_validation() {
        let cage = quadratic(vec![[0.1, 0.2, 0.3]]);
        let marker = cage.marker_json();
        let back = CageInfluences::from_marker(&marker, cage.coords.clone()).expect("roundtrip");
        assert_eq!(back, cage);

        // Wrong joint count for the order → refused.
        let bad = json!({ "order": 3, "joints": [0, 1, 2] });
        assert!(CageInfluences::from_marker(&bad, vec![]).is_none());
        // Unknown order → refused.
        let bad = json!({ "order": 4, "joints": (0..64).collect::<Vec<u16>>() });
        assert!(CageInfluences::from_marker(&bad, vec![]).is_none());
    }
}
