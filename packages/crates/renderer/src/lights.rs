//! Lighting data and GPU uploads.

pub mod ibl;

use std::sync::LazyLock;

use awsm_renderer_core::{
    brdf_lut::generate::BrdfLut,
    buffers::{BufferDescriptor, BufferUsage},
    cubemap::{CubemapBytesLayout, CubemapFace},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};
use slotmap::{new_key_type, SecondaryMap, SlotMap};
use thiserror::Error;

use crate::{
    bind_groups::{BindGroupCreate, BindGroups},
    lights::ibl::Ibl,
    transforms::TransformKey,
    AwsmRenderer, AwsmRendererLogging,
};

// Lights live in a uniform buffer: the access pattern is the
// canonical "every pixel of a wavefront reads the same light index in
// lockstep", which is exactly what uniform memory + constant cache are
// tuned for. Practical light count is bounded by the 64 KB uniform-max
// limit divided by `Light::BYTE_SIZE` = 1024 lights, far above any
// realistic scene's total light count.
pub const MAX_PUNCTUAL_LIGHTS: usize = 1024;
static PUNCTUAL_BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_uniform().with_copy_dst());

static INFO_BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_uniform().with_copy_dst());

impl AwsmRenderer {
    /// Sets the BRDF LUT texture used for IBL.
    pub fn set_brdf_lut(&mut self, brdf_lut: BrdfLut) {
        self.lights.brdf_lut = brdf_lut;
        self.mark_brdf_lut_real();
        self.bind_groups
            .mark_create(BindGroupCreate::BrdfLutTextures);
    }
    /// Sets image-based lighting textures.
    pub fn set_ibl(&mut self, ibl: Ibl) {
        self.lights.ibl = ibl;
        self.mark_ibl_real();
        self.bind_groups.mark_create(BindGroupCreate::IblTextures);
        self.lights.lighting_info_gpu_dirty = true;
    }

    /// Updates one IBL `prefiltered_env` cubemap face in-place.
    pub fn update_ibl_prefiltered_env_face(
        &self,
        face: CubemapFace,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
        layout: CubemapBytesLayout,
    ) -> crate::error::Result<()> {
        self.update_cubemap_texture_face(
            self.lights.ibl.prefiltered_env.texture_key,
            face,
            mip_level,
            width,
            height,
            data,
            layout,
        )
    }

    /// Updates all six IBL `prefiltered_env` cubemap faces in-place.
    pub fn update_ibl_prefiltered_env_all_faces(
        &self,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
        layout: CubemapBytesLayout,
    ) -> crate::error::Result<()> {
        self.update_cubemap_texture_all_faces(
            self.lights.ibl.prefiltered_env.texture_key,
            mip_level,
            width,
            height,
            data,
            layout,
        )
    }

    /// Regenerates IBL `prefiltered_env` mipmaps from mip level 0.
    pub async fn regenerate_ibl_prefiltered_env_mipmaps(&self) -> crate::error::Result<()> {
        self.regenerate_cubemap_texture_mipmaps(
            self.lights.ibl.prefiltered_env.texture_key,
            self.lights.ibl.prefiltered_env.mip_count,
        )
        .await
    }

    /// Updates one IBL irradiance cubemap face in-place.
    pub fn update_ibl_irradiance_face(
        &self,
        face: CubemapFace,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
        layout: CubemapBytesLayout,
    ) -> crate::error::Result<()> {
        self.update_cubemap_texture_face(
            self.lights.ibl.irradiance.texture_key,
            face,
            mip_level,
            width,
            height,
            data,
            layout,
        )
    }

    /// Updates all six IBL irradiance cubemap faces in-place.
    pub fn update_ibl_irradiance_all_faces(
        &self,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
        layout: CubemapBytesLayout,
    ) -> crate::error::Result<()> {
        self.update_cubemap_texture_all_faces(
            self.lights.ibl.irradiance.texture_key,
            mip_level,
            width,
            height,
            data,
            layout,
        )
    }

    /// Regenerates IBL irradiance mipmaps from mip level 0.
    pub async fn regenerate_ibl_irradiance_mipmaps(&self) -> crate::error::Result<()> {
        self.regenerate_cubemap_texture_mipmaps(
            self.lights.ibl.irradiance.texture_key,
            self.lights.ibl.irradiance.mip_count,
        )
        .await
    }
}

