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
use slotmap::{new_key_type, SlotMap};
use thiserror::Error;

use crate::{
    bind_groups::{BindGroupCreate, BindGroups},
    lights::ibl::Ibl,
    AwsmRenderer, AwsmRendererLogging,
};

static PUNCTUAL_BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage().with_copy_dst());

static INFO_BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_uniform().with_copy_dst());

impl AwsmRenderer {
    /// Sets the BRDF LUT texture used for IBL.
    pub fn set_brdf_lut(&mut self, brdf_lut: BrdfLut) {
        self.lights.brdf_lut = brdf_lut;
        self.bind_groups
            .mark_create(BindGroupCreate::BrdfLutTextures);
    }
    /// Sets image-based lighting textures.
    pub fn set_ibl(&mut self, ibl: Ibl) {
        self.lights.ibl = ibl;
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
    // We do not use DynamicUniformBuffer here because we need dense sequential access in the gpu
    // not stable offsets per-key that DynamicUniformBuffer provides (with holes, etc)
    // instead, we rebuild a fresh Vec<u8> when the gpu is dirty
    // however, we do need to track the size so we can resize the gpu buffer if needed
    punctual_gpu_size: usize,
    punctual_gpu_dirty: bool,
    lighting_info_gpu_dirty: bool,
}

impl Lights {
    /// Size in bytes for a single punctual light.
    pub const PUNCTUAL_LIGHT_SIZE: usize = 64;
    /// Size in bytes for the lighting info block.
    pub const INFO_SIZE: usize = 16; // 2 * u32 for mipmap counts, 1 for number of lights, and 1 for padding

    /// Creates light buffers and initializes IBL state.
    pub fn new(gpu: &AwsmRendererWebGpu, ibl: Ibl, brdf_lut: BrdfLut) -> Result<Self> {
        // GPU size should never be 0
        let punctual_gpu_size = Self::PUNCTUAL_LIGHT_SIZE;

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
            ibl,
            brdf_lut,
            punctual_gpu_size,
            punctual_gpu_dirty: true,
            lighting_info_gpu_dirty: true,
            gpu_punctual_buffer,
            gpu_info_buffer,
        })
    }

    /// Removes all lights.
    pub fn clear(&mut self) {
        self.lights.clear();
        self.punctual_gpu_dirty = true;
        self.lighting_info_gpu_dirty = true;
    }

    /// Inserts a light and returns its key.
    pub fn insert(&mut self, light: Light) -> Result<LightKey> {
        let key = self.lights.insert(light.clone());

        self.punctual_gpu_dirty = true;
        self.lighting_info_gpu_dirty = true;
        Ok(key)
    }

    /// Removes a light by key.
    pub fn remove(&mut self, key: LightKey) {
        self.lights.remove(key);
        self.punctual_gpu_dirty = true;
        self.lighting_info_gpu_dirty = true;
    }

    /// Updates a light in place.
    pub fn update(&mut self, key: LightKey, f: impl FnOnce(&mut Light)) {
        if let Some(light) = self.lights.get_mut(key) {
            f(light);
            self.punctual_gpu_dirty = true;
        }
    }

    /// Writes lighting buffers to the GPU if dirty.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        if self.punctual_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings {
                Some(
                    tracing::span!(
                        tracing::Level::INFO,
                        "Punctual Lights Storage Buffer GPU write"
                    )
                    .entered(),
                )
            } else {
                None
            };

            let punctual_light_buffer: Vec<u8> = self
                .lights
                .values()
                .flat_map(|light| light.storage_buffer_data())
                .collect();

            // GPU size should never be 0, so use at least PUNCTUAL_LIGHT_SIZE
            let target_gpu_size = if punctual_light_buffer.len() > self.punctual_gpu_size {
                // Grow with 2x headroom
                (punctual_light_buffer.len() * 2).max(Self::PUNCTUAL_LIGHT_SIZE)
            } else if punctual_light_buffer.len() < self.punctual_gpu_size / 2 {
                // Shrink if using less than half
                punctual_light_buffer.len().max(Self::PUNCTUAL_LIGHT_SIZE)
            } else {
                // Keep current size
                self.punctual_gpu_size
            };

            if target_gpu_size != self.punctual_gpu_size {
                self.gpu_punctual_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(Some("Lights"), target_gpu_size, *PUNCTUAL_BUFFER_USAGE)
                        .into(),
                )?;

                self.punctual_gpu_size = target_gpu_size;

                bind_groups.mark_create(BindGroupCreate::LightsResize);
            }

            if !punctual_light_buffer.is_empty() {
                gpu.write_buffer(
                    &self.gpu_punctual_buffer,
                    None,
                    punctual_light_buffer.as_slice(),
                    None,
                    None,
                )?;
            }

            // for (index, chunk) in punctual_light_buffer.chunks_exact(64).enumerate() {
            //     let values =
            //         unsafe { std::slice::from_raw_parts(chunk.as_ptr() as *const f32, 16) };
            //     tracing::info!("{}: {:?}", index, values);
            // }

            self.punctual_gpu_dirty = false;
        }

        if self.lighting_info_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Lighting Info GPU write").entered())
            } else {
                None
            };

            let mut data = vec![0u8; Self::INFO_SIZE];
            data[0..4].copy_from_slice(&(self.lights.len() as u32).to_ne_bytes());
            data[4..8].copy_from_slice(&self.ibl.prefiltered_env.mip_count.to_ne_bytes());
            data[8..12].copy_from_slice(&self.ibl.irradiance.mip_count.to_ne_bytes());

            gpu.write_buffer(&self.gpu_info_buffer, None, &*data, None, None)?;

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

    /// Returns a numeric tag for shader selection.
    pub fn enum_value(&self) -> f32 {
        // f32 since we aren't bitcasting, we're reading as item in packed vec4<f32>
        match self {
            Light::Directional { .. } => 1.0,
            Light::Point { .. } => 2.0,
            Light::Spot { .. } => 3.0,
        }
    }

    // matches LightPacked
    /// Returns the packed storage buffer payload for this light.
    pub fn storage_buffer_data(&self) -> [u8; Self::BYTE_SIZE] {
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
                // row 4
                write((&self.enum_value()).into());
                write(Value::SkipN32(3)); // skip outer cone and padding
            }
            Light::Point {
                color,
                intensity,
                position,
                range,
            } => {
                // row 1
                write(position.into());
                write(range.into());
                // row 2
                write(Value::SkipN32(4)); // skip direction and inner cone
                                          // row 3
                write(color.into());
                write(intensity.into());
                // row 4
                write((&self.enum_value()).into());
                write(Value::SkipN32(3)); // skip outer cone and padding
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
                // row 1
                write(position.into());
                write(range.into());
                // row 2
                write(direction.into());
                write((&inner_cos).into());
                // row 3
                write(color.into());
                write(intensity.into());
                // row 4
                write((&self.enum_value()).into());
                write((&outer_cos).into());
                write(Value::SkipN32(2)); // skip padding
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
