//! Last-frame pixel coverage per mesh.
//!
//! The GPU coverage pass (a tiny compute pass that does one atomic-add
//! per pixel into `mesh_pixel_counts[meta_index]`) is the producer;
//! this module is the **consumer** — CPU-side state that downstream
//! paths (skinning-skip, material-LOD) consult.
//!
//! When the producer is disabled (`features.coverage_lod == false`)
//! the table stays empty and consumers fall through to their
//! conservative "always update" behaviour. The producer calls
//! `MeshCoverage::ingest` once per frame with the
//! `{mesh_key → pixel_count}` snapshot read back from the GPU buffer.

use slotmap::SecondaryMap;

use crate::meshes::MeshKey;

/// Per-frame pixel coverage table — read by the skinning gate and
/// the material-LOD path.
#[derive(Default)]
pub struct MeshCoverage {
    counts: SecondaryMap<MeshKey, u32>,
    frame_when_populated: u64,
}

impl MeshCoverage {
    /// Replace the table with this frame's GPU readback. `frame_index`
    /// is the renderer's monotonic counter — consumers can detect
    /// stale data by comparing.
    pub fn ingest(&mut self, snapshot: impl IntoIterator<Item = (MeshKey, u32)>, frame_index: u64) {
        self.counts.clear();
        for (key, count) in snapshot {
            self.counts.insert(key, count);
        }
        self.frame_when_populated = frame_index;
    }

    /// Last frame's pixel coverage for the mesh, or `None` if the GPU
    /// pass hasn't populated this entry yet.
    pub fn pixel_count(&self, mesh_key: MeshKey) -> Option<u32> {
        self.counts.get(mesh_key).copied()
    }

    /// True when the mesh contributed at least one pixel last frame.
    /// `None` (no readback yet) is treated as visible — conservative.
    pub fn is_visible_last_frame(&self, mesh_key: MeshKey) -> bool {
        self.pixel_count(mesh_key).map(|c| c > 0).unwrap_or(true)
    }

    /// True when this mesh's coverage is below `threshold` pixels —
    /// the signal a cheap-material LOD path uses to swap to a cheaper
    /// material variant. `None` (no readback yet) is treated as above
    /// threshold so the expensive variant runs by default.
    pub fn is_below_threshold(&self, mesh_key: MeshKey, threshold: u32) -> bool {
        self.pixel_count(mesh_key)
            .map(|c| c < threshold)
            .unwrap_or(false)
    }

    /// Frame index of the most recent `ingest`. Comparable to the
    /// renderer's `frame_index` — consumers gate stale data this way.
    pub fn frame_when_populated(&self) -> u64 {
        self.frame_when_populated
    }

    /// True when no coverage data has been ingested yet (fresh boot,
    /// or the GPU compute pass is disabled).
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// Total entries in the table — useful for instrumentation /
    /// debug overlays.
    pub fn len(&self) -> usize {
        self.counts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slotmap::{DenseSlotMap, Key};

    #[test]
    fn ingest_populates_table() {
        let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
        let k = keys.insert(());
        let mut coverage = MeshCoverage::default();
        coverage.ingest([(k, 42)], 7);
        assert_eq!(coverage.pixel_count(k), Some(42));
        assert_eq!(coverage.frame_when_populated(), 7);
        // Sanity: KeyData is comparable.
        let _ = k.data();
    }

    #[test]
    fn missing_entry_is_conservatively_visible() {
        let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
        let k = keys.insert(());
        let coverage = MeshCoverage::default();
        assert!(coverage.is_visible_last_frame(k));
    }

    #[test]
    fn zero_coverage_is_not_visible() {
        let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
        let k = keys.insert(());
        let mut coverage = MeshCoverage::default();
        coverage.ingest([(k, 0)], 1);
        assert!(!coverage.is_visible_last_frame(k));
    }

    #[test]
    fn threshold_check() {
        let mut keys: DenseSlotMap<MeshKey, ()> = DenseSlotMap::with_key();
        let k = keys.insert(());
        let mut coverage = MeshCoverage::default();
        coverage.ingest([(k, 50)], 1);
        assert!(coverage.is_below_threshold(k, 100));
        assert!(!coverage.is_below_threshold(k, 25));
    }
}
