//! Pipeline + bind-group-layout setup for the fat-line renderer.

use awsm_renderer_core::{
    bind_groups::{BindGroupLayoutResource, BufferBindingLayout, BufferBindingType},
    compare::CompareFunction,
    pipeline::{
        depth_stencil::DepthStencilState,
        fragment::{BlendComponent, BlendFactor, BlendOperation, BlendState, ColorTargetState},
        multisample::MultisampleState,
        primitive::PrimitiveState,
    },
    renderer::AwsmRendererWebGpu,
};

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey, BindGroupLayouts,
    },
    error::Result,
    pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts},
    pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey},
    render_passes::lines::shader::cache_key::ShaderCacheKeyLine,
    render_textures::RenderTextureFormats,
    shaders::Shaders,
};

/// Pipeline-variant axes: depth-test mode × MSAA on/off.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct LineVariantKey {
    pub depth_test_always: bool,
    pub msaa: bool,
}

/// All 4 line-pipeline variants (`(depth_test_always, msaa)` cross
/// product), in `variant_index` order.
pub(super) const VARIANT_KEYS: [LineVariantKey; 4] = [
    LineVariantKey {
        depth_test_always: false,
        msaa: false,
    },
    LineVariantKey {
        depth_test_always: false,
        msaa: true,
    },
    LineVariantKey {
        depth_test_always: true,
        msaa: false,
    },
    LineVariantKey {
        depth_test_always: true,
        msaa: true,
    },
];

/// The four registered render pipelines for the line renderer, indexed
/// by [`LineVariantKey`].
pub(super) struct LinePipelines {
    pub bind_group_layout_key: BindGroupLayoutKey,
    pub variants: [RenderPipelineKey; 4],
}

/// Pre-resolved layouts + descriptors for the line renderer's 4
/// pipeline variants. Returned by
/// [`LinePipelines::build_descriptors`] and consumed by
/// [`LinePipelines::from_resolved`] after the orchestrator pools the
/// `pipeline_cache_keys` into the cross-system render-pipeline batch.
pub(super) struct LinePipelinesDescriptors {
    pub bind_group_layout_key: BindGroupLayoutKey,
    pub pipeline_cache_keys: Vec<RenderPipelineCacheKey>,
}

impl LinePipelines {
    /// Sync-apart-from-shader-resolve descriptor build. Registers the
    /// bind-group + pipeline layouts, fetches the (cache-hit pre-warmed
    /// by `RenderPasses::new`) line shader key, and produces the 4
    /// variant `RenderPipelineCacheKey`s the orchestrator pools.
    pub(super) async fn build_descriptors(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        shaders: &mut Shaders,
        formats: &RenderTextureFormats,
    ) -> Result<LinePipelinesDescriptors> {
        let bind_group_layout_cache_key = BindGroupLayoutCacheKey {
            entries: vec![
                BindGroupLayoutCacheKeyEntry {
                    // @binding(0) camera_raw : uniform
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: false,
                    visibility_compute: false,
                },
                BindGroupLayoutCacheKeyEntry {
                    // @binding(1) segments : read-only storage
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::ReadOnlyStorage),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: false,
                    visibility_compute: false,
                },
                BindGroupLayoutCacheKeyEntry {
                    // @binding(2) line_uniform : uniform
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: false,
                    visibility_compute: false,
                },
            ],
        };

        let bind_group_layout_key = bind_group_layouts.get_key(gpu, bind_group_layout_cache_key)?;

        // Route through the shared `Shaders` cache. The line shader is
        // pre-warmed by `RenderPasses::new`'s cross-pass shader
        // ensure_keys, so this is a sync cache hit.
        let shader_key = shaders.get_key(gpu, ShaderCacheKeyLine).await?;

        let pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_group_layout_key]),
        )?;

        let pipeline_cache_keys: Vec<RenderPipelineCacheKey> = VARIANT_KEYS
            .iter()
            .map(|v| build_pipeline_cache_key(shader_key, pipeline_layout_key, formats, *v))
            .collect();

        Ok(LinePipelinesDescriptors {
            bind_group_layout_key,
            pipeline_cache_keys,
        })
    }

    /// Folds resolved pipeline keys back into the typed `LinePipelines`.
    pub(super) fn from_resolved(
        descs: LinePipelinesDescriptors,
        resolved: Vec<RenderPipelineKey>,
    ) -> Self {
        debug_assert_eq!(resolved.len(), 4);
        let mut variants = [RenderPipelineKey::default(); 4];
        for (v, key) in VARIANT_KEYS.iter().zip(resolved) {
            variants[variant_index(*v)] = key;
        }
        Self {
            bind_group_layout_key: descs.bind_group_layout_key,
            variants,
        }
    }

    pub(super) fn get(&self, variant: LineVariantKey) -> RenderPipelineKey {
        self.variants[variant_index(variant)]
    }
}

pub(super) fn variant_index(variant: LineVariantKey) -> usize {
    (variant.depth_test_always as usize) << 1 | (variant.msaa as usize)
}

fn build_pipeline_cache_key(
    shader_key: crate::shaders::ShaderKey,
    pipeline_layout_key: PipelineLayoutKey,
    formats: &RenderTextureFormats,
    variant: LineVariantKey,
) -> RenderPipelineCacheKey {
    let compare = if variant.depth_test_always {
        CompareFunction::Always
    } else {
        CompareFunction::Less
    };

    let depth_stencil = DepthStencilState::new(formats.depth)
        .with_depth_write_enabled(false)
        .with_depth_compare(compare);

    let color_target = ColorTargetState::new(formats.color).with_blend(BlendState::new(
        BlendComponent::new()
            .with_src_factor(BlendFactor::SrcAlpha)
            .with_dst_factor(BlendFactor::OneMinusSrcAlpha)
            .with_operation(BlendOperation::Add),
        BlendComponent::new()
            .with_src_factor(BlendFactor::One)
            .with_dst_factor(BlendFactor::OneMinusSrcAlpha)
            .with_operation(BlendOperation::Add),
    ));

    let primitive =
        PrimitiveState::new().with_topology(web_sys::GpuPrimitiveTopology::TriangleStrip);

    let mut pipeline_cache_key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive)
        .with_depth_stencil(depth_stencil)
        .with_push_fragment_targets(vec![color_target]);

    if variant.msaa {
        pipeline_cache_key =
            pipeline_cache_key.with_multisample(MultisampleState::new().with_count(4));
    }

    pipeline_cache_key
}
