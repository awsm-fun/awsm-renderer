//! Render entry points and render context.

use awsm_renderer_core::command::{
    color::Color,
    render_pass::{
        ColorAttachment, DepthStencilAttachment, RenderPassDescriptor, RenderPassEncoder,
    },
    CommandEncoder, LoadOp, StoreOp,
};
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use awsm_renderer_core::texture::blit::blit_tex;

use crate::anti_alias::AntiAliasing;
use crate::bind_groups::{BindGroupCreate, BindGroupRecreateContext, BindGroups};
use crate::error::{AwsmError, Result};
use crate::instances::Instances;
use crate::materials::Materials;
use crate::meshes::Meshes;
use crate::pipelines::Pipelines;
use crate::post_process::PostProcessing;
use crate::render_passes::RenderPasses;
use crate::render_textures::{RenderTextureViews, RenderTextures};
use crate::transforms::Transforms;
use crate::{AwsmRenderer, AwsmRendererLogging};

/// Optional callbacks around render passes.
#[derive(Default)]
pub struct RenderHooks {
    /// Runs before per-frame CPU->GPU writes and pass execution.
    pub pre_render: Option<Box<dyn Fn(&mut AwsmRenderer) -> Result<()>>>,
    /// Runs before geometry/light/material passes (advanced setup use-cases).
    pub first_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after geometry passes and before light culling/material opaque shading.
    ///
    /// Use this for advanced visibility-buffer extensions that need to contribute additional
    /// world-space opaque geometry.
    pub after_geometry_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after opaque->transparent blit and before world transparent materials.
    pub before_transparent_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after world transparent materials and before HUD transparent rendering.
    pub after_transparent_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after display pass and before command submission.
    pub last_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after command submission.
    pub post_render: Option<Box<dyn Fn(&mut AwsmRenderer) -> Result<()>>>,
}

impl AwsmRenderer {
    // this should only be called once per frame
    // the various underlying raw data can be updated on their own cadence
    // or just call .update_all() right before .render() for convenience
    /// Executes a full render with optional hooks.
    pub fn render(&mut self, hooks: Option<&RenderHooks>) -> Result<()> {
        if let Some(hook) = hooks.and_then(|h| h.pre_render.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings {
                    Some(tracing::span!(tracing::Level::INFO, "PreRender Hook").entered())
                } else {
                    None
                };
                hook(self)?;
            }
        }

        let _maybe_span_guard = if self.logging.render_timings {
            Some(tracing::span!(tracing::Level::INFO, "Render").entered())
        } else {
            None
        };

        self.render_textures.next_frame();