/// Light storage and GPU buffers.
pub struct Lights {
    pub gpu_punctual_buffer: web_sys::GpuBuffer,
    pub gpu_info_buffer: web_sys::GpuBuffer,
    pub ibl: Ibl,
    pub brdf_lut: BrdfLut,
    lights: SlotMap<LightKey, Light>,
    // Optional binding from a light to the transform node that drives its
    // world pose. glTF lights attached to animated nodes (e.g. fireflies)
    // bake their pose at load, but their node transform keeps animating —
    // `update_from_transforms` re-derives position/direction each frame for
    // any light listed here. Lights without an entry (procedural, manually
    // positioned) are left untouched.
    node_transforms: SecondaryMap<LightKey, TransformKey>,
    // We do not use DynamicUniformBuffer here because we need dense sequential access in the gpu
    // not stable offsets per-key that DynamicUniformBuffer provides (with holes, etc)
    // instead, we rebuild a fresh Vec<u8> when the gpu is dirty.
    //
    // The buffer is allocated once at the uniform-max size
    // (`MAX_PUNCTUAL_LIGHTS * Light::BYTE_SIZE` = 64 KB) and never
    // resized — uniform-buffer bindings must reference a buffer that's
    // at least as large as the declared binding range, and changing the
    // size at runtime would force a bind-group recreate every time the
    // light count changes. The wasted memory at low light counts (e.g.
    // 64 KB for an 8-light scene) is the price for stable bindings.
    punctual_gpu_dirty: bool,
    lighting_info_gpu_dirty: bool,
    punctual_uploader: crate::buffer::mapped_uploader::MappedUploader,
    info_uploader: crate::buffer::mapped_uploader::MappedUploader,
    /// Reused staging bytes for the punctual-light pack in `write_gpu`.
    /// Rebuilt (cleared, capacity kept) every dirty frame — a moving light
    /// dirties every frame, so a fresh `Vec` here was a per-frame heap
    /// allocation.
    punctual_scratch: Vec<u8>,
}

impl Lights {
    /// Size in bytes for a single punctual light.
    pub const PUNCTUAL_LIGHT_SIZE: usize = 64;
    /// Max directional lights packed into the info uniform's directional
    /// index list. Directional lights are rare (sun / moon / fill); any
    /// beyond this are simply dropped from the bounded directional walk.
    pub const MAX_DIRECTIONAL_LIGHTS: usize = 8;
    /// Size in bytes for the lighting info block. Layout (matches the
    /// `LightsInfoPacked` WGSL struct):
    ///   data: vec4<u32> (16) — x=n_lights, y/z=IBL mip counts, w=n_directional
    ///   directional: array<vec4<u32>, 2> (32) — packed indices of the ≤8 directionals
    pub const INFO_SIZE: usize = 48;

