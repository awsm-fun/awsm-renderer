//! High-level renderer API and shared modules.

#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::match_like_matches_macro)]
#![allow(clippy::vec_init_then_push)]
pub mod anti_alias;
pub mod bind_group_layout;
pub mod bind_groups;
pub mod bounds;
pub mod buffer;
pub mod camera;
pub mod coverage;
pub mod debug;
pub mod decals;
pub mod dynamic_materials;
pub mod environment;
pub mod error;
pub mod features;
pub mod frame_globals;
pub mod frustum;
pub mod instances;
pub mod light_buckets;
pub mod lights;
pub mod materials;
pub mod meshes;
pub mod opaque_mipgen;
pub mod optimization_policy;
pub mod picker;
pub mod pipeline_layouts;
pub mod pipeline_scheduler;
pub mod pipelines;
pub mod post_process;
pub mod profile;
pub mod raw_mesh;
pub mod render;
pub mod render_passes;
pub mod render_textures;
pub mod renderable;
pub mod scene_spatial;
pub mod shaders;
pub mod shadows;
pub mod textures;
pub mod transforms;
pub mod update;
pub mod web_global;
pub mod workers;
// re-export
pub mod core {
    pub use awsm_renderer_core::*;
}
#[cfg(feature = "animation")]
pub mod animation;

mod renderer;
pub use renderer::*;

// `AwsmRendererLogging` lives in `crate::debug`; the crate root re-exports
// it crate-internally so modules can keep referencing `crate::AwsmRendererLogging`.
// (Previously the now-relocated `use crate::{… debug::AwsmRendererLogging …}`
// at the root provided this alias by virtue of private-at-root visibility.)
pub(crate) use debug::AwsmRendererLogging;
