//! Lazy pipeline pool for the **masked** (alpha-tested) shadow caster.
//!
//! Keyed by `(shader_id, instancing, cube_face)` and populated on demand by the
//! texture-finalize flow (parallel to the geometry masked pool). A missing entry
//! means "not compiled yet" and the shadow render path falls back to the plain
//! (solid, depth-only) shadow pipeline — so a masked caster still casts a shadow,
//! just a rectangular (un-cut) one, until its masked variant lands.
//!
//! Each variant compiles two shaders (instancing on/off) and four pipelines
//! (instancing × cube_face): `cube_face` only flips `front_face` (cube faces
//! apply a post-projection Y-flip), `instancing` selects the storage-array vs
//! uniform-with-dynamic-offset meta binding (and the per-instance transform
//! vertex buffer). The depth-state / cull / bias are shared with the plain
//! shadow caster via [`shadow_pipeline_cache_key`]; the masked variant only adds
//! a fragment stage (`with_force_fragment_stage`) that discards below the cutoff.

use std::collections::HashMap;

use awsm_renderer_materials::MaterialShaderId;

use crate::dynamic_materials::ShadingBase;
use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
use crate::pipelines::render_pipeline::RenderPipelineKey;
use crate::render_passes::geometry::bind_group::GeometryBindGroups;
use crate::render_passes::geometry::shader::masked_cache_key::DynamicAlphaShaderInfo;
use crate::render_passes::shadow_masked::bind_group::ShadowMaskedBindGroup;
use crate::render_passes::RenderPassInitContext;
use crate::shadows::shader::masked_cache_key::ShaderCacheKeyShadowMasked;
use crate::shadows::shadow_pipeline_cache_key;

/// Lookup key for one compiled masked-shadow pipeline.
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct MaskedShadowPipelineKeyId {
    pub shader_id: MaterialShaderId,
    pub instancing: bool,
    pub cube_face: bool,
    /// Double-sided (no-cull) caster variant — a cutout panel / leaf quad casts
    /// nothing under Front culling, so its masked-shadow pipeline must render
    /// both faces too (mirrors the plain caster's `double_sided` split).
    pub double_sided: bool,
}

/// Inputs describing one masked-shadow variant to (re)compile.
#[derive(Clone)]
pub struct MaskedShadowVariant {
    pub shader_id: MaterialShaderId,
    pub base: ShadingBase,
    pub dynamic_alpha: Option<DynamicAlphaShaderInfo>,
}

/// Lazy pool of masked-shadow render pipelines.
pub struct ShadowMaskedPipelines {
    /// Pipeline layout for non-instanced casters (storage-array meta).
    pipeline_layout_key_storage: PipelineLayoutKey,
    /// Pipeline layout for instanced casters (uniform-with-dynamic-offset meta).
    pipeline_layout_key_uniform: PipelineLayoutKey,
    main: HashMap<MaskedShadowPipelineKeyId, RenderPipelineKey>,
}