    /// Creates light buffers and initializes IBL state.
    pub fn new(gpu: &AwsmRendererWebGpu, ibl: Ibl, brdf_lut: BrdfLut) -> Result<Self> {
        // Fixed-size uniform allocation (see field doc). 64 KB total.
        let punctual_gpu_size = MAX_PUNCTUAL_LIGHTS * Self::PUNCTUAL_LIGHT_SIZE;

        let gpu_punctual_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Punctual Lights"),
                punctual_gpu_size,
                *PUNCTUAL_BUFFER_USAGE,
            )
            .into(),
        )?;

        let gpu_info_buffer = gpu.create_buffer(
            &BufferDescriptor::new(Some("Lights Info"), Self::INFO_SIZE, *INFO_BUFFER_USAGE).into(),
        )?;

        Ok(Lights {
            lights: SlotMap::with_key(),
            node_transforms: SecondaryMap::new(),
            ibl,
            brdf_lut,
            punctual_gpu_dirty: true,
            lighting_info_gpu_dirty: true,
            gpu_punctual_buffer,
            gpu_info_buffer,
            punctual_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "Punctual Lights",
            ),
            info_uploader: crate::buffer::mapped_uploader::MappedUploader::new("Lights Info"),
            punctual_scratch: Vec::new(),
        })
    }

    /// Mapped-ring upload telemetry for the lights buffers.
    /// Aggregates punctual + info uploaders.
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        let mut s = self.punctual_uploader.stats();
        let b = self.info_uploader.stats();
        s.peak_ring_depth_used = s.peak_ring_depth_used.max(b.peak_ring_depth_used);
        s.fallback_count += b.fallback_count;
        s.map_async_wait_ms += b.map_async_wait_ms;
        s.bytes_uploaded_via_ring += b.bytes_uploaded_via_ring;
        s.bytes_uploaded_via_fallback += b.bytes_uploaded_via_fallback;
        s.bytes_uploaded_via_writebuffer += b.bytes_uploaded_via_writebuffer;
        s.resize_count += b.resize_count;
        s
    }

    /// Removes all lights. `pub(crate)` for the same reason as
    /// [`Self::remove`] — external callers must go through
    /// [`AwsmRenderer::clear_lights`](crate::AwsmRenderer::clear_lights)
    /// so the shadow subsystem can drop every per-light slot /
    /// throttle / params entry in lockstep.
    pub(crate) fn clear(&mut self) {
        self.lights.clear();
        self.node_transforms.clear();
        self.punctual_gpu_dirty = true;
        self.lighting_info_gpu_dirty = true;
    }

    /// Inserts a light and returns its key. `pub(crate)` — external
    /// callers must go through
    /// [`AwsmRenderer::insert_light`](crate::AwsmRenderer::insert_light)
    /// so the per-light shadow params can be registered in lockstep.
    /// The coordinated API mirrors
    /// [`AwsmRenderer::remove_light`](crate::AwsmRenderer::remove_light) /
    /// [`AwsmRenderer::clear_lights`](crate::AwsmRenderer::clear_lights);
    /// keeping both sides of the lifecycle on one entry point makes
    /// it impossible to desynchronise the lights buffer and the
    /// shadow subsystem.
    pub(crate) fn insert(&mut self, light: Light) -> Result<LightKey> {
        let key = self.lights.insert(light.clone());

        self.punctual_gpu_dirty = true;
        self.lighting_info_gpu_dirty = true;
        Ok(key)
    }

    /// Removes a light by key. `pub(crate)` so callers can't bypass
    /// the coordinated shadow cleanup — every external removal must
    /// go through [`AwsmRenderer::remove_light`](crate::AwsmRenderer::remove_light),
    /// which calls `Shadows::on_light_removed` first so the cube-pool
    /// slot, the throttle history, and the per-light shadow params
    /// don't leak when the underlying light goes away.
    pub(crate) fn remove(&mut self, key: LightKey) {
        self.lights.remove(key);
        self.node_transforms.remove(key);
        self.punctual_gpu_dirty = true;
        self.lighting_info_gpu_dirty = true;
    }

    /// Binds a light to the transform node that drives its world pose, so
    /// [`Self::update_from_transforms`] re-derives the light's position
    /// (and direction, for spot/directional) whenever that node's world
    /// matrix changes — e.g. a glTF light parented to an animated node.
    pub fn bind_transform(&mut self, key: LightKey, transform_key: TransformKey) {
        if self.lights.contains_key(key) {
            self.node_transforms.insert(key, transform_key);
        }
    }

    /// Re-derives the world pose of every transform-bound light whose node
    /// moved this frame. `dirty` is the per-frame dirty-transform set from
    /// [`Transforms::take_dirty_meshes`](crate::transforms::Transforms::take_dirty_meshes),
    /// mapping each updated `TransformKey` to its fresh world matrix.
    /// Marks the punctual buffer dirty if any light changed so `write_gpu`
    /// re-uploads it. Cheap: O(transform-bound lights), and a no-op when
    /// nothing moved.
    pub fn update_from_transforms(
        &mut self,
        dirty: &std::collections::HashMap<TransformKey, glam::Mat4>,
    ) {
        if dirty.is_empty() || self.node_transforms.is_empty() {
            return;
        }

        let mut changed = false;
        for (light_key, transform_key) in self.node_transforms.iter() {
            if let Some(world) = dirty.get(transform_key) {
                if let Some(light) = self.lights.get_mut(light_key) {
                    light.apply_world_transform(world);
                    changed = true;
                }
            }
        }

        if changed {
            self.punctual_gpu_dirty = true;
        }
    }

    /// Updates a light in place. **Not safe for `Light` variant
    /// changes** (Directional ↔ Point ↔ Spot) — those would desync the
    /// shadow subsystem's cube-pool and atlas allocations. Use
    /// [`AwsmRenderer::update_light`](crate::AwsmRenderer::update_light)
    /// for any mutation that might flip the variant.
    pub fn update(&mut self, key: LightKey, f: impl FnOnce(&mut Light)) {
        if let Some(light) = self.lights.get_mut(key) {
            f(light);
            self.punctual_gpu_dirty = true;
        }
    }

    /// Force the next `write_gpu` to repack the punctual storage
    /// buffer. Lights doesn't observe shadow state — the descriptor
    /// index that lands in `LightPacked.row4.z` is resolved at pack
    /// time via the `shadow_index_for` callback — so when the shadow
    /// subsystem changes a light's `descriptor_base` (e.g. shadows
    /// toggled on/off, hardness changed) it must call this to
    /// invalidate the cached packing.
    pub fn mark_punctual_dirty(&mut self) {
        self.punctual_gpu_dirty = true;
    }

    /// Returns the light associated with a key, or `None` if the key
    /// is unknown.
    pub fn get(&self, key: LightKey) -> Option<&Light> {
        self.lights.get(key)
    }

    /// Iterates every active punctual light (point + spot — directional
    /// lights have infinite bounds and are excluded). The per-mesh
    /// light-list build path consumes this.
    pub fn iter_active_punctual(&self) -> impl Iterator<Item = (LightKey, &Light)> {
        self.lights
            .iter()
            .filter(|(_, light)| matches!(light, Light::Point { .. } | Light::Spot { .. }))
    }

    /// Iterate every directional light. Directional lights bypass the
    /// per-mesh slice (they affect every mesh) and live in a small
    /// global prefix that the shader walks unconditionally.
    pub fn iter_directional(&self) -> impl Iterator<Item = (LightKey, &Light)> {
        self.lights
            .iter()
            .filter(|(_, light)| matches!(light, Light::Directional { .. }))
    }

    /// Iterate every light, regardless of kind.
    pub fn iter(&self) -> impl Iterator<Item = (LightKey, &Light)> {
        self.lights.iter()
    }

    /// Total number of lights (any kind).
    pub fn len(&self) -> usize {
        self.lights.len()
    }

    /// Whether there are any lights of any kind.
    pub fn is_empty(&self) -> bool {
        self.lights.is_empty()
    }

    /// Stable index (`0..len()`) of a light within `self.lights.iter()`.
    /// Matches the order `write_gpu` packs lights into the storage
    /// buffer — the per-mesh slice's `mesh_light_indices[i]` reads this
    /// to point into the packed light data.
    pub fn index_of(&self, key: LightKey) -> Option<u32> {
        self.lights
            .iter()
            .position(|(k, _)| k == key)
            .map(|i| i as u32)
    }

    /// Writes lighting buffers to the GPU if dirty.
    ///
    /// `shadow_index_for` resolves each light's shadow descriptor
    /// index — supplied by `Shadows` so the GPU-side `LightPacked`
    /// row 4 carries the index alongside the kind / outer-cone bytes.
    /// Pass `|_| crate::shadows::SHADOW_INDEX_NONE` to disable shadow
    /// indexing entirely.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
        shadow_index_for: impl Fn(LightKey) -> u32,
    ) -> Result<()> {
        if self.punctual_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(
                    tracing::span!(
                        tracing::Level::INFO,
                        "Punctual Lights Uniform Buffer GPU write"
                    )
                    .entered(),
                )
            } else {
                None
            };
            // Suppress the unused-bind-groups warning at this site —
            // we used to mark `LightsResize` here when the buffer
            // changed size. Now the buffer is fixed at MAX_PUNCTUAL_LIGHTS
            // so there's never a resize to broadcast.
            let _ = bind_groups;

            if self.lights.len() > MAX_PUNCTUAL_LIGHTS {
                tracing::warn!(
                    "{} lights exceeds MAX_PUNCTUAL_LIGHTS ({MAX_PUNCTUAL_LIGHTS}); trailing lights will be dropped this frame",
                    self.lights.len(),
                );
            }

            self.punctual_scratch.clear();
            for (key, light) in self.lights.iter().take(MAX_PUNCTUAL_LIGHTS) {
                self.punctual_scratch
                    .extend_from_slice(&light.storage_buffer_data(shadow_index_for(key)));
            }
            let punctual_light_buffer = &self.punctual_scratch;

            if !punctual_light_buffer.is_empty() {
                // The punctual buffer is fixed-size (MAX_PUNCTUAL_LIGHTS *
                // PUNCTUAL_LIGHT_SIZE). We upload only the prefix that
                // holds the live lights — the rest of the buffer stays
                // at whatever its last contents were (the shader reads
                // up to `info.light_count` so the tail is unobserved).
                let n = punctual_light_buffer.len();
                let buffer_size = MAX_PUNCTUAL_LIGHTS * Self::PUNCTUAL_LIGHT_SIZE;
                self.punctual_uploader.write_dirty_ranges(
                    gpu,
                    &self.gpu_punctual_buffer,
                    buffer_size,
                    punctual_light_buffer.as_slice(),
                    &[(0, n)],
                )?;
            }

            self.punctual_gpu_dirty = false;
        }

        if self.lighting_info_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Lighting Info GPU write").entered())
            } else {
                None
            };

            // Fixed 48-byte block — stack array, no per-frame heap allocation.
            let mut data = [0u8; Self::INFO_SIZE];
            data[0..4].copy_from_slice(&(self.lights.len() as u32).to_ne_bytes());
            data[4..8].copy_from_slice(&self.ibl.prefiltered_env.mip_count.to_ne_bytes());
            data[8..12].copy_from_slice(&self.ibl.irradiance.mip_count.to_ne_bytes());

            // Directional index list for the bounded per-pixel directional
            // walk. `data.w` = count; `[16..48]` = up to 8 packed-array
            // indices. The index is the light's position in the same
            // `iter().take(MAX_PUNCTUAL_LIGHTS)` order used to pack the
            // storage buffer, so it matches `get_light(i)` on the GPU.
            let mut n_directional: u32 = 0;
            for (i, (_key, light)) in self.lights.iter().take(MAX_PUNCTUAL_LIGHTS).enumerate() {
                if matches!(light, Light::Directional { .. })
                    && (n_directional as usize) < Self::MAX_DIRECTIONAL_LIGHTS
                {
                    let off = 16 + n_directional as usize * 4;
                    data[off..off + 4].copy_from_slice(&(i as u32).to_ne_bytes());
                    n_directional += 1;
                }
            }
            data[12..16].copy_from_slice(&n_directional.to_ne_bytes());

            self.info_uploader.write_dirty_ranges(
                gpu,
                &self.gpu_info_buffer,
                Self::INFO_SIZE,
                &data,
                &[(0, Self::INFO_SIZE)],
            )?;

            self.lighting_info_gpu_dirty = false;
        }
        Ok(())
    }
}

