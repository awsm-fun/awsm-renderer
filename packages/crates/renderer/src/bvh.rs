//! Software-BVH acceleration structures for off-screen reflections
//! (docs/plans/bvh-reflections.md — reflection-plan Tier 7).
//!
//! BLAS: one binary BVH per unique STATIC mesh resource, built CPU-side at
//! mesh commit (`Meshes::resolve_one`, where `GeometrySource.positions` /
//! `.indices` are still resident) and flattened into two shared
//! [`DynamicStorageBuffer`]s — 32-byte nodes and 48-byte expanded triangles
//! (3 × vec4, no index indirection on the GPU). Skinned meshes and meshes
//! over [`BvhStore::MAX_TRIS_PER_MESH`] are excluded (logged, not fatal):
//! their reflections stay on the SSR/probe path.
//!
//! TLAS: a flat instance array rebuilt each enabled frame (a linear GPU-side
//! scan — correct and fast to a few hundred instances; a real tree is a
//! follow-up when a scene proves it). Each instance carries the inverse
//! world matrix (rays are traced in OBJECT space; `t` is shared between
//! spaces because the direction is transformed unnormalized), the world
//! AABB for early reject, its BLAS bases, and a premultiplied emissive
//! color for the constrained hit shading.
//!
//! The BLAS is built at commit UNCONDITIONALLY (not gated on the
//! `ssr.bvh_reflections` toggle): the source geometry is consumed at commit,
//! so a lazily-built BLAS could never cover meshes committed while the
//! toggle was off. The cost is bounded by the per-mesh triangle cap and is
//! a few MB for typical scenes; the GPU upload itself only happens while
//! the feature is enabled (`write_gpu` is called from the SSR path).

use awsm_renderer_core::renderer::AwsmRendererWebGpu;

use crate::buffer::dynamic_storage::DynamicStorageBuffer;
use crate::buffer::mapped_uploader::MappedUploader;
use awsm_renderer_core::error::AwsmCoreError;

type Result<T> = std::result::Result<T, AwsmCoreError>;
use crate::meshes::MeshResourceKey;

/// GPU node layout (32 bytes, matches `BvhNode` in `ssr_wgsl/bvh_trace.wgsl`):
/// `min.xyz, a, max.xyz, b` where a leaf sets `a = 0x8000_0000 | first_tri`
/// (tri index LOCAL to this BLAS) and `b = tri_count`; an internal node sets
/// `a = left_child`, `b = right_child` (node indices LOCAL to this BLAS).
const NODE_BYTES: usize = 32;
/// GPU triangle layout: 3 × vec4<f32> (xyz + pad) = 48 bytes.
const TRI_BYTES: usize = 48;
/// Leaf size — small leaves keep traversal shallow without exploding nodes.
const LEAF_TRIS: usize = 4;
/// The WGSL traversal stack is `array<u32, 28>` and its push guard drops
/// children when `sp >= 26` — so the builder must never emit internal nodes
/// past depth 24 (a depth-d internal node's children push at sp ≤ d+1).
/// Median splits on ≤ MAX_TRIS_PER_MESH tris are ~log2(64k/4) ≈ 14 deep in
/// practice; the cap is a hard safety net that now actually honors the
/// traversal contract instead of merely gesturing at it.
const MAX_DEPTH: usize = 24;

/// GPU TLAS instance layout (112 bytes, matches `BvhInstance` in
/// `ssr_wgsl/bvh_trace.wgsl`): inv_world mat4 + emissive vec4 +
/// world_min vec4 (w = bitcast node base, in NODES) + world_max vec4
/// (w = bitcast tri base, in VEC4s).
pub const INSTANCE_BYTES: usize = 112;

