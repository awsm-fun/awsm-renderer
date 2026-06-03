//! Simple blit pipeline for texture copies.

use std::{borrow::Cow, cell::RefCell, collections::HashMap};

use web_sys::GpuTextureView;

use crate::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
        BindGroupLayoutResource, BindGroupResource, TextureBindingLayout,
    },
    command::{
        color::Color,
        render_pass::{ColorAttachment, RenderPassDescriptor},
        LoadOp, StoreOp,
    },
    error::Result,
    pipeline::{
        fragment::{ColorTargetState, FragmentState},
        layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
        multisample::MultisampleState,
        primitive::PrimitiveState,
        vertex::VertexState,
        RenderPipelineDescriptor,
    },
    renderer::AwsmRendererWebGpu,
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    texture::{TextureFormat, TextureFormatKey, TextureSampleType, TextureViewDimension},
};

/// Cached pipeline and layout for blit operations.
#[derive(Debug, Clone)]
pub struct BlitPipeline {
    pub render_pipeline: web_sys::GpuRenderPipeline,
    pub bind_group_layout: web_sys::GpuBindGroupLayout,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BlitPipelineCacheKey {
    dst_format: TextureFormatKey,
    dst_sample_count: Option<u32>,
}
thread_local! {
    static BLIT_PIPELINE: RefCell<HashMap<BlitPipelineCacheKey, BlitPipeline>> = RefCell::new(HashMap::new());
}

static SHADER_SOURCE: &str = r#"
    @group(0) @binding(0) var src_tex: texture_2d<f32>;

    @vertex
    fn vert_main(@builtin(vertex_index) vertex_index: u32) -> FragmentInput {
        var out: FragmentInput;

        // Generate oversized triangle vertices using bit manipulation
        // Goal: vertex 0→(-1,-1), vertex 1→(3,-1), vertex 2→(-1,3)

        // X coordinate generation:
        // vertex_index: 0 → 0<<1 = 0 → 0&2 = 0 → 0*2-1 = -1 ✓
        // vertex_index: 1 → 1<<1 = 2 → 2&2 = 2 → 2*2-1 = 3  ✓
        // vertex_index: 2 → 2<<1 = 4 → 4&2 = 0 → 0*2-1 = -1 ✓
        let x = f32((vertex_index << 1u) & 2u) * 2.0 - 1.0;

        // Y coordinate generation:
        // vertex_index: 0 → 0&2 = 0 → 0*2-1 = -1 ✓
        // vertex_index: 1 → 1&2 = 0 → 0*2-1 = -1 ✓
        // vertex_index: 2 → 2&2 = 2 → 2*2-1 = 3  ✓
        let y = f32(vertex_index & 2u) * 2.0 - 1.0;

        out.full_screen_quad_position = vec4<f32>(x, y, 0.0, 1.0);

        return out;
    }

    struct FragmentInput {
        @builtin(position) full_screen_quad_position: vec4<f32>,
    }