        self.transforms
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.materials
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.instances
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.meshes
            .skins
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.meshes
            .morphs
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.meshes
            .meta
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.textures.write_texture_transforms_gpu(
            &self.logging,
            &self.gpu,
            &mut self.bind_groups,
        )?;
        self.meshes
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.camera
            .write_gpu(&self.logging, &self.gpu, &self.bind_groups)?;
        // Shadows must fit cascades + populate the descriptor buffer
        // *before* the lights buffer is packed — `Lights::write_gpu`
        // queries `shadow_index_for` per-light and bakes the result
        // into `LightPacked.row4.z`.
        self.shadows.write_gpu(
            &self.logging,
            &self.gpu,
            &mut self.bind_groups,
            &self.camera,
            &self.lights,
            &self.meshes,
        )?;
        {
            let shadows = &self.shadows;
            self.lights
                .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups, |key| {
                    shadows.descriptor_index_for_light(key)
                })?;
        }

        let render_texture_views = self
            .render_textures
            .views(&self.gpu, self.anti_aliasing.clone())?;

        if render_texture_views.size_changed {
            self.bind_groups
                .mark_create(BindGroupCreate::TextureViewRecreate);
        }

        self.bind_groups.recreate(
            BindGroupRecreateContext {
                gpu: &self.gpu,
                render_texture_views: &render_texture_views,
                textures: &self.textures,
                materials: &self.materials,
                bind_group_layouts: &mut self.bind_group_layouts,
                meshes: &self.meshes,
                camera: &self.camera,
                environment: &self.environment,
                lights: &self.lights,
                transforms: &self.transforms,
                instances: &self.instances,
                anti_aliasing: &self.anti_aliasing,
                shadows: &self.shadows,
            },
            &mut self.render_passes,
            &mut self.picker,
        )?;

        let ctx = RenderContext {
            gpu: &self.gpu,
            command_encoder: self.gpu.create_command_encoder(Some("Rendering")),
            render_texture_views,
            logging: &self.logging,
            render_textures: &self.render_textures,
            transforms: &self.transforms,
            meshes: &self.meshes,
            materials: &self.materials,
            pipelines: &self.pipelines,
            instances: &self.instances,
            bind_groups: &self.bind_groups,
            render_passes: &self.render_passes,
            anti_aliasing: &self.anti_aliasing,
            post_processing: &self.post_processing,
            clear_color: &self._clear_color,
        };

        let renderables = self.collect_renderables(&ctx)?;

        if let Some(hook) = hooks.and_then(|h| h.first_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings {
                    Some(tracing::span!(tracing::Level::INFO, "FirstPass Hook").entered())
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Geometry RenderPass").entered())
            } else {
                None
            };

            self.render_passes
                .geometry
                .render(&ctx, &renderables.opaque, false)?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "HUD Geometry RenderPass").entered())
            } else {
                None
            };

            self.render_passes
                .geometry
                .render(&ctx, &renderables.hud, true)?;
        }

        if let Some(hook) = hooks.and_then(|h| h.after_geometry_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings {
                    Some(tracing::span!(tracing::Level::INFO, "AfterGeometryPass Hook").entered())
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        // Shadow generation pass — runs between the geometry passes
        // and light culling so the shading passes downstream sample
        // the freshly-written shadow maps. Short-circuits when there
        // are no active shadow casters.
        if self.shadows.any_active() {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Shadow Generation").entered())
            } else {
                None
            };
            crate::shadows::render_pass::record(&ctx, &self.shadows)?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Light Culling RenderPass").entered())
            } else {
                None
            };

            self.render_passes.light_culling.render(&ctx)?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Clear opaque").entered())
            } else {
                None
            };

            self.render_textures.clear_opaque(&self.gpu)?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Material Opaque RenderPass").entered())
            } else {
                None
            };

            self.render_passes
                .material_opaque
                .render(&ctx, renderables.opaque)?;
        }

        // Build the opaque RT mip chain when any visible transparent
        // material uses transmission. The transparent pass uses these
        // mips for hardware-filtered background sampling at refraction
        // points instead of a multi-tap blur. Skipped entirely on frames
        // with no transmissive material — they pay zero overhead.
        let scene_has_transmission = renderables
            .transparent
            .iter()
            .any(|r| self.materials.has_transmission(r.material_key()));
        if scene_has_transmission {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Opaque Mipgen").entered())
            } else {
                None
            };
            // Clone the texture handle and mip count out of the inner
            // borrow first; that drops the immutable `self.render_textures`
            // borrow before we take a mutable borrow on `self.opaque_mipgen`.
            // GpuTexture is a wasm-bindgen JS handle — `.clone()` is a
            // refcount bump, not a texture copy.
            let opaque_info = self
                .render_textures
                .inner()
                .map(|inner| (inner.opaque.clone(), inner.opaque_mip_count));
            // The mipgen caches per-mip views + bind groups across frames.
            // We invalidate explicitly when the render textures were just
            // recreated (resize / AA change) so the cache stays paired
            // with the right `GpuTexture` identity.
            if ctx.render_texture_views.size_changed {
                self.opaque_mipgen.invalidate();
            }
            if let Some((texture, mip_count)) = opaque_info {
                self.opaque_mipgen
                    .record(&self.gpu, &ctx.command_encoder, &texture, mip_count)?;
            }
        }

        {
            let _maybe_span_guard = if ctx.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Opaque to Transparent Blit").entered())
            } else {
                None
            };

            blit_tex(
                match &ctx.anti_aliasing.msaa_sample_count {
                    Some(sample_count) if *sample_count == 4 => {
                        &ctx.render_textures
                            .opaque_to_transparent_blit_pipeline_msaa_4
                    }
                    None => {
                        &ctx.render_textures
                            .opaque_to_transparent_blit_pipeline_no_anti_alias
                    }
                    Some(count) => {
                        return Err(AwsmError::UnsupportedMsaaCount(*count));
                    }
                },
                match &ctx.anti_aliasing.msaa_sample_count {
                    Some(sample_count) if *sample_count == 4 => {
                        &ctx.render_texture_views
                            .opaque_to_transparent_blit_bind_group_msaa_4
                    }
                    None => {
                        &ctx.render_texture_views
                            .opaque_to_transparent_blit_bind_group_no_anti_alias
                    }
                    Some(count) => {
                        return Err(AwsmError::UnsupportedMsaaCount(*count));
                    }
                },
                &ctx.render_texture_views.transparent,
                &ctx.command_encoder,
            )?;
        }

        // Built-in line render pass — must run after the opaque->transparent
        // blit (so depth + transparent target are populated) and before any
        // `before_transparent_pass` hook so editor overlays can draw on top.
        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Line RenderPass").entered())
            } else {
                None
            };
            self.lines.render(&ctx)?;
        }

        if let Some(hook) = hooks.and_then(|h| h.before_transparent_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings {
                    Some(
                        tracing::span!(tracing::Level::INFO, "BeforeTransparentPass Hook")
                            .entered(),
                    )
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(
                    tracing::span!(tracing::Level::INFO, "Material Transparent RenderPass")
                        .entered(),
                )
            } else {
                None
            };

            self.render_passes
                .material_transparent
                .render(&ctx, renderables.transparent, false)?;
        }

        if let Some(hook) = hooks.and_then(|h| h.after_transparent_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings {
                    Some(
                        tracing::span!(tracing::Level::INFO, "AfterTransparentPass Hook").entered(),
                    )
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "HUD RenderPass").entered())
            } else {
                None
            };

            self.render_passes
                .material_transparent
                .render(&ctx, renderables.hud, true)?;
        }

        // if None, it's handled by MSAA resolve in transparent pass
        if let Some(bind_group) = &ctx
            .render_texture_views
            .transparent_to_composite_blit_bind_group_no_anti_alias
        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(
                    tracing::span!(tracing::Level::INFO, "Non-antialised composite blit").entered(),
                )
            } else {
                None
            };

            blit_tex(
                &ctx.render_textures
                    .transparent_to_composite_blit_pipeline_no_anti_alias,
                bind_group,
                &ctx.render_texture_views.composite,
                &ctx.command_encoder,
            )?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Effects RenderPass").entered())
            } else {
                None
            };

            self.render_passes.effects.render(&ctx)?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Display RenderPass").entered())
            } else {
                None
            };

            self.render_passes.display.render(&ctx)?;
        }

        if let Some(hook) = hooks.and_then(|h| h.last_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings {
                    Some(tracing::span!(tracing::Level::INFO, "LastPass Hook").entered())
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        self.gpu.submit_commands(&ctx.command_encoder.finish());

        if let Some(hook) = hooks.and_then(|h| h.post_render.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings {
                    Some(tracing::span!(tracing::Level::INFO, "PostRender Hook").entered())
                } else {
                    None
                };
                hook(self)?;
            }
        }
        Ok(())
    }
}

