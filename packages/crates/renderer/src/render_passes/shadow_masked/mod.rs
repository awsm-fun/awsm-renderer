//! **Masked** (alpha-tested) shadow caster resources — the B2 hole-shaped-shadow
//! infrastructure.
//!
//! Held on [`RenderPasses`](crate::render_passes::RenderPasses) rather than on
//! `Shadows`: the augmented group-0 bind group reads the same material / mesh /
//! texture buffers the masked geometry pass does (via `BindGroupRecreateContext`),
//! plus the shadow subsystem's stable `shadow_view` buffer — owning it here lets
//! it fold into the unified bind-group recreate dispatch without a `&mut Shadows`
//! borrow conflict (the recreate context already holds `&Shadows`). The shadow
//! render pass reaches it through `ctx.render_passes.shadow_masked`.
//!
//! The shader + cache key live under `crate::shadows::shader::masked_*` (the
//! shadow subsystem's askama dir); only the bind group + lazy pipeline pool live
//! here.

pub mod bind_group;
pub mod pipeline;

use bind_group::ShadowMaskedBindGroup;
use pipeline::ShadowMaskedPipelines;

/// Masked-shadow caster bind group + lazy pipeline pool.
pub struct ShadowMaskedRenderPass {
    /// Augmented group-0 bind group (shadow_view + material data + texture pool).
    pub bind_group: ShadowMaskedBindGroup,
    /// Lazy per-`shader_id` pool of masked-shadow pipelines. Empty until a
    /// masked material's variant is compiled by the texture-finalize flow.
    pub pipelines: ShadowMaskedPipelines,
}
