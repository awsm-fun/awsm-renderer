//! Geometry pass pipeline setup.

use std::sync::LazyLock;

use awsm_renderer_core::compare::CompareFunction;
use awsm_renderer_core::pipeline::depth_stencil::DepthStencilState;
use awsm_renderer_core::pipeline::fragment::ColorTargetState;
use awsm_renderer_core::pipeline::multisample::MultisampleState;
use awsm_renderer_core::pipeline::primitive::{
    CullMode, FrontFace, PrimitiveState, PrimitiveTopology,
};
use awsm_renderer_core::pipeline::vertex::{
    VertexAttribute, VertexBufferLayout, VertexFormat, VertexStepMode,
};
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use awsm_renderer_core::texture::TextureFormat;

use crate::anti_alias::AntiAliasing;
use crate::error::{AwsmError, Result};
use crate::meshes::buffer_info::MeshBufferVertexInfo;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::pipelines::Pipelines;
use crate::render_passes::geometry::shader::cache_key::ShaderCacheKeyGeometry;
use crate::render_passes::{geometry::bind_group::GeometryBindGroups, RenderPassInitContext};
use crate::shaders::{ShaderKey, Shaders};

pub static VERTEX_BUFFER_LAYOUT: LazyLock<VertexBufferLayout> = LazyLock::new(|| {
    VertexBufferLayout {
        // this is the stride across all of the attributes
        // position (12) + triangle_index (4) + barycentric (8) + normal (12) + tangent (16) + original_vertex_index (4) = 56 bytes
        array_stride: MeshBufferVertexInfo::VISIBILITY_GEOMETRY_BYTE_SIZE as u64,
        step_mode: None,
        attributes: vec![
            // Position (vec3<f32>) at offset 0
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            // Triangle ID (u32) at offset 12
            VertexAttribute {
                format: VertexFormat::Uint32,
                offset: 12,
                shader_location: 1,
            },
            // Barycentric coordinates (vec2<f32>) at offset 16
            VertexAttribute {
                format: VertexFormat::Float32x2,
                offset: 16,
                shader_location: 2,
            },
            // Normal (vec3<f32>) at offset 24
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 24,
                shader_location: 3,
            },
            // Tangent (vec4<f32>) at offset 36
            VertexAttribute {
                format: VertexFormat::Float32x4,
                offset: 36,
                shader_location: 4,
            },
            // Original vertex index (u32) at offset 52 - for indexed skin/morph access
            VertexAttribute {
                format: VertexFormat::Uint32,
                offset: 52,
                shader_location: 5,
            },
        ],
    }
});

pub static VERTEX_BUFFER_LAYOUT_INSTANCING: LazyLock<VertexBufferLayout> = LazyLock::new(|| {
    let mut vertex_buffer_layout_instancing = VertexBufferLayout {
        // this is the stride across all of the attributes
        array_stride: MeshBufferVertexInfo::INSTANCING_BYTE_SIZE as u64,
        step_mode: Some(VertexStepMode::Instance),
        attributes: Vec::new(),
    };

    let start_location = VERTEX_BUFFER_LAYOUT.attributes.len() as u32;

    for i in 0..4 {
        vertex_buffer_layout_instancing
            .attributes
            .push(VertexAttribute {
                format: VertexFormat::Float32x4,
                offset: i * 16,
                shader_location: start_location + i as u32,
            });
    }

    vertex_buffer_layout_instancing
});

/// Pipeline layout and render pipelines for the geometry pass.
pub struct GeometryPipelines {
    /// Pipeline layout for the non-instanced variant — @group(2) is
    /// the storage-array meta binding.
    pub pipeline_layout_key_storage: PipelineLayoutKey,
    /// Pipeline layout for the instanced variant — @group(2) is the
    /// legacy uniform-with-dynamic-offset meta binding.
    pub pipeline_layout_key_uniform: PipelineLayoutKey,
    render_pipeline_keys: GeometryRenderPipelineKeys,
}