/// Context passed to render passes during a frame.
pub struct RenderContext<'a> {
    pub gpu: &'a AwsmRendererWebGpu,
    pub command_encoder: CommandEncoder,
    pub render_texture_views: RenderTextureViews,
    pub logging: &'a AwsmRendererLogging,
    pub render_textures: &'a RenderTextures,
    pub transforms: &'a Transforms,
    pub meshes: &'a Meshes,
    pub pipelines: &'a Pipelines,
    pub materials: &'a Materials,
    pub instances: &'a Instances,
    pub bind_groups: &'a BindGroups,
    pub render_passes: &'a RenderPasses,
    pub anti_aliasing: &'a AntiAliasing,
    pub post_processing: &'a PostProcessing,
    pub clear_color: &'a Color,
}

impl<'a> RenderContext<'a> {
    /// Begins a visibility-buffer extension pass for world-space opaque geometry.
    ///
    /// This pass loads the existing visibility attachments and world depth, allowing custom hooks
    /// to append opaque geometry before light culling/material-opaque shading.
    pub fn begin_world_geometry_extension_pass(
        &'a self,
        label: Option<&'a str>,
    ) -> Result<RenderPassEncoder> {
        self.command_encoder
            .begin_render_pass(
                &RenderPassDescriptor {
                    label,
                    color_attachments: vec![
                        ColorAttachment::new(
                            &self.render_texture_views.visibility_data,
                            LoadOp::Load,
                            StoreOp::Store,
                        ),
                        ColorAttachment::new(
                            &self.render_texture_views.barycentric,
                            LoadOp::Load,
                            StoreOp::Store,
                        ),
                        ColorAttachment::new(
                            &self.render_texture_views.normal_tangent,
                            LoadOp::Load,
                            StoreOp::Store,
                        ),
                        ColorAttachment::new(
                            &self.render_texture_views.barycentric_derivatives,
                            LoadOp::Load,
                            StoreOp::Store,
                        ),
                    ],
                    depth_stencil_attachment: Some(
                        DepthStencilAttachment::new(&self.render_texture_views.depth)
                            .with_depth_load_op(LoadOp::Load)
                            .with_depth_store_op(StoreOp::Store),
                    ),
                    ..Default::default()
                }
                .into(),
            )
            .map_err(Into::into)
    }

