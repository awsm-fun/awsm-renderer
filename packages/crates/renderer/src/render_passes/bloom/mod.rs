//! COD/Jimenez mip-pyramid bloom pass.
//!
//! Builds a bloom pyramid from the HDR `composite` texture via one compute
//! pass with four step kinds — prefilter (composite → mip 0 with a soft-knee
//! threshold), a 13-tap downsample chain, a progressive 9-tap tent-filter
//! upsample chain (coarsest → finest, accumulated into a ping-pong up
//! pyramid), and a combine that tent-taps the accumulated mip 0 into the
//! full-res `bloom` render texture the effects pass samples.
//!
//! Mirrors the HZB build (`render_passes/hzb/`): `rgba16float` mip pyramids
//! ([`texture::BloomTexture`]) with per-mip storage views + `view_all`
//! sample-side views, shared bind-group layouts, and dispatches coalesced
//! into a single `begin_compute_pass`.

pub mod bind_group;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
pub mod texture;
