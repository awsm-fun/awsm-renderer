//! Lazy pipeline pool for the **custom-vertex** geometry variant.
//!
//! A material whose registration carries a non-empty `wgsl_vertex` body
//! (`DynamicMaterials::vertex_shader_info_for(shader_id)` → `Some`) gets its own
//! geometry pipeline: the geometry VERTEX shader compiles the gated
//! `custom_displace_vertex` hook, while the fragment stays the PLAIN geometry
//! fragment (writes the visibility buffer; this variant is opaque, not
//! alpha-tested).
//!
//! Mirrors [`super::masked_pipeline::GeometryMaskedPipelines`] end-to-end:
//! keyed by `(msaa, shader_id, cull)`, populated on demand by the
//! texture-finalize flow, falls back to the plain (solid) geometry pipeline
//! when an entry is missing so a mesh is never dropped — it just renders
//! un-displaced until its custom-vertex variant lands.
//!
//! The variant REUSES the masked geometry bind groups (the custom-vertex
//! `bind_groups.wgsl` includes the masked decls verbatim, so the group-0 layout
//! is identical — the hook's `material_data_load` reads the `materials` storage
//! buffer + samples the texture pool the masked group declares).
//!
//! THE ONE GENUINELY NEW PIECE — a per-draw UV0 vertex buffer. The
//! custom-vertex vertex shader declares `@location(10) uv0: vec2<f32>` (gated on
//! `has_custom_vertex`), so the pipeline's vertex-buffer layout MUST supply it.
//! The visibility geometry buffer (slot 0) carries no UV (its 56-byte stride is
//! position/triangle_index/barycentric/normal/tangent/original_vertex_index),
//! and the per-mesh UV0 lives interleaved in the *transparency* custom-attribute
//! buffer under a different vertex ordering — there is no UV stream that matches
//! the visibility-buffer vertex order. So the variant binds a SHARED zero buffer
//! at the uv0 slot with `array_stride: 0` (every vertex reads offset 0 → uv =
//! `vec2(0.0)`). This keeps the hook always fed (so position-/normal-/instance-
//! driven displacement works on any mesh, with or without authored UVs); a
//! follow-on can plumb real per-vertex UV once the visibility buffer carries it.
//! Location 10 does not collide with the instancing locations 6-9.

use std::collections::HashMap;

use awsm_materials::MaterialShaderId;
use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
use awsm_renderer_core::compare::CompareFunction;
use awsm_renderer_core::pipeline::depth_stencil::DepthStencilState;
use awsm_renderer_core::pipeline::fragment::ColorTargetState;
use awsm_renderer_core::pipeline::multisample::MultisampleState;
use awsm_renderer_core::pipeline::primitive::{
    CullMode, FrontFace, PrimitiveState, PrimitiveTopology,
};
use awsm_renderer_core::pipeline::vertex::{VertexAttribute, VertexBufferLayout, VertexFormat};

use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::render_passes::geometry::bind_group::GeometryBindGroups;
use crate::render_passes::geometry::masked_bind_group::GeometryMaskedBindGroup;
use crate::render_passes::geometry::pipeline::{
    GeometryCullKey, VERTEX_BUFFER_LAYOUT, VERTEX_BUFFER_LAYOUT_INSTANCING,
};
use crate::render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo;
use crate::render_passes::geometry::shader::custom_vertex_cache_key::ShaderCacheKeyGeometryCustomVertex;
use crate::render_passes::RenderPassInitContext;

/// Shader location of the custom-vertex UV0 attribute. Matches
/// `@location(10) uv0` in `geometry_wgsl/vertex.wgsl` (gated on
/// `has_custom_vertex`). Sits above the instancing locations 6-9.
pub const CUSTOM_VERTEX_UV0_LOCATION: u32 = 10;

/// Lookup key for one compiled custom-vertex pipeline.
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct CustomVertexPipelineKeyId {
    pub msaa_sample_count: Option<u32>,
    pub shader_id: MaterialShaderId,
    pub cull: GeometryCullKey,
}

/// Inputs describing one custom-vertex variant to (re)compile.
#[derive(Clone)]
pub struct CustomVertexVariant {
    pub shader_id: MaterialShaderId,
    pub dynamic_vertex: DynamicVertexShaderInfo,
    /// Variant takes per-instance vertex attributes. When true the layout adds
    /// the instancing buffer (slot 1) and the uv0 buffer moves to slot 2.
    pub instancing_transforms: bool,
}

