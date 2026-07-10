//! Decal composite pass.
//!
//! After the decal compute writes its per-pixel result into
//! `decal_color`, this pass alpha-blits the touched pixels onto the
//! frame's `transparent` target. The decal compute marks touched
//! pixels with `alpha = 1.0` (rgb = composited color) and untouched
//! pixels with `alpha = 0.0`; the composite's fragment shader uses
//! `discard` to skip the latter, preserving whatever was already in
//! `transparent` (e.g. the opaque→transparent blit's contents).
//!
//! Two pipeline variants are kept so the same composite serves both
//! the MSAA-off (single-sample render attachment) and MSAA-4 paths.
//! On MSAA-4 a single fragment-shader output broadcasts to all four
//! samples — that's the standard MSAA fragment-stage behavior.

use std::borrow::Cow;

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
        BindGroupLayoutResource, BindGroupResource, TextureBindingLayout,
    },
    command::{
        render_pass::{ColorAttachment, RenderPassDescriptor},
        LoadOp, StoreOp,
    },
    pipeline::{
        fragment::{ColorTargetState, FragmentState},
        layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
        multisample::MultisampleState,
        primitive::PrimitiveState,
        vertex::VertexState,
        RenderPipelineDescriptor,
    },
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    texture::{TextureSampleType, TextureViewDimension},
};

use crate::{
    bind_groups::{AwsmBindGroupError, BindGroupRecreateContext},
    error::Result,
    render::RenderContext,
    render_passes::RenderPassInitContext,
};

const SHADER_SOURCE: &str = r#"
@group(0) @binding(0) var decal_color: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vert_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    return out;
}

@fragment
fn frag_main(in: VsOut) -> @location(0) vec4<f32> {
    let coords = vec2<i32>(in.pos.xy);
    let c = textureLoad(decal_color, coords, 0);
    // alpha < 0.5 marks "no decal touched this pixel" — preserve
    // whatever's already in the transparent target.
    if (c.a < 0.5) {
        discard;
    }
    return vec4<f32>(c.rgb, 1.0);
}
"#;

pub struct MaterialDecalComposite {
    bind_group_layout: web_sys::GpuBindGroupLayout,
    pipeline_singlesampled: web_sys::GpuRenderPipeline,
    pipeline_multisampled: web_sys::GpuRenderPipeline,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl MaterialDecalComposite {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        Self::new_static(ctx.gpu.clone(), ctx.render_texture_formats.color).await
    }

    /// `'static` constructor for the render-loop kick path
    /// ([`MaterialDecalRenderPass::kick_compile`](crate::render_passes::material_decal::render_pass::MaterialDecalRenderPass::kick_compile)):
    /// takes only a cloned gpu handle + the color format BY VALUE, so the
    /// returned future is `'static` — it can be stored on the pass and polled
    /// across frames (`now_or_never`) without borrowing the renderer.
    pub async fn new_static(
        gpu: awsm_renderer_core::renderer::AwsmRendererWebGpu,
        format: awsm_renderer_core::texture::TextureFormat,
    ) -> Result<Self> {
        Self::new_inner(&gpu, format).await
    }

    async fn new_inner(
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        format: awsm_renderer_core::texture::TextureFormat,
    ) -> Result<Self> {
        let shader_module = gpu.compile_shader(
            &ShaderModuleDescriptor::new(SHADER_SOURCE, Some("Decal Composite shader")).into(),
        );
        shader_module.validate_shader().await?;

        let bind_group_layout = gpu.create_bind_group_layout(
            &BindGroupLayoutDescriptor::new(Some("Decal Composite BGL"))
                .with_entries(vec![BindGroupLayoutEntry::new(
                    0,
                    BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_sample_type(TextureSampleType::UnfilterableFloat)
                            .with_view_dimension(TextureViewDimension::N2d),
                    ),
                )
                .with_visibility_fragment()])
                .into(),
        )?;

        let pipeline_layout = gpu.create_pipeline_layout(
            &PipelineLayoutDescriptor::new(
                Some("Decal Composite Layout"),
                vec![bind_group_layout.clone()],
            )
            .into(),
        );

