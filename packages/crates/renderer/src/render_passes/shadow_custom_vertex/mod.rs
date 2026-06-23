//! **Custom-vertex** shadow caster resources — the displaced-shadow path that
//! keeps a custom-vertex material's shadow glued to its lit geometry (no
//! detached / smooth silhouette).
//!
//! Mirrors [`crate::render_passes::shadow_masked`]: held on
//! [`RenderPasses`](crate::render_passes::RenderPasses) rather than on `Shadows`
//! so its augmented group-0 bind group can fold into the unified bind-group
//! recreate dispatch (the recreate context already holds `&Shadows`). The shadow
//! render pass reaches it through `ctx.render_passes.shadow_custom_vertex`.
//!
//! The augmented group 0 is the **masked-shadow** group 0 (shadow_view +
//! materials + frame_globals + texture pool), reused verbatim — the custom-vertex
//! shadow bind groups declare the identical bindings, with VERTEX visibility added
//! (the displacement hook runs in the vertex stage). So there's no second bind
//! group: the render pass binds `shadow_masked.bind_group` for this pass too.
//!
//! The shader + cache key + template live under
//! `crate::shadows::shader::custom_vertex_*` (the shadow subsystem's askama dir);
//! only the lazy pipeline pool + the shared zero uv0 buffer live here.

pub mod pipeline;

use pipeline::ShadowCustomVertexPipelines;

/// Custom-vertex shadow caster lazy pipeline pool + shared zero uv0 buffer.
pub struct ShadowCustomVertexRenderPass {
    /// Lazy per-`shader_id` pool of custom-vertex shadow pipelines. Empty until a
    /// custom-vertex material's variant is compiled by the texture-finalize flow.
    pub pipelines: ShadowCustomVertexPipelines,
}