/// Punctual light definitions.
#[derive(Debug, Clone)]
pub enum Light {
    Directional {
        color: [f32; 3],
        intensity: f32,
        direction: [f32; 3],
    },
    Point {
        color: [f32; 3],
        intensity: f32,
        position: [f32; 3],
        range: f32,
    },
    Spot {
        color: [f32; 3],
        intensity: f32,
        position: [f32; 3],
        direction: [f32; 3],
        range: f32,
        inner_angle: f32,
        outer_angle: f32,
    },
}

impl Light {
    /// Packed byte size for a light in the storage buffer.
    pub const BYTE_SIZE: usize = 64;

    /// Re-derives this light's world-space pose from a node world matrix
    /// (the glTF/transform convention: position is the translation column,
    /// direction is the local `-Z` axis rotated into world space). Point
    /// lights only carry a position; directional lights only a direction;
    /// spot lights carry both. Color / intensity / range / cone angles are
    /// intrinsic and left untouched.
    pub fn apply_world_transform(&mut self, world: &glam::Mat4) {
        use glam::{Vec3, Vec4Swizzles};

        let position = world.w_axis.xyz().to_array();
        let world_forward = world.transform_vector3(Vec3::new(0.0, 0.0, -1.0));
        let direction = if world_forward.length_squared() > 1e-12 {
            world_forward.normalize().to_array()
        } else {
            [0.0, 0.0, -1.0]
        };

        match self {
            Light::Directional { direction: d, .. } => *d = direction,
            Light::Point { position: p, .. } => *p = position,
            Light::Spot {
                position: p,
                direction: d,
                ..
            } => {
                *p = position;
                *d = direction;
            }
        }
    }

