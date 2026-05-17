//! Transparent material pass pipeline setup.

use awsm_renderer_core::compare::CompareFunction;
use awsm_renderer_core::pipeline::depth_stencil::DepthStencilState;
use awsm_renderer_core::pipeline::fragment::{
    BlendComponent, BlendFactor, BlendOperation, BlendState, ColorTargetState,
};
use awsm_renderer_core::pipeline::multisample::MultisampleState;
use awsm_renderer_core::pipeline::primitive::{
    CullMode, FrontFace, PrimitiveState, PrimitiveTopology,
};
use awsm_renderer_core::pipeline::vertex::{
    VertexAttribute, VertexBufferLayout, VertexFormat, VertexStepMode,
};
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use awsm_renderer_core::texture::TextureFormat;
use slotmap::SecondaryMap;

use crate::anti_alias::AntiAliasing;
use crate::error::Result;
use crate::meshes::buffer_info::{
    MeshBufferInfo, MeshBufferInfoKey, MeshBufferInfos, MeshBufferVertexAttributeInfo,
    MeshBufferVertexInfo,
};
use crate::meshes::mesh::Mesh;
use crate::meshes::MeshKey;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::pipelines::Pipelines;
use crate::render_passes::{
    material_transparent::{
        bind_group::MaterialTransparentBindGroups,
        shader::cache_key::ShaderCacheKeyMaterialTransparent,
    },
    RenderPassInitContext,
};
use crate::render_textures::RenderTextureFormats;
use crate::shaders::{ShaderKey, Shaders};
use crate::textures::Textures;

/// Render pipeline cache for transparent materials.
pub struct MaterialTransparentPipelines {
    pipeline_layout_key: PipelineLayoutKey,
    render_pipeline_keys: SecondaryMap<MeshKey, RenderPipelineKey>,
}