pub struct BvhStore {
    nodes: DynamicStorageBuffer<MeshResourceKey>,
    tris: DynamicStorageBuffer<MeshResourceKey>,
    /// Exact stored triangle count per key. The budget accounting MUST use
    /// this, not `tris.size(key)` — the buddy allocator rounds allocations
    /// up to a power of two, so subtracting the allocated size on remove
    /// drifts `total_tris` toward zero over import/delete churn and quietly
    /// disarms the `MAX_TOTAL_TRIS` guard.
    tri_counts: slotmap::SecondaryMap<MeshResourceKey, usize>,
    /// Total triangles stored (across all BLASes) — the byte-cap guard.
    total_tris: usize,
    nodes_dirty: bool,
    tris_dirty: bool,
    pub nodes_gpu: web_sys::GpuBuffer,
    pub tris_gpu: web_sys::GpuBuffer,
    nodes_uploader: MappedUploader,
    tris_uploader: MappedUploader,
    /// Set when either GPU buffer was recreated (bind groups must rebuild).
    pub buffers_recreated: bool,
}

impl BvhStore {
    /// Per-mesh triangle cap — bigger meshes are skipped (their reflections
    /// stay on the probe path). Keeps a single pathological import from
    /// ballooning the shared buffers.
    pub const MAX_TRIS_PER_MESH: usize = 65_536;
    /// Total triangle budget across every BLAS (~48 MB of triangle data).
    pub const MAX_TOTAL_TRIS: usize = 1_000_000;

    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let nodes = DynamicStorageBuffer::new(64 * 1024, Some("BvhNodes".into()));
        let tris = DynamicStorageBuffer::new(64 * 1024, Some("BvhTris".into()));
        let nodes_gpu = create_storage_buffer(gpu, "BvhNodes", nodes.capacity())?;
        let tris_gpu = create_storage_buffer(gpu, "BvhTris", tris.capacity())?;
        Ok(Self {
            nodes,
            tris,
            tri_counts: slotmap::SecondaryMap::new(),
            total_tris: 0,
            nodes_dirty: false,
            tris_dirty: false,
            nodes_gpu,
            tris_gpu,
            nodes_uploader: MappedUploader::new("BvhNodes"),
            tris_uploader: MappedUploader::new("BvhTris"),
            buffers_recreated: false,
        })
    }

    /// Build + store the BLAS for one committed mesh resource. Skinned or
    /// over-cap meshes are skipped silently-but-logged; the caller doesn't
    /// need to care.
    pub fn add(&mut self, key: MeshResourceKey, positions: &[[f32; 3]], indices: &[u32]) {
        let tri_count = indices.len() / 3;
        if tri_count == 0 {
            return;
        }
        if tri_count > Self::MAX_TRIS_PER_MESH {
            tracing::info!(
                "bvh: skipping mesh resource with {tri_count} tris (cap {})",
                Self::MAX_TRIS_PER_MESH
            );
            return;
        }
        if self.total_tris + tri_count > Self::MAX_TOTAL_TRIS {
            tracing::warn!(
                "bvh: total triangle budget exhausted ({} + {tri_count} > {}); skipping",
                self.total_tris,
                Self::MAX_TOTAL_TRIS
            );
            return;
        }
        let (node_bytes, tri_bytes) = build_blas(positions, indices);
        // Buddy offsets are 256-aligned: divisible by 32 (nodes) and by 16
        // (vec4s) — the WGSL indexes nodes by element and tris by vec4.
        let node_ok = self.nodes.update(key, &node_bytes).is_ok();
        let tri_ok = self.tris.update(key, &tri_bytes).is_ok();
        if !(node_ok && tri_ok) {
            tracing::warn!("bvh: BLAS store failed (buffer growth); skipping mesh");
            self.nodes.remove(key);
            self.tris.remove(key);
            return;
        }
        self.tri_counts.insert(key, tri_count);
        self.total_tris += tri_count;
        self.nodes_dirty = true;
        self.tris_dirty = true;
    }

    pub fn remove(&mut self, key: MeshResourceKey) {
        if let Some(count) = self.tri_counts.remove(key) {
            self.total_tris = self.total_tris.saturating_sub(count);
        }
        self.nodes.remove(key);
        self.tris.remove(key);
    }

    /// Whether this resource has a BLAS (statics under the caps).
    pub fn has(&self, key: MeshResourceKey) -> bool {
        self.nodes.contains_key(key)
    }

    /// (node_base_elements, tri_base_vec4s) for a stored BLAS.
    pub fn bases(&self, key: MeshResourceKey) -> Option<(u32, u32)> {
        let n = self.nodes.offset(key)?;
        let t = self.tris.offset(key)?;
        Some(((n / NODE_BYTES) as u32, (t / 16) as u32))
    }

    /// Flush CPU state to the GPU. Called from the render loop only while
    /// `ssr.bvh_reflections` is enabled — the CPU mirror accumulates dirt
    /// while the feature is off and catches up on the first enabled frame.
    pub fn write_gpu(&mut self, gpu: &AwsmRendererWebGpu) -> Result<()> {
        self.buffers_recreated = false;
        if self.nodes_dirty {
            flush(
                gpu,
                &mut self.nodes,
                &mut self.nodes_gpu,
                &mut self.nodes_uploader,
                "BvhNodes",
                &mut self.buffers_recreated,
            )?;
            self.nodes_dirty = false;
        }
        if self.tris_dirty {
            flush(
                gpu,
                &mut self.tris,
                &mut self.tris_gpu,
                &mut self.tris_uploader,
                "BvhTris",
                &mut self.buffers_recreated,
            )?;
            self.tris_dirty = false;
        }
        Ok(())
    }
}

