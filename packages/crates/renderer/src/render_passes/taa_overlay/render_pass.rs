//! The TAA fallback's deforming-mesh velocity overlay: layouts, pipelines,
//! group 2 and the draw (see the module docs in `mod.rs`).

use std::borrow::Cow;
use std::collections::HashMap;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, StorageTextureAccess, StorageTextureBindingLayout,
    TextureBindingLayout,
};
use awsm_renderer_core::buffers::{BufferBinding, BufferDescriptor, BufferUsage};
use awsm_renderer_core::command::render_pass::{DepthStencilAttachment, RenderPassDescriptor};
use awsm_renderer_core::compare::CompareFunction;
use awsm_renderer_core::pipeline::depth_stencil::DepthStencilState;
use awsm_renderer_core::pipeline::primitive::{
    CullMode, FrontFace, PrimitiveState, PrimitiveTopology,
};
use awsm_renderer_core::texture::{TextureFormat, TextureSampleType, TextureViewDimension};
use awsm_renderer_materials::MaterialShaderId;

use crate::anti_alias::TaaFallbackOverlay;
use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::meshes::meta::geometry_meta::GEOMETRY_MESH_META_BYTE_ALIGNMENT;
use crate::meshes::meta::material_meta::MATERIAL_MESH_META_BYTE_ALIGNMENT;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::render::RenderContext;
use crate::render_passes::geometry::bind_group::{GeometryBindGroups, GeometryMetaGroups};
use crate::render_passes::geometry::custom_vertex_pipeline::uv0_vertex_buffer_layout;
use crate::render_passes::geometry::masked_bind_group::GeometryMaskedBindGroup;
use crate::render_passes::geometry::pipeline::{
    GeometryTargets, PREV_INSTANCE_VERTEX_SLOT, VERTEX_BUFFER_LAYOUT,
    VERTEX_BUFFER_LAYOUT_INSTANCING, VERTEX_BUFFER_LAYOUT_PREV_INSTANCING,
};
use crate::render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo;
use crate::render_passes::taa_overlay::shader::cache_key::{
    ShaderCacheKeyTaaOverlay, TaaOverlayCustomVertex,
};
use crate::render_passes::RenderPassInitContext;
use crate::renderable::Renderable;

/// Counters per material-meta slot in the debug check (see the fragment).
const CHECK_WORDS_PER_SLOT: usize = 3;

/// The three draw shapes of the plain (no vertex hook) overlay, as in the
/// geometry pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Shape {
    /// Non-instanced, mesh meta from the storage array
    /// (`indirect-first-instance`).
    StorageMeta,
    /// Non-instanced, uniform meta with a dynamic offset (also every masked
    /// mesh, as in the geometry pass).
    UniformMeta,
    /// Instanced (uniform meta), with last frame's instance transforms.
    Instancing,
}

const SHAPES: [Shape; 3] = [Shape::StorageMeta, Shape::UniformMeta, Shape::Instancing];

/// The geometry pass draws single-sided meshes with back faces culled and
/// double-sided ones with none (`collect_renderables`). The overlay copies
/// that only to cull work early: a fragment of a face the geometry pass did
/// not keep fails the identity test anyway.
const CULL_MODES: [CullMode; 2] = [CullMode::None, CullMode::Back];

fn cull_index(cull: CullMode) -> usize {
    if cull == CullMode::None {
        0
    } else {
        1
    }
}

/// Group 2 of the overlay: the geometry meta (as the geometry pass binds it)
/// plus the identity targets, the velocity texture, and in the debug check
/// the counters and the depth texture.
pub struct TaaOverlayMetaGroups {
    storage_layout_key: BindGroupLayoutKey,
    uniform_layout_key: BindGroupLayoutKey,
    storage: Option<web_sys::GpuBindGroup>,
    uniform: Option<web_sys::GpuBindGroup>,
}