impl MaterialTransparentPipelines {
    /// Creates pipeline layout state for transparent materials.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialTransparentBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_cache_key = PipelineLayoutCacheKey::new(vec![
            bind_groups.main_bind_group_layout_key,
            bind_groups.lights_bind_group_layout_key,
            bind_groups.texture_pool_textures_bind_group_layout_key,
            bind_groups.mesh_material_bind_group_layout_key,
        ]);

        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            pipeline_layout_cache_key,
        )?;

        Ok(Self {
            pipeline_layout_key,
            render_pipeline_keys: SecondaryMap::new(),
        })
    }

    /// Creates and caches a render pipeline for a mesh.
    pub async fn set_render_pipeline_key(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        mesh: &Mesh,
        mesh_key: MeshKey,
        buffer_info_key: MeshBufferInfoKey,
        shaders: &mut Shaders,
        pipelines: &mut Pipelines,
        material_bind_groups: &MaterialTransparentBindGroups,
        pipeline_layouts: &PipelineLayouts,
        mesh_buffer_infos: &MeshBufferInfos,
        anti_aliasing: &AntiAliasing,
        _textures: &Textures,
        render_texture_formats: &RenderTextureFormats,
    ) -> Result<RenderPipelineKey> {
        let mesh_buffer_info = mesh_buffer_infos.get(buffer_info_key)?;

        let shader_cache_key = ShaderCacheKeyMaterialTransparent {
            attributes: mesh_buffer_info.into(),
            texture_pool_arrays_len: material_bind_groups.texture_pool_arrays_len,
            texture_pool_samplers_len: material_bind_groups.texture_pool_sampler_keys.len() as u32,
            msaa_sample_count: anti_aliasing.msaa_sample_count,
            mipmaps: anti_aliasing.mipmap,
            instancing_transforms: mesh.instanced,
        };

        let shader_key = shaders.get_key(gpu, shader_cache_key).await?;

        let color_targets = &[
            ColorTargetState::new(render_texture_formats.color).with_blend(BlendState::new(
                BlendComponent::new()
                    .with_src_factor(BlendFactor::One)
                    .with_dst_factor(BlendFactor::OneMinusSrcAlpha)
                    .with_operation(BlendOperation::Add),
                BlendComponent::new()
                    .with_src_factor(BlendFactor::One)
                    .with_dst_factor(BlendFactor::OneMinusSrcAlpha)
                    .with_operation(BlendOperation::Add),
            )),
        ];

        let render_pipeline_key = render_pipeline_key(
            gpu,
            shaders,
            pipelines,
            pipeline_layouts,
            render_texture_formats.depth,
            self.pipeline_layout_key,
            shader_key,
            vertex_buffer_layouts(mesh, mesh_buffer_info),
            color_targets,
            anti_aliasing.msaa_sample_count,
            if mesh.double_sided {
                CullMode::None
            } else {
                CullMode::Back
            },
            mesh.hud,
        )
        .await?;

        self.render_pipeline_keys
            .insert(mesh_key, render_pipeline_key);

        Ok(render_pipeline_key)
    }

    /// Returns the cached render pipeline key for a mesh, if present.
    pub fn get_render_pipeline_key(&self, mesh_key: MeshKey) -> Option<RenderPipelineKey> {
        self.render_pipeline_keys.get(mesh_key).cloned()
    }

    /// Copies a cached pipeline key from one mesh to another.
    pub fn clone_render_pipeline_key(&mut self, from: MeshKey, to: MeshKey) {
        if let Some(key) = self.render_pipeline_keys.get(from).cloned() {
            self.render_pipeline_keys.insert(to, key);
        }
    }

    /// Removes the cached render pipeline key for a mesh, if present.
    pub fn remove_render_pipeline_key(&mut self, mesh_key: MeshKey) -> Option<RenderPipelineKey> {
        self.render_pipeline_keys.remove(mesh_key)
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
    _is_hud: bool,
) -> Result<RenderPipelineKey> {
    let primitive_state = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(FrontFace::Ccw)
        .with_cull_mode(cull_mode);

    // Transparency requires depth-test ON (so opaque geometry in
    // front correctly occludes a translucent surface behind it) but
    // depth-write OFF. Writing depth from a translucent fragment
    // makes subsequent translucent fragments at *any* greater depth
    // fail the LessEqual test, which produces two visible bugs:
    //
    //   1. Cross-mesh: a particle behind a transparent dome pane
    //      (or vice versa) gets culled by whichever drew first,
    //      so the user sees one occluding the other instead of
    //      compositing through.
    //   2. Intra-emitter: particles in one billboard emitter draw
    //      in instance-buffer order, not depth order — the first
    //      instance writes its depth and any farther instance in
    //      the same cloud disappears.
    //
    // Back-to-front sort in `collect_renderables` keeps the
    // composite ordering correct without needing depth-write.
    let depth_stencil = DepthStencilState::new(depth_texture_format)
        .with_depth_write_enabled(false)
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

fn vertex_buffer_layouts(mesh: &Mesh, buffer_info: &MeshBufferInfo) -> Vec<VertexBufferLayout> {
    let mut out = vec![VertexBufferLayout {
        // this is the stride across all of the attributes
        // position (12) + normal (12) + tangent (16) = 40 bytes
        array_stride: MeshBufferVertexInfo::TRANSPARENCY_GEOMETRY_BYTE_SIZE as u64,
        step_mode: None,
        attributes: vec![
            // Position (vec3<f32>)
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            // Normal (vec3<f32>)
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            // Tangent (vec4<f32>)
            VertexAttribute {
                format: VertexFormat::Float32x4,
                offset: 24,
                shader_location: 2,
            },
        ],
    }];

    if mesh.instanced {
        let mut vertex_buffer_layout_instancing = VertexBufferLayout {
            // this is the stride across all of the attributes
            array_stride: MeshBufferVertexInfo::INSTANCING_BYTE_SIZE as u64,
            step_mode: Some(VertexStepMode::Instance),
            attributes: Vec::new(),
        };

        let start_location = out[0].attributes.len() as u32;

        for i in 0..4 {
            vertex_buffer_layout_instancing
                .attributes
                .push(VertexAttribute {
                    format: VertexFormat::Float32x4,
                    offset: i * 16,
                    shader_location: start_location + i as u32,
                });
        }

        out.push(vertex_buffer_layout_instancing);
    }

    let mut attributes = vec![];

    let mut offset = 0;

    let start_shader_location = out
        .last()
        .unwrap()
        .attributes
        .last()
        .unwrap()
        .shader_location
        + 1;

    for (shader_location, attribute_info) in (start_shader_location..).zip(
        buffer_info
            .triangles
            .vertex_attributes
            .iter()
            .filter(|x| x.is_custom_attribute()),
    ) {
        let custom_attribute_info = match attribute_info {
            MeshBufferVertexAttributeInfo::Custom(info) => info,
            _ => unreachable!("Expected custom attribute info"),
        };

        attributes.push(VertexAttribute {
            format: custom_attribute_info.vertex_format(),
            offset,
            shader_location,
        });

        offset += attribute_info.vertex_size() as u64;
    }

    out.push(VertexBufferLayout {
        array_stride: offset,
        step_mode: None,
        attributes,
    });

    out
}
