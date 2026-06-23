//! COMBINED **masked + custom-vertex** shadow caster resources — the displaced
//! AND cutout shadow path for a material that is BOTH glTF `MASK` AND carries a
//! `wgsl_vertex` displacement body.
//!
//! Mirrors [`crate::render_passes::shadow_custom_vertex`]: held on
//! [`RenderPasses`](crate::render_passes::RenderPasses) so its augmented group-0
//! bind group can fold into the unified bind-group recreate dispatch. The shadow
//! render pass reaches it through `ctx.render_passes.shadow_masked_custom_vertex`.
//!
//! The augmented group 0 is the **masked-shadow** group 0 (shadow_view +
//! materials + frame_globals + texture pool, with VERTEX visibility added),
//! reused verbatim — the same group the masked-shadow and custom-vertex-shadow
//! passes bind. So there's no second bind group: the render pass binds
//! `shadow_masked.bind_group` for this pass too.
//!
//! The shader + cache key + template live under
//! `crate::shadows::shader::masked_custom_vertex_*`; only the lazy pipeline pool +
//! the shared zero uv0 buffer live here.

pub mod pipeline;

use pipeline::ShadowMaskedCustomVertexPipelines;

/// Combined masked + custom-vertex shadow caster lazy pipeline pool + shared zero
/// uv0 buffer.
pub struct ShadowMaskedCustomVertexRenderPass {
    /// Lazy per-`shader_id` pool of combined masked + custom-vertex shadow
    /// pipelines. Empty until a material that is BOTH Mask AND custom-vertex has
    /// its variant compiled by the texture-finalize flow.
    pub pipelines: ShadowMaskedCustomVertexPipelines,
}
