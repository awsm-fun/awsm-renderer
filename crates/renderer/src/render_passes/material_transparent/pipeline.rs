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
        // Note: this adapter exposes only `maxBindGroups=4` so the
        // transparent pipeline cannot also bind the shared shadow
        // 16.B layout: `lights` folded into `main`, freeing slot 1
        // for `shadows`. Slot order is `main / shadows / texture_pool
        // / mesh_material` — matches `get_bind_groups`'s return order.
        let pipeline_layout_cache_key = PipelineLayoutCacheKey::new(vec![
            bind_groups.main_bind_group_layout_key,
            bind_groups.shadows_bind_group_layout_key,
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

    /// Creates and caches a render pipeline for a single mesh.
    ///
    /// `material_has_transmission` is supplied by the caller (computed
    /// from `Materials::has_transmission(mesh.material_key)`) because
    /// this function lives in a sub-module that doesn't carry a
    /// `&Materials` of its own. It drives the depth-write state — see
    /// `build_transparent_pipeline_cache_key` for the rationale.
    ///
    /// Thin wrapper over [`Self::set_render_pipeline_keys_batched`].
    /// Hot-path one-mesh callers (procedural meshes inserted live,
    /// instancing-promotion) keep their existing single-await API;
    /// bulk callers (gltf populate, texture-pool finalize) should use
    /// the batched form so Dawn parallelises the compiles.
    #[allow(clippy::too_many_arguments)]
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
        textures: &Textures,
        render_texture_formats: &RenderTextureFormats,
        material_has_transmission: bool,
        material_base: crate::dynamic_materials::ShadingBase,
        material_pbr_features: u32,
    ) -> Result<RenderPipelineKey> {
        let keys = self
            .set_render_pipeline_keys_batched(
                gpu,
                std::iter::once(TransparentMeshPipelineRequest {
                    mesh,
                    mesh_key,
                    buffer_info_key,
                    has_transmission: material_has_transmission,
                    base: material_base,
                    pbr_features: material_pbr_features,
                }),
                shaders,
                pipelines,
                material_bind_groups,
                pipeline_layouts,
                mesh_buffer_infos,
                anti_aliasing,
                textures,
                render_texture_formats,
            )
            .await?;
        Ok(keys[0])
    }

    /// Updates the per-pass pipeline layout key after a
    /// texture-pool layout change. Sync; just rebuilds the
    /// `PipelineLayouts` cache entry against the new bind-group
    /// layouts.
    pub fn refresh_pipeline_layout(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut crate::bind_group_layout::BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        bind_groups: &MaterialTransparentBindGroups,
    ) -> Result<()> {
        let pipeline_layout_cache_key = PipelineLayoutCacheKey::new(vec![
            bind_groups.main_bind_group_layout_key,
            bind_groups.shadows_bind_group_layout_key,
            bind_groups.texture_pool_textures_bind_group_layout_key,
            bind_groups.mesh_material_bind_group_layout_key,
        ]);
        self.pipeline_layout_key =
            pipeline_layouts.get_key(gpu, bind_group_layouts, pipeline_layout_cache_key)?;
        Ok(())
    }

    /// Returns the shader cache keys this batch would compile.
    /// Sync; the pooled `finalize_gpu_textures` path uses this to
    /// merge per-mesh transparent shader-warm into a single
    /// cross-pass `Shaders::ensure_keys`.
    pub fn shader_cache_keys_for_requests<'a, I>(
        requests: I,
        material_bind_groups: &MaterialTransparentBindGroups,
        mesh_buffer_infos: &MeshBufferInfos,
        anti_aliasing: &AntiAliasing,
    ) -> Result<Vec<ShaderCacheKeyMaterialTransparent>>
    where
        I: IntoIterator<Item = &'a TransparentMeshPipelineRequest<'a>>,
    {
        let texture_pool_arrays_len = material_bind_groups.texture_pool_arrays_len;
        let texture_pool_samplers_len = material_bind_groups.texture_pool_sampler_keys.len() as u32;
        let mut out = Vec::new();
        for req in requests {
            let mesh_buffer_info = mesh_buffer_infos.get(req.buffer_info_key)?;
            out.push(ShaderCacheKeyMaterialTransparent {
                attributes: mesh_buffer_info.into(),
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: anti_aliasing.msaa_sample_count,
                mipmaps: anti_aliasing.mipmap,
                base: req.base,
                pbr_features: req.pbr_features,
                dispatch_hash: 0,
                dynamic_shader_id: None,
                dynamic_shader: None,
                instancing_transforms: req.mesh.instanced,
                // GPU light-culling consumer-side cache key field
                // (froxel slice count baked into the z-slice math).
                froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
            });
        }
        Ok(out)
    }

    /// Builds the per-mesh render pipeline cache keys. Requires that
    /// `shaders` has already warmed every cache key returned by
    /// [`Self::shader_cache_keys_for_requests`]. Sync apart from the
    /// `Shaders::get_key` hash hits.
    #[allow(clippy::too_many_arguments)]
    pub async fn pipeline_cache_keys_for_requests<'a, I>(
        &self,
        gpu: &AwsmRendererWebGpu,
        requests: I,
        shaders: &mut Shaders,
        material_bind_groups: &MaterialTransparentBindGroups,
        mesh_buffer_infos: &MeshBufferInfos,
        anti_aliasing: &AntiAliasing,
        render_texture_formats: &RenderTextureFormats,
    ) -> Result<Vec<RenderPipelineCacheKey>>
    where
        I: IntoIterator<Item = &'a TransparentMeshPipelineRequest<'a>>,
    {
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

        let texture_pool_arrays_len = material_bind_groups.texture_pool_arrays_len;
        let texture_pool_samplers_len = material_bind_groups.texture_pool_sampler_keys.len() as u32;

        let mut out = Vec::new();
        for req in requests {
            let mesh_buffer_info = mesh_buffer_infos.get(req.buffer_info_key)?;
            let shader_cache_key = ShaderCacheKeyMaterialTransparent {
                attributes: mesh_buffer_info.into(),
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: anti_aliasing.msaa_sample_count,
                mipmaps: anti_aliasing.mipmap,
                base: req.base,
                pbr_features: req.pbr_features,
                dispatch_hash: 0,
                dynamic_shader_id: None,
                dynamic_shader: None,
                instancing_transforms: req.mesh.instanced,
                froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
            };
            let shader_key = shaders.get_key(gpu, shader_cache_key).await?;
            let vbo_layouts = vertex_buffer_layouts(req.mesh, mesh_buffer_info);
            let cull_mode = if req.mesh.double_sided {
                CullMode::None
            } else {
                CullMode::Back
            };
            out.push(build_transparent_pipeline_cache_key(
                render_texture_formats.depth,
                self.pipeline_layout_key,
                shader_key,
                vbo_layouts,
                color_targets,
                anti_aliasing.msaa_sample_count,
                cull_mode,
                req.has_transmission,
            ));
        }
        Ok(out)
    }

    /// Installs pre-resolved render pipeline keys against the given
    /// mesh keys. Sync; the caller is responsible for running
    /// `RenderPipelines::ensure_keys` with the corresponding cache
    /// keys first.
    pub fn install_per_mesh_keys(
        &mut self,
        mesh_keys: impl IntoIterator<Item = MeshKey>,
        pipeline_keys: impl IntoIterator<Item = RenderPipelineKey>,
    ) {
        for (mesh_key, pipeline_key) in mesh_keys.into_iter().zip(pipeline_keys) {
            self.render_pipeline_keys.insert(mesh_key, pipeline_key);
        }
    }

    /// Batched form of [`Self::set_render_pipeline_key`] — issues one
    /// `Shaders::ensure_keys` + one `RenderPipelines::ensure_keys` for
    /// the entire request list, then folds the results into the
    /// per-mesh `render_pipeline_keys` map.
    ///
    /// On a cold PSO disk cache this turns "N meshes × per-mesh wall
    /// clock for shader + pipeline compile" into max(shader_compile) +
    /// max(pipeline_compile) bounded by Dawn's compile pool. Returns
    /// the resolved keys in request order so the caller can pair them
    /// with their inputs (e.g. for follow-up bookkeeping).
    #[allow(clippy::too_many_arguments)]
    pub async fn set_render_pipeline_keys_batched<'a, I>(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        requests: I,
        shaders: &mut Shaders,
        pipelines: &mut Pipelines,
        material_bind_groups: &MaterialTransparentBindGroups,
        pipeline_layouts: &PipelineLayouts,
        mesh_buffer_infos: &MeshBufferInfos,
        anti_aliasing: &AntiAliasing,
        _textures: &Textures,
        render_texture_formats: &RenderTextureFormats,
    ) -> Result<Vec<RenderPipelineKey>>
    where
        I: IntoIterator<Item = TransparentMeshPipelineRequest<'a>>,
    {
        // Collect inputs into a vec so we can iterate twice.
        let requests: Vec<TransparentMeshPipelineRequest<'a>> = requests.into_iter().collect();
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        let texture_pool_arrays_len = material_bind_groups.texture_pool_arrays_len;
        let texture_pool_samplers_len = material_bind_groups.texture_pool_sampler_keys.len() as u32;

        // Build all shader cache keys first.
        let mut shader_cache_keys: Vec<ShaderCacheKeyMaterialTransparent> =
            Vec::with_capacity(requests.len());
        for req in &requests {
            let mesh_buffer_info = mesh_buffer_infos.get(req.buffer_info_key)?;
            shader_cache_keys.push(ShaderCacheKeyMaterialTransparent {
                attributes: mesh_buffer_info.into(),
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: anti_aliasing.msaa_sample_count,
                mipmaps: anti_aliasing.mipmap,
                base: req.base,
                pbr_features: req.pbr_features,
                dispatch_hash: 0,
                dynamic_shader_id: None,
                dynamic_shader: None,
                instancing_transforms: req.mesh.instanced,
                froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
            });
        }

        // Batch 1: shader compiles in parallel.
        shaders
            .ensure_keys(
                gpu,
                shader_cache_keys
                    .iter()
                    .cloned()
                    .map(crate::shaders::ShaderCacheKey::from),
            )
            .await?;

        // Build per-mesh pipeline cache keys (shaders now warm).
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
        let mut pipeline_cache_keys: Vec<RenderPipelineCacheKey> =
            Vec::with_capacity(requests.len());
        for (req, shader_cache_key) in requests.iter().zip(shader_cache_keys.iter()) {
            let mesh_buffer_info = mesh_buffer_infos.get(req.buffer_info_key)?;
            let shader_key = shaders.get_key(gpu, shader_cache_key.clone()).await?;
            let vbo_layouts = vertex_buffer_layouts(req.mesh, mesh_buffer_info);
            let cull_mode = if req.mesh.double_sided {
                CullMode::None
            } else {
                CullMode::Back
            };
            pipeline_cache_keys.push(build_transparent_pipeline_cache_key(
                render_texture_formats.depth,
                self.pipeline_layout_key,
                shader_key,
                vbo_layouts,
                color_targets,
                anti_aliasing.msaa_sample_count,
                cull_mode,
                req.has_transmission,
            ));
        }

        // Batch 2: render pipeline creates in parallel.
        let pipeline_keys = pipelines
            .render
            .ensure_keys(gpu, shaders, pipeline_layouts, pipeline_cache_keys)
            .await?;

        // Fold per-mesh map.
        for (req, &pipeline_key) in requests.iter().zip(pipeline_keys.iter()) {
            self.render_pipeline_keys.insert(req.mesh_key, pipeline_key);
        }

        Ok(pipeline_keys)
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

/// Build (do not create) a transparent-pipeline cache key. The
/// per-mesh depth-write flag is documented inline below — that flag
/// is part of the cache key so transmissive and non-transmissive
/// transparents get distinct pipelines.
///
/// Pure-sync; the batched caller hands the resulting keys to
/// `RenderPipelines::ensure_keys` to compile them in parallel.
fn build_transparent_pipeline_cache_key(
    depth_texture_format: TextureFormat,
    pipeline_layout_key: PipelineLayoutKey,
    shader_key: ShaderKey,
    vertex_buffer_layouts: Vec<VertexBufferLayout>,
    color_targets: &[ColorTargetState],
    msaa_sample_count: Option<u32>,
    cull_mode: CullMode,
    has_transmission: bool,
) -> RenderPipelineCacheKey {
    let primitive_state = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(FrontFace::Ccw)
        .with_cull_mode(cull_mode);

    // Depth-write is *per-material*:
    //
    //   - Transmissive (`KHR_materials_transmission`) surfaces want
    //     depth_write ON so a double-sided glass bowl draws only the
    //     near-facing fragment per pixel; without it the back face
    //     also draws and its refraction composites over the front
    //     face's, doubling the transmission and wiping the
    //     silhouette.
    //
    //   - Pure alpha-blend surfaces (smoke, dome panes, sprites)
    //     want depth_write OFF so layered transparents can compose
    //     correctly under the back-to-front sort in
    //     `collect_renderables`. With depth_write on, the first-
    //     drawn (farthest) transparent fragment writes depth and
    //     any closer transparent at the same screen pixel still
    //     passes the LessEqual test fine — but two transparents at
    //     overlapping depths in the SAME emitter or an emitter +
    //     dome combo end up culled instead of composited.
    let depth_stencil = DepthStencilState::new(depth_texture_format)
        .with_depth_write_enabled(has_transmission)
        .with_depth_compare(CompareFunction::LessEqual);

    let mut pipeline_cache_key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive_state)
        .with_depth_stencil(depth_stencil);

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

    pipeline_cache_key
}

/// One mesh's worth of transparent-pipeline build input, used by
/// [`MaterialTransparentPipelines::set_render_pipeline_keys_batched`].
pub struct TransparentMeshPipelineRequest<'a> {
    pub mesh: &'a Mesh,
    pub mesh_key: MeshKey,
    pub buffer_info_key: MeshBufferInfoKey,
    pub has_transmission: bool,
    /// Shading family of this mesh's material — the transparent fragment
    /// specializes its body at compile time on it (no uber runtime
    /// branch). Derive via `Materials::transparent_variant`.
    pub base: crate::dynamic_materials::ShadingBase,
    /// PBR feature mask this transparent PBR pipeline is specialized for
    /// (`PbrFeatures::from_material(..).bits()`); inert for non-PBR.
    pub pbr_features: u32,
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