impl GeometryMetaGroups for TaaOverlayMetaGroups {
    fn storage_meta_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.storage
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("TAA overlay (storage meta)".to_string()))
    }
    fn uniform_meta_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.uniform
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("TAA overlay (uniform meta)".to_string()))
    }
}

fn group2_layout(storage_meta: bool, debug: bool) -> BindGroupLayoutCacheKey {
    let fragment = |resource| BindGroupLayoutCacheKeyEntry {
        resource,
        visibility_vertex: false,
        visibility_fragment: true,
        visibility_compute: false,
    };
    let meta = if storage_meta {
        BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage)
    } else {
        BufferBindingLayout::new()
            .with_binding_type(BufferBindingType::Uniform)
            .with_dynamic_offset(true)
    };
    let uint_texture = || {
        BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Uint),
        )
    };
    let mut entries = vec![
        // 0: the mesh meta, read by the vertex stage only.
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(meta),
            visibility_vertex: true,
            visibility_fragment: false,
            visibility_compute: false,
        },
        // 1: visibility_data, 2: barycentric (the identity the geometry pass
        // wrote).
        fragment(uint_texture()),
        fragment(uint_texture()),
        // 3: the prep pass's velocity texture.
        fragment(BindGroupLayoutResource::StorageTexture(
            StorageTextureBindingLayout::new(TextureFormat::Rg32float)
                .with_view_dimension(TextureViewDimension::N2d)
                .with_access(StorageTextureAccess::WriteOnly),
        )),
    ];
    if debug {
        // 4: the check counters, 5: depth (also the pass's read-only
        // attachment).
        entries.push(fragment(BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
        )));
        entries.push(fragment(BindGroupLayoutResource::Texture(
            TextureBindingLayout::new()
                .with_view_dimension(TextureViewDimension::N2d)
                .with_sample_type(TextureSampleType::Depth),
        )));
    }
    BindGroupLayoutCacheKey { entries }
}

/// The debug check's state (`AwsmRenderer::set_taa_debug_overlay_check`).
struct DebugCheck {
    /// `CHECK_WORDS_PER_SLOT` u32 per material-meta slot.
    counts: std::cell::RefCell<Option<(web_sys::GpuBuffer, usize)>>,
    /// The meshes the last frame's overlay drew.
    drawn: std::cell::RefCell<Vec<crate::meshes::MeshKey>>,
}

/// One mesh in [`crate::AwsmRenderer::debug_read_taa_overlay_check`].
#[derive(Clone, Debug)]
pub struct TaaOverlayMeshCheck {
    pub mesh: crate::meshes::MeshKey,
    /// Pixels whose vector the overlay wrote for this mesh.
    pub written: u32,
    /// Of those, pixels where the overlay's fragment depth differs from the
    /// stored depth (what a depth-EQUAL test would have lost).
    pub depth_not_equal: u32,
    /// Of those, pixels whose fragment fails the production depth compare
    /// against the stored depth (what the production pipeline loses).
    pub depth_compare_fails: u32,
}

pub struct TaaOverlayRenderPass {
    tier: TaaFallbackOverlay,
    meta: TaaOverlayMetaGroups,
    plain_layout_storage: PipelineLayoutKey,
    plain_layout_uniform: PipelineLayoutKey,
    custom_layout: Option<PipelineLayoutKey>,
    /// `[shape][cull]`.
    plain: HashMap<(Shape, usize), RenderPipelineKey>,
    /// Per vertex-hook material (canonical shader id) and cull.
    custom: HashMap<(MaterialShaderId, usize), RenderPipelineKey>,
    debug: Option<DebugCheck>,
    /// Cost measurement only (`AwsmRenderer::set_taa_debug_overlay_repeat`):
    /// draws of the pass per frame, 1 normally. Every repeat stores the same
    /// vectors.
    debug_repeat: std::cell::Cell<u32>,
}

