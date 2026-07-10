//! COD/Jimenez mip-pyramid bloom pass.
//!
//! Builds a bloom pyramid from the HDR `composite` texture via one compute
//! pass with three step kinds — prefilter (composite → mip 0 with a soft-knee
//! threshold), a 13-tap downsample chain, and a mip-sum combine that writes the
//! wide glow into the full-res `bloom` render texture the effects pass samples.
//!
//! Mirrors the HZB build (`render_passes/hzb/`): an `rgba16float` mip pyramid
//! ([`texture::BloomTexture`]) with per-mip storage views + a `view_all`
//! sample-side view, one shared bind-group layout, and dispatches coalesced
//! into a single `begin_compute_pass`.

pub mod bind_group;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
pub mod texture;