impl ShadowMaskedPipelines {
    /// Resolves the two masked-shadow pipeline layouts (augmented group 0 + the
    /// geometry transforms/meta/animation groups) and returns an empty pool.
    pub fn new(
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &ShadowMaskedBindGroup,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<Self> {
        let (pipeline_layout_key_storage, pipeline_layout_key_uniform) =
            resolve_layout_keys(ctx, masked_bind_group, geometry_bind_groups)?;
        Ok(Self {
            pipeline_layout_key_storage,
            pipeline_layout_key_uniform,
            main: HashMap::new(),
        })
    }

    /// Re-resolves the pipeline layouts after the masked group-0 layout changed
    /// (texture-pool growth). Existing pool entries are cleared — the caller
    /// recompiles the live variants against the new layout.
    pub fn relayout(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &ShadowMaskedBindGroup,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<()> {
        let (storage, uniform) = resolve_layout_keys(ctx, masked_bind_group, geometry_bind_groups)?;
        self.pipeline_layout_key_storage = storage;
        self.pipeline_layout_key_uniform = uniform;
        self.main.clear();
        Ok(())
    }

    /// Compiles one masked-shadow variant: two shaders (instancing on/off) ×
    /// four pipelines (instancing × cube_face) folded into the pool. Mirrors the
    /// geometry masked pool's `ensure_variant`.
    pub async fn ensure_variant(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &ShadowMaskedBindGroup,
        variant: &MaskedShadowVariant,
    ) -> Result<()> {
        for instancing in [false, true] {
            let shader_cache = ShaderCacheKeyShadowMasked {
                texture_pool_arrays_len: masked_bind_group.texture_pool_arrays_len,
                texture_pool_samplers_len: masked_bind_group.texture_pool_sampler_keys.len() as u32,
                shader_id: variant.shader_id,
                base: variant.base,
                dynamic_alpha: variant.dynamic_alpha.clone(),
                instancing_transforms: instancing,
            };
            ctx.shaders
                .ensure_keys(ctx.gpu, vec![shader_cache.clone().into()])
                .await?;
            let shader_key = ctx.shaders.get_key(ctx.gpu, shader_cache).await?;

            let layout_key = if instancing {
                self.pipeline_layout_key_uniform
            } else {
                self.pipeline_layout_key_storage
            };

            // Depth-only, bias-matched pipelines shared with the plain caster;
            // `with_force_fragment_stage` adds the discard fragment. Four cull
            // variants per shader: (cube_face × double_sided). Single-sided uses
            // Front cull; double-sided uses no-cull so thin cutout panels cast.
            let combos: Vec<(bool, bool)> = [false, true]
                .into_iter()
                .flat_map(|cube_face| [false, true].into_iter().map(move |ds| (cube_face, ds)))
                .collect();
            let cache_keys: Vec<_> = combos
                .iter()
                .map(|&(cube_face, double_sided)| {
                    shadow_pipeline_cache_key(
                        shader_key,
                        layout_key,
                        instancing,
                        cube_face,
                        double_sided,
                    )
                    .with_force_fragment_stage()
                })
                .collect();

            let keys = ctx
                .pipelines
                .render
                .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
                .await?;

            for ((cube_face, double_sided), key) in combos.into_iter().zip(keys) {
                self.main.insert(
                    MaskedShadowPipelineKeyId {
                        shader_id: variant.shader_id,
                        instancing,
                        cube_face,
                        double_sided,
                    },
                    key,
                );
            }
        }
        Ok(())
    }

    /// Drops every compiled masked-shadow pipeline.
    pub fn clear(&mut self) {
        self.main.clear();
    }

    /// Looks up a compiled masked-shadow pipeline for a caster's `(shader_id,
    /// instancing, cube_face)`. `None` → not compiled yet (render path falls
    /// back to the plain solid shadow pipeline).
    pub fn get(
        &self,
        shader_id: MaterialShaderId,
        instancing: bool,
        cube_face: bool,
        double_sided: bool,
    ) -> Option<RenderPipelineKey> {
        self.main
            .get(&MaskedShadowPipelineKeyId {
                shader_id,
                instancing,
                cube_face,
                double_sided,
            })
            .copied()
    }
}

fn resolve_layout_keys(
    ctx: &mut RenderPassInitContext<'_>,
    masked_bind_group: &ShadowMaskedBindGroup,
    geometry_bind_groups: &GeometryBindGroups,
) -> Result<(PipelineLayoutKey, PipelineLayoutKey)> {
    let storage = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![
            masked_bind_group.bind_group_layout_key,
            geometry_bind_groups.transforms.bind_group_layout_key,
            geometry_bind_groups.meta.storage_layout_key,
            geometry_bind_groups.animation.bind_group_layout_key,
        ]),
    )?;
    let uniform = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![
            masked_bind_group.bind_group_layout_key,
            geometry_bind_groups.transforms.bind_group_layout_key,
            geometry_bind_groups.meta.uniform_layout_key,
            geometry_bind_groups.animation.bind_group_layout_key,
        ]),
    )?;
    Ok((storage, uniform))
}