    /// Culling radius below which a point/spot light's inverse-square
    /// contribution is treated as negligible, used to bound the influence
    /// AABB for *unlimited-range* lights (glTF lights that omit `range`).
    /// Solving `intensity / d^2 = CUTOFF` for `d` gives the distance at
    /// which radiance drops under this threshold; the shader itself keeps
    /// applying pure inverse-square with no cutoff, so this only affects
    /// the light-bucket overlap test, never the shaded result.
    const UNLIMITED_RANGE_CUTOFF: f32 = 1e-3;

    /// Influence radius used for the culling AABB. For a finite glTF
    /// `range` that value is used directly; for an unlimited-range light
    /// (`range <= 0`) a finite radius is derived from intensity so the
    /// light still gets bucketed onto the meshes it actually reaches —
    /// otherwise a zero `range` collapses the AABB to a point and the
    /// light is culled from (almost) everything.
    pub(crate) fn influence_radius(intensity: f32, range: f32) -> f32 {
        if range > 0.0 {
            range
        } else {
            (intensity.max(0.0) / Self::UNLIMITED_RANGE_CUTOFF).sqrt()
        }
    }

    /// Conservative world-space AABB for this light's influence volume.
    /// Returns `None` for directional lights (they have no bounded
    /// influence — they're applied globally via the directional-prefix
    /// path).
    ///
    /// Point lights: sphere centered at `position` with radius `range`.
    /// Spot lights: sphere centered at `position` with radius `range`
    /// (conservative — the actual spot cone is tighter, but a sphere is
    /// a cheap correct upper bound for AABB overlap testing). Lights with
    /// an unlimited `range` (`<= 0`) derive a finite radius from intensity
    /// (see [`Self::influence_radius`]).
    pub fn world_aabb(&self) -> Option<crate::bounds::Aabb> {
        use glam::Vec3;
        match self {
            Light::Directional { .. } => None,
            Light::Point {
                position,
                range,
                intensity,
                ..
            } => {
                let center = Vec3::from_array(*position);
                let extent = Vec3::splat(Self::influence_radius(*intensity, *range));
                Some(crate::bounds::Aabb {
                    min: center - extent,
                    max: center + extent,
                })
            }
            Light::Spot {
                position,
                range,
                intensity,
                ..
            } => {
                let center = Vec3::from_array(*position);
                let extent = Vec3::splat(Self::influence_radius(*intensity, *range));
                Some(crate::bounds::Aabb {
                    min: center - extent,
                    max: center + extent,
                })
            }
        }
    }

