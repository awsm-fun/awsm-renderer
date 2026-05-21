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
use crate::scene_spatial::SceneSpatial;
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
        // Per-mesh light slice path (Option F follow-up to Cluster
        // 2.1.c). Patches slice fields into each affected mesh's
        // MaterialMeshMeta and uploads the packed indices buffer.
        // MUST run BEFORE `meshes.meta.write_gpu` so the slice patches
        // land in the same meta upload.
        self.mesh_light_indices_gpu.write_gpu(
            &self.gpu,
            &self.light_buckets,
            &self.lights,
            &mut self.meshes,
            &mut self.bind_groups,
        )?;
        // Decals (Cluster 6.4) — upload per-decal data if anything
        // changed since last frame. No-op when no decals exist.
        self.decals.write_gpu(&self.gpu, &mut self.bind_groups)?;
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
            &self.bind_group_layouts,
            &mut self.bind_groups,
            &self.camera,
            &self.lights,
            &self.scene_spatial,
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

        // Resize the HZB texture to match the live viewport. This
        // recreates the per-mip views, so the HZB bind groups must
        // also be rebuilt — the `TextureViewRecreate` event above
        // covers that since size_changed implies viewport resize.
        if self
            .render_passes
            .hzb
            .ensure_size(&self.gpu, render_texture_views.width, render_texture_views.height)?
        {
            self.bind_groups
                .mark_create(BindGroupCreate::TextureViewRecreate);
        }

        // Classify buckets are sized to fit the current viewport's
        // tile count. The grow-with-2x path keeps the reallocation
        // away from the steady-state per-frame work. Reset the header
        // every frame so the atomic counters start at 0.
        let tile_count = render_texture_views
            .width
            .div_ceil(8)
            .saturating_mul(render_texture_views.height.div_ceil(8));
        if self
            .material_classify_buffers
            .ensure_capacity(&self.gpu, tile_count)?
        {
            self.bind_groups
                .mark_create(BindGroupCreate::MaterialClassifyBuffersResize);
        }
        self.material_classify_buffers.reset_header(&self.gpu)?;

        // Build a snapshot of the active mesh count so we can size the
        // occlusion-cull buffers before bind groups are recreated.
        // Refining this to the actual opaque-renderable count requires
        // `collect_renderables` which runs later; this upper bound is
        // fine for capacity planning.
        let occlusion_needed = self.meshes.len() as u32;
        if self
            .occlusion_buffers
            .ensure_capacity(&self.gpu, occlusion_needed)?
        {
            self.bind_groups
                .mark_create(BindGroupCreate::OcclusionBuffersResize);
        }

        // §16.4.C: decal classify buckets sized to viewport tile count.
        let decal_tile_x = render_texture_views.width.div_ceil(8);
        let decal_tile_y = render_texture_views.height.div_ceil(8);
        if self
            .decal_classify_buffers
            .ensure_capacity(&self.gpu, decal_tile_x, decal_tile_y)?
        {
            self.bind_groups
                .mark_create(BindGroupCreate::DecalClassifyBuffersResize);
        }
        // Reset the per-tile atomic counts every frame so classify
        // starts against an empty bucket set.
        self.decal_classify_buffers.reset(&self.gpu)?;

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
                mesh_light_indices_gpu: &self.mesh_light_indices_gpu,
                material_classify_buffers: &self.material_classify_buffers,
                decals: &self.decals,
                occlusion_buffers: &self.occlusion_buffers,
                hzb_full_view: self.render_passes.hzb.texture.view_all.clone(),
                decal_classify_buffers: &self.decal_classify_buffers,
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
            scene_spatial: &self.scene_spatial,
            material_classify_buffers: &self.material_classify_buffers,
        };

        let renderables = self.collect_renderables(&ctx)?;

        // Snapshot the opaque renderables' world AABBs while we still
        // hold the borrow (`renderables.opaque` is consumed by the
        // material-opaque pass below). The occlusion-cull pass uses
        // this snapshot once HZB is built.
        let occlusion_aabbs: Vec<crate::bounds::Aabb> = renderables
            .opaque
            .iter()
            .filter_map(|r| r.world_aabb().cloned())
            .collect();

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

        // Material classify: per-tile scan of the visibility buffer
        // produces the indirect-dispatch args + tile buckets the
        // opaque pipelines consume below. Runs once per frame; cheap
        // (~few hundred microseconds on a 4K viewport).
        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Material Classify RenderPass").entered())
            } else {
                None
            };
            self.render_passes.material_classify.render(&ctx)?;
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

        // Projection decals (Cluster 6.4). Runs after the blit so
        // `transparent_tex` already holds the opaque shading result;
        // the decal pass overwrites the small subset of pixels its
        // volumes cover with the alpha-blended composite, leaving
        // every other pixel as the blit produced it. No-op when no
        // decals are active or MSAA is on (the v1 path doesn't have
        // a multisampled storage-binding target — see
        // `MaterialDecalRenderPass::render`).
        {
            let _maybe_span_guard = if ctx.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Material Decal RenderPass").entered())
            } else {
                None
            };
            self.render_passes
                .material_decal
                .render(&ctx, &self.decals)?;
        }

        // HZB build (Cluster 7.1, plan §16.6). Runs after opaque /
        // decal so the depth buffer holds the final scene depth.
        // Consumed by the occlusion-cull pass below.
        {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "HZB RenderPass").entered())
            } else {
                None
            };
            self.render_passes.hzb.render(&ctx)?;
        }

        // Occlusion cull (Cluster 7.2 / §16.7 Phase 1). Pack the
        // active opaque renderables' world AABBs into the GPU instance
        // buffer, then dispatch a compute shader that frustum + HZB
        // tests each. v1 doesn't *consume* the output yet — Phase 2
        // splits the geometry pass into survivor halves and gates
        // `drawIndirect` against this.
        let occlusion_instance_count = {
            let stride =
                crate::render_passes::occlusion::buffers::OCCLUSION_INSTANCE_STRIDE;
            let mut bytes: Vec<u8> = Vec::with_capacity(occlusion_aabbs.len() * stride);
            for aabb in &occlusion_aabbs {
                bytes.extend_from_slice(&aabb.min.x.to_le_bytes());
                bytes.extend_from_slice(&aabb.min.y.to_le_bytes());
                bytes.extend_from_slice(&aabb.min.z.to_le_bytes());
                bytes.extend_from_slice(&0u32.to_le_bytes()); // _pad0
                bytes.extend_from_slice(&aabb.max.x.to_le_bytes());
                bytes.extend_from_slice(&aabb.max.y.to_le_bytes());
                bytes.extend_from_slice(&aabb.max.z.to_le_bytes());
                bytes.extend_from_slice(&0u32.to_le_bytes()); // _pad1
                bytes.extend_from_slice(&0u32.to_le_bytes()); // mesh_meta_offset (unused in v1)
                bytes.extend_from_slice(&0u32.to_le_bytes()); // instance_attr_base
                bytes.extend_from_slice(&0u32.to_le_bytes()); // last_frame_visible
                bytes.extend_from_slice(&0u32.to_le_bytes()); // _pad2
            }
            let count = (bytes.len() / stride) as u32;
            if count > 0 {
                self.gpu.write_buffer(
                    &self.occlusion_buffers.instances_buffer,
                    None,
                    bytes.as_slice(),
                    None,
                    None,
                )?;
            }
            count
        };
        if occlusion_instance_count > 0 {
            let _maybe_span_guard = if self.logging.render_timings {
                Some(
                    tracing::span!(
                        tracing::Level::INFO,
                        "Occlusion Cull RenderPass",
                        instances = occlusion_instance_count
                    )
                    .entered(),
                )
            } else {
                None
            };
            self.render_passes
                .occlusion
                .render(&ctx, occlusion_instance_count)?;
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
    /// Renderer-owned spatial index. Per-pass culling (camera + shadow)
    /// descends through this instead of walking `meshes` linearly.
    pub scene_spatial: &'a SceneSpatial,
    /// Classify-pass output (Cluster 6.1). The opaque material pass
    /// uses this buffer both as a storage binding (for the per-bucket
    /// tile lookup) and as the indirect-args source for
    /// `dispatchWorkgroupsIndirect`.
    pub material_classify_buffers:
        &'a crate::render_passes::material_classify::buffers::ClassifyBuffers,
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
