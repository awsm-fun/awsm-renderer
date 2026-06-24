//! Lazy pipeline pool for the **custom-vertex** shadow caster.
//!
//! Keyed by `(shader_id, cube_face, double_sided)` and populated on demand by the
//! texture-finalize flow (parallel to the masked-shadow pool). A missing entry
//! means "not compiled yet" and the shadow render path falls back to the plain
//! (solid, depth-only) shadow pipeline — so a custom-vertex caster still casts a
//! shadow, just an UN-displaced one, until its variant lands.
//!
//! Non-instanced only (matches the geometry custom-vertex pool + the masked-
//! shadow pool's compiled shape) — so two pipelines per shader (cube_face ×
//! double_sided). The depth-state / cull / bias mirror the plain shadow caster
//! ([`shadow_pipeline_cache_key`]); the custom-vertex variant differs by (a) the
//! vertex shader that compiles the `custom_displace_vertex` hook and (b) a SECOND
//! vertex buffer at slot 1 supplying the `@location(10) uv0` attribute (the shared
//! zero buffer, `array_stride: 0` → every vertex reads `vec2(0.0)`), exactly like
//! the geometry custom-vertex pipeline.
//!
//! Reuses the MASKED-SHADOW group-0 bind group (augmented with vertex visibility
//! on `materials` / `frame_globals` / the texture pool — see
//! [`ShadowMaskedBindGroup`]), so the layout key comes from that group.

use std::collections::HashMap;

use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
use awsm_renderer_core::compare::CompareFunction;
use awsm_renderer_core::pipeline::depth_stencil::DepthStencilState;
use awsm_renderer_core::pipeline::multisample::MultisampleState;
use awsm_renderer_core::pipeline::primitive::{
    CullMode, FrontFace, PrimitiveState, PrimitiveTopology,
};
use awsm_renderer_core::pipeline::vertex::{VertexAttribute, VertexBufferLayout, VertexFormat};
use awsm_renderer_core::texture::TextureFormat;
use awsm_renderer_materials::MaterialShaderId;

use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey};
use crate::render_passes::geometry::bind_group::GeometryBindGroups;
use crate::render_passes::geometry::custom_vertex_pipeline::CUSTOM_VERTEX_UV0_LOCATION;
use crate::render_passes::geometry::pipeline::VERTEX_BUFFER_LAYOUT;
use crate::render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo;
use crate::render_passes::shadow_masked::bind_group::ShadowMaskedBindGroup;
use crate::render_passes::RenderPassInitContext;
use crate::shadows::shader::custom_vertex_cache_key::ShaderCacheKeyShadowCustomVertex;

/// Lookup key for one compiled custom-vertex shadow pipeline.
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct ShadowCustomVertexPipelineKeyId {
    pub shader_id: MaterialShaderId,
    pub cube_face: bool,
    /// Double-sided (no-cull) caster variant — thin / open geometry casts nothing
    /// under Front culling, so its custom-vertex shadow pipeline must render both
    /// faces too (mirrors the plain + masked caster's `double_sided` split).
    pub double_sided: bool,
}

/// Inputs describing one custom-vertex shadow variant to (re)compile.
#[derive(Clone)]
pub struct ShadowCustomVertexVariant {
    pub shader_id: MaterialShaderId,
    pub dynamic_vertex: DynamicVertexShaderInfo,
}

/// Lazy pool of custom-vertex shadow render pipelines.
pub struct ShadowCustomVertexPipelines {
    /// Pipeline layout for non-instanced casters (storage-array meta), built from
    /// the augmented masked-shadow group 0.
    pipeline_layout_key_storage: PipelineLayoutKey,
    main: HashMap<ShadowCustomVertexPipelineKeyId, RenderPipelineKey>,
    /// Shared zero buffer bound at the uv0 slot (`array_stride: 0` → every vertex
    /// reads `vec2(0.0)`). Owned here so the shadow draw path can bind it.
    uv0_zero_buffer: web_sys::GpuBuffer,
}

impl ShadowCustomVertexPipelines {
    /// Resolves the custom-vertex shadow pipeline layout (augmented masked-shadow
    /// group 0 + the geometry transforms/meta/animation groups) + allocates the
    /// shared zero uv0 buffer, returning an empty pool.
    pub fn new(
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &ShadowMaskedBindGroup,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_key_storage =
            resolve_layout_key(ctx, masked_bind_group, geometry_bind_groups)?;
        let uv0_zero_buffer = create_uv0_zero_buffer(ctx)?;
        Ok(Self {
            pipeline_layout_key_storage,
            main: HashMap::new(),
            uv0_zero_buffer,
        })
    }

