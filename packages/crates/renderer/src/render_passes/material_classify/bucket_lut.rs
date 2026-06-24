//! `shader_id → bucket_index` lookup table for the classify pass (§4a).
//!
//! Replaces the old O(buckets) per-pixel `shader_id == SHADER_ID_*`
//! if/else chain with a single O(1) storage load (`bucket_lut[raw_sid]`).
//! The id space is sparse (first-party `0..=4`, dynamic `10_000..` with
//! holes — see `materials/src/shader_id.rs`), so the array length is
//! `max_live_shader_id + 1`; the untouched `5..9_999` hole is never read
//! (classify only indexes ids that exist in `bucket_entries`) so it never
//! even enters cache. Access is warp-coherent (neighbouring pixels share a
//! material → same slot). At the 1024-material benchmark the table is
//! ≈ `11_024` u32 ≈ 44 KB — trivial, fully cache-resident.
//!
//! **Why this lives OUTSIDE `ClassifyBuffers`:** that struct reallocates
//! via `*self = Self::new(...)` on viewport resize, which would silently
//! wipe the LUT with no repopulate trigger (a resize does NOT fire
//! `relayout_bucket_buffers`). The LUT's lifecycle is bound to the bucket
//! *set*, not the viewport — it changes only when a registration /
//! unregistration changes `bucket_entries` — so it owns its own buffer +
//! capacity and is rebuilt only in `relayout_bucket_buffers`.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

use crate::dynamic_materials::BucketEntry;

/// Sentinel meaning "this shader_id maps to no live bucket". Mirrors the
/// WGSL `0xFFFFFFFFu` fall-through — the data-driven equivalent of the old
/// "no `if/else` arm matched, contribute no bucket bit" behaviour.
pub const BUCKET_LUT_NOT_FOUND: u32 = 0xFFFF_FFFF;

/// GPU storage buffer mapping `shader_id → bucket_index`, bound read-only
/// into the classify compute pass.
pub struct MaterialBucketLut {
    /// `array<u32>` storage buffer. `buffer[raw_sid]` is the bucket index
    /// (position in `bucket_entries`) or [`BUCKET_LUT_NOT_FOUND`].
    pub buffer: web_sys::GpuBuffer,
    /// Current backing-buffer capacity in u32 slots.
    capacity: u32,
}

impl MaterialBucketLut {
    /// Creates the LUT buffer for the given (sorted) bucket entries and
    /// uploads it. At boot this is seeded from `first_party_bucket_entries`
    /// so a scene that never registers a dynamic material still classifies
    /// its first-party buckets correctly.
    pub fn new(gpu: &AwsmRendererWebGpu, entries: &[BucketEntry]) -> Result<Self, AwsmCoreError> {
        let bytes = build_lut_bytes(entries);
        let capacity = (bytes.len() / 4) as u32;
        let buffer = create_buffer(gpu, capacity)?;
        gpu.write_buffer(&buffer, None, bytes.as_slice(), None, None)?;
        Ok(Self { buffer, capacity })
    }

    /// Rebuilds + uploads the LUT for the current bucket set. Grows
    /// (recreates) the backing buffer when the highest live shader_id
    /// climbs past the current capacity; shrinks are kept in-place (the
    /// stale tail is never indexed because classify only reads ids present
    /// in `bucket_entries`). Returns `true` if the buffer was recreated, so
    /// the caller rebuilds the classify bind group (its binding moved).
    pub fn ensure(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        entries: &[BucketEntry],
    ) -> Result<bool, AwsmCoreError> {
        let bytes = build_lut_bytes(entries);
        let needed = (bytes.len() / 4) as u32;
        let recreated = if needed > self.capacity {
            self.buffer = create_buffer(gpu, needed)?;
            self.capacity = needed;
            true
        } else {
            false
        };
        gpu.write_buffer(&self.buffer, None, bytes.as_slice(), None, None)?;
        Ok(recreated)
    }
}