    /// Begins a world-space transparent effect pass that targets the transparent color buffer and
    /// shared scene depth.
    pub fn begin_world_transparent_pass(
        &'a self,
        label: Option<&'a str>,
    ) -> Result<RenderPassEncoder> {
        let mut color_attachment = ColorAttachment::new(
            &self.render_texture_views.transparent,
            LoadOp::Load,
            StoreOp::Store,
        );

        if self.anti_aliasing.msaa_sample_count.is_some() {
            color_attachment =
                color_attachment.with_resolve_target(&self.render_texture_views.composite);
        }

        self.command_encoder
            .begin_render_pass(
                &RenderPassDescriptor {
                    label,
                    color_attachments: vec![color_attachment],
                    depth_stencil_attachment: Some(
                        DepthStencilAttachment::new(&self.render_texture_views.depth)
                            .with_depth_load_op(LoadOp::Load)
                            .with_depth_store_op(StoreOp::Store),
                    ),
                    ..Default::default()
                }
                .into(),
            )
            .map_err(Into::into)
    }

    /// Begins a HUD transparent pass using the shared transparent color target and HUD depth.
    ///
    /// This matches the renderer's built-in HUD pass behavior:
    /// depth is cleared to `1.0` and then depth-tested/written within HUD space.
    pub fn begin_hud_transparent_pass(
        &'a self,
        label: Option<&'a str>,
    ) -> Result<RenderPassEncoder> {
        let mut color_attachment = ColorAttachment::new(
            &self.render_texture_views.transparent,
            LoadOp::Load,
            StoreOp::Store,
        );

        if self.anti_aliasing.msaa_sample_count.is_some() {
            color_attachment =
                color_attachment.with_resolve_target(&self.render_texture_views.composite);
        }

        self.command_encoder
            .begin_render_pass(
                &RenderPassDescriptor {
                    label,
                    color_attachments: vec![color_attachment],
                    depth_stencil_attachment: Some(
                        DepthStencilAttachment::new(&self.render_texture_views.hud_depth)
                            .with_depth_load_op(LoadOp::Clear)
                            .with_depth_clear_value(1.0)
                            .with_depth_store_op(StoreOp::Store),
                    ),
                    ..Default::default()
                }
                .into(),
            )
            .map_err(Into::into)
    }

    /// Begins a pass that loads the already-rendered swapchain image.
    ///
    /// This is intended for `RenderHooks::last_pass` overlays, where you want to draw on top of
    /// the display output without clearing it.
    pub fn begin_display_overlay_pass(
        &'a self,
        label: Option<&'a str>,
    ) -> Result<RenderPassEncoder> {
        self.command_encoder
            .begin_render_pass(
                &RenderPassDescriptor {
                    label,
                    color_attachments: vec![ColorAttachment::new(
                        &self.gpu.current_context_texture_view()?,
                        LoadOp::Load,
                        StoreOp::Store,
                    )],
                    ..Default::default()
                }
                .into(),
            )
            .map_err(Into::into)
    }
}
