use std::cell::RefCell;
use std::collections::HashMap;

use crate::bind_groups::{
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindGroupLayoutResource, BufferBindingLayout,
    BufferBindingType, StorageTextureAccess, StorageTextureBindingLayout, TextureBindingLayout,
};
use crate::error::Result;
use crate::pipeline::layout::{PipelineLayoutDescriptor, PipelineLayoutKind};
use crate::pipeline::{ComputePipelineDescriptor, ProgrammableStage};
use crate::renderer::{AwsmRendererWebGpu, DeviceId};
use crate::shaders::{ShaderModuleDescriptor, ShaderModuleExt};
use crate::texture::{TextureFormat, TextureSampleType, TextureViewDimension};

// Per-device: the atlas compute pipeline + its shader module are device-bound,
// so each renderer's device keys into its own slot (see `DeviceId`).
thread_local! {
    static ATLAS_PIPELINE: RefCell<HashMap<DeviceId, AtlasPipeline>> = RefCell::new(HashMap::new());
    static ATLAS_SHADER_MODULE: RefCell<HashMap<DeviceId, web_sys::GpuShaderModule>> = RefCell::new(HashMap::new());
}

#[derive(Clone)]
pub(super) struct AtlasPipeline {
    pub compute_pipeline: web_sys::GpuComputePipeline,
    pub bind_group_layout: web_sys::GpuBindGroupLayout,
}

pub(super) async fn get_atlas_pipeline(gpu: &AwsmRendererWebGpu) -> Result<AtlasPipeline> {
    let device = gpu.device_id();
    let pipeline = ATLAS_PIPELINE.with(|pipeline_cell| pipeline_cell.borrow().get(&device).cloned());

    if let Some(pipeline) = pipeline {
        return Ok(pipeline);
    }

    let shader_module =
        ATLAS_SHADER_MODULE.with(|shader_module| shader_module.borrow().get(&device).cloned());

    let shader_module = match shader_module {
        Some(module) => module,
        None => {
            let shader_module = gpu.compile_shader(
                &ShaderModuleDescriptor::new(include_str!("./shader.wgsl"), Some("Atlas Shader"))
                    .into(),
            );

            shader_module.validate_shader().await?;

            ATLAS_SHADER_MODULE.with(|shader_module_rc| {
                shader_module_rc
                    .borrow_mut()
                    .insert(device, shader_module.clone());
            });

            shader_module
        }
    };

    let compute = ProgrammableStage::new(&shader_module, None);

    let bind_group_layout = gpu.create_bind_group_layout(
        &BindGroupLayoutDescriptor::new(Some("Atlas Bind Group Layout"))
            .with_entries(vec![
                BindGroupLayoutEntry::new(
                    0,
                    BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_sample_type(TextureSampleType::Float)
                            .with_view_dimension(TextureViewDimension::N2d),
                    ),
                )
                .with_visibility_compute(),
                BindGroupLayoutEntry::new(
                    1,
                    BindGroupLayoutResource::StorageTexture(
                        StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                            .with_view_dimension(TextureViewDimension::N2dArray)
                            .with_access(StorageTextureAccess::WriteOnly),
                    ),
                )
                .with_visibility_compute(),
                BindGroupLayoutEntry::new(
                    2,
                    BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                    ),
                )
                .with_visibility_compute(),
            ])
            .into(),
    )?;

    let layout = gpu.create_pipeline_layout(
        &PipelineLayoutDescriptor::new(
            Some("Atlas Pipeline Layout"),
            vec![bind_group_layout.clone()],
        )
        .into(),
    );

    let layout = PipelineLayoutKind::Custom(&layout);

    let pipeline_descriptor =
        ComputePipelineDescriptor::new(compute, layout.clone(), Some("Atlas Pipeline"));

    let pipeline = gpu
        .create_compute_pipeline(&pipeline_descriptor.into())
        .await?;

    ATLAS_PIPELINE.with(|pipeline_cell| {
        let pipeline = AtlasPipeline {
            compute_pipeline: pipeline,
            bind_group_layout,
        };
        pipeline_cell.borrow_mut().insert(device, pipeline.clone());
        Ok(pipeline)
    })
}