    /// Returns a numeric tag for shader selection.
    pub fn enum_value(&self) -> f32 {
        // f32 since we aren't bitcasting, we're reading as item in packed vec4<f32>
        match self {
            Light::Directional { .. } => 1.0,
            Light::Point { .. } => 2.0,
            Light::Spot { .. } => 3.0,
        }
    }

    /// Stable kind discriminant used by `AwsmRenderer::update_light` to
    /// detect light-kind changes that would desync shadow state (cube
    /// slot for point → not-point, 2D atlas tile for directional →
    /// not-directional). Different enum from `enum_value` because that's
    /// for shader packing.
    pub fn kind_discriminant(&self) -> u8 {
        match self {
            Light::Directional { .. } => 0,
            Light::Point { .. } => 1,
            Light::Spot { .. } => 2,
        }
    }

    // matches LightPacked
    /// Returns the packed storage buffer payload for this light.
    ///
    /// `shadow_index` is bit-cast into `LightPacked.kind_outer_pad.z`
    /// (the f32 slot at offset 56) so the shading shader can recover
    /// it with `bitcast<u32>`. Pass
    /// [`crate::shadows::SHADOW_INDEX_NONE`] (== `u32::MAX`) for
    /// lights that don't cast shadows.
    pub fn storage_buffer_data(&self, shadow_index: u32) -> [u8; Self::BYTE_SIZE] {
        let mut data = [0u8; Self::BYTE_SIZE];
        let mut offset = 0;

        #[derive(Debug)]
        enum Value<'a> {
            F32(&'a f32),
            Vec3(&'a [f32; 3]),
            SkipVec3,
            SkipN32(usize),
        }

        impl<'a> From<&'a f32> for Value<'a> {
            fn from(value: &'a f32) -> Self {
                Value::F32(value)
            }
        }

        impl<'a> From<&'a [f32; 3]> for Value<'a> {
            fn from(value: &'a [f32; 3]) -> Self {
                Value::Vec3(value)
            }
        }