        // Build both descriptors first, then issue both Promises
        // back-to-back via `create_render_pipeline_promise` so Dawn's
        // compile pool parallelises the two compiles. We bypass
        // `RenderPipelines::ensure_keys` here because this composite
        // owns its own bind-group layout + pipeline layout (not in
        // any of the cache crates) — wiring it through the cache
        // would be more work than the parallelisation it gains.
        let descriptor_singlesampled = {
            let vertex = VertexState::new(&shader_module, None);
            let fragment =
                FragmentState::new(&shader_module, None, vec![ColorTargetState::new(format)]);
            let descriptor = RenderPipelineDescriptor::new(vertex, Some("Decal Composite (1x)"))
                .with_primitive(PrimitiveState::new())
                .with_layout(PipelineLayoutKind::Custom(&pipeline_layout))
                .with_fragment(fragment);
            web_sys::GpuRenderPipelineDescriptor::from(descriptor)
        };
        let descriptor_multisampled = {
            let vertex = VertexState::new(&shader_module, None);
            let fragment =
                FragmentState::new(&shader_module, None, vec![ColorTargetState::new(format)]);
            let descriptor = RenderPipelineDescriptor::new(vertex, Some("Decal Composite (4x)"))
                .with_primitive(PrimitiveState::new())
                .with_layout(PipelineLayoutKind::Custom(&pipeline_layout))
                .with_fragment(fragment)
                .with_multisample(MultisampleState::new().with_count(4));
            web_sys::GpuRenderPipelineDescriptor::from(descriptor)
        };
        let promise_singlesampled = wasm_bindgen_futures::JsFuture::from(
            gpu.create_render_pipeline_promise(&descriptor_singlesampled),
        );
        let promise_multisampled = wasm_bindgen_futures::JsFuture::from(
            gpu.create_render_pipeline_promise(&descriptor_multisampled),
        );
        let (pipeline_singlesampled, pipeline_multisampled) =
            futures::future::try_join(promise_singlesampled, promise_multisampled)
                .await
                .map_err(awsm_renderer_core::error::AwsmCoreError::pipeline_creation)?;
        let pipeline_singlesampled: web_sys::GpuRenderPipeline =
            wasm_bindgen::JsCast::unchecked_into(pipeline_singlesampled);
        let pipeline_multisampled: web_sys::GpuRenderPipeline =
            wasm_bindgen::JsCast::unchecked_into(pipeline_multisampled);

        Ok(Self {
            bind_group_layout,
            pipeline_singlesampled,
            pipeline_multisampled,
            bind_group: None,
        })
    }

    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Decal Composite".to_string()))
    }

    /// Rebuilds the composite's bind group against the live
    /// `decal_color` view. Called on `TextureViewRecreate`. Only
    /// invoked when `features.decals` is on, so the `decal_color`
    /// view is always `Some` here.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let decal_color_view = ctx
            .render_texture_views
            .decal_color
            .as_ref()
            .expect("decal_color view missing despite decals feature on");
        let entries = vec![BindGroupEntry::new(
            0,
            BindGroupResource::TextureView(Cow::Borrowed(decal_color_view)),
        )];
        let descriptor =
            BindGroupDescriptor::new(&self.bind_group_layout, Some("Decal Composite"), entries);
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }

    /// Records the composite render pass — fullscreen triangle, no
    /// vertex buffer; the per-fragment `discard` preserves untouched
    /// pixels of `transparent`.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        // A freshly (lazily) installed composite hasn't had its bind group
        // created yet — that happens at the next `TextureViewRecreate` drain
        // (the install paths mark it, so this lasts at most one frame). Skip
        // gracefully rather than erroring: a returned Err here would abandon
        // the WHOLE frame's encoder (black screen), which is a far worse
        // failure than the decal not compositing for a frame.
        let Some(bind_group) = self.bind_group.as_ref() else {
            tracing::debug!(
                "decal composite: bind group not yet created (pending TextureViewRecreate) — skipping this frame"
            );
            return Ok(());
        };
        let pipeline = if ctx.anti_aliasing.msaa_sample_count.is_some() {
            &self.pipeline_multisampled
        } else {
            &self.pipeline_singlesampled
        };

        let render_pass = ctx.command_encoder.begin_render_pass(
            &RenderPassDescriptor {
                label: Some("Decal Composite Pass"),
                color_attachments: vec![ColorAttachment::new(
                    &ctx.render_texture_views.transparent,
                    LoadOp::Load,
                    StoreOp::Store,
                )],
                depth_stencil_attachment: None,
                ..Default::default()
            }
            .into(),
        )?;
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, bind_group, None)?;
        render_pass.draw(3);
        render_pass.end();
        Ok(())
    }
}