impl GeometryPipelines {
    /// Creates geometry pipeline layouts and cached keys.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &GeometryBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_key_storage = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                bind_groups.camera.bind_group_layout_key,
                bind_groups.transforms.bind_group_layout_key,
                bind_groups.meta.storage_layout_key,
                bind_groups.animation.bind_group_layout_key,
            ]),
        )?;
        let pipeline_layout_key_uniform = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                bind_groups.camera.bind_group_layout_key,
                bind_groups.transforms.bind_group_layout_key,
                bind_groups.meta.uniform_layout_key,
                bind_groups.animation.bind_group_layout_key,
            ]),
        )?;

        let render_pipeline_keys = GeometryRenderPipelineKeys::new(
            ctx,
            pipeline_layout_key_storage,
            pipeline_layout_key_uniform,
        )
        .await?;

        Ok(Self {
            pipeline_layout_key_storage,
            pipeline_layout_key_uniform,
            render_pipeline_keys,
        })
    }

    /// Returns the render pipeline key for the requested options.
    pub fn get_render_pipeline_key(
        &self,
        opts: GeometryRenderPipelineKeyOpts<'_>,
    ) -> Result<RenderPipelineKey> {
        let level = match opts.anti_aliasing.has_msaa_checked()? {
            true => &self.render_pipeline_keys.msaa_4_anti_alias,
            false => &self.render_pipeline_keys.no_anti_alias,
        };

        // Variant selection:
        //  - instanced → uniform-with-dynamic-offset binding (the
        //    `instance_index` range across one drawIndirect would
        //    otherwise collide with neighbouring meshes' meta slots).
        //  - non-instanced under `meta_storage_array` → storage-array
        //    binding indexed by `instance_index`; requires the
        //    `indirect-first-instance` WebGPU feature.
        //  - non-instanced under `!meta_storage_array` → portable
        //    uniform-with-dynamic-offset binding (same layout as
        //    instanced for @group(2), different vertex inputs).
        let level = if opts.instancing {
            &level.instancing
        } else if opts.meta_storage_array {
            &level.no_instancing_storage_meta
        } else {
            &level.no_instancing_uniform_meta
        };

        let level = match opts.cull_mode {
            CullMode::None => &level.no_cull,
            CullMode::Back => &level.back_cull,
            CullMode::Front => &level.front_cull,
            _ => {
                return Err(AwsmError::UnsupportedCullMode(opts.cull_mode));
            }
        };

        Ok(level.render_pipeline_key)
    }
}

/// Options for selecting a geometry render pipeline.
pub struct GeometryRenderPipelineKeyOpts<'a> {
    pub anti_aliasing: &'a AntiAliasing,
    pub instancing: bool,
    pub cull_mode: CullMode,
    /// Pick the storage-array meta binding variant (non-instanced,
    /// `indirect-first-instance` available). When false the
    /// non-instanced path uses uniform-with-dynamic-offset.
    /// Ignored when `instancing` is true (instanced is always
    /// uniform-with-dynamic-offset).
    pub meta_storage_array: bool,
}

/// Collection of geometry pipeline keys keyed by MSAA and instancing options.
pub struct GeometryRenderPipelineKeys {
    pub no_anti_alias: GeometryRenderPipelineKeysLevel1,
    pub msaa_4_anti_alias: GeometryRenderPipelineKeysLevel1,
}

impl GeometryRenderPipelineKeys {
    /// Creates geometry pipeline keys for all supported configurations.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        pipeline_layout_key_storage: PipelineLayoutKey,
        pipeline_layout_key_uniform: PipelineLayoutKey,
    ) -> Result<Self> {
        Ok(Self {
            no_anti_alias: GeometryRenderPipelineKeysLevel1::new(
                ctx,
                pipeline_layout_key_storage,
                pipeline_layout_key_uniform,
                None,
            )
            .await?,
            msaa_4_anti_alias: GeometryRenderPipelineKeysLevel1::new(
                ctx,
                pipeline_layout_key_storage,
                pipeline_layout_key_uniform,
                Some(4),
            )
            .await?,
        })
    }
}

/// Geometry pipeline keys keyed by instancing + meta-binding shape.
pub struct GeometryRenderPipelineKeysLevel1 {
    /// Non-instanced + storage-array meta binding (requires
    /// `indirect-first-instance`). Routed to by the geometry pass
    /// when the device exposes the feature.
    pub no_instancing_storage_meta: GeometryRenderPipelineKeysLevel2,
    /// Non-instanced + uniform-with-dynamic-offset meta binding
    /// (portable). Same shader / layout shape as the instanced path
    /// for @group(2), different vertex inputs.
    pub no_instancing_uniform_meta: GeometryRenderPipelineKeysLevel2,
    /// Instanced. Always uniform-with-dynamic-offset meta binding.
    pub instancing: GeometryRenderPipelineKeysLevel2,
}

impl GeometryRenderPipelineKeysLevel1 {
    /// Creates geometry pipeline keys for the three (instancing,
    /// meta-binding) variants. The instanced path is always
    /// uniform-with-dynamic-offset; the non-instanced path carries
    /// both shapes so the runtime can route to either based on
    /// `indirect_first_instance`.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        pipeline_layout_key_storage: PipelineLayoutKey,
        pipeline_layout_key_uniform: PipelineLayoutKey,
        msaa_samples: Option<u32>,
    ) -> Result<Self> {
        Ok(Self {
            no_instancing_storage_meta: GeometryRenderPipelineKeysLevel2::new(
                ctx,
                pipeline_layout_key_storage,
                msaa_samples,
                false,
                true,
            )
            .await?,
            no_instancing_uniform_meta: GeometryRenderPipelineKeysLevel2::new(
                ctx,
                pipeline_layout_key_uniform,
                msaa_samples,
                false,
                false,
            )
            .await?,
            instancing: GeometryRenderPipelineKeysLevel2::new(
                ctx,
                pipeline_layout_key_uniform,
                msaa_samples,
                true,
                false,
            )
            .await?,
        })
    }
}