fn create_storage_buffer(
    gpu: &AwsmRendererWebGpu,
    label: &str,
    size: usize,
) -> Result<web_sys::GpuBuffer> {
    use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
    gpu.create_buffer(
        &BufferDescriptor::new(
            Some(label),
            size,
            BufferUsage::new().with_storage().with_copy_dst(),
        )
        .into(),
    )
}

fn flush(
    gpu: &AwsmRendererWebGpu,
    cpu: &mut DynamicStorageBuffer<MeshResourceKey>,
    gpu_buffer: &mut web_sys::GpuBuffer,
    uploader: &mut MappedUploader,
    label: &str,
    recreated: &mut bool,
) -> Result<()> {
    if let Some(new_size) = cpu.take_gpu_needs_resize() {
        *gpu_buffer = create_storage_buffer(gpu, label, new_size)?;
        *recreated = true;
        cpu.clear_dirty_ranges();
        gpu.write_buffer(gpu_buffer, None, cpu.raw_slice(), None, None)?;
    } else {
        let ranges = cpu.take_dirty_ranges();
        uploader.write_dirty_ranges(
            gpu,
            gpu_buffer,
            cpu.raw_slice().len(),
            cpu.raw_slice(),
            &ranges,
        )?;
        cpu.recycle_dirty_ranges(ranges);
    }
    Ok(())
}

// ------------------------------------------------------------------ builder

struct BuildTri {
    idx: [u32; 3],
    centroid: [f32; 3],
    min: [f32; 3],
    max: [f32; 3],
}

