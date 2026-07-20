//! Real SMAA 1x (Jimenez et al.) — spatial morphological anti-aliasing.
//!
//! Three logical passes; the first two live here as a lazy pre-pass (mirroring
//! the bloom pass's lifecycle), the third rides the EXISTING effects-pass
//! `smaa_anti_alias` hook:
//!
//! 1. **Edge detection** (`edges.wgsl`): luma-contrast edges of the HDR
//!    `composite`, detected in COMPRESSED space (`t = s/(1+s)`, the same
//!    Reinhard-style curve as the MSAA edge resolve) so hot emissive edges
//!    register perceptually. Local contrast adaptation suppresses weak edges
//!    next to much stronger ones. Output: `rgba8unorm` edges texture (RG).
//! 2. **Blend-weight calculation** (`weights.wgsl`): for each edge pixel,
//!    search along the edge line (up to [`shader::SMAA_MAX_SEARCH_STEPS`]) to
//!    find the segment ends, classify the crossing pattern at each end, and
//!    compute the coverage area ANALYTICALLY (the trapezoid rule that the
//!    reference implementation's precomputed AreaTex encodes for orthogonal
//!    patterns — diagonal patterns are handled by the orthogonal path, a
//!    known-modest quality tradeoff that avoids shipping AreaTex/SearchTex
//!    binary assets). Output: `rgba8unorm` weights texture
//!    `(up, down, left, right)`.
//! 3. **Neighborhood blending**: implemented in the effects shader behind its
//!    `smaa_anti_alias` template flag (see
//!    `effects_wgsl/helpers/smaa.wgsl`) — blends each pixel toward its
//!    neighbors by the computed weights, in compressed space, then expands
//!    back to HDR. Reusing the effects hook keeps the existing
//!    toggle/recompile machinery (`set_anti_aliasing` → effects pipeline
//!    re-key) untouched.
//!
//! SMAA 1x is purely SPATIAL: no history buffer, no reprojection, therefore
//! no ghosting by construction — and it does NOT replace MSAA; it cleans the
//! residual aliasing (texture/shader/HDR-edge crawl) that geometric MSAA
//! resolve can't reach.

pub mod bind_group;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
pub mod texture;
