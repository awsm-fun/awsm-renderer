pub mod cache_key;
pub mod template;

/// Max steps of the edge-segment search in each direction (pixels). The
/// reference implementation uses SearchTex to walk 4px per fetch; this
/// implementation walks 1px per fetch, so keep the cap modest — 24px covers
/// the shallow staircases that read as "swimming" without SearchTex assets.
pub const SMAA_MAX_SEARCH_STEPS: u32 = 24;
