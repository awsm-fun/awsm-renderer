//! sRGB to linear texture conversion helpers.

use std::{borrow::Cow, cell::RefCell, collections::HashMap};

use crate::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
        BindGroupLayoutResource, BindGroupResource, StorageTextureAccess,
        StorageTextureBindingLayout, TextureBindingLayout,
    },
    command::compute_pass::ComputePassDescriptor,
    error::{AwsmCoreError, Result},
    pipeline::{
        layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
        ComputePipelineDescriptor, ProgrammableStage,
    },
    renderer::AwsmRendererWebGpu,
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    texture::{
        texture_format_to_wgsl_storage, TextureFormat, TextureFormatKey, TextureSampleType,
        TextureViewDescriptor, TextureViewDimension,
    },
};

#[derive(Debug, Clone)]
struct ConvertSrgbPipeline {
    pub compute_pipeline: web_sys::GpuComputePipeline,
    pub bind_group_layout: web_sys::GpuBindGroupLayout,
}

thread_local! {
    static CONVERT_SRGB_PIPELINE: RefCell<HashMap<TextureFormatKey, ConvertSrgbPipeline>> = RefCell::new(HashMap::new());
}

// sRGB to linear conversion shader
// Converts a single layer of a 2D texture from sRGB to linear color space
fn shader_source(format: TextureFormat) -> Result<String> {
    let storage_format = texture_format_to_wgsl_storage(format)?;

    Ok(format!(
        r#"
            @group(0) @binding(0) var src: texture_2d<f32>;
            @group(0) @binding(1) var dst: texture_storage_2d<{storage_format}, write>;

            struct Params {{
                width: u32,
                height: u32,
            }};

            @group(0) @binding(2) var<uniform> params: Params;

            // sRGB to linear conversion
            // Uses the standard sRGB transfer function
            fn srgb_to_linear(color: vec3<f32>) -> vec3<f32> {{
                let cutoff = vec3<f32>(0.04045);
                let low = color / 12.92;
                let high = pow((color + 0.055) / 1.055, vec3<f32>(2.4));
                return select(high, low, color <= cutoff);
            }}

            @compute @workgroup_size(8, 8)
            fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
                // Bounds check
                if (gid.x >= params.width || gid.y >= params.height) {{
                    return;
                }}

                let coord = vec2<i32>(gid.xy);
                var color = textureLoad(src, coord, 0);

                // Convert RGB from sRGB to linear, preserve alpha
                let converted = srgb_to_linear(color.rgb);
                color = vec4<f32>(converted, color.a);

                textureStore(dst, coord, color);
            }}
            "#
    ))
}

/// Convert a 2D texture from sRGB to linear color space
///
/// # Arguments
/// * `gpu` - WebGPU renderer
/// * `command_encoder` - Command encoder to add the conversion pass to
/// * `src_texture` - Source texture (must be readable, format must match dst_texture)
/// * `dst_texture` - Destination texture (must have STORAGE_BINDING usage)
/// * `width` - Texture width
/// * `height` - Texture height
pub async fn convert_srgb_to_linear(
    gpu: &AwsmRendererWebGpu,
    command_encoder: &crate::command::CommandEncoder,
    src_texture: &web_sys::GpuTexture,
    dst_texture: &web_sys::GpuTexture,
    width: u32,
    height: u32,
) -> Result<()> {
    use crate::buffers::{BufferDescriptor, BufferUsage};

    // Get pipeline
    let ConvertSrgbPipeline {
        compute_pipeline,
        bind_group_layout,
    } = get_pipeline(gpu, dst_texture.format()).await?;

    // Create texture views
    let src_view = src_texture
        .create_view_with_descriptor(
            &TextureViewDescriptor::new(Some("sRGB Source"))
                .with_dimension(TextureViewDimension::N2d)
                .with_mip_level_count(1)
                .into(),
        )
        .map_err(AwsmCoreError::create_texture_view)?;

    let dst_view = dst_texture
        .create_view_with_descriptor(
            &TextureViewDescriptor::new(Some("Linear Dest"))
                .with_dimension(TextureViewDimension::N2d)
                .with_mip_level_count(1)
                .into(),
        )
        .map_err(AwsmCoreError::create_texture_view)?;

    // Build uniform buffer data
    let params_data: Vec<u32> = vec![width, height];

    let params_buffer = gpu.create_buffer(
        &BufferDescriptor::new(
            Some("sRGB Conversion Params"),
            params_data.len() * 4,
            BufferUsage::new().with_uniform().with_copy_dst(),
        )
        .into(),
    )?;

    // Convert to bytes and write buffer
    let params_bytes: Vec<u8> = params_data.iter().flat_map(|&x| x.to_le_bytes()).collect();
    gpu.write_buffer(&params_buffer, None, &*params_bytes, None, None)?;

    // Create bind group
    let buffer_binding = crate::buffers::BufferBinding::new(&params_buffer);
    let bind_group = gpu.create_bind_group(&<web_sys::GpuBindGroupDescriptor>::from(
        BindGroupDescriptor::new(
            &bind_group_layout,
            Some("sRGB Conversion Bind Group"),
            vec![
                BindGroupEntry::new(0, BindGroupResource::TextureView(Cow::Borrowed(&src_view))),
                BindGroupEntry::new(1, BindGroupResource::TextureView(Cow::Borrowed(&dst_view))),
                BindGroupEntry::new(2, BindGroupResource::Buffer(buffer_binding)),
            ],
        ),
    ));

    let workgroup_size_x = width.div_ceil(8);
    let workgroup_size_y = height.div_ceil(8);

    // Dispatch compute shader
    let compute_pass = command_encoder.begin_compute_pass(Some(
        &ComputePassDescriptor::new(Some("sRGB Conversion Pass")).into(),
    ));
    compute_pass.set_pipeline(&compute_pipeline);
    compute_pass.set_bind_group(0, &bind_group, None)?;
    compute_pass.dispatch_workgroups(workgroup_size_x, Some(workgroup_size_y), None);
    compute_pass.end();

    Ok(())
}

