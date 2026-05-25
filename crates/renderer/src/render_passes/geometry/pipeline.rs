//! Geometry pass pipeline setup.

use std::collections::HashMap;
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

use crate::anti_alias::AntiAliasing;
use crate::error::{AwsmError, Result};
use crate::meshes::buffer_info::MeshBufferVertexInfo;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::render_passes::geometry::shader::cache_key::ShaderCacheKeyGeometry;
use crate::render_passes::{geometry::bind_group::GeometryBindGroups, RenderPassInitContext};

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum PipelineShape {
    NoInstancingStorageMeta,
    NoInstancingUniformMeta,
    Instancing,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum CullKey {
    None,
    Back,
    Front,
}

impl CullKey {
    fn from_cull_mode(mode: CullMode) -> Result<Self> {
        match mode {
            CullMode::None => Ok(Self::None),
            CullMode::Back => Ok(Self::Back),
            CullMode::Front => Ok(Self::Front),
            other => Err(AwsmError::UnsupportedCullMode(other)),
        }
    }
}

impl GeometryPipelines {
    /// Creates geometry pipeline layouts and cached keys.
    ///
    /// Compiles all 18 leaf pipeline variants in **two batches**:
    /// first all unique shader variants (`(instancing × meta_storage ×
    /// msaa)`) concurrently via `Shaders::ensure_keys`, then all 18
    /// pipelines (`2 msaa × 3 (instancing × meta) × 3 cull`)
    /// concurrently via `RenderPipelines::ensure_keys`.
    ///
    /// On a cold PSO disk cache this is the single biggest wall-clock
    /// win in the renderer init path — the previous nested `.await?`
    /// loop strictly serialised every leaf compile against the
    /// preceding one.
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

        // Enumerate every leaf descriptor.
        //
        // 2 msaa × 3 (instancing × meta-binding) × 3 cull = 18 leaf
        // pipelines. Holding the full set in one Vec lets us issue
        // shader and pipeline batches in two parallel passes.
        struct LeafDesc {
            shader_cache: ShaderCacheKeyGeometry,
            pipeline_layout_key: PipelineLayoutKey,
            msaa_samples: Option<u32>,
            instancing: bool,
            cull_mode: CullMode,
            msaa_4: bool,
            shape: PipelineShape,
        }

        const CULL_MODES: &[CullMode] = &[CullMode::None, CullMode::Back, CullMode::Front];

        let mut leaves: Vec<LeafDesc> = Vec::with_capacity(18);
        for msaa_samples in [None, Some(4u32)] {
            for &shape in &[
                PipelineShape::NoInstancingStorageMeta,
                PipelineShape::NoInstancingUniformMeta,
                PipelineShape::Instancing,
            ] {
                let (instancing, meta_storage_array, layout_key) = match shape {
                    PipelineShape::NoInstancingStorageMeta => {
                        (false, true, pipeline_layout_key_storage)
                    }
                    PipelineShape::NoInstancingUniformMeta => {
                        (false, false, pipeline_layout_key_uniform)
                    }
                    PipelineShape::Instancing => (true, false, pipeline_layout_key_uniform),
                };
                let shader_cache = ShaderCacheKeyGeometry {
                    instancing_transforms: instancing,
                    meta_storage_array,
                    msaa_samples,
                };
                for &cull_mode in CULL_MODES {
                    leaves.push(LeafDesc {
                        shader_cache: shader_cache.clone(),
                        pipeline_layout_key: layout_key,
                        msaa_samples,
                        instancing,
                        cull_mode,
                        msaa_4: msaa_samples == Some(4),
                        shape,
                    });
                }
            }
        }

        // Batch 1: prewarm every shader variant. `Shaders::ensure_keys`
        // dedups internally; the 18 leaves cover 8 unique shader
        // points (2 msaa × 3 (instancing × meta) collapsed across
        // cull mode which has no shader effect).
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                leaves
                    .iter()
                    .map(|l| crate::shaders::ShaderCacheKey::from(l.shader_cache.clone())),
            )
            .await?;

        // Resolve shader keys (all cache hits) + build the 18
        // pipeline cache keys.
        let color_targets = [
            ColorTargetState::new(ctx.render_texture_formats.visiblity_data),
            ColorTargetState::new(ctx.render_texture_formats.barycentric),
            ColorTargetState::new(ctx.render_texture_formats.normal_tangent),
            ColorTargetState::new(ctx.render_texture_formats.barycentric_derivatives),
        ];
        let depth_format = ctx.render_texture_formats.depth;

        let mut pipeline_cache_keys: Vec<RenderPipelineCacheKey> = Vec::with_capacity(leaves.len());
        for leaf in &leaves {
            let shader_key = ctx
                .shaders
                .get_key(ctx.gpu, leaf.shader_cache.clone())
                .await?;
            pipeline_cache_keys.push(build_geometry_cache_key(
                shader_key,
                leaf.pipeline_layout_key,
                depth_format,
                &color_targets,
                leaf.msaa_samples,
                leaf.instancing,
                leaf.cull_mode,
            ));
        }

        // Batch 2: 18 pipelines in parallel.
        let pipeline_keys = ctx
            .pipelines
            .render
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                pipeline_cache_keys,
            )
            .await?;

        // Fold flat result vec back into the nested level-1/2/3
        // struct shape the runtime `get_render_pipeline_key` walks.
        let mut slots: HashMap<(bool, PipelineShape, CullKey), RenderPipelineKey> =
            HashMap::with_capacity(leaves.len());
        for (leaf, key) in leaves.into_iter().zip(pipeline_keys) {
            slots.insert(
                (
                    leaf.msaa_4,
                    leaf.shape,
                    CullKey::from_cull_mode(leaf.cull_mode)?,
                ),
                key,
            );
        }
        // All 18 slots are populated by the enumeration above; a
        // missing slot indicates a programming bug in this function
        // (e.g. a forgotten variant), not a runtime condition.
        let take = |msaa_4: bool, shape: PipelineShape, cull: CullKey| -> RenderPipelineKey {
            *slots.get(&(msaa_4, shape, cull)).unwrap_or_else(|| {
                panic!(
                    "geometry pipeline slot missing: msaa_4={msaa_4} shape={shape:?} cull={cull:?}"
                );
            })
        };
        let level2 = |msaa_4: bool, shape: PipelineShape| -> GeometryRenderPipelineKeysLevel2 {
            GeometryRenderPipelineKeysLevel2 {
                no_cull: GeometryRenderPipelineKeysLevel3 {
                    render_pipeline_key: take(msaa_4, shape, CullKey::None),
                },
                back_cull: GeometryRenderPipelineKeysLevel3 {
                    render_pipeline_key: take(msaa_4, shape, CullKey::Back),
                },
                front_cull: GeometryRenderPipelineKeysLevel3 {
                    render_pipeline_key: take(msaa_4, shape, CullKey::Front),
                },
            }
        };
        let level1 = |msaa_4: bool| -> GeometryRenderPipelineKeysLevel1 {
            GeometryRenderPipelineKeysLevel1 {
                no_instancing_storage_meta: level2(msaa_4, PipelineShape::NoInstancingStorageMeta),
                no_instancing_uniform_meta: level2(msaa_4, PipelineShape::NoInstancingUniformMeta),
                instancing: level2(msaa_4, PipelineShape::Instancing),
            }
        };

        let render_pipeline_keys = GeometryRenderPipelineKeys {
            no_anti_alias: level1(false),
            msaa_4_anti_alias: level1(true),
        };

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

/// Geometry pipeline keys keyed by cull mode.
pub struct GeometryRenderPipelineKeysLevel2 {
    pub no_cull: GeometryRenderPipelineKeysLevel3,
    pub back_cull: GeometryRenderPipelineKeysLevel3,
    pub front_cull: GeometryRenderPipelineKeysLevel3,
}

/// Leaf geometry pipeline key holder.
pub struct GeometryRenderPipelineKeysLevel3 {
    pub render_pipeline_key: RenderPipelineKey,
}

/// Builds a `RenderPipelineCacheKey` for one geometry leaf — the
/// pure-sync version of the previous async per-leaf builder, lifted
/// out so we can collect descriptors in bulk and hand them to
/// `RenderPipelines::ensure_keys`.
fn build_geometry_cache_key(
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

    let mut vertex_buffer_layouts = vec![VERTEX_BUFFER_LAYOUT.clone()];
    if instancing {
        vertex_buffer_layouts.push(VERTEX_BUFFER_LAYOUT_INSTANCING.clone());
    }

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
