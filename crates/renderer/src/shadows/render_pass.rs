//! Shadow generation render pass — depth-only rendering of every
//! shadow-casting renderable into the atlas / cube pool.
//!
//! Phase 2 status: scaffolded. The shader template, vertex layout, and
//! cascade math are wired; the per-frame dispatch (creating the
//! depth-only pipeline, recording a render pass per shadow view,
//! iterating renderables) is not yet implemented and lives behind the
//! `Shadows::any_active()` guard in `render.rs` — that returns `false`
//! in phase 2, so the shadow pass currently produces no output.

use crate::error::Result;
use crate::render::RenderContext;

/// Records the shadow-generation render passes for every active
/// shadow caster in the current frame.
///
/// Phase 2: no-op. Phase 4 lands the actual dispatch.
pub fn record(_ctx: &RenderContext) -> Result<()> {
    Ok(())
}