        let mut write = |value: Value| match value {
            Value::F32(value) => {
                let bytes = value.to_ne_bytes();
                data[offset..offset + 4].copy_from_slice(&bytes);
                offset += 4;
            }
            Value::Vec3(values) => {
                let values_u8 =
                    unsafe { std::slice::from_raw_parts(values.as_ptr() as *const u8, 12) };
                data[offset..offset + 12].copy_from_slice(values_u8);
                offset += 12;
            }
            Value::SkipVec3 => {
                offset += 12;
            }
            Value::SkipN32(count) => {
                offset += 4 * count;
            }
        };

        // Layout is:
        // struct LightPacked {
        //   // pos.xyz + range
        //   pos_range: vec4<f32>,
        //   // dir.xyz + inner_cone
        //   dir_inner: vec4<f32>,
        //   // color.rgb + intensity
        //   color_intensity: vec4<f32>,
        //   // kind (as uint) + outer_cone + 2 pads (or extra params)
        //   kind_outer_pad: vec4<f32>,
        // };

        // Bit-cast the shadow index into an f32 so it shares the
        // `kind_outer_pad: vec4<f32>` row layout. WGSL recovers the
        // original bits via `bitcast<u32>(p.kind_outer_pad.z)`.
        let shadow_index_f32 = f32::from_bits(shadow_index);

        match self {
            Light::Directional {
                color,
                intensity,
                direction,
            } => {
                // row 1
                write(Value::SkipVec3); // skip position
                write(Value::SkipN32(1)); // skip range
                                          // row 2
                write(direction.into());
                write(Value::SkipN32(1)); // skip inner cone
                                          // row 3
                write(color.into());
                write(intensity.into());
                // row 4: kind, _, shadow_index, _
                write((&self.enum_value()).into());
                write(Value::SkipN32(1)); // skip outer_cone (unused for directional)
                write((&shadow_index_f32).into());
                write(Value::SkipN32(1)); // pad
            }
            Light::Point {
                color,
                intensity,
                position,
                range,
            } => {
                // row 1. Pack the *effective* influence radius, not the raw
                // glTF range: an unlimited-range light (`range <= 0`) would
                // otherwise upload a 0 here, collapsing the GPU froxel
                // light-culling sphere to a point so the light only lights
                // the single tile it sits in — a hard-edged quad on nearby
                // surfaces. The shader's inverse-square keeps near-field
                // lighting unchanged and just gains a smooth cutoff at the
                // influence edge.
                let gpu_range = Self::influence_radius(*intensity, *range);
                write(position.into());
                write((&gpu_range).into());
                // row 2
                write(Value::SkipN32(4)); // skip direction and inner cone
                                          // row 3
                write(color.into());
                write(intensity.into());
                // row 4: kind, _, shadow_index, _
                write((&self.enum_value()).into());
                write(Value::SkipN32(1)); // skip outer_cone (unused for point)
                write((&shadow_index_f32).into());
                write(Value::SkipN32(1)); // pad
            }
            Light::Spot {
                color,
                intensity,
                position,
                direction,
                range,
                inner_angle,
                outer_angle,
            } => {
                // The shader compares against cosines (`dot(light_dir, axis)`),
                // so pre-compute cos(angle) here instead of storing raw radians.
                let inner_cos = inner_angle.cos();
                let outer_cos = outer_angle.cos();
                // row 1. Effective influence radius (see the point-light
                // arm) so unlimited-range spots still cover their froxels.
                let gpu_range = Self::influence_radius(*intensity, *range);
                write(position.into());
                write((&gpu_range).into());
                // row 2
                write(direction.into());
                write((&inner_cos).into());
                // row 3
                write(color.into());
                write(intensity.into());
                // row 4: kind, outer_cone, shadow_index, _
                write((&self.enum_value()).into());
                write((&outer_cos).into());
                write((&shadow_index_f32).into());
                write(Value::SkipN32(1)); // pad
            }
        }

        data
    }
}

new_key_type! {
    /// Opaque key for lights.
    pub struct LightKey;
}

/// Result type for light operations.
type Result<T> = std::result::Result<T, AwsmLightError>;

/// Light-related errors.
#[derive(Error, Debug)]
pub enum AwsmLightError {
    #[error("[light] {0:?}")]
    Core(#[from] AwsmCoreError),
}