/// Build one BLAS: returns (node_bytes, tri_bytes) in the GPU layouts.
fn build_blas(positions: &[[f32; 3]], indices: &[u32]) -> (Vec<u8>, Vec<u8>) {
    let tri_count = indices.len() / 3;
    let mut tris: Vec<BuildTri> = Vec::with_capacity(tri_count);
    for t in 0..tri_count {
        let idx = [indices[t * 3], indices[t * 3 + 1], indices[t * 3 + 2]];
        let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
        for &i in &idx {
            let p = positions[i as usize];
            for k in 0..3 {
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        let centroid = [
            (mn[0] + mx[0]) * 0.5,
            (mn[1] + mx[1]) * 0.5,
            (mn[2] + mx[2]) * 0.5,
        ];
        tris.push(BuildTri {
            idx,
            centroid,
            min: mn,
            max: mx,
        });
    }

    // Nodes accumulate in DFS order; `order` is the reordered tri sequence.
    struct Node {
        min: [f32; 3],
        max: [f32; 3],
        a: u32,
        b: u32,
    }
    let mut nodes: Vec<Node> = Vec::with_capacity(tri_count / LEAF_TRIS * 2 + 1);
    let mut order: Vec<u32> = Vec::with_capacity(tri_count);

    fn bounds(tris: &[BuildTri], lo: usize, hi: usize) -> ([f32; 3], [f32; 3]) {
        let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
        for t in &tris[lo..hi] {
            for k in 0..3 {
                mn[k] = mn[k].min(t.min[k]);
                mx[k] = mx[k].max(t.max[k]);
            }
        }
        (mn, mx)
    }

    // Recursive helper via explicit stack: each entry builds node `ni` over
    // tris[lo..hi] at `depth`.
    let mut stack: Vec<(usize, usize, usize, usize)> = Vec::new();
    let (mn, mx) = bounds(&tris, 0, tris.len());
    nodes.push(Node {
        min: mn,
        max: mx,
        a: 0,
        b: 0,
    });
    stack.push((0, tris.len(), 0, 0));

    while let Some((lo, hi, ni, depth)) = stack.pop() {
        let count = hi - lo;
        if count <= LEAF_TRIS || depth >= MAX_DEPTH {
            let first = order.len() as u32;
            for k in lo..hi {
                order.push(k as u32);
            }
            nodes[ni].a = 0x8000_0000 | first;
            nodes[ni].b = count as u32;
            continue;
        }
        // median split on the longest centroid axis
        let (mut cmin, mut cmax) = ([f32::MAX; 3], [f32::MIN; 3]);
        for t in &tris[lo..hi] {
            for k in 0..3 {
                cmin[k] = cmin[k].min(t.centroid[k]);
                cmax[k] = cmax[k].max(t.centroid[k]);
            }
        }
        let ext = [cmax[0] - cmin[0], cmax[1] - cmin[1], cmax[2] - cmin[2]];
        let axis = if ext[0] >= ext[1] && ext[0] >= ext[2] {
            0
        } else if ext[1] >= ext[2] {
            1
        } else {
            2
        };
        let mid = lo + count / 2;
        tris[lo..hi].select_nth_unstable_by(count / 2, |x, y| {
            x.centroid[axis]
                .partial_cmp(&y.centroid[axis])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let (lmn, lmx) = bounds(&tris, lo, mid);
        let (rmn, rmx) = bounds(&tris, mid, hi);
        let li = nodes.len();
        nodes.push(Node {
            min: lmn,
            max: lmx,
            a: 0,
            b: 0,
        });
        let ri = nodes.len();
        nodes.push(Node {
            min: rmn,
            max: rmx,
            a: 0,
            b: 0,
        });
        nodes[ni].a = li as u32;
        nodes[ni].b = ri as u32;
        stack.push((lo, mid, li, depth + 1));
        stack.push((mid, hi, ri, depth + 1));
    }

    let mut node_bytes = Vec::with_capacity(nodes.len() * NODE_BYTES);
    for n in &nodes {
        for v in n.min {
            node_bytes.extend_from_slice(&v.to_ne_bytes());
        }
        node_bytes.extend_from_slice(&n.a.to_ne_bytes());
        for v in n.max {
            node_bytes.extend_from_slice(&v.to_ne_bytes());
        }
        node_bytes.extend_from_slice(&n.b.to_ne_bytes());
    }
    let mut tri_bytes = Vec::with_capacity(order.len() * TRI_BYTES);
    for &t in &order {
        let tri = &tris[t as usize];
        for &i in &tri.idx {
            let p = positions[i as usize];
            tri_bytes.extend_from_slice(&p[0].to_ne_bytes());
            tri_bytes.extend_from_slice(&p[1].to_ne_bytes());
            tri_bytes.extend_from_slice(&p[2].to_ne_bytes());
            tri_bytes.extend_from_slice(&0.0f32.to_ne_bytes());
        }
    }
    (node_bytes, tri_bytes)
}

/// Append one TLAS instance to `out` in the GPU layout. `bases` come from
/// [`BvhStore::bases`]; `emissive` is already premultiplied by strength.
#[allow(clippy::too_many_arguments)]
pub fn push_instance(
    out: &mut Vec<u8>,
    inv_world: &glam::Mat4,
    world_min: [f32; 3],
    world_max: [f32; 3],
    node_base: u32,
    tri_base: u32,
    emissive: [f32; 3],
) {
    for c in inv_world.to_cols_array() {
        out.extend_from_slice(&c.to_ne_bytes());
    }
    for v in emissive {
        out.extend_from_slice(&v.to_ne_bytes());
    }
    out.extend_from_slice(&0.0f32.to_ne_bytes());
    for v in world_min {
        out.extend_from_slice(&v.to_ne_bytes());
    }
    out.extend_from_slice(&node_base.to_ne_bytes());
    for v in world_max {
        out.extend_from_slice(&v.to_ne_bytes());
    }
    out.extend_from_slice(&tri_base.to_ne_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit quad's BLAS: one root (leaf) node containing both triangles,
    /// bounds match, triangles land expanded in the reordered array.
    #[test]
    fn quad_blas_shape() {
        let positions = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let indices = [0u32, 1, 2, 0, 2, 3];
        let (nodes, tris) = build_blas(&positions, &indices);
        assert_eq!(nodes.len(), NODE_BYTES, "2 tris <= leaf size: one node");
        assert_eq!(tris.len(), 2 * TRI_BYTES);
        let a = u32::from_ne_bytes(nodes[12..16].try_into().unwrap());
        let b = u32::from_ne_bytes(nodes[28..32].try_into().unwrap());
        assert_eq!(a, 0x8000_0000, "leaf flag, first tri 0");
        assert_eq!(b, 2, "two triangles");
        let mx = f32::from_ne_bytes(nodes[16..20].try_into().unwrap());
        assert_eq!(mx, 1.0, "root max.x");
    }

    /// Splitting engages above the leaf size and children partition the
    /// parent's triangles; every leaf's range lands inside the reordered
    /// triangle array.
    #[test]
    fn split_produces_consistent_tree() {
        // A strip of 64 tiny quads along +X.
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for i in 0..64 {
            let x = i as f32;
            let base = positions.len() as u32;
            positions.push([x, 0.0, 0.0]);
            positions.push([x + 0.9, 0.0, 0.0]);
            positions.push([x + 0.9, 1.0, 0.0]);
            positions.push([x, 1.0, 0.0]);
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        let (nodes, tris) = build_blas(&positions, &indices);
        let n_nodes = nodes.len() / NODE_BYTES;
        let n_tris = tris.len() / TRI_BYTES;
        assert_eq!(n_tris, 128);
        assert!(n_nodes > 1, "must have split");
        // Walk the tree: every leaf range must be in-bounds and disjointly
        // cover all 128 triangles.
        let node = |i: usize| -> ([f32; 3], u32, [f32; 3], u32) {
            let o = i * NODE_BYTES;
            let f =
                |k: usize| f32::from_ne_bytes(nodes[o + k * 4..o + k * 4 + 4].try_into().unwrap());
            let u =
                |k: usize| u32::from_ne_bytes(nodes[o + k * 4..o + k * 4 + 4].try_into().unwrap());
            ([f(0), f(1), f(2)], u(3), [f(4), f(5), f(6)], u(7))
        };
        let mut covered = [false; 128];
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let (_, a, _, b) = node(i);
            if a & 0x8000_0000 != 0 {
                let first = (a & 0x7fff_ffff) as usize;
                for c in covered.iter_mut().skip(first).take(b as usize) {
                    assert!(!*c, "leaf ranges must be disjoint");
                    *c = true;
                }
            } else {
                stack.push(a as usize);
                stack.push(b as usize);
            }
        }
        assert!(covered.iter().all(|&c| c), "leaves must cover every tri");
    }
}
