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
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            VertexAttribute {
                format: VertexFormat::Uint32,
                offset: 12,
                shader_location: 1,
            },
            VertexAttribute {
                format: VertexFormat::Float32x2,
                offset: 16,
                shader_location: 2,
            },
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 24,
                shader_location: 3,
            },
            VertexAttribute {
                format: VertexFormat::Float32x4,
                offset: 36,
                shader_location: 4,
            },
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
    pub pipeline_layout_key_storage: PipelineLayoutKey,
    pub pipeline_layout_key_uniform: PipelineLayoutKey,
    render_pipeline_keys: GeometryRenderPipelineKeys,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum GeometryPipelineShape {
    NoInstancingStorageMeta,
    NoInstancingUniformMeta,
    Instancing,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum GeometryCullKey {
    None,
    Back,
    Front,
}

impl GeometryCullKey {
    fn from_cull_mode(mode: CullMode) -> Result<Self> {
        match mode {
            CullMode::None => Ok(Self::None),
            CullMode::Back => Ok(Self::Back),
            CullMode::Front => Ok(Self::Front),
            other => Err(AwsmError::UnsupportedCullMode(other)),
        }
    }
}

/// Per-leaf identity used by [`GeometryPipelines::from_resolved`] to
/// fold a flat result vec into the nested struct.
#[derive(Clone, Copy, Debug)]
pub struct GeometryLeafSlot {
    pub msaa_4: bool,
    pub shape: GeometryPipelineShape,
    pub cull: GeometryCullKey,
}

/// Output of [`GeometryPipelines::build_descriptors`]: pipeline
/// layout keys, the 18 render-pipeline cache keys (in input order),
/// and matching slot identifiers. Consumed by `RenderPasses::new`
/// (which pools the cache keys with every other pass's into one
/// `RenderPipelines::ensure_keys` call) and then handed back to
/// [`GeometryPipelines::from_resolved`].
pub struct GeometryPrewarmDescriptors {
    pub pipeline_layout_key_storage: PipelineLayoutKey,
    pub pipeline_layout_key_uniform: PipelineLayoutKey,
    pub pipeline_cache_keys: Vec<RenderPipelineCacheKey>,
    pub slots: Vec<GeometryLeafSlot>,
}

impl GeometryPipelines {
    /// Creates geometry pipeline layouts and cached keys.
    ///
    /// Wrapper for the standalone path — pre-warms 8 shader variants
    /// then compiles 18 pipelines in one `ensure_keys` batch. The
    /// cross-pass pooling path in `RenderPasses::new` calls
    /// [`Self::shader_cache_keys`] + [`Self::build_descriptors`] +
    /// [`Self::from_resolved`] directly so the geometry + opaque +
    /// every other pass's shader and pipeline compiles share one
    /// global batch.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &GeometryBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys())
            .await?;
        let descs = Self::build_descriptors(ctx, bind_groups).await?;
        let pipeline_keys = ctx
            .pipelines
            .render
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                descs.pipeline_cache_keys.clone(),
            )
            .await?;
        Self::from_resolved(&descs, pipeline_keys)
    }

    /// 8 unique shader cache keys: `(msaa × instancing × meta_storage)`
    /// collapsed across cull mode (cull has no shader effect).
    pub fn shader_cache_keys() -> Vec<crate::shaders::ShaderCacheKey> {
        let mut keys = Vec::with_capacity(8);
        for msaa_samples in [None, Some(4u32)] {
            for (instancing, meta_storage_array) in [(false, true), (false, false), (true, false)] {
                keys.push(
                    ShaderCacheKeyGeometry {
                        instancing_transforms: instancing,
                        meta_storage_array,
                        msaa_samples,
                    }
                    .into(),
                );
            }
        }
        keys
    }

    /// Resolves the bind-group-derived pipeline layouts + builds the
    /// 18 leaf render-pipeline cache keys (and matching slot ids).
    /// Requires that [`Self::shader_cache_keys`] has already been
    /// `Shaders::ensure_keys`'d.
    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &GeometryBindGroups,
    ) -> Result<GeometryPrewarmDescriptors> {
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

        let color_targets = [
            ColorTargetState::new(ctx.render_texture_formats.visiblity_data),
            ColorTargetState::new(ctx.render_texture_formats.barycentric),
            ColorTargetState::new(ctx.render_texture_formats.normal_tangent),
            ColorTargetState::new(ctx.render_texture_formats.barycentric_derivatives),
        ];
        let depth_format = ctx.render_texture_formats.depth;

        const CULL_MODES: &[CullMode] = &[CullMode::None, CullMode::Back, CullMode::Front];

        let mut pipeline_cache_keys = Vec::with_capacity(18);
        let mut slots = Vec::with_capacity(18);

        for msaa_samples in [None, Some(4u32)] {
            for &shape in &[
                GeometryPipelineShape::NoInstancingStorageMeta,
                GeometryPipelineShape::NoInstancingUniformMeta,
                GeometryPipelineShape::Instancing,
            ] {
                let (instancing, meta_storage_array, layout_key) = match shape {
                    GeometryPipelineShape::NoInstancingStorageMeta => {
                        (false, true, pipeline_layout_key_storage)
                    }
                    GeometryPipelineShape::NoInstancingUniformMeta => {
                        (false, false, pipeline_layout_key_uniform)
                    }
                    GeometryPipelineShape::Instancing => (true, false, pipeline_layout_key_uniform),
                };
                let shader_cache = ShaderCacheKeyGeometry {
                    instancing_transforms: instancing,
                    meta_storage_array,
                    msaa_samples,
                };
                let shader_key = ctx.shaders.get_key(ctx.gpu, shader_cache).await?;
                for &cull_mode in CULL_MODES {
                    pipeline_cache_keys.push(build_geometry_cache_key(
                        shader_key,
                        layout_key,
                        depth_format,
                        &color_targets,
                        msaa_samples,
                        instancing,
                        cull_mode,
                    ));
                    slots.push(GeometryLeafSlot {
                        msaa_4: msaa_samples == Some(4),
                        shape,
                        cull: GeometryCullKey::from_cull_mode(cull_mode)?,
                    });
                }
            }
        }

        Ok(GeometryPrewarmDescriptors {
            pipeline_layout_key_storage,
            pipeline_layout_key_uniform,
            pipeline_cache_keys,
            slots,
        })
    }

    /// Folds resolved pipeline keys back into the nested level-1/2/3
    /// struct shape `get_render_pipeline_key` walks.
    pub fn from_resolved(
        descs: &GeometryPrewarmDescriptors,
        pipeline_keys: Vec<RenderPipelineKey>,
    ) -> Result<Self> {
        let mut slot_map: HashMap<
            (bool, GeometryPipelineShape, GeometryCullKey),
            RenderPipelineKey,
        > = HashMap::with_capacity(descs.slots.len());
        for (slot, key) in descs.slots.iter().zip(pipeline_keys) {
            slot_map.insert((slot.msaa_4, slot.shape, slot.cull), key);
        }
        let take = |msaa_4: bool,
                    shape: GeometryPipelineShape,
                    cull: GeometryCullKey|
         -> RenderPipelineKey {
            *slot_map.get(&(msaa_4, shape, cull)).unwrap_or_else(|| {
                panic!(
                    "geometry pipeline slot missing: msaa_4={msaa_4} shape={shape:?} cull={cull:?}"
                );
            })
        };
        let level2 =
            |msaa_4: bool, shape: GeometryPipelineShape| -> GeometryRenderPipelineKeysLevel2 {
                GeometryRenderPipelineKeysLevel2 {
                    no_cull: GeometryRenderPipelineKeysLevel3 {
                        render_pipeline_key: take(msaa_4, shape, GeometryCullKey::None),
                    },
                    back_cull: GeometryRenderPipelineKeysLevel3 {
                        render_pipeline_key: take(msaa_4, shape, GeometryCullKey::Back),
                    },
                    front_cull: GeometryRenderPipelineKeysLevel3 {
                        render_pipeline_key: take(msaa_4, shape, GeometryCullKey::Front),
                    },
                }
            };
        let level1 = |msaa_4: bool| -> GeometryRenderPipelineKeysLevel1 {
            GeometryRenderPipelineKeysLevel1 {
                no_instancing_storage_meta: level2(
                    msaa_4,
                    GeometryPipelineShape::NoInstancingStorageMeta,
                ),
                no_instancing_uniform_meta: level2(
                    msaa_4,
                    GeometryPipelineShape::NoInstancingUniformMeta,
                ),
                instancing: level2(msaa_4, GeometryPipelineShape::Instancing),
            }
        };

        Ok(Self {
            pipeline_layout_key_storage: descs.pipeline_layout_key_storage,
            pipeline_layout_key_uniform: descs.pipeline_layout_key_uniform,
            render_pipeline_keys: GeometryRenderPipelineKeys {
                no_anti_alias: level1(false),
                msaa_4_anti_alias: level1(true),
            },
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

pub struct GeometryRenderPipelineKeyOpts<'a> {
    pub anti_aliasing: &'a AntiAliasing,
    pub instancing: bool,
    pub cull_mode: CullMode,
    pub meta_storage_array: bool,
}

pub struct GeometryRenderPipelineKeys {
    pub no_anti_alias: GeometryRenderPipelineKeysLevel1,
    pub msaa_4_anti_alias: GeometryRenderPipelineKeysLevel1,
}

pub struct GeometryRenderPipelineKeysLevel1 {
    pub no_instancing_storage_meta: GeometryRenderPipelineKeysLevel2,
    pub no_instancing_uniform_meta: GeometryRenderPipelineKeysLevel2,
    pub instancing: GeometryRenderPipelineKeysLevel2,
}

pub struct GeometryRenderPipelineKeysLevel2 {
    pub no_cull: GeometryRenderPipelineKeysLevel3,
    pub back_cull: GeometryRenderPipelineKeysLevel3,
    pub front_cull: GeometryRenderPipelineKeysLevel3,
}

pub struct GeometryRenderPipelineKeysLevel3 {
    pub render_pipeline_key: RenderPipelineKey,
}

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
