//! High-level renderer API and shared modules.
//!
//! # The `scene-schema` feature (optional schema → runtime bridge)
//!
//! [`awsm-renderer-scene`](https://docs.rs/awsm-renderer-scene) is the pure-data,
//! on-disk authoring format (`EditorProject`, saved as `project.json`). The
//! renderer holds the *runtime* equivalents of those types. Enabling the
//! `scene-schema` feature pulls in `awsm-renderer-scene` (an optional dep) and
//! compiles a set of `From<awsm_renderer_scene::*>` impls so a consumer can convert
//! authored data into renderer config with a single `.into()`:
//!
//! ```ignore
//! let project: awsm_renderer_scene::EditorProject = serde_json::from_str(&text)?;
//! renderer.set_shadows_config(project.shadows.into());
//! ```
//!
//! It is **off by default**: a bare runtime consumer never pays for the schema
//! crate, and the editor frontends keep their own bridges. It exists mainly for
//! standalone *players* that load `EditorProject` bundles and want first-party
//! conversion that lives next to the runtime structs (so it can never drift).
//!
//! ## Adding more conversions
//!
//! This is an **extension point**, not a one-off. Today the only bridge is
//! shadows (`shadows::schema_convert`, gated by `#[cfg(feature =
//! "scene-schema")]`). To bridge another subsystem (environment, lights,
//! materials, …), add a feature-gated `schema_convert` module beside that
//! subsystem's runtime types and write the `From` impls there, following
//! `shadows::schema_convert` as the template. Keeping each bridge next to the
//! structs it maps is what stops the conversions from rotting.

#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::match_like_matches_macro)]
#![allow(clippy::vec_init_then_push)]
pub mod anti_alias;
pub mod bind_group_layout;
pub mod bind_groups;
pub mod bounds;
pub mod buffer;
pub mod bvh;
pub mod camera;
pub mod cameras;
#[cfg(feature = "lod")]
pub mod cluster_lod;
pub mod coverage;
pub mod debug;
pub mod decals;
pub mod depth_convention;
pub mod dynamic_materials;
pub mod environment;
pub mod error;
pub mod features;
pub mod frame_globals;
pub mod frustum;
pub mod instances;
pub mod light_buckets;
pub mod lights;
pub mod load_phase;
pub mod loading;
#[cfg(feature = "lod")]
pub mod lod;
pub mod materials;
pub mod mesh_pack;
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
pub mod size;
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

#[cfg(test)]
mod shader_completeness;
#[cfg(test)]
mod wgsl_validation;
pub use renderer::*;

/// Re-export the bucket registration-cap config (§2) at the crate root for
/// discoverability alongside `AwsmRendererBuilder::with_bucket_config`.
pub use dynamic_materials::BucketConfig;

pub use load_phase::LoadPhase;

/// The load-transaction progress surface — reported by
/// [`AwsmRenderer::commit_load`] and [`AwsmRenderer::loading_stats`].
pub use loading::{LoadPhase as CommitLoadPhase, LoadingStats};

// `AwsmRendererLogging` lives in `crate::debug`; the crate root re-exports
// it crate-internally so modules can keep referencing `crate::AwsmRendererLogging`.
// (Previously the now-relocated `use crate::{… debug::AwsmRendererLogging …}`
// at the root provided this alias by virtue of private-at-root visibility.)
pub(crate) use debug::AwsmRendererLogging;
