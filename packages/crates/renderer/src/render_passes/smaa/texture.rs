//! SMAA textures.
//!
//! - **edges** + **weights**: full-res `rgba8unorm` intermediates, allocated
//!   only while SMAA is enabled (the whole pass is dropped on disable — SMAA
//!   off is zero-cost); recreated on viewport resize via
//!   [`super::render_pass::SmaaRenderPass::ensure_size`].
//! - **AreaTex** (160×560 RG8) + **SearchTex** (64×16 R8): the REFERENCE
//!   implementation's precomputed pattern textures, embedded byte-exact from
//!   the canonical distribution (iryoku/smaa, MIT) and uploaded once at pass
//!   construction. AreaTex encodes the per-pattern coverage areas (orthogonal
//!   + diagonal regions); SearchTex accelerates the edge-end searches.

use awsm_renderer_core::{
    command::copy_texture::{TexelCopyBufferLayout, TexelCopyTextureInfo},
    error::Result,
    renderer::AwsmRendererWebGpu,
    texture::{Extent3d, TextureDescriptor, TextureFormat, TextureUsage},
};

const AREA_TEX_BYTES: &[u8] = include_bytes!("textures/AreaTex.bin");
const SEARCH_TEX_BYTES: &[u8] = include_bytes!("textures/SearchTex.bin");
const AREA_TEX_SIZE: (u32, u32) = (160, 560);
const SEARCH_TEX_SIZE: (u32, u32) = (64, 16);

pub struct SmaaTextures {
    #[allow(dead_code)]
    edges_tex: web_sys::GpuTexture,
    pub edges_view: web_sys::GpuTextureView,
    #[allow(dead_code)]
    weights_tex: web_sys::GpuTexture,
    pub weights_view: web_sys::GpuTextureView,
    #[allow(dead_code)]
    area_tex: web_sys::GpuTexture,
    pub area_view: web_sys::GpuTextureView,
    #[allow(dead_code)]
    search_tex: web_sys::GpuTexture,
    pub search_view: web_sys::GpuTextureView,
    pub width: u32,
    pub height: u32,
}

impl SmaaTextures {
    pub fn new(gpu: &AwsmRendererWebGpu, width: u32, height: u32) -> Result<Self> {
        let (edges_tex, edges_view) = create_intermediate(gpu, "SMAA Edges", width, height)?;
        let (weights_tex, weights_view) = create_intermediate(gpu, "SMAA Weights", width, height)?;
        let (area_tex, area_view) = upload_static(
            gpu,
            "SMAA AreaTex",
            TextureFormat::Rg8unorm,
            AREA_TEX_SIZE,
            AREA_TEX_BYTES,
            2,
        )?;
        let (search_tex, search_view) = upload_static(
            gpu,
            "SMAA SearchTex",
            TextureFormat::R8unorm,
            SEARCH_TEX_SIZE,
            SEARCH_TEX_BYTES,
            1,
        )?;
        Ok(Self {
            edges_tex,
            edges_view,
            weights_tex,
            weights_view,
            area_tex,
            area_view,
            search_tex,
            search_view,
            width,
            height,
        })
    }

    /// Recreate only the full-res intermediates for a new viewport, reusing
    /// the static AreaTex/SearchTex uploads.
    pub fn resize(&mut self, gpu: &AwsmRendererWebGpu, width: u32, height: u32) -> Result<()> {
        let (edges_tex, edges_view) = create_intermediate(gpu, "SMAA Edges", width, height)?;
        let (weights_tex, weights_view) = create_intermediate(gpu, "SMAA Weights", width, height)?;
        std::mem::replace(&mut self.edges_tex, edges_tex).destroy();
        std::mem::replace(&mut self.weights_tex, weights_tex).destroy();
        self.edges_view = edges_view;
        self.weights_view = weights_view;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Release every GPU texture (disable path — handles are otherwise only
    /// reclaimed by JS GC).
    pub fn destroy(self) {
        self.edges_tex.destroy();
        self.weights_tex.destroy();
        self.area_tex.destroy();
        self.search_tex.destroy();
    }
}

fn create_intermediate(
    gpu: &AwsmRendererWebGpu,
    label: &str,
    width: u32,
    height: u32,
) -> Result<(web_sys::GpuTexture, web_sys::GpuTextureView)> {
    let tex = gpu.create_texture(
        &TextureDescriptor::new(
            TextureFormat::Rgba8unorm,
            Extent3d::new(width, Some(height), None),
            TextureUsage::new()
                .with_texture_binding()
                .with_storage_binding(),
        )
        .with_label(label)
        .into(),
    )?;
    let view = tex.create_view().map_err(|e| {
        awsm_renderer_core::error::AwsmCoreError::create_texture_view(
            format!("{label}: {e:?}").into(),
        )
    })?;
    Ok((tex, view))
}

fn upload_static(
    gpu: &AwsmRendererWebGpu,
    label: &str,
    format: TextureFormat,
    (width, height): (u32, u32),
    bytes: &[u8],
    bytes_per_pixel: u32,
) -> Result<(web_sys::GpuTexture, web_sys::GpuTextureView)> {
    let tex = gpu.create_texture(
        &TextureDescriptor::new(
            format,
            Extent3d::new(width, Some(height), None),
            TextureUsage::new().with_texture_binding().with_copy_dst(),
        )
        .with_label(label)
        .into(),
    )?;
    let destination = TexelCopyTextureInfo::new(&tex);
    let layout = TexelCopyBufferLayout::new()
        .with_bytes_per_row(width * bytes_per_pixel)
        .with_rows_per_image(height);
    let size = Extent3d::new(width, Some(height), None);
    gpu.write_texture(&destination.into(), bytes, &layout.into(), &size.into())?;
    let view = tex.create_view().map_err(|e| {
        awsm_renderer_core::error::AwsmCoreError::create_texture_view(
            format!("{label}: {e:?}").into(),
        )
    })?;
    Ok((tex, view))
}
