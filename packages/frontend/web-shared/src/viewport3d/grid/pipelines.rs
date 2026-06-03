use awsm_renderer::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
    },
    core::{
        bind_groups::{
            BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
            BufferBindingLayout, BufferBindingType,
        },
        buffers::BufferBinding,
        compare::CompareFunction,
        pipeline::{
            depth_stencil::DepthStencilState,
            fragment::{BlendComponent, BlendFactor, BlendOperation, BlendState, ColorTargetState},
            multisample::MultisampleState,
            primitive::PrimitiveState,
        },
        shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    },
    pipeline_layouts::PipelineLayoutCacheKey,
    pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey},
    AwsmRenderer,
};

pub struct EditorPipelines {
    pub grid_bind_group: web_sys::GpuBindGroup,
    pub grid_pipeline_msaa_4_key: RenderPipelineKey,
    pub grid_pipeline_singlesampled_key: RenderPipelineKey,
}

impl EditorPipelines {
    pub async fn load(renderer: &mut AwsmRenderer) -> anyhow::Result<Self> {
        let bind_group_layout_cache_key = BindGroupLayoutCacheKey {
            entries: vec![BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            }],
        };

        let bind_group_layout_key = renderer
            .bind_group_layouts
            .get_key(&renderer.gpu, bind_group_layout_cache_key)?;

        let grid_pipeline_msaa_4_key =
            load_grid_pipeline(renderer, bind_group_layout_key, Some(4)).await?;
        let grid_pipeline_singlesampled_key =
            load_grid_pipeline(renderer, bind_group_layout_key, None).await?;

        let grid_bind_group = renderer.gpu.create_bind_group(
            &BindGroupDescriptor::new(
                renderer.bind_group_layouts.get(bind_group_layout_key)?,
                Some("Grid Camera"),
                vec![BindGroupEntry::new(
                    0,
                    BindGroupResource::Buffer(BufferBinding::new(&renderer.camera.gpu_buffer)),
                )],
            )
            .into(),
        );

        Ok(Self {
            grid_pipeline_msaa_4_key,
            grid_pipeline_singlesampled_key,
            grid_bind_group,
        })
    }
}

async fn load_grid_pipeline(
    renderer: &mut AwsmRenderer,
    bind_group_layout_key: BindGroupLayoutKey,
    msaa_sample_count: Option<u32>,
) -> anyhow::Result<RenderPipelineKey> {
    let shader_source = include_str!("shaders/grid.wgsl");

    let shader_module = renderer
        .gpu
        .compile_shader(&ShaderModuleDescriptor::new(shader_source, Some("grid shader")).into());

    shader_module.validate_shader().await?;

    let shader_key = renderer.shaders.insert_uncached(shader_module);

    let pipeline_layout_cache_key = PipelineLayoutCacheKey::new(vec![bind_group_layout_key]);

    let pipeline_layout_key = renderer.pipeline_layouts.get_key(
        &renderer.gpu,
        &renderer.bind_group_layouts,
        pipeline_layout_cache_key,
    )?;

    let depth_stencil = DepthStencilState::new(renderer.render_textures.formats.depth)
        .with_depth_write_enabled(false)
        .with_depth_compare(CompareFunction::Less);

    // Standard non-premultiplied alpha blending
    let color_target =
        ColorTargetState::new(renderer.render_textures.formats.color).with_blend(BlendState::new(
            BlendComponent::new()
                .with_src_factor(BlendFactor::SrcAlpha)
                .with_dst_factor(BlendFactor::OneMinusSrcAlpha)
                .with_operation(BlendOperation::Add),
            BlendComponent::new()
                .with_src_factor(BlendFactor::One)
                .with_dst_factor(BlendFactor::OneMinusSrcAlpha)
                .with_operation(BlendOperation::Add),
        ));
    //let color_target = ColorTargetState::new(renderer.render_textures.formats.color);

    let mut pipeline_cache_key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(PrimitiveState::new())
        .with_depth_stencil(depth_stencil)
        .with_push_fragment_targets(vec![color_target]);

    if let Some(sample_count) = msaa_sample_count {
        pipeline_cache_key =
            pipeline_cache_key.with_multisample(MultisampleState::new().with_count(sample_count));
    }

    let render_pipeline_key = renderer
        .pipelines
        .render
        .get_key(
            &renderer.gpu,
            &renderer.shaders,
            &renderer.pipeline_layouts,
            pipeline_cache_key,
        )
        .await?;

    Ok(render_pipeline_key)
}