    /// Re-resolves the pipeline layout after the masked-shadow group-0 layout
    /// changed (texture-pool growth). Existing pool entries are cleared — the
    /// caller recompiles the live variants against the new layout.
    pub fn relayout(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &ShadowMaskedBindGroup,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<()> {
        self.pipeline_layout_key_storage =
            resolve_layout_key(ctx, masked_bind_group, geometry_bind_groups)?;
        self.main.clear();
        Ok(())
    }

    /// Compiles one custom-vertex shadow variant (shader + the
    /// cube_face × double_sided pipelines) folded into the pool. Mirrors the
    /// masked-shadow pool's `ensure_variant`.
    pub async fn ensure_variant(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &ShadowMaskedBindGroup,
        variant: &ShadowCustomVertexVariant,
    ) -> Result<()> {
        let shader_cache = ShaderCacheKeyShadowCustomVertex {
            shader_id: variant.shader_id,
            dynamic_vertex: variant.dynamic_vertex.clone(),
            texture_pool_arrays_len: masked_bind_group.texture_pool_arrays_len,
            texture_pool_samplers_len: masked_bind_group.texture_pool_sampler_keys.len() as u32,
            instancing_transforms: false,
        };
        ctx.shaders
            .ensure_keys(ctx.gpu, vec![shader_cache.clone().into()])
            .await?;
        let shader_key = ctx.shaders.get_key(ctx.gpu, shader_cache).await?;

        // (cube_face, double_sided) — non-instanced only.
        let combos: Vec<(bool, bool)> = [false, true]
            .into_iter()
            .flat_map(|cube_face| [false, true].into_iter().map(move |ds| (cube_face, ds)))
            .collect();
        let cache_keys: Vec<_> = combos
            .iter()
            .map(|&(cube_face, double_sided)| {
                build_shadow_custom_vertex_cache_key(
                    shader_key,
                    self.pipeline_layout_key_storage,
                    cube_face,
                    double_sided,
                )
            })
            .collect();

        let keys = ctx
            .pipelines
            .render
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;

        for ((cube_face, double_sided), key) in combos.into_iter().zip(keys) {
            self.main.insert(
                ShadowCustomVertexPipelineKeyId {
                    shader_id: variant.shader_id,
                    cube_face,
                    double_sided,
                },
                key,
            );
        }
        Ok(())
    }

    /// Drops every compiled custom-vertex shadow pipeline.
    pub fn clear(&mut self) {
        self.main.clear();
    }

    /// Looks up a compiled custom-vertex shadow pipeline for a caster's
    /// `(shader_id, cube_face, double_sided)`. `None` → not compiled yet (render
    /// path falls back to the plain solid shadow pipeline → un-displaced shadow).
    pub fn get(
        &self,
        shader_id: MaterialShaderId,
        cube_face: bool,
        double_sided: bool,
    ) -> Option<RenderPipelineKey> {
        self.main
            .get(&ShadowCustomVertexPipelineKeyId {
                shader_id,
                cube_face,
                double_sided,
            })
            .copied()
    }

    /// The shared zero uv0 buffer to bind at the custom-vertex uv0 slot (slot 1).
    pub fn uv0_zero_buffer(&self) -> &web_sys::GpuBuffer {
        &self.uv0_zero_buffer
    }
}

/// Builds the 8-byte shared zero uv0 buffer (one `vec2<f32>` of zeros). The
/// custom-vertex layout binds it with `array_stride: 0`, so this single buffer
/// satisfies any vertex count.
fn create_uv0_zero_buffer(ctx: &mut RenderPassInitContext<'_>) -> Result<web_sys::GpuBuffer> {
    let buffer = ctx.gpu.create_buffer(
        &BufferDescriptor::new(
            Some("Shadow Custom Vertex - uv0 zero"),
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
    masked_bind_group: &ShadowMaskedBindGroup,
    geometry_bind_groups: &GeometryBindGroups,
) -> Result<PipelineLayoutKey> {
    // Non-instanced: storage-array meta (the shadow pass binds the storage meta
    // group and sets `first_instance = mesh_meta_idx`).
    Ok(ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![
            masked_bind_group.bind_group_layout_key,
            geometry_bind_groups.transforms.bind_group_layout_key,
            geometry_bind_groups.meta.storage_layout_key,
            geometry_bind_groups.animation.bind_group_layout_key,
        ]),
    )?)
}

/// The uv0 vertex-buffer layout: `array_stride: 0` so every vertex reads the
/// single `vec2<f32>` at offset 0 of the shared zero buffer. Same layout the
/// geometry custom-vertex pipeline uses (`@location(10)`).
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

/// Depth-only custom-vertex shadow pipeline cache key. Mirrors
/// [`crate::shadows::shadow_pipeline_cache_key`] (Front cull, cube Cw, slope
/// bias, single-sample, Depth32float) but adds the uv0 buffer at slot 1.
/// Non-instanced only, so the geometry buffer is the sole base vertex stream.
fn build_shadow_custom_vertex_cache_key(
    shader_key: crate::shaders::ShaderKey,
    pipeline_layout_key: PipelineLayoutKey,
    cube_face: bool,
    double_sided: bool,
) -> RenderPipelineCacheKey {
    let front_face = if cube_face {
        FrontFace::Cw
    } else {
        FrontFace::Ccw
    };
    let cull_mode = if double_sided {
        CullMode::None
    } else {
        CullMode::Front
    };
    let primitive = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(front_face)
        .with_cull_mode(cull_mode);

    let depth_stencil = DepthStencilState::new(TextureFormat::Depth32float)
        .with_depth_write_enabled(true)
        .with_depth_compare(CompareFunction::LessEqual)
        .with_depth_bias(1)
        .with_depth_bias_slope_scale(1.5);

    let multisample = MultisampleState::new().with_count(1);

    // Slot 0: visibility geometry (locations 0-5). Slot 1: the uv0 buffer
    // (location 10). No instancing slot — non-instanced only.
    let mut key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive)
        .with_depth_stencil(depth_stencil)
        .with_multisample(multisample);
    key = key.with_push_vertex_buffer_layout(VERTEX_BUFFER_LAYOUT.clone());
    key = key.with_push_vertex_buffer_layout(uv0_vertex_buffer_layout());
    key
}
