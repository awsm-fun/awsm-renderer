//! GPU storage for the decal subsystem — per-decal data + uniform
//! count, packed into a single storage buffer the
//! [`MaterialDecalRenderPass`](crate::render_passes::material_decal::render_pass::MaterialDecalRenderPass)
//! reads per-pixel.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};
use slotmap::SlotMap;

use crate::{
    bind_groups::BindGroups,
    decals::data::{Decal, DecalBlendMode, DecalKey},
};

/// Hard cap on simultaneously active decals per frame. The GPU
/// storage is sized for this count; raising it costs `MAX_DECAL_COUNT
/// × 96 B = 12 KB` at 128, which is still trivial. Picked so a
/// representative scene (bullet holes, posters, weathering) doesn't
/// hit the cap; bump if needed.
pub const MAX_DECAL_COUNT: u32 = 128;

/// Bytes per packed decal on the GPU. Layout (vec4-aligned for
/// std430 storage compatibility):
/// ```text
///   0..64   inverse_transform: mat4x4<f32>
///  64..68   texture_index: u32
///  68..72   alpha: f32
///  72..76   blend_mode: u32
///  76..80   _pad: u32 (vec4 alignment)
/// ```
pub const DECAL_STRIDE_BYTES: usize = 80;

/// Bytes for the buffer header (decal count + padding to vec4
/// alignment, then the array). `count` lives at offset 0; the per-
/// decal array starts at `HEADER_BYTES`.
pub const DECAL_HEADER_BYTES: usize = 16;

const TOTAL_BUFFER_BYTES: usize = DECAL_HEADER_BYTES + DECAL_STRIDE_BYTES * MAX_DECAL_COUNT as usize;

/// Runtime decal collection.
///
/// Owns the slotmap of authored decals plus the single GPU storage
/// buffer the material_decal pass binds. CPU scratch is staged to
/// `staging_bytes` once per frame and uploaded via `write_buffer` —
/// matches the [`Lights`](crate::lights::Lights) write-once-per-frame
/// shape.
pub struct Decals {
    decals: SlotMap<DecalKey, Decal>,
    /// Insertion order — used to rebuild the GPU buffer
    /// deterministically. `SlotMap`'s iteration order is stable per
    /// inserted entry but the index of an entry shifts on remove;
    /// the keyed order here means the buffer slot index of a given
    /// `DecalKey` is whatever its position in this `Vec` is, which
    /// the rendering side doesn't need (decals are iterated in bulk
    /// per pixel).
    order: Vec<DecalKey>,
    gpu_buffer: web_sys::GpuBuffer,
    /// CPU staging buffer reused across frames. Sized for
    /// `MAX_DECAL_COUNT` decals plus the header.
    staging_bytes: Vec<u8>,
    /// Set when any decal added / updated / removed; cleared on the
    /// next `write_gpu` call.
    dirty: bool,
}

