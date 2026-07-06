//! Lazy pipeline pool for the **masked** (alpha-tested) geometry variant.
//!
//! Keyed by `(msaa, shader_id, cull)` and populated on demand — built-in PBR
//! masked pipelines are (re)built by the texture-finalize flow (the pool size
//! lives in the shader cache key); dynamic/custom masked pipelines are compiled
//! by the same per-shader-id scheduler that compiles the opaque pipelines.
//! Mirrors `MaterialOpaquePipelines`'s lazy `main` map: a missing entry means
//! "not compiled yet" and the render path falls back to the plain (solid)
//! geometry pipeline, so a mesh is never dropped — it just renders un-cut until
//! its masked variant lands.

use std::collections::HashMap;

use awsm_renderer_core::compare::CompareFunction;
use awsm_renderer_core::pipeline::depth_stencil::DepthStencilState;
use awsm_renderer_core::pipeline::fragment::ColorTargetState;
use awsm_renderer_core::pipeline::multisample::MultisampleState;
use awsm_renderer_core::pipeline::primitive::{
    CullMode, FrontFace, PrimitiveState, PrimitiveTopology,
};
use awsm_renderer_materials::MaterialShaderId;

use crate::dynamic_materials::ShadingBase;
use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::render_passes::geometry::bind_group::GeometryBindGroups;
use crate::render_passes::geometry::masked_bind_group::GeometryMaskedBindGroup;
use crate::render_passes::geometry::pipeline::{GeometryCullKey, VERTEX_BUFFER_LAYOUT};
use crate::render_passes::geometry::shader::masked_cache_key::{
    DynamicAlphaShaderInfo, ShaderCacheKeyGeometryMasked,
};
use crate::render_passes::RenderPassInitContext;

/// Lookup key for one compiled masked pipeline.
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct MaskedPipelineKeyId {
    pub msaa_sample_count: Option<u32>,
    pub shader_id: MaterialShaderId,
    pub cull: GeometryCullKey,
}

/// Inputs describing one masked variant to (re)compile.
#[derive(Clone)]
pub struct MaskedVariant {
    pub shader_id: MaterialShaderId,
    pub base: ShadingBase,
    pub dynamic_alpha: Option<DynamicAlphaShaderInfo>,
}

/// Lazy pool of masked geometry render pipelines.
pub struct GeometryMaskedPipelines {
    pipeline_layout_key: PipelineLayoutKey,
    main: HashMap<MaskedPipelineKeyId, RenderPipelineKey>,
}

const CULL_MODES: &[CullMode] = &[CullMode::None, CullMode::Back, CullMode::Front];

impl GeometryMaskedPipelines {
    /// Resolves the masked pipeline layout (augmented group 0 + the plain
    /// geometry transforms/uniform-meta/animation groups) and returns an empty
    /// pool. Pipelines are added later via [`Self::build_descriptors`] +
    /// [`Self::insert`].
    pub fn new(
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &GeometryMaskedBindGroup,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_key = resolve_layout_key(ctx, masked_bind_group, geometry_bind_groups)?;
        Ok(Self {
            pipeline_layout_key,
            main: HashMap::new(),
        })
    }

    /// True when a masked variant for `(msaa, shader_id)` is already compiled
    /// (any cull — [`Self::ensure_variant`] inserts all three together). The
    /// texture-finalize gate probes this so a material that ROUTES masked with
    /// no texture change (e.g. the editor flipping a builtin's alpha mode to
    /// Mask) still gets its variant compiled instead of silently falling back
    /// to the solid pipeline forever.
    pub fn has_variant(
        &self,
        msaa_sample_count: Option<u32>,
        shader_id: MaterialShaderId,
    ) -> bool {
        self.main
            .keys()
            .any(|k| k.msaa_sample_count == msaa_sample_count && k.shader_id == shader_id)
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

    /// Compiles one masked variant (shader + the 3 cull-mode pipelines at the
    /// active MSAA) and folds the resolved keys into the pool. Self-contained:
    /// ensures the shader, then the render pipelines, then inserts — so the
    /// texture-finalize flow (built-in PBR) and the dynamic scheduler (custom)
    /// can each call it directly without threading cross-pass batches.
    pub async fn ensure_variant(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &GeometryMaskedBindGroup,
        variant: &MaskedVariant,
    ) -> Result<()> {
        let msaa_samples = match ctx.anti_aliasing.msaa_sample_count {
            Some(4) => Some(4u32),
            _ => None,
        };

        let shader_cache = ShaderCacheKeyGeometryMasked {
            texture_pool_arrays_len: masked_bind_group.texture_pool_arrays_len,
            texture_pool_samplers_len: masked_bind_group.texture_pool_sampler_keys.len() as u32,
            shader_id: variant.shader_id,
            base: variant.base,
            dynamic_alpha: variant.dynamic_alpha.clone(),
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
            pipeline_cache_keys.push(build_masked_cache_key(
                shader_key,
                self.pipeline_layout_key,
                depth_format,
                &color_targets,
                msaa_samples,
                cull_mode,
            ));
            slots.push(MaskedPipelineKeyId {
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

    /// Drops every compiled masked pipeline. Used before recompiling against a
    /// changed bucket layout / texture pool.
    pub fn clear(&mut self) {
        self.main.clear();
    }

    /// Looks up a compiled masked pipeline for a mesh's `(msaa, shader_id,
    /// cull)`. `None` → not compiled yet (render path falls back to the plain
    /// geometry pipeline so the mesh still draws, un-cut).
    pub fn get(
        &self,
        msaa_sample_count: Option<u32>,
        shader_id: MaterialShaderId,
        cull_mode: CullMode,
    ) -> Option<RenderPipelineKey> {
        let cull = GeometryCullKey::from_cull_mode(cull_mode).ok()?;
        self.main
            .get(&MaskedPipelineKeyId {
                msaa_sample_count,
                shader_id,
                cull,
            })
            .copied()
    }
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
            // Masked meshes take the uniform-with-dynamic-offset meta path.
            geometry_bind_groups.meta.uniform_layout_key,
            geometry_bind_groups.animation.bind_group_layout_key,
        ]),
    )?)
}

fn build_masked_cache_key(
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

    let mut key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive_state)
        .with_depth_stencil(depth_stencil)
        .with_push_vertex_buffer_layout(VERTEX_BUFFER_LAYOUT.clone());
    if let Some(sample_count) = msaa_samples {
        key = key.with_multisample(MultisampleState::new().with_count(sample_count));
    }
    for target in color_targets {
        key = key.with_push_fragment_targets(vec![target.clone()]);
    }
    key
}