impl TaaOverlayRenderPass {
    /// Builds the layouts and the plain pipelines (6). Vertex-hook variants
    /// compile per material ([`Self::ensure_custom_vertex_variant`]).
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        geometry_bind_groups: &GeometryBindGroups,
        masked_bind_group: &GeometryMaskedBindGroup,
        tier: TaaFallbackOverlay,
        debug: bool,
    ) -> Result<Self> {
        let storage_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, group2_layout(true, debug))?;
        let uniform_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, group2_layout(false, debug))?;
        let (transforms_layout, animation_layout) =
            geometry_bind_groups.layout_keys(ctx, GeometryTargets::Velocity)?;
        let layout = |ctx: &mut RenderPassInitContext<'_>, group0, group2| {
            ctx.pipeline_layouts.get_key(
                ctx.gpu,
                ctx.bind_group_layouts,
                PipelineLayoutCacheKey::new(vec![
                    group0,
                    transforms_layout,
                    group2,
                    animation_layout,
                ]),
            )
        };
        let plain_layout_storage = layout(
            ctx,
            geometry_bind_groups.camera.bind_group_layout_key,
            storage_layout_key,
        )?;
        let plain_layout_uniform = layout(
            ctx,
            geometry_bind_groups.camera.bind_group_layout_key,
            uniform_layout_key,
        )?;
        let custom_layout = if tier.vertex_hooks() {
            Some(layout(
                ctx,
                masked_bind_group.bind_group_layout_key,
                uniform_layout_key,
            )?)
        } else {
            None
        };

        let mut pass = Self {
            tier,
            meta: TaaOverlayMetaGroups {
                storage_layout_key,
                uniform_layout_key,
                storage: None,
                uniform: None,
            },
            plain_layout_storage,
            plain_layout_uniform,
            custom_layout,
            plain: HashMap::new(),
            custom: HashMap::new(),
            debug: debug.then(|| DebugCheck {
                counts: std::cell::RefCell::new(None),
                drawn: std::cell::RefCell::new(Vec::new()),
            }),
            debug_repeat: std::cell::Cell::new(1),
        };

        let mut shader_keys = Vec::new();
        for shape in SHAPES {
            shader_keys.push(pass.shader_key(
                ctx,
                shape_instancing(shape),
                shape == Shape::StorageMeta,
                None,
            ));
        }
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                shader_keys
                    .iter()
                    .cloned()
                    .map(crate::shaders::ShaderCacheKey::from)
                    .collect::<Vec<_>>(),
            )
            .await?;
        let mut cache_keys = Vec::new();
        let mut slots = Vec::new();
        for (shape, shader_cache) in SHAPES.into_iter().zip(shader_keys) {
            let shader = ctx.shaders.get_key(ctx.gpu, shader_cache).await?;
            let layout = if shape == Shape::StorageMeta {
                pass.plain_layout_storage
            } else {
                pass.plain_layout_uniform
            };
            for cull in CULL_MODES {
                cache_keys.push(pass.pipeline_cache_key(
                    ctx,
                    shader,
                    layout,
                    shape == Shape::Instancing,
                    false,
                    cull,
                ));
                slots.push((shape, cull_index(cull)));
            }
        }
        let keys = ctx
            .pipelines
            .render
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;
        pass.plain = slots.into_iter().zip(keys).collect();
        Ok(pass)
    }

    /// Which deforming meshes this pass covers.
    pub fn tier(&self) -> TaaFallbackOverlay {
        self.tier
    }

    /// Built as the debug check variant.
    pub fn debug(&self) -> bool {
        self.debug.is_some()
    }

    fn shader_key(
        &self,
        ctx: &RenderPassInitContext<'_>,
        instancing_transforms: bool,
        meta_storage_array: bool,
        custom_vertex: Option<TaaOverlayCustomVertex>,
    ) -> ShaderCacheKeyTaaOverlay {
        ShaderCacheKeyTaaOverlay {
            instancing_transforms,
            meta_storage_array,
            custom_vertex,
            debug: self.debug.is_some(),
            reverse_z: ctx.features.reverse_z,
        }
    }

    fn pipeline_cache_key(
        &self,
        ctx: &RenderPassInitContext<'_>,
        shader: crate::shaders::ShaderKey,
        layout: PipelineLayoutKey,
        instancing: bool,
        custom_vertex: bool,
        cull: CullMode,
    ) -> RenderPipelineCacheKey {
        let primitive = PrimitiveState::new()
            .with_topology(PrimitiveTopology::TriangleList)
            .with_front_face(FrontFace::Ccw)
            .with_cull_mode(cull);
        // Read-only depth, the main camera's compare: culls what is behind the
        // stored surface early. The identity test decides what is written, so
        // a fragment a few ulps behind its own stored depth would only be lost
        // to this test (measured by the debug check, which compares in the
        // shader and so tests `always` here).
        let depth_compare = if self.debug.is_some() {
            CompareFunction::Always
        } else {
            ctx.features.depth().compare()
        };
        let depth = DepthStencilState::new(ctx.render_texture_formats.depth)
            .with_depth_write_enabled(false)
            .with_depth_compare(depth_compare);
        let mut key = RenderPipelineCacheKey::new(shader, layout)
            .with_primitive(primitive)
            .with_depth_stencil(depth)
            .with_push_vertex_buffer_layout(VERTEX_BUFFER_LAYOUT.clone())
            .with_force_fragment_stage();
        if instancing {
            key = key.with_push_vertex_buffer_layout(VERTEX_BUFFER_LAYOUT_INSTANCING.clone());
            debug_assert_eq!(PREV_INSTANCE_VERTEX_SLOT, 2);
            key = key.with_push_vertex_buffer_layout(VERTEX_BUFFER_LAYOUT_PREV_INSTANCING.clone());
        }
        if custom_vertex {
            // Slot 1, as the geometry pass's custom-vertex draw binds it.
            key = key.with_push_vertex_buffer_layout(uv0_vertex_buffer_layout());
        }
        key
    }

    /// A vertex-hook material has its overlay variant (both cull modes).
    pub fn has_custom_vertex_variant(&self, shader_id: MaterialShaderId) -> bool {
        self.custom.contains_key(&(shader_id, 0))
    }

    /// The masked group-0 layout changed (texture pool growth): rebuild the
    /// vertex-hook layout and forget the variants (the caller recompiles
    /// them).
    pub fn relayout_custom_vertex(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        geometry_bind_groups: &GeometryBindGroups,
        masked_bind_group: &GeometryMaskedBindGroup,
    ) -> Result<()> {
        self.custom.clear();
        if !self.tier.vertex_hooks() {
            return Ok(());
        }
        let (transforms_layout, animation_layout) =
            geometry_bind_groups.layout_keys(ctx, GeometryTargets::Velocity)?;
        self.custom_layout = Some(ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                masked_bind_group.bind_group_layout_key,
                transforms_layout,
                self.meta.uniform_layout_key,
                animation_layout,
            ]),
        )?);
        Ok(())
    }

    /// Compiles the overlay variant of a vertex-hook material (no-op when the
    /// device's storage-buffer limit leaves hooks reactive).
    pub async fn ensure_custom_vertex_variant(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &GeometryMaskedBindGroup,
        shader_id: MaterialShaderId,
        dynamic_vertex: &DynamicVertexShaderInfo,
    ) -> Result<()> {
        let Some(layout) = self.custom_layout.filter(|_| self.tier.vertex_hooks()) else {
            return Ok(());
        };
        // The fragment stage sees the masked group 0's texture-pool arrays
        // plus the overlay's two identity targets. Past the device's sampled-
        // texture limit the hook stays reactive (no variant, so not covered).
        let sampled = masked_bind_group.texture_pool_arrays_len + 2;
        if sampled
            > ctx
                .gpu
                .device
                .limits()
                .max_sampled_textures_per_shader_stage()
        {
            return Ok(());
        }
        let shader_cache = self.shader_key(
            ctx,
            false,
            false,
            Some(TaaOverlayCustomVertex {
                shader_id,
                dynamic_vertex: dynamic_vertex.clone(),
                texture_pool_arrays_len: masked_bind_group.texture_pool_arrays_len,
                texture_pool_samplers_len: masked_bind_group.texture_pool_samplers_len,
            }),
        );
        ctx.shaders
            .ensure_keys(ctx.gpu, vec![shader_cache.clone().into()])
            .await?;
        let shader = ctx.shaders.get_key(ctx.gpu, shader_cache).await?;
        let cache_keys: Vec<RenderPipelineCacheKey> = CULL_MODES
            .iter()
            .map(|&cull| self.pipeline_cache_key(ctx, shader, layout, false, true, cull))
            .collect();
        let keys = ctx
            .pipelines
            .render
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;
        for (cull, key) in CULL_MODES.iter().zip(keys) {
            self.custom.insert((shader_id, cull_index(*cull)), key);
        }
        Ok(())
    }

    /// Every pipeline the pass holds (released when it is dropped).
    pub(crate) fn pipelines(&self) -> Vec<RenderPipelineKey> {
        self.plain
            .values()
            .chain(self.custom.values())
            .copied()
            .collect()
    }

    /// Group 2 (both meta shapes).
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let views = &ctx.render_texture_views;
        let Some(velocity) = views.velocity.as_ref().filter(|_| views.prep_velocity) else {
            // TAA is leaving the fallback; the pass is dropped by
            // `set_anti_aliasing`.
            self.meta.storage = None;
            self.meta.uniform = None;
            return Ok(());
        };
        let counts = self.debug.as_ref().map(|d| {
            // One slot per material-meta slot, sized to the meta buffer.
            let slots = (ctx.meshes.meta.material_gpu_buffer().size() as usize)
                / MATERIAL_MESH_META_BYTE_ALIGNMENT;
            let bytes = (slots.max(1) * CHECK_WORDS_PER_SLOT * 4).max(16);
            let mut counts = d.counts.borrow_mut();
            if counts.as_ref().is_none_or(|(_, size)| *size < bytes) {
                if let Some((old, _)) = counts.take() {
                    old.destroy();
                }
                let buffer = ctx
                    .gpu
                    .create_buffer(
                        &BufferDescriptor::new(
                            Some("TAA Overlay Check"),
                            bytes,
                            BufferUsage::new()
                                .with_storage()
                                .with_copy_src()
                                .with_copy_dst(),
                        )
                        .into(),
                    )
                    .ok();
                *counts = buffer.map(|b| (b, bytes));
            }
            counts.as_ref().map(|(b, _)| b.clone())
        });
        for storage_meta in [true, false] {
            let layout_key = if storage_meta {
                self.meta.storage_layout_key
            } else {
                self.meta.uniform_layout_key
            };
            let meta_buffer = ctx.meshes.meta.geometry_gpu_buffer();
            let meta = if storage_meta {
                BufferBinding::new(meta_buffer)
            } else {
                BufferBinding::new(meta_buffer).with_size(GEOMETRY_MESH_META_BYTE_ALIGNMENT)
            };
            let mut entries = vec![
                BindGroupEntry::new(0, BindGroupResource::Buffer(meta)),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::TextureView(Cow::Borrowed(&views.visibility_data)),
                ),
                BindGroupEntry::new(
                    2,
                    BindGroupResource::TextureView(Cow::Borrowed(&views.barycentric)),
                ),
                BindGroupEntry::new(3, BindGroupResource::TextureView(Cow::Borrowed(velocity))),
            ];
            if let Some(counts) = counts.as_ref() {
                let Some(counts) = counts.as_ref() else {
                    return Err(AwsmBindGroupError::NotFound(
                        "TAA overlay check counters".to_string(),
                    )
                    .into());
                };
                entries.push(BindGroupEntry::new(
                    4,
                    BindGroupResource::Buffer(BufferBinding::new(counts)),
                ));
                entries.push(BindGroupEntry::new(
                    5,
                    BindGroupResource::TextureView(Cow::Borrowed(&views.depth)),
                ));
            }
            let descriptor = BindGroupDescriptor::new(
                ctx.bind_group_layouts.get(layout_key)?,
                Some("TAA Velocity Overlay"),
                entries,
            );
            let group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
            if storage_meta {
                self.meta.storage = group;
            } else {
                self.meta.uniform = group;
            }
        }
        Ok(())
    }

    /// Draws every visible opaque renderable the mesh meta marks for the
    /// overlay this frame (`MeshMeta::overlay_draws`). No render pass at all
    /// when there is none. Runs after the prep pass (which wrote every
    /// pixel's reconstructed vector) and before anything reads the vectors.
    pub fn render(
        &self,
        ctx: &RenderContext,
        geometry: &crate::render_passes::geometry::render_pass::GeometryRenderPass,
        renderables: &[Renderable],
    ) -> Result<()> {
        let meta = &ctx.meshes.meta;
        let wanted = |r: &&Renderable| !r.hud && meta.overlay_draws(r.key);
        if let Some(debug) = self.debug.as_ref() {
            debug.drawn.borrow_mut().clear();
            if let Some((counts, size)) = debug.counts.borrow().as_ref() {
                ctx.command_encoder
                    .clear_buffer(counts, Some(0), Some(*size as u32));
            }
        }
        if !meta.has_overlay_draws() || !renderables.iter().any(|r| wanted(&r)) {
            return Ok(());
        }

        // Depth is attached read-only (load ops must be unset).
        let depth =
            DepthStencilAttachment::new(&ctx.render_texture_views.depth).with_depth_read_only(true);
        let render_pass = ctx.command_encoder.begin_render_pass(
            &RenderPassDescriptor {
                label: Some("TAA Velocity Overlay"),
                color_attachments: Vec::new(),
                depth_stencil_attachment: Some(depth),
                timestamp_writes: ctx.gpu_timestamps.and_then(|t| t.writes_for("TAA Overlay")),
                ..Default::default()
            }
            .into(),
        )?;
        // Groups 1 and 3 are the geometry pass's TAA ones (current +
        // previous transforms; morphs and skins).
        render_pass.set_bind_group(
            1,
            geometry.bind_groups.transforms.get_bind_group_for(true)?,
            None,
        )?;
        render_pass.set_bind_group(3, geometry.bind_groups.animation.get_bind_group()?, None)?;

        let camera_group = geometry.bind_groups.camera.get_bind_group()?;
        let uv0_zero = geometry.custom_vertex_pipelines.uv0_zero_buffer();
        let indirect_first_instance = ctx.features.indirect_first_instance_enabled();
        for _ in 0..self.debug_repeat.get().max(1) {
            // Group 0: the camera, or the masked group 0 for vertex hooks.
            let mut group0_masked: Option<bool> = None;
            let mut last_pipeline = None;
            for renderable in renderables.iter().filter(wanted) {
                let cull = cull_index(renderable.cull_mode);
                let custom_vertex = renderable
                    .geometry_custom_vertex_render_pipeline_key()
                    .is_some()
                    || renderable
                        .geometry_masked_custom_vertex_render_pipeline_key()
                        .is_some();
                let pipeline = if custom_vertex {
                    let shader_id = ctx.materials.canonical_shader_id(renderable.material_key());
                    self.custom.get(&(shader_id, cull)).copied()
                } else {
                    let shape = if renderable.instanced {
                        Shape::Instancing
                    } else if renderable.geometry_masked_render_pipeline_key().is_none()
                        && indirect_first_instance
                    {
                        Shape::StorageMeta
                    } else {
                        Shape::UniformMeta
                    };
                    self.plain.get(&(shape, cull)).copied()
                };
                // A hook whose overlay variant is not compiled is not covered
                // (reactive instead): `update_non_rigid_gates` makes the same
                // decision.
                let Some(pipeline) = pipeline else {
                    continue;
                };
                if group0_masked != Some(custom_vertex) {
                    if custom_vertex {
                        render_pass.set_bind_group(
                            0,
                            geometry.masked_bind_group.get_bind_group()?,
                            None,
                        )?;
                    } else {
                        render_pass.set_bind_group(0, camera_group, None)?;
                    }
                    group0_masked = Some(custom_vertex);
                }
                if last_pipeline != Some(pipeline) {
                    render_pass.set_pipeline(ctx.pipelines.render.get(pipeline)?);
                    last_pipeline = Some(pipeline);
                }
                renderable.push_taa_overlay_commands(
                    ctx,
                    &render_pass,
                    &self.meta,
                    custom_vertex.then_some(uv0_zero),
                )?;
                if let Some(debug) = self.debug.as_ref() {
                    debug.drawn.borrow_mut().push(renderable.key);
                }
            }
        }
        render_pass.end();
        Ok(())
    }

    /// See the `debug_repeat` field.
    #[doc(hidden)]
    pub fn set_debug_repeat(&self, n: u32) {
        self.debug_repeat.set(n.max(1));
    }

    /// Releases the debug counters.
    pub fn destroy(self) {
        if let Some(debug) = self.debug {
            if let Some((buffer, _)) = debug.counts.borrow_mut().take() {
                buffer.destroy();
            }
        }
    }

    /// The debug check of the last rendered frame: per mesh the overlay drew,
    /// the pixels it wrote. Copies and maps the counters (between frames,
    /// after `render()`).
    pub(crate) fn debug_read_check(
        &self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        meta: &crate::meshes::meta::MeshMeta,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<TaaOverlayMeshCheck>>>>>
    {
        let submitted =
            (|| -> Result<(web_sys::GpuBuffer, Vec<(crate::meshes::MeshKey, usize)>)> {
                let debug = self.debug.as_ref().ok_or_else(|| {
                    crate::error::AwsmError::from(AwsmBindGroupError::NotFound(
                    "TAA overlay check: set_taa_debug_overlay_check(true) before set_anti_aliasing"
                        .to_string(),
                ))
                })?;
                let counts = debug.counts.borrow();
                let (counts, size) = counts.as_ref().ok_or_else(|| {
                    crate::error::AwsmError::from(AwsmBindGroupError::NotFound(
                        "TAA overlay check counters (no frame rendered)".to_string(),
                    ))
                })?;
                let mut drawn: Vec<(crate::meshes::MeshKey, usize)> = Vec::new();
                for key in debug.drawn.borrow().iter() {
                    if drawn.iter().any(|(k, _)| k == key) {
                        continue;
                    }
                    let slot =
                        meta.material_buffer_offset(*key)? / MATERIAL_MESH_META_BYTE_ALIGNMENT;
                    drawn.push((*key, slot));
                }
                let readback = gpu.create_buffer(
                    &BufferDescriptor::new(
                        Some("TAA Overlay Check Readback"),
                        *size,
                        BufferUsage::new().with_copy_dst().with_map_read(),
                    )
                    .into(),
                )?;
                let encoder = gpu.create_command_encoder(Some("TAA Overlay Check Readback"));
                encoder.copy_buffer_to_buffer(counts, 0, &readback, 0, *size as u32)?;
                gpu.submit_commands(&encoder.finish());
                Ok((readback, drawn))
            })();
        Box::pin(async move {
            let (readback, drawn) = submitted?;
            let bytes = awsm_renderer_core::buffers::extract_buffer_vec(&readback, None).await?;
            readback.destroy();
            let word = |i: usize| -> u32 {
                bytes
                    .get(i * 4..i * 4 + 4)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .unwrap_or(0)
            };
            Ok(drawn
                .into_iter()
                .map(|(mesh, slot)| {
                    let base = slot * CHECK_WORDS_PER_SLOT;
                    TaaOverlayMeshCheck {
                        mesh,
                        written: word(base),
                        depth_not_equal: word(base + 1),
                        depth_compare_fails: word(base + 2),
                    }
                })
                .collect())
        })
    }
}

fn shape_instancing(shape: Shape) -> bool {
    shape == Shape::Instancing
}
