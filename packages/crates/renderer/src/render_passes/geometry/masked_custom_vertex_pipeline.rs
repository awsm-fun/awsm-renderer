//! Lazy pipeline pool for the COMBINED **masked + custom-vertex** geometry
//! variant — a material that is BOTH glTF `MASK` (alpha-tested) AND carries a
//! `wgsl_vertex` displacement body.
//!
//! The union of [`super::masked_pipeline::GeometryMaskedPipelines`] (the
//! alpha-test fragment that `discard`s below the cutoff) and
//! [`super::custom_vertex_pipeline::GeometryCustomVertexPipelines`] (the vertex
//! stage that compiles the `custom_displace_vertex` hook). The assembled module
//! displaces the silhouette AND alpha-cuts it; without this pool a Mask +
//! custom-vertex material displaces but renders a SOLID silhouette (it would
//! fall through to the plain custom-vertex pool, whose fragment is the plain
//! visibility writer).
//!
//! Keyed by `(msaa, shader_id, cull)`, populated on demand by the
//! texture-finalize flow, falls back (via the render-pass precedence) to the
//! plain custom-vertex / masked / solid pipeline when an entry is missing so a
//! mesh is never dropped.
//!
//! Layout-wise this is identical to the plain custom-vertex pool: it REUSES the
//! masked geometry bind groups (group 0) and the same uv0 vertex-buffer layout
//! (`@location(10)`, `array_stride: 0` → shared zero buffer). Only the compiled
//! shader (the combined cache key) differs.

use std::collections::HashMap;

use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
use awsm_renderer_core::compare::CompareFunction;
use awsm_renderer_core::pipeline::depth_stencil::DepthStencilState;
use awsm_renderer_core::pipeline::fragment::ColorTargetState;
use awsm_renderer_core::pipeline::multisample::MultisampleState;
use awsm_renderer_core::pipeline::primitive::{
    CullMode, FrontFace, PrimitiveState, PrimitiveTopology,
};
use awsm_renderer_core::pipeline::vertex::{VertexAttribute, VertexBufferLayout, VertexFormat};
use awsm_renderer_materials::MaterialShaderId;

use crate::dynamic_materials::ShadingBase;
use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::render_passes::geometry::bind_group::GeometryBindGroups;
use crate::render_passes::geometry::custom_vertex_pipeline::CUSTOM_VERTEX_UV0_LOCATION;
use crate::render_passes::geometry::masked_bind_group::GeometryMaskedBindGroup;
use crate::render_passes::geometry::pipeline::{GeometryCullKey, VERTEX_BUFFER_LAYOUT};
use crate::render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo;
use crate::render_passes::geometry::shader::masked_cache_key::DynamicAlphaShaderInfo;
use crate::render_passes::geometry::shader::masked_custom_vertex_cache_key::ShaderCacheKeyGeometryMaskedCustomVertex;
use crate::render_passes::RenderPassInitContext;

/// Lookup key for one compiled combined masked + custom-vertex pipeline.
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct MaskedCustomVertexPipelineKeyId {
    pub msaa_sample_count: Option<u32>,
    pub shader_id: MaterialShaderId,
    pub cull: GeometryCullKey,
}

/// Inputs describing one combined masked + custom-vertex variant to (re)compile.
#[derive(Clone)]
pub struct MaskedCustomVertexVariant {
    pub shader_id: MaterialShaderId,
    pub base: ShadingBase,
    pub dynamic_vertex: DynamicVertexShaderInfo,
    pub dynamic_alpha: Option<DynamicAlphaShaderInfo>,
}

/// Lazy pool of combined masked + custom-vertex geometry render pipelines.
pub struct GeometryMaskedCustomVertexPipelines {
    pipeline_layout_key: PipelineLayoutKey,
    main: HashMap<MaskedCustomVertexPipelineKeyId, RenderPipelineKey>,
    /// Shared zero buffer bound at the uv0 slot (`array_stride: 0` → every
    /// vertex reads `vec2(0.0)`). Owned here so the draw path can bind it.
    uv0_zero_buffer: web_sys::GpuBuffer,
}

const CULL_MODES: &[CullMode] = &[CullMode::None, CullMode::Back, CullMode::Front];

impl GeometryMaskedCustomVertexPipelines {
    /// Resolves the (masked) pipeline layout + allocates the shared zero uv0
    /// buffer and returns an empty pool. Pipelines are added later via
    /// [`Self::ensure_variant`]. Mirrors
    /// [`super::custom_vertex_pipeline::GeometryCustomVertexPipelines::new`].
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
    /// recompiles the live variants against the new layout.
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