impl Decals {
    /// Creates an empty decal collection backed by a freshly-allocated
    /// GPU storage buffer sized for [`MAX_DECAL_COUNT`].
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self, AwsmCoreError> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Decals"),
                TOTAL_BUFFER_BYTES,
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;
        Ok(Self {
            decals: SlotMap::with_key(),
            order: Vec::new(),
            gpu_buffer,
            staging_bytes: vec![0u8; TOTAL_BUFFER_BYTES],
            dirty: true,
        })
    }

    /// Returns the GPU storage buffer — bound at the material_decal
    /// pass's main bind group as read-only storage.
    pub fn gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.gpu_buffer
    }

    /// Number of decals currently active.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// True when the decal table is empty — the render path uses
    /// this to skip the decal compute pass entirely on no-decal
    /// frames.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Inserts a decal and returns its handle. Marks the GPU buffer
    /// dirty for the next [`Self::write_gpu`].
    pub fn insert(&mut self, decal: Decal) -> Result<DecalKey, AwsmDecalError> {
        if self.order.len() as u32 >= MAX_DECAL_COUNT {
            return Err(AwsmDecalError::TooManyDecals(MAX_DECAL_COUNT));
        }
        let key = self.decals.insert(decal);
        self.order.push(key);
        self.dirty = true;
        Ok(key)
    }

    /// Read-only access for inspectors / debug overlays.
    pub fn get(&self, key: DecalKey) -> Option<&Decal> {
        self.decals.get(key)
    }

    /// Mutates a decal in place. The closure receives a `&mut Decal`
    /// — re-derive `inverse_transform` + `world_aabb` if the caller
    /// changes `transform` (use [`Decal::new`] as the canonical
    /// constructor to avoid manual derivation).
    pub fn update(&mut self, key: DecalKey, f: impl FnOnce(&mut Decal)) {
        if let Some(d) = self.decals.get_mut(key) {
            f(d);
            self.dirty = true;
        }
    }

    /// Removes the decal. Returns `true` if it existed.
    pub fn remove(&mut self, key: DecalKey) -> bool {
        if self.decals.remove(key).is_some() {
            self.order.retain(|k| *k != key);
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Iterates active decals — used by the render pass for
    /// per-frame data uploads or by debug overlays.
    pub fn iter(&self) -> impl Iterator<Item = (DecalKey, &Decal)> + '_ {
        self.order.iter().filter_map(|k| {
            self.decals.get(*k).map(|d| (*k, d))
        })
    }

    /// Per-frame GPU upload. Walks active decals in `order`, packs
    /// each to its strided slot, and writes the resulting prefix to
    /// the GPU. Skipped when nothing changed since last frame.
    pub fn write_gpu(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<(), AwsmCoreError> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;

        let count = self.order.len() as u32;
        // Header: count at offset 0; the rest of the header bytes
        // stay zeroed (vec4 alignment padding).
        self.staging_bytes[0..4].copy_from_slice(&count.to_le_bytes());
        for i in 4..DECAL_HEADER_BYTES {
            self.staging_bytes[i] = 0;
        }

        for (slot, key) in self.order.iter().enumerate() {
            let Some(decal) = self.decals.get(*key) else {
                continue;
            };
            let base = DECAL_HEADER_BYTES + slot * DECAL_STRIDE_BYTES;
            let cols = decal.inverse_transform.to_cols_array();
            let bytes: &[u8] =
                unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
            self.staging_bytes[base..base + 64].copy_from_slice(bytes);
            self.staging_bytes[base + 64..base + 68]
                .copy_from_slice(&decal.texture_index.to_le_bytes());
            self.staging_bytes[base + 68..base + 72]
                .copy_from_slice(&decal.alpha.to_le_bytes());
            let blend_mode_u32 = match decal.blend_mode {
                DecalBlendMode::AlphaBlend => 0u32,
            };
            self.staging_bytes[base + 72..base + 76]
                .copy_from_slice(&blend_mode_u32.to_le_bytes());
            // Trailing 4 bytes stay zero — vec4 alignment pad.
            self.staging_bytes[base + 76..base + 80].copy_from_slice(&[0u8; 4]);
        }

        let used = DECAL_HEADER_BYTES + self.order.len() * DECAL_STRIDE_BYTES;
        // Zero the tail so a previously-larger decal set doesn't
        // leave stale bytes the shader walks past `count`.
        for i in used..self.staging_bytes.len() {
            self.staging_bytes[i] = 0;
        }
        gpu.write_buffer(
            &self.gpu_buffer,
            None,
            self.staging_bytes.as_slice(),
            None,
            None,
        )?;
        // The decal pass's bind group binds this storage buffer; the
        // buffer handle is stable across frames (we re-use the same
        // GpuBuffer) so no resize event is needed in v1. If the cap
        // ever becomes dynamic, mark a `DecalsResize` event here.
        let _ = bind_groups;
        Ok(())
    }
}

/// Decal-subsystem errors.
#[derive(Debug, thiserror::Error)]
pub enum AwsmDecalError {
    #[error("too many decals (cap: {0})")]
    TooManyDecals(u32),
    /// Returned by `insert_decal` when the renderer was built with
    /// the `decals` feature gate off (plan §16.F). The per-decal GPU
    /// buffer + classify + shading + composite passes are not
    /// allocated in that mode, so any decal insertion would be a
    /// silent no-op without this error.
    #[error("decals feature is not enabled (RendererFeatures::decals = false)")]
    FeatureNotEnabled,
}