async fn get_pipeline(
    gpu: &AwsmRendererWebGpu,
    format: TextureFormat,
) -> Result<ConvertSrgbPipeline> {
    let key: TextureFormatKey = format.into();

    if let Some(pipeline) =
        CONVERT_SRGB_PIPELINE.with(|pipeline_cell| pipeline_cell.borrow().get(&key).cloned())
    {
        return Ok(pipeline);
    }

    let shader_source = shader_source(format)?;
    let shader_module = gpu.compile_shader(
        &ShaderModuleDescriptor::new(&shader_source, Some("sRGB Conversion Shader")).into(),
    );

    shader_module.validate_shader().await?;

    let compute = ProgrammableStage::new(&shader_module, None);

    let bind_group_layout = gpu.create_bind_group_layout(
        &BindGroupLayoutDescriptor::new(Some("sRGB Conversion Bind Group Layout"))
            .with_entries(vec![
                // Binding 0: Source texture (sRGB)
                BindGroupLayoutEntry::new(
                    0,
                    BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_sample_type(TextureSampleType::Float)
                            .with_view_dimension(TextureViewDimension::N2d)
                            .with_multisampled(false),
                    ),
                )
                .with_visibility_compute(),
                // Binding 1: Destination texture (linear - storage)
                BindGroupLayoutEntry::new(
                    1,
                    BindGroupLayoutResource::StorageTexture(
                        StorageTextureBindingLayout::new(format)
                            .with_view_dimension(TextureViewDimension::N2d)
                            .with_access(StorageTextureAccess::WriteOnly),
                    ),
                )
                .with_visibility_compute(),
                // Binding 2: Uniform buffer with parameters
                BindGroupLayoutEntry::new(
                    2,
                    BindGroupLayoutResource::Buffer(crate::bind_groups::BufferBindingLayout::new()),
                )
                .with_visibility_compute(),
            ])
            .into(),
    )?;

    let pipeline_layout = gpu.create_pipeline_layout(
        &PipelineLayoutDescriptor::new(
            Some("sRGB Conversion Pipeline Layout"),
            vec![bind_group_layout.clone()],
        )
        .into(),
    );

    let pipeline_descriptor = ComputePipelineDescriptor::new(
        compute,
        PipelineLayoutKind::Custom(&pipeline_layout),
        Some("sRGB Conversion Pipeline"),
    );

    let pipeline = gpu
        .create_compute_pipeline(&pipeline_descriptor.into())
        .await?;

    CONVERT_SRGB_PIPELINE.with(|pipeline_cell| {
        let pipeline = ConvertSrgbPipeline {
            compute_pipeline: pipeline,
            bind_group_layout,
        };
        pipeline_cell.borrow_mut().insert(key, pipeline.clone());
        Ok(pipeline)
    })
}
