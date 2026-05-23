//! Environment and skybox helpers.

use std::sync::LazyLock;

use awsm_renderer_core::cubemap::images::CubemapBitmapColors;
use awsm_renderer_core::cubemap::{CubemapBytesLayout, CubemapFace, CubemapImage};
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use awsm_renderer_core::sampler::{AddressMode, FilterMode, MipmapFilterMode};

use crate::bind_groups::BindGroupCreate;
use crate::error::Result;
use crate::textures::{CubemapTextureKey, SamplerCacheKey, Textures};
use crate::AwsmRenderer;

impl AwsmRenderer {
    /// Sets the active skybox.
    pub fn set_skybox(&mut self, skybox: Skybox) {
        self.environment.skybox = skybox;
        self.bind_groups
            .mark_create(BindGroupCreate::EnvironmentSkyboxCreate);
    }

    /// Updates one skybox cubemap face in-place.
    pub fn update_skybox_face(
        &self,
        face: CubemapFace,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
        layout: CubemapBytesLayout,
    ) -> crate::error::Result<()> {
        self.update_cubemap_texture_face(
            self.environment.skybox.texture_key,
            face,
            mip_level,
            width,
            height,
            data,
            layout,
        )
    }

    /// Updates all six skybox cubemap faces in-place.
    pub fn update_skybox_all_faces(
        &self,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
        layout: CubemapBytesLayout,
    ) -> crate::error::Result<()> {
        self.update_cubemap_texture_all_faces(
            self.environment.skybox.texture_key,
            mip_level,
            width,
            height,
            data,
            layout,
        )
    }

    /// Regenerates skybox cubemap mipmaps from mip level 0.
    pub async fn regenerate_skybox_mipmaps(&self) -> crate::error::Result<()> {
        self.regenerate_cubemap_texture_mipmaps(
            self.environment.skybox.texture_key,
            self.environment.skybox.mip_count,
        )
        .await
    }
}

/// Global environment state.
#[derive(Clone)]
pub struct Environment {
    pub skybox: Skybox,
}

/// Skybox texture and sampler data.
#[derive(Clone)]
pub struct Skybox {
    pub texture_key: CubemapTextureKey,
    pub texture_view: web_sys::GpuTextureView,
    pub sampler: web_sys::GpuSampler,
    pub mip_count: u32,
}

static SAMPLER_CACHE_KEY: LazyLock<SamplerCacheKey> = LazyLock::new(|| SamplerCacheKey {
    address_mode_u: Some(AddressMode::ClampToEdge),
    address_mode_v: Some(AddressMode::ClampToEdge),
    address_mode_w: Some(AddressMode::ClampToEdge),
    mag_filter: Some(FilterMode::Linear),
    min_filter: Some(FilterMode::Linear),
    mipmap_filter: Some(MipmapFilterMode::Linear),
    max_anisotropy: Some(16),
    ..Default::default()
});

impl Skybox {
    /// Returns the sampler cache key used for skyboxes.
    pub fn sampler_cache_key() -> SamplerCacheKey {
        SAMPLER_CACHE_KEY.clone()
    }

    /// Creates a skybox from an existing cubemap texture.
    pub fn new(
        texture_key: CubemapTextureKey,
        texture_view: web_sys::GpuTextureView,
        sampler: web_sys::GpuSampler,
        mip_count: u32,
    ) -> Self {
        Self {
            texture_key,
            texture_view,
            sampler,
            mip_count,
        }
    }

    /// Creates a skybox from solid colors.
    pub async fn new_colors(
        gpu: &AwsmRendererWebGpu,
        textures: &mut Textures,
        default_colors: CubemapBitmapColors,
    ) -> Result<Self> {
        let resources = Self::prepare_resources(gpu, default_colors).await?;
        Self::register(gpu, textures, resources)
    }

    /// Phase-1: async-only resource creation. See `IblTexture::prepare_resources`.
    pub async fn prepare_resources(
        gpu: &AwsmRendererWebGpu,
        default_colors: CubemapBitmapColors,
    ) -> Result<SkyboxResources> {
        let (texture, view, mip_count) = CubemapImage::new_colors(default_colors, 256, 256)
            .await?
            .create_texture_and_view(gpu, Some("Skybox Cubemap"))
            .await?;
        Ok(SkyboxResources {
            texture,
            view,
            mip_count,
        })
    }

    /// Phase-2: sync registration. See `IblTexture::register`.
    pub fn register(
        gpu: &AwsmRendererWebGpu,
        textures: &mut Textures,
        resources: SkyboxResources,
    ) -> Result<Self> {
        let texture_key = textures.insert_cubemap(resources.texture);
        let sampler_key = textures.get_sampler_key(gpu, Self::sampler_cache_key())?;
        let sampler = textures.get_sampler(sampler_key)?.clone();
        Ok(Self::new(
            texture_key,
            resources.view,
            sampler,
            resources.mip_count,
        ))
    }
}

/// Detached GPU resources produced by [`Skybox::prepare_resources`].
pub struct SkyboxResources {
    pub texture: web_sys::GpuTexture,
    pub view: web_sys::GpuTextureView,
    pub mip_count: u32,
}

impl Environment {
    /// Creates an environment with a skybox.
    pub fn new(skybox: Skybox) -> Self {
        Self { skybox }
    }
}