    @fragment
    fn frag_main(in: FragmentInput) -> @location(0) vec4<f32> {
        let coords = vec2<i32>(in.full_screen_quad_position.xy);

        return textureLoad(src_tex, coords, 0);
    }
"#;

/// Blits a texture view into another view using a cached pipeline.
pub async fn blit_tex_simple(
    gpu: &AwsmRendererWebGpu,
    src_view: &web_sys::GpuTextureView,
    dst_view: &web_sys::GpuTextureView,
    dst_format: TextureFormat,
    dst_sample_count: Option<u32>,
    command_encoder: &crate::command::CommandEncoder,
) -> Result<()> {
    let pipeline = blit_get_pipeline(gpu, dst_format, dst_sample_count).await?;
    let bind_group = blit_get_bind_group(gpu, &pipeline, src_view);
    blit_tex(&pipeline, &bind_group, dst_view, command_encoder)?;

    Ok(())
}

/// Records a blit render pass into the command encoder.
pub fn blit_tex(
    pipeline: &BlitPipeline,
    bind_group: &web_sys::GpuBindGroup,
    dst_view: &web_sys::GpuTextureView,
    command_encoder: &crate::command::CommandEncoder,
) -> Result<()> {
    let render_pass = command_encoder.begin_render_pass(
        &RenderPassDescriptor {
            label: Some("Blit Render Pass"),
            color_attachments: vec![
                ColorAttachment::new(dst_view, LoadOp::Clear, StoreOp::Store)
                    .with_clear_color(&Color::ZERO),
            ],
            depth_stencil_attachment: None,
            ..Default::default()
        }
        .into(),
    )?;

    render_pass.set_bind_group(0, bind_group, None)?;
    render_pass.set_pipeline(&pipeline.render_pipeline);

    render_pass.draw(3);

    render_pass.end();

    Ok(())
}

/// Creates a bind group for a blit pipeline and source view.
pub fn blit_get_bind_group(
    gpu: &AwsmRendererWebGpu,
    pipeline: &BlitPipeline,
    src_view: &GpuTextureView,
) -> web_sys::GpuBindGroup {
    gpu.create_bind_group(
        &BindGroupDescriptor::new(
            &pipeline.bind_group_layout,
            Some("Blit Bind Group"),
            vec![BindGroupEntry::new(
                0,
                BindGroupResource::TextureView(Cow::Borrowed(src_view)),
            )],
        )
        .into(),
    )
}

/// Returns a cached blit pipeline for the given format and sample count.
pub async fn blit_get_pipeline(
    gpu: &AwsmRendererWebGpu,
    dst_format: TextureFormat,
    dst_sample_count: Option<u32>,
) -> Result<BlitPipeline> {
    let key = BlitPipelineCacheKey {
        dst_format: dst_format.into(),
        dst_sample_count,
    };
    if let Some(pipeline) =
        BLIT_PIPELINE.with(|pipeline_cell| pipeline_cell.borrow().get(&key).cloned())
    {
        return Ok(pipeline);
    }

    let shader_module =
        gpu.compile_shader(&ShaderModuleDescriptor::new(SHADER_SOURCE, Some("Blit shader")).into());

    shader_module.validate_shader().await?;

    let bind_group_layout = gpu.create_bind_group_layout(
        &BindGroupLayoutDescriptor::new(Some("Blit Bind Group Layout"))
            .with_entries(vec![
                // Binding 0: Source texture
                BindGroupLayoutEntry::new(
                    0,
                    BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_sample_type(TextureSampleType::Float)
                            .with_view_dimension(TextureViewDimension::N2d),
                    ),
                )
                .with_visibility_fragment(),
            ])
            .into(),
    )?;

    let pipeline_layout = gpu.create_pipeline_layout(
        &PipelineLayoutDescriptor::new(
            Some("Blit pipeline Layout"),
            vec![bind_group_layout.clone()],
        )
        .into(),
    );

    let vertex = VertexState::new(&shader_module, None);
    let fragment = FragmentState::new(
        &shader_module,
        None,
        vec![ColorTargetState::new(dst_format)],
    );
    let mut pipeline_descriptor = RenderPipelineDescriptor::new(vertex, Some("Blit"))
        .with_primitive(PrimitiveState::new())
        .with_layout(PipelineLayoutKind::Custom(&pipeline_layout))
        .with_fragment(fragment);

    if let Some(sample_count) = dst_sample_count {
        pipeline_descriptor =
            pipeline_descriptor.with_multisample(MultisampleState::new().with_count(sample_count));
    }

    let render_pipeline = gpu
        .create_render_pipeline(&pipeline_descriptor.into())
        .await?;

    BLIT_PIPELINE.with(|pipeline_cell| {
        let pipeline = BlitPipeline {
            render_pipeline,
            bind_group_layout,
        };
        pipeline_cell.borrow_mut().insert(key, pipeline.clone());
        Ok(pipeline)
    })
}
