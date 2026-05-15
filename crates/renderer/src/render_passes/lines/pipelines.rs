//! Pipeline + bind-group-layout setup for the fat-line renderer.

use awsm_renderer_core::{
    bind_groups::{
        BindGroupLayoutResource, BufferBindingLayout, BufferBindingType,
    },
    compare::CompareFunction,
    pipeline::{
        depth_stencil::DepthStencilState,
        fragment::{BlendComponent, BlendFactor, BlendOperation, BlendState, ColorTargetState},
        multisample::MultisampleState,
        primitive::PrimitiveState,
    },
    renderer::AwsmRendererWebGpu,
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
};

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
        BindGroupLayouts,
    },
    error::Result,
    pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayouts},
    pipelines::{
        render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey},
        Pipelines,
    },
    render_textures::RenderTextureFormats,
    shaders::Shaders,
};

/// Pipeline-variant axes: depth-test mode × MSAA on/off.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct LineVariantKey {
    pub depth_test_always: bool,
    pub msaa: bool,
}

/// The four registered render pipelines for the line renderer, indexed
/// by [`LineVariantKey`].
pub(super) struct LinePipelines {
    pub bind_group_layout_key: BindGroupLayoutKey,
    pub variants: [RenderPipelineKey; 4],
}

impl LinePipelines {
    pub(super) async fn load(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        pipelines: &mut Pipelines,
        shaders: &mut Shaders,
        formats: &RenderTextureFormats,
    ) -> Result<Self> {
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

        let bind_group_layout_key =
            bind_group_layouts.get_key(gpu, bind_group_layout_cache_key)?;

        let shader_source = include_str!("shader/line_wgsl/line.wgsl");
        let shader_module = gpu.compile_shader(
            &ShaderModuleDescriptor::new(shader_source, Some("line shader")).into(),
        );
        shader_module.validate_shader().await?;
        let shader_key = shaders.insert_uncached(shader_module);

        let pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_group_layout_key]),
        )?;

        let mut variants = [RenderPipelineKey::default(); 4];
        for depth_test_always in [false, true] {
            for msaa in [false, true] {
                let idx = variant_index(LineVariantKey {
                    depth_test_always,
                    msaa,
                });
                variants[idx] = build_pipeline(
                    gpu,
                    pipelines,
                    shaders,
                    pipeline_layouts,
                    shader_key,
                    pipeline_layout_key,
                    formats,
                    LineVariantKey {
                        depth_test_always,
                        msaa,
                    },
                )
                .await?;
            }
        }

        Ok(Self {
            bind_group_layout_key,
            variants,
        })
    }

    pub(super) fn get(&self, variant: LineVariantKey) -> RenderPipelineKey {
        self.variants[variant_index(variant)]
    }
}

pub(super) fn variant_index(variant: LineVariantKey) -> usize {
    (variant.depth_test_always as usize) << 1 | (variant.msaa as usize)
}

#[allow(clippy::too_many_arguments)]
async fn build_pipeline(
    gpu: &AwsmRendererWebGpu,
    pipelines: &mut Pipelines,
    shaders: &Shaders,
    pipeline_layouts: &PipelineLayouts,
    shader_key: crate::shaders::ShaderKey,
    pipeline_layout_key: crate::pipeline_layouts::PipelineLayoutKey,
    formats: &RenderTextureFormats,
    variant: LineVariantKey,
) -> Result<RenderPipelineKey> {
    let compare = if variant.depth_test_always {
        CompareFunction::Always
    } else {
        CompareFunction::Less
    };

    let depth_stencil = DepthStencilState::new(formats.depth)
        .with_depth_write_enabled(false)
        .with_depth_compare(compare);

    let color_target =
        ColorTargetState::new(formats.color).with_blend(BlendState::new(
            BlendComponent::new()
                .with_src_factor(BlendFactor::SrcAlpha)
                .with_dst_factor(BlendFactor::OneMinusSrcAlpha)
                .with_operation(BlendOperation::Add),
            BlendComponent::new()
                .with_src_factor(BlendFactor::One)
                .with_dst_factor(BlendFactor::OneMinusSrcAlpha)
                .with_operation(BlendOperation::Add),
        ));

    let primitive = PrimitiveState::new()
        .with_topology(web_sys::GpuPrimitiveTopology::TriangleStrip);

    let mut pipeline_cache_key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive)
        .with_depth_stencil(depth_stencil)
        .with_push_fragment_targets(vec![color_target]);

    if variant.msaa {
        pipeline_cache_key =
            pipeline_cache_key.with_multisample(MultisampleState::new().with_count(4));
    }

    let key = pipelines
        .render
        .get_key(gpu, shaders, pipeline_layouts, pipeline_cache_key)
        .await?;

    Ok(key)
}