    /// Compiles one combined variant (shader + the 3 cull-mode pipelines at the
    /// active MSAA) and folds the resolved keys into the pool. Mirrors the plain
    /// custom-vertex pool's `ensure_variant`.
    pub async fn ensure_variant(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &GeometryMaskedBindGroup,
        variant: &MaskedCustomVertexVariant,
    ) -> Result<()> {
        let msaa_samples = match ctx.anti_aliasing.msaa_sample_count {
            Some(4) => Some(4u32),
            _ => None,
        };

        let shader_cache = ShaderCacheKeyGeometryMaskedCustomVertex {
            shader_id: variant.shader_id,
            base: variant.base,
            dynamic_vertex: variant.dynamic_vertex.clone(),
            dynamic_alpha: variant.dynamic_alpha.clone(),
            texture_pool_arrays_len: masked_bind_group.texture_pool_arrays_len,
            texture_pool_samplers_len: masked_bind_group.texture_pool_samplers_len,
            msaa_samples,
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
            pipeline_cache_keys.push(build_cache_key(
                shader_key,
                self.pipeline_layout_key,
                depth_format,
                &color_targets,
                msaa_samples,
                cull_mode,
            ));
            slots.push(MaskedCustomVertexPipelineKeyId {
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

    /// Drops every compiled combined pipeline.
    pub fn clear(&mut self) {
        self.main.clear();
    }

    /// Looks up a compiled combined pipeline for a mesh's `(msaa, shader_id,
    /// cull)`. `None` → not compiled yet (render path falls back via the
    /// precedence in `collect_renderables` to the plain custom-vertex / masked /
    /// solid pipeline so the mesh still draws).
    pub fn get(
        &self,
        msaa_sample_count: Option<u32>,
        shader_id: MaterialShaderId,
        cull_mode: CullMode,
    ) -> Option<RenderPipelineKey> {
        let cull = GeometryCullKey::from_cull_mode(cull_mode).ok()?;
        self.main
            .get(&MaskedCustomVertexPipelineKeyId {
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
/// combined layout binds it with `array_stride: 0`, so this single buffer
/// satisfies any vertex count.
fn create_uv0_zero_buffer(ctx: &mut RenderPassInitContext<'_>) -> Result<web_sys::GpuBuffer> {
    let buffer = ctx.gpu.create_buffer(
        &BufferDescriptor::new(
            Some("Geometry Masked Custom Vertex - uv0 zero"),
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
            // Combined meshes take the uniform-with-dynamic-offset meta path
            // (the non-instanced shape, matching the plain custom-vertex pool).
            geometry_bind_groups.meta.uniform_layout_key,
            geometry_bind_groups.animation.bind_group_layout_key,
        ]),
    )?)
}

/// The uv0 vertex-buffer layout: `array_stride: 0` so every vertex reads the
/// single `vec2<f32>` at offset 0 of the shared zero buffer. Same layout the
/// plain custom-vertex pipeline uses (`@location(10)`).
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

fn build_cache_key(
    shader_key: crate::shaders::ShaderKey,
    pipeline_layout_key: PipelineLayoutKey,
    depth_format: awsm_renderer_core::texture::TextureFormat,
    color_targets: &[ColorTargetState],
    msaa_samples: Option<u32>,
    cull_mode: CullMode,
) -> RenderPipelineCacheKey {
    let primitive_state = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(FrontFace::Ccw)
        .with_cull_mode(cull_mode);

    let depth_stencil = DepthStencilState::new(depth_format)
        .with_depth_write_enabled(true)
        .with_depth_compare(CompareFunction::LessEqual);

    // Slot 0: plain visibility geometry. Final slot: the uv0 buffer (location
    // 10). Non-instanced only (no instancing slot) — matches the plain
    // custom-vertex pool's non-instanced shape.
    let mut key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive_state)
        .with_depth_stencil(depth_stencil)
        .with_push_vertex_buffer_layout(VERTEX_BUFFER_LAYOUT.clone())
        .with_push_vertex_buffer_layout(uv0_vertex_buffer_layout());
    if let Some(sample_count) = msaa_samples {
        key = key.with_multisample(MultisampleState::new().with_count(sample_count));
    }
    for target in color_targets {
        key = key.with_push_fragment_targets(vec![target.clone()]);
    }
    key
}