/// Lazy pool of custom-vertex geometry render pipelines.
pub struct GeometryCustomVertexPipelines {
    pipeline_layout_key: PipelineLayoutKey,
    main: HashMap<CustomVertexPipelineKeyId, RenderPipelineKey>,
    /// Shared zero buffer bound at the uv0 slot (`array_stride: 0` → every
    /// vertex reads `vec2(0.0)`). Owned here so the draw path can bind it.
    uv0_zero_buffer: web_sys::GpuBuffer,
}

const CULL_MODES: &[CullMode] = &[CullMode::None, CullMode::Back, CullMode::Front];

impl GeometryCustomVertexPipelines {
    /// Resolves the (masked) pipeline layout + allocates the shared zero uv0
    /// buffer and returns an empty pool. Pipelines are added later via
    /// [`Self::ensure_variant`]. Mirrors
    /// [`super::masked_pipeline::GeometryMaskedPipelines::new`].
    pub fn new(
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &GeometryMaskedBindGroup,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_key = resolve_layout_key(ctx, masked_bind_group, geometry_bind_groups)?;
        let uv0_zero_buffer = create_uv0_zero_buffer(ctx)?;
        Ok(Self {
            pipeline_layout_key,
            main: HashMap::new(),
            uv0_zero_buffer,
        })
    }

    /// Re-resolves the pipeline layout after the masked group-0 layout changed
    /// (texture-pool growth). Existing pool entries are cleared — the caller
    /// recompiles the live variants against the new layout. Mirrors
    /// [`super::masked_pipeline::GeometryMaskedPipelines::relayout`].
    pub fn relayout(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &GeometryMaskedBindGroup,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<()> {
        self.pipeline_layout_key =
            resolve_layout_key(ctx, masked_bind_group, geometry_bind_groups)?;
        self.main.clear();
        Ok(())
    }

    /// Compiles one custom-vertex variant (shader + the 3 cull-mode pipelines at
    /// the active MSAA) and folds the resolved keys into the pool. Self-contained
    /// — mirrors [`super::masked_pipeline::GeometryMaskedPipelines::ensure_variant`].
    pub async fn ensure_variant(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &GeometryMaskedBindGroup,
        variant: &CustomVertexVariant,
    ) -> Result<()> {
        let msaa_samples = match ctx.anti_aliasing.msaa_sample_count {
            Some(4) => Some(4u32),
            _ => None,
        };

        let shader_cache = ShaderCacheKeyGeometryCustomVertex {
            shader_id: variant.shader_id,
            dynamic_vertex: variant.dynamic_vertex.clone(),
            texture_pool_arrays_len: masked_bind_group.texture_pool_arrays_len,
            texture_pool_samplers_len: masked_bind_group.texture_pool_sampler_keys.len() as u32,
            msaa_samples,
            instancing_transforms: variant.instancing_transforms,
            // The reused masked bind groups declare the uniform meta binding, so
            // the assembled module is only consistent with the uniform-meta shape.
            meta_storage_array: false,
        };
        ctx.shaders
            .ensure_keys(ctx.gpu, vec![shader_cache.clone().into()])
            .await?;
        let shader_key = ctx.shaders.get_key(ctx.gpu, shader_cache).await?;

        let color_targets = [
            ColorTargetState::new(ctx.render_texture_formats.visiblity_data),
            ColorTargetState::new(ctx.render_texture_formats.barycentric),
            ColorTargetState::new(ctx.render_texture_formats.normal_tangent),
            ColorTargetState::new(ctx.render_texture_formats.barycentric_derivatives),
        ];
        let depth_format = ctx.render_texture_formats.depth;

        let mut pipeline_cache_keys = Vec::with_capacity(CULL_MODES.len());
        let mut slots = Vec::with_capacity(CULL_MODES.len());
        for &cull_mode in CULL_MODES {
            pipeline_cache_keys.push(build_custom_vertex_cache_key(
                shader_key,
                self.pipeline_layout_key,
                depth_format,
                &color_targets,
                msaa_samples,
                variant.instancing_transforms,
                cull_mode,
            ));
            slots.push(CustomVertexPipelineKeyId {
                msaa_sample_count: msaa_samples,
                shader_id: variant.shader_id,
                cull: GeometryCullKey::from_cull_mode(cull_mode)?,
            });
        }

        let keys = ctx
            .pipelines
            .render
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                pipeline_cache_keys,
            )
            .await?;
        for (slot, key) in slots.into_iter().zip(keys) {
            self.main.insert(slot, key);
        }
        Ok(())
    }

