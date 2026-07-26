//! Froxel volumetric scattering — light shafts, beams visible in the air.
//!
//! The analytic haze in the effects pass (`post_process::Atmosphere`) gives
//! aerial perspective: distance fades toward a colour. It cannot give beams,
//! because it never asks which lights reach a point in the medium. This pass
//! does, over a froxel volume registered to the light-culling grid so the
//! medium is lit by exactly the lights the surfaces in that column are.
//!
//! Design + landing sequence: `docs/plans/volumetrics.md`.

pub mod bind_group;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
pub mod texture;