/// Geometry pipeline keys keyed by cull mode.
pub struct GeometryRenderPipelineKeysLevel2 {
    pub no_cull: GeometryRenderPipelineKeysLevel3,
    pub back_cull: GeometryRenderPipelineKeysLevel3,
    pub front_cull: GeometryRenderPipelineKeysLevel3,
}

impl GeometryRenderPipelineKeysLevel2 {
    /// Creates geometry pipeline keys for all cull modes.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        pipeline_layout_key: PipelineLayoutKey,
        msaa_samples: Option<u32>,
        instancing: bool,
        meta_storage_array: bool,
    ) -> Result<Self> {
        Ok(Self {
            no_cull: GeometryRenderPipelineKeysLevel3::new(
                ctx,
                pipeline_layout_key,
                msaa_samples,
                instancing,
                meta_storage_array,
                CullMode::None,
            )
            .await?,
            back_cull: GeometryRenderPipelineKeysLevel3::new(
                ctx,
                pipeline_layout_key,
                msaa_samples,
                instancing,
                meta_storage_array,
                CullMode::Back,
            )
            .await?,
            front_cull: GeometryRenderPipelineKeysLevel3::new(
                ctx,
                pipeline_layout_key,
                msaa_samples,
                instancing,
                meta_storage_array,
                CullMode::Front,
            )
            .await?,
        })
    }
}

/// Leaf geometry pipeline key holder.
pub struct GeometryRenderPipelineKeysLevel3 {
    pub render_pipeline_key: RenderPipelineKey,
}

impl GeometryRenderPipelineKeysLevel3 {
    /// Creates a geometry render pipeline key for a specific configuration.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        pipeline_layout_key: PipelineLayoutKey,
        msaa_samples: Option<u32>,
        instancing: bool,
        meta_storage_array: bool,
        cull_mode: CullMode,
    ) -> Result<Self> {
        let shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyGeometry {
                    instancing_transforms: instancing,
                    meta_storage_array,
                    msaa_samples,
                },
            )
            .await?;

        let mut vertex_buffer_layouts = vec![VERTEX_BUFFER_LAYOUT.clone()];
        if instancing {
            vertex_buffer_layouts.push(VERTEX_BUFFER_LAYOUT_INSTANCING.clone());
        }

        let color_targets = &[
            ColorTargetState::new(ctx.render_texture_formats.visiblity_data),
            ColorTargetState::new(ctx.render_texture_formats.barycentric),
            ColorTargetState::new(ctx.render_texture_formats.normal_tangent),
            ColorTargetState::new(ctx.render_texture_formats.barycentric_derivatives),
        ];

        Ok(Self {
            render_pipeline_key: render_pipeline_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipelines,
                ctx.pipeline_layouts,
                ctx.render_texture_formats.depth,
                pipeline_layout_key,
                shader_key,
                vertex_buffer_layouts.clone(),
                color_targets,
                msaa_samples,
                cull_mode,
            )
            .await?,
        })
    }
}

async fn render_pipeline_key(
    gpu: &AwsmRendererWebGpu,
    shaders: &mut Shaders,
    pipelines: &mut Pipelines,
    pipeline_layouts: &PipelineLayouts,
    depth_texture_format: TextureFormat,
    pipeline_layout_key: PipelineLayoutKey,
    shader_key: ShaderKey,
    vertex_buffer_layouts: Vec<VertexBufferLayout>,
    color_targets: &[ColorTargetState],
    msaa_sample_count: Option<u32>,
    cull_mode: CullMode,
) -> Result<RenderPipelineKey> {
    let primitive_state = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(FrontFace::Ccw)
        .with_cull_mode(cull_mode);

    let depth_stencil = DepthStencilState::new(depth_texture_format)
        .with_depth_write_enabled(true)
        .with_depth_compare(CompareFunction::LessEqual);

    let mut pipeline_cache_key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive_state.clone())
        .with_depth_stencil(depth_stencil.clone());

    for layout in vertex_buffer_layouts {
        pipeline_cache_key = pipeline_cache_key.with_push_vertex_buffer_layout(layout);
    }

    if let Some(sample_count) = msaa_sample_count {
        pipeline_cache_key =
            pipeline_cache_key.with_multisample(MultisampleState::new().with_count(sample_count));
    }

    for target in color_targets {
        pipeline_cache_key = pipeline_cache_key.with_push_fragment_targets(vec![target.clone()]);
    }

    Ok(pipelines
        .render
        .get_key(gpu, shaders, pipeline_layouts, pipeline_cache_key)
        .await?)
}
