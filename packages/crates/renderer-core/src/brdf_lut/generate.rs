//! BRDF LUT generation utilities.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::bind_groups::BindGroupLayoutDescriptor;
use crate::command::render_pass::{ColorAttachment, RenderPassDescriptor};
use crate::command::{LoadOp, StoreOp};
use crate::error::{AwsmCoreError, Result};
use crate::pipeline::fragment::{ColorTargetState, FragmentState};
use crate::pipeline::layout::{PipelineLayoutDescriptor, PipelineLayoutKind};
use crate::pipeline::vertex::VertexState;
use crate::pipeline::RenderPipelineDescriptor;
use crate::renderer::{AwsmRendererWebGpu, DeviceId};
use crate::sampler::{AddressMode, FilterMode, MipmapFilterMode, SamplerDescriptor};
use crate::shaders::{ShaderModuleDescriptor, ShaderModuleExt};
use crate::texture::{Extent3d, TextureDescriptor, TextureFormat, TextureUsage};

// Per-device caches: the BRDF-LUT pipeline + sampler are device-bound GPU
// objects, so a second renderer (different device) keys into its own slot
// rather than reusing the first device's pipeline (which would throw a
// cross-device validation error). See `DeviceId`.
thread_local! {
    static BRDF_LUT_PIPELINE: RefCell<HashMap<DeviceId, web_sys::GpuRenderPipeline>> = RefCell::new(HashMap::new());
    static BRDF_SAMPLER: RefCell<HashMap<DeviceId, web_sys::GpuSampler>> = RefCell::new(HashMap::new());
}

/// Generated BRDF lookup texture and sampler.
pub struct BrdfLut {
    pub texture: web_sys::GpuTexture,
    pub view: web_sys::GpuTextureView,
    pub sampler: web_sys::GpuSampler,
}

/// Options for BRDF LUT generation.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct BrdfLutOptions {
    pub width: u32,
    pub height: u32,
}

impl BrdfLutOptions {
    /// Creates options with explicit dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl Default for BrdfLutOptions {
    fn default() -> Self {
        Self {
            width: 1024,
            height: 1024,
        }
    }
}

impl BrdfLut {
    /// Generates a BRDF LUT texture.
    pub async fn new(gpu: &AwsmRendererWebGpu, options: BrdfLutOptions) -> Result<Self> {
        let render_pipeline = get_pipeline(gpu).await?;

        let command_encoder = gpu.create_command_encoder(Some("BRDF Lut Command Encoder"));

        let texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Rgba16float,
                Extent3d::new(options.width, Some(options.height), None),
                TextureUsage::new()
                    .with_copy_dst()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .into(),
        )?;

        let texture_view = texture
            .create_view()
            .map_err(|e| AwsmCoreError::TextureView(format!("{e:?}")))?;

        let render_pass = command_encoder.begin_render_pass(
            &RenderPassDescriptor {
                label: Some("BRDF Lut Render Pass"),
                color_attachments: vec![ColorAttachment::new(
                    &texture_view,
                    LoadOp::Clear,
                    StoreOp::Store,
                )],
                ..Default::default()
            }
            .into(),
        )?;

        render_pass.set_pipeline(&render_pipeline);

        // No vertex buffer needed!
        render_pass.draw(3);

        render_pass.end();

        let command_buffer = command_encoder.finish();
        gpu.submit_commands(&command_buffer);

        let sampler = get_sampler(gpu).await?;

        Ok(Self {
            texture,
            view: texture_view,
            sampler,
        })
    }
}

async fn get_pipeline(gpu: &AwsmRendererWebGpu) -> Result<web_sys::GpuRenderPipeline> {
    let device = gpu.device_id();
    if let Some(pipeline) =
        BRDF_LUT_PIPELINE.with(|pipeline_cell| pipeline_cell.borrow().get(&device).cloned())
    {
        return Ok(pipeline);
    }

    let shader_source = include_str!("./shader.wgsl");
    let shader_module = gpu.compile_shader(
        &ShaderModuleDescriptor::new(shader_source, Some("BRDF Lut Shader")).into(),
    );

    shader_module.validate_shader().await?;

    let bind_group_layout = gpu.create_bind_group_layout(
        &BindGroupLayoutDescriptor::new(Some("BRDF Lut Bind Group Layout"))
            .with_entries(vec![])
            .into(),
    )?;

    let layout = gpu.create_pipeline_layout(
        &PipelineLayoutDescriptor::new(
            Some("BRDF Pipeline Layout"),
            vec![bind_group_layout.clone()],
        )
        .into(),
    );
    let layout = PipelineLayoutKind::Custom(&layout);

    let pipeline_descriptor = RenderPipelineDescriptor::new(
        VertexState::new(&shader_module, None),
        Some("BRDF Lut Pipeline"),
    )
    .with_layout(layout)
    .with_fragment(FragmentState::new(
        &shader_module,
        None,
        vec![ColorTargetState::new(TextureFormat::Rgba16float)],
    ));

    let render_pipeline = gpu
        .create_render_pipeline(&pipeline_descriptor.into())
        .await?;

    BRDF_LUT_PIPELINE.with(|pipeline_cell| {
        pipeline_cell
            .borrow_mut()
            .insert(device, render_pipeline.clone());
        Ok(render_pipeline)
    })
}

async fn get_sampler(gpu: &AwsmRendererWebGpu) -> Result<web_sys::GpuSampler> {
    let device = gpu.device_id();
    if let Some(sampler) =
        BRDF_SAMPLER.with(|sampler_cell| sampler_cell.borrow().get(&device).cloned())
    {
        return Ok(sampler);
    }

    let sampler = gpu.create_sampler(Some(
        &SamplerDescriptor {
            address_mode_u: Some(AddressMode::ClampToEdge),
            address_mode_v: Some(AddressMode::ClampToEdge),
            address_mode_w: Some(AddressMode::ClampToEdge),
            mag_filter: Some(FilterMode::Linear),
            min_filter: Some(FilterMode::Linear),
            mipmap_filter: Some(MipmapFilterMode::Linear),
            max_anisotropy: Some(16),
            label: Some("BRDF LUT Sampler"),
            ..Default::default()
        }
        .into(),
    ));

    BRDF_SAMPLER.with(|sampler_cell| {
        sampler_cell.borrow_mut().insert(device, sampler.clone());
        Ok(sampler)
    })
}
