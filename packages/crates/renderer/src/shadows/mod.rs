//! Shadow mapping subsystem.
//!
//! The `Shadows` struct sits on [`AwsmRenderer`](crate::AwsmRenderer)
//! and owns every GPU resource needed for shadow generation and
//! sampling: a 2D PCF/PCSS atlas, an RGBA16F EVSM atlas (allocated
//! lazily), a depth cubemap-array slot pool for point lights, the
//! descriptor uniform buffer that the material-opaque shading pass
//! reads at sample time, and the depth-only render pipeline used for
//! shadow generation.
//!
//! This file is module wiring only — the actual implementation lives
//! in named siblings (`state`, `helpers`, `consts`, `record`, `api`).

pub mod cascade;
pub mod config;
pub mod error;
pub mod evsm;
pub mod importance;
pub mod light_shadow;
pub mod quality_tier;
pub mod render_pass;
#[cfg(feature = "scene-schema")]
pub mod schema_convert;
pub mod shader;

mod api;
mod consts;
mod helpers;
mod record;
mod state;

pub use cascade::Cascade;
pub use config::ShadowsConfig;
pub use error::AwsmShadowError;
pub use evsm::{EvsmDescriptors, EvsmPass};
pub use light_shadow::{
    CubeFaceUpdateRate, EvsmCutoff, FarCascadeUpdateRate, LightShadowHardness, LightShadowParams,
    MeshShadowFlags,
};
pub use quality_tier::{ShadowQualityPreset, ShadowQualityTier};
pub use shader::{cache_key::ShaderCacheKeyShadow, template::ShaderTemplateShadow};

pub use consts::{
    clamp_point_shadow_resolution, MAX_SHADOW_DESCRIPTORS, MAX_SHADOW_VIEWS,
    MIN_POINT_SHADOW_RESOLUTION, POINT_SHADOW_NEAR, POINT_SHADOW_RESOLUTION, SHADOW_ATLAS_MAX_SIZE,
    SHADOW_DESCRIPTOR_BYTES, SHADOW_GLOBALS_BYTES, SHADOW_INDEX_NONE, SHADOW_VIEW_BYTES,
    SHADOW_VIEW_STRIDE,
};
pub use record::{EvsmDispatchEntry, LightShadowRecord, LightShadowView, ShadowViewThrottle};
pub use state::{Shadows, ShadowsDescriptors};
