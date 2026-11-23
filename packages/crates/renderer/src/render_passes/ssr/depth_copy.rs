//! SSR depth copy: sample 0 of the multisampled depth, as a single-sample texture.
//!
//! Every SSR stage reads only sample 0, and the trace reads it up to hundreds
//! of times a pixel at scattered texels. With the march's path held fixed, those
//! loads cost 2.7x more from the multisampled target than from a single-sample
//! one on an M1 Max in Chrome 158; with the loads taken out, the two were within
//! 0.2 ms. One draw puts the same values where they are cheap to read.

use std::borrow::Cow;

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
        BindGroupLayoutResource, BindGroupResource, TextureBindingLayout,
    },
    command::{
        render_pass::{DepthStencilAttachment, RenderPassDescriptor},
        LoadOp, StoreOp,
    },
    compare::CompareFunction,
    pipeline::{
        depth_stencil::DepthStencilState,
        fragment::FragmentState,
        layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
        primitive::PrimitiveState,
        vertex::VertexState,
        RenderPipelineDescriptor,
    },
    shaders::ShaderModuleDescriptor,
    texture::{TextureSampleType, TextureViewDimension},
};

use crate::{
    bind_groups::{AwsmBindGroupError, BindGroupRecreateContext},
    error::Result,
    render::RenderContext,
    render_passes::{ssr::composite::PassPipeline, RenderPassInitContext},
};

pub(crate) const SHADER_SOURCE: &str = r#"
@group(0) @binding(0) var src_tex: texture_depth_multisampled_2d;

@vertex
fn vert_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn frag_main(@builtin(position) pos: vec4<f32>) -> @builtin(frag_depth) f32 {
    return textureLoad(src_tex, vec2<i32>(pos.xy), 0);
}
"#;

pub(crate) struct SsrDepthCopy {
    layout: web_sys::GpuBindGroupLayout,
    pub(crate) pipeline: PassPipeline,
    bind_group: Option<web_sys::GpuBindGroup>,
    pass_descriptor: Option<web_sys::GpuRenderPassDescriptor>,
}

impl SsrDepthCopy {
    pub(crate) async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let gpu = ctx.gpu;
        let shader = gpu.compile_shader(
            &ShaderModuleDescriptor::new(SHADER_SOURCE, Some("SSR Depth Copy shader")).into(),
        );
        let layout = gpu.create_bind_group_layout(
            &BindGroupLayoutDescriptor::new(Some("SSR Depth Copy BGL"))
                .with_entries(vec![BindGroupLayoutEntry::new(
                    0,
                    BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_sample_type(TextureSampleType::Depth)
                            .with_view_dimension(TextureViewDimension::N2d)
                            .with_multisampled(true),
                    ),
                )
                .with_visibility_fragment()])
                .into(),
        )?;
        let pipeline_layout = gpu.create_pipeline_layout(
            &PipelineLayoutDescriptor::new(Some("SSR Depth Copy Layout"), vec![layout.clone()])
                .into(),
        );
        let descriptor =
            RenderPipelineDescriptor::new(VertexState::new(&shader, None), Some("SSR Depth Copy"))
                .with_primitive(PrimitiveState::new())
                .with_layout(PipelineLayoutKind::Custom(&pipeline_layout))
                .with_depth_stencil(
                    DepthStencilState::new(ctx.render_texture_formats.depth)
                        .with_depth_compare(CompareFunction::Always)
                        .with_depth_write_enabled(true),
                )
                .with_fragment(FragmentState::new(&shader, None, vec![]));
        let pipeline = PassPipeline::new(ctx, &shader, descriptor).await?;
        Ok(Self {
            layout,
            pipeline,
            bind_group: None,
            pass_descriptor: None,
        })
    }

    pub(crate) fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) {
        let entries = vec![BindGroupEntry::new(
            0,
            BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
        )];
        let descriptor = BindGroupDescriptor::new(&self.layout, Some("SSR Depth Copy"), entries);
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        self.pass_descriptor = Some(
            RenderPassDescriptor {
                label: Some("SSR Depth Copy Pass"),
                color_attachments: vec![],
                depth_stencil_attachment: Some(
                    DepthStencilAttachment::new(&ctx.render_texture_views.ssr_depth)
                        .with_depth_clear_value(0.0)
                        .with_depth_load_op(LoadOp::Clear)
                        .with_depth_store_op(StoreOp::Store),
                ),
                ..Default::default()
            }
            .into(),
        );
    }

    pub(crate) fn render(&self, ctx: &RenderContext) -> Result<()> {
        let Some(pipeline) = self.pipeline.ready() else {
            return Ok(());
        };
        let missing = || AwsmBindGroupError::NotFound("SSR Depth Copy".to_string());
        let descriptor = self.pass_descriptor.as_ref().ok_or_else(missing)?;
        let bind_group = self.bind_group.as_ref().ok_or_else(missing)?;
        let render_pass = ctx.command_encoder.begin_render_pass(descriptor)?;
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, bind_group, None)?;
        render_pass.draw(3);
        render_pass.end();
        Ok(())
    }
}
