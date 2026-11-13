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
    /// Bumped every time the pool is emptied — same role as
    /// `GeometryMaskedPipelines::epoch`.
    epoch: u64,
}

/// The two shapes every masked-shadow variant compiles: instancing off, on.
const INSTANCING_SHAPES: [bool; 2] = [false, true];

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
            epoch: 0,
        })
    }

    /// See the `epoch` field.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// True when a masked-shadow variant for `shader_id` is already compiled
    /// (any shape — every shape of a variant is inserted together).
    pub fn has_variant(&self, shader_id: MaterialShaderId) -> bool {
        self.main.keys().any(|k| k.shader_id == shader_id)
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
        self.epoch += 1;
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
        self.ensure_variants(ctx, masked_bind_group, std::slice::from_ref(variant))
            .await
    }

    /// [`Self::ensure_variant`] for several variants as ONE batch (see
    /// `GeometryMaskedPipelines::ensure_variants`): both instancing shapes of
    /// every variant go into a single shader ensure and a single pipeline
    /// ensure, so they compile concurrently instead of one wave per variant
    /// per shape. All-or-nothing on error.
    pub async fn ensure_variants(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
        masked_bind_group: &ShadowMaskedBindGroup,
        variants: &[MaskedShadowVariant],
    ) -> Result<()> {
        if variants.is_empty() {
            return Ok(());
        }
        let shader_keys = ctx
            .shaders
            .ensure_keys(
                ctx.gpu,
                Self::shader_cache_keys(masked_bind_group, variants),
            )
            .await?;
        let (cache_keys, slots) = self.pipeline_cache_keys(ctx, variants, &shader_keys);
        let keys = ctx
            .pipelines
            .render
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;
        self.insert_resolved(slots, keys);
        Ok(())
    }

    /// The shaders `variants` compile at the masked bind group's current
    /// texture-pool shape: two per variant (instancing off, then on), in
    /// variant order.
    pub(crate) fn shader_cache_keys(
        masked_bind_group: &ShadowMaskedBindGroup,
        variants: &[MaskedShadowVariant],
    ) -> Vec<crate::shaders::ShaderCacheKey> {
        variants
            .iter()
            .flat_map(|variant| INSTANCING_SHAPES.into_iter().map(move |i| (variant, i)))
            .map(|(variant, instancing)| {
                ShaderCacheKeyShadowMasked {
                    texture_pool_arrays_len: masked_bind_group.texture_pool_arrays_len,
                    texture_pool_samplers_len: masked_bind_group.texture_pool_samplers_len,
                    shader_id: variant.shader_id,
                    base: variant.base,
                    dynamic_alpha: variant.dynamic_alpha.clone(),
                    instancing_transforms: instancing,
                }
                .into()
            })
            .collect()
    }

    /// The pipelines of `variants` and the pool slot each one fills.
    /// `shader_keys` is the resolved [`Self::shader_cache_keys`] (two per
    /// variant). Sync — no compile.
    pub(crate) fn pipeline_cache_keys(
        &self,
        ctx: &RenderPassInitContext<'_>,
        variants: &[MaskedShadowVariant],
        shader_keys: &[crate::shaders::ShaderKey],
    ) -> (
        Vec<crate::pipelines::render_pipeline::RenderPipelineCacheKey>,
        Vec<MaskedShadowPipelineKeyId>,
    ) {
        // One entry per (variant, instancing shape) — the order
        // `shader_cache_keys` emits.
        let shapes = variants
            .iter()
            .flat_map(|variant| INSTANCING_SHAPES.into_iter().map(move |i| (variant, i)));

        // Depth-only, bias-matched pipelines shared with the plain caster;
        // `with_force_fragment_stage` adds the discard fragment. Four cull
        // variants per shader: (cube_face × double_sided). Single-sided uses
        // Front cull; double-sided uses no-cull so thin cutout panels cast.
        let combos: Vec<(bool, bool)> = [false, true]
            .into_iter()
            .flat_map(|cube_face| [false, true].into_iter().map(move |ds| (cube_face, ds)))
            .collect();
        let mut cache_keys = Vec::with_capacity(shader_keys.len() * combos.len());
        let mut slots = Vec::with_capacity(shader_keys.len() * combos.len());
        for ((variant, instancing), shader_key) in shapes.zip(shader_keys) {
            let layout_key = if instancing {
                self.pipeline_layout_key_uniform
            } else {
                self.pipeline_layout_key_storage
            };
            for &(cube_face, double_sided) in &combos {
                cache_keys.push(
                    shadow_pipeline_cache_key(
                        *shader_key,
                        layout_key,
                        instancing,
                        cube_face,
                        double_sided,
                        ctx.features.depth(),
                    )
                    .with_force_fragment_stage(),
                );
                slots.push(MaskedShadowPipelineKeyId {
                    shader_id: variant.shader_id,
                    instancing,
                    cube_face,
                    double_sided,
                });
            }
        }
        (cache_keys, slots)
    }

    /// Records compiled pipelines under the slots [`Self::pipeline_cache_keys`]
    /// returned for them.
    pub(crate) fn insert_resolved(
        &mut self,
        slots: Vec<MaskedShadowPipelineKeyId>,
        keys: Vec<RenderPipelineKey>,
    ) {
        for (slot, key) in slots.into_iter().zip(keys) {
            self.main.insert(slot, key);
        }
    }

    /// Drops every compiled masked-shadow pipeline.
    pub fn clear(&mut self) {
        self.main.clear();
        self.epoch += 1;
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