/// Builds the LUT byte image: `(max_shader_id + 1)` u32 slots, all
/// pre-filled with [`BUCKET_LUT_NOT_FOUND`] (`0xFF` bytes ⇒ `0xFFFFFFFF`),
/// then each live `shader_id` slot stamped with its bucket index (its
/// position in the sorted `bucket_entries` list). Little-endian to match
/// the rest of the classify buffers (`to_ne_bytes`, wasm = LE = GPU).
fn build_lut_bytes(entries: &[BucketEntry]) -> Vec<u8> {
    let max_sid = entries
        .iter()
        .map(|e| e.shader_id.as_u32())
        .max()
        .unwrap_or(0);
    let len = max_sid as usize + 1;
    // 0xFF bytes == 0xFFFFFFFF == BUCKET_LUT_NOT_FOUND for every slot.
    let mut bytes = vec![0xFFu8; len * 4];
    for (bucket_index, entry) in entries.iter().enumerate() {
        let slot = entry.shader_id.as_u32() as usize;
        let base = slot * 4;
        bytes[base..base + 4].copy_from_slice(&(bucket_index as u32).to_ne_bytes());
    }
    bytes
}

fn create_buffer(
    gpu: &AwsmRendererWebGpu,
    capacity_u32: u32,
) -> Result<web_sys::GpuBuffer, AwsmCoreError> {
    gpu.create_buffer(
        &BufferDescriptor::new(
            Some("MaterialClassifyBucketLut"),
            (capacity_u32.max(1) * 4) as usize,
            BufferUsage::new().with_storage().with_copy_dst(),
        )
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_materials::{first_party_bucket_entries, BucketEntry, ShadingBase};
    use awsm_renderer_materials::MaterialShaderId;

    fn u32s(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    fn entry(shader_id: MaterialShaderId, name: &str) -> BucketEntry {
        BucketEntry {
            shader_id,
            base: ShadingBase::Custom,
            pbr_features: 0,
            name: name.to_string(),
        }
    }

    #[test]
    fn lut_maps_sparse_holey_id_list_to_bucket_index() {
        // Mirrors the real sparse layout: first-party 0,3 + dynamic
        // 10000,10005 with holes. bucket_index = position in the list.
        let entries = vec![
            entry(MaterialShaderId::SKYBOX, "skybox"),
            entry(MaterialShaderId::TOON, "toon"),
            entry(MaterialShaderId::from_dynamic_raw(10_000), "a"),
            entry(MaterialShaderId::from_dynamic_raw(10_005), "b"),
        ];
        let lut = u32s(&build_lut_bytes(&entries));
        assert_eq!(lut.len(), 10_006); // max_sid (10005) + 1
        assert_eq!(lut[0], 0);
        assert_eq!(lut[3], 1);
        assert_eq!(lut[10_000], 2);
        assert_eq!(lut[10_005], 3);
        // Every hole reads NOT_FOUND.
        assert_eq!(lut[1], BUCKET_LUT_NOT_FOUND);
        assert_eq!(lut[2], BUCKET_LUT_NOT_FOUND);
        assert_eq!(lut[4], BUCKET_LUT_NOT_FOUND);
        assert_eq!(lut[9_999], BUCKET_LUT_NOT_FOUND);
        assert_eq!(lut[10_001], BUCKET_LUT_NOT_FOUND);
        // Exactly `bucket_count` non-sentinel entries (LUT integrity).
        let mapped = lut.iter().filter(|&&v| v != BUCKET_LUT_NOT_FOUND).count();
        assert_eq!(mapped, entries.len());
    }

    #[test]
    fn lut_seeds_from_first_party_entries() {
        let entries = first_party_bucket_entries();
        let lut = u32s(&build_lut_bytes(&entries));
        // Skybox is id 0 → bucket 0 (sorts first).
        assert_eq!(lut[0], 0);
        let mapped = lut.iter().filter(|&&v| v != BUCKET_LUT_NOT_FOUND).count();
        assert_eq!(mapped, entries.len());
    }
}