    /// Drops every compiled custom-vertex pipeline.
    pub fn clear(&mut self) {
        self.main.clear();
    }

    /// Looks up a compiled custom-vertex pipeline for a mesh's `(msaa,
    /// shader_id, cull)`. `None` → not compiled yet (render path falls back to
    /// the plain geometry pipeline so the mesh still draws, un-displaced).
    pub fn get(
        &self,
        msaa_sample_count: Option<u32>,
        shader_id: MaterialShaderId,
        cull_mode: CullMode,
    ) -> Option<RenderPipelineKey> {
        let cull = GeometryCullKey::from_cull_mode(cull_mode).ok()?;
        self.main
            .get(&CustomVertexPipelineKeyId {
                msaa_sample_count,
                shader_id,
                cull,
            })
            .copied()
    }

    /// The shared zero uv0 buffer to bind at the custom-vertex uv0 slot.
    pub fn uv0_zero_buffer(&self) -> &web_sys::GpuBuffer {
        &self.uv0_zero_buffer
    }
}

/// Builds the 8-byte shared zero uv0 buffer (one `vec2<f32>` of zeros). The
/// custom-vertex layout binds it with `array_stride: 0`, so this single buffer
/// satisfies any vertex count.
fn create_uv0_zero_buffer(ctx: &mut RenderPassInitContext<'_>) -> Result<web_sys::GpuBuffer> {
    let buffer = ctx.gpu.create_buffer(
        &BufferDescriptor::new(
            Some("Geometry Custom Vertex - uv0 zero"),
            // one vec2<f32>
            8,
            BufferUsage::new().with_vertex(),
        )
        .into(),
    )?;
    Ok(buffer)
}

fn resolve_layout_key(
    ctx: &mut RenderPassInitContext<'_>,
    masked_bind_group: &GeometryMaskedBindGroup,
    geometry_bind_groups: &GeometryBindGroups,
) -> Result<PipelineLayoutKey> {
    Ok(ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![
            masked_bind_group.bind_group_layout_key,
            geometry_bind_groups.transforms.bind_group_layout_key,
            // Custom-vertex meshes take the uniform-with-dynamic-offset meta path.
            geometry_bind_groups.meta.uniform_layout_key,
            geometry_bind_groups.animation.bind_group_layout_key,
        ]),
    )?)
}

/// The uv0 vertex-buffer layout: `array_stride: 0` so every vertex reads the
/// single `vec2<f32>` at offset 0 of the shared zero buffer.
fn uv0_vertex_buffer_layout() -> VertexBufferLayout {
    VertexBufferLayout {
        array_stride: 0,
        step_mode: None,
        attributes: vec![VertexAttribute {
            format: VertexFormat::Float32x2,
            offset: 0,
            shader_location: CUSTOM_VERTEX_UV0_LOCATION,
        }],
    }
}

fn build_custom_vertex_cache_key(
    shader_key: crate::shaders::ShaderKey,
    pipeline_layout_key: PipelineLayoutKey,
    depth_format: awsm_renderer_core::texture::TextureFormat,
    color_targets: &[ColorTargetState],
    msaa_samples: Option<u32>,
    instancing: bool,
    cull_mode: CullMode,
) -> RenderPipelineCacheKey {
    let primitive_state = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(FrontFace::Ccw)
        .with_cull_mode(cull_mode);

    let depth_stencil = DepthStencilState::new(depth_format)
        .with_depth_write_enabled(true)
        .with_depth_compare(CompareFunction::LessEqual);

    // Slot 0: plain visibility geometry. Slot 1: instancing (when instanced).
    // Final slot: the uv0 buffer (location 10) — kept LAST so it doesn't shift
    // the instancing locations 6-9.
    let mut vertex_buffer_layouts = vec![VERTEX_BUFFER_LAYOUT.clone()];
    if instancing {
        vertex_buffer_layouts.push(VERTEX_BUFFER_LAYOUT_INSTANCING.clone());
    }
    vertex_buffer_layouts.push(uv0_vertex_buffer_layout());

    let mut key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive_state)
        .with_depth_stencil(depth_stencil);
    for layout in vertex_buffer_layouts {
        key = key.with_push_vertex_buffer_layout(layout);
    }
    if let Some(sample_count) = msaa_samples {
        key = key.with_multisample(MultisampleState::new().with_count(sample_count));
    }
    for target in color_targets {
        key = key.with_push_fragment_targets(vec![target.clone()]);
    }
    key
}
