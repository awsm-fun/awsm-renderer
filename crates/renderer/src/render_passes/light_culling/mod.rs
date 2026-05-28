//! GPU light-culling compute pass.
//!
//! Builds a per-froxel light list (3D grid of `(tile_x, tile_y, z_slice)`
//! cells, exponential view-space depth slices) consumed by the
//! transparent shader and — when the per-mesh CPU bucket marks a mesh
//! as oversized — by the opaque shader's oversized-mesh override path.
//!
//! See `docs/plans/light-culling.md` for the full design.

pub mod bind_group;
pub mod buffers;
pub mod pipeline;
pub mod render_pass;
pub mod shader;

pub use buffers::{
    LightCullingBuffers, DEFAULT_MAX_PER_FROXEL_CAPACITY, DEFAULT_SLICE_COUNT, TILE_PIXEL_SIZE,
};
