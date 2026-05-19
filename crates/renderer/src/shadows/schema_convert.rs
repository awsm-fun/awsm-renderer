//! `From` impls bridging `awsm_scene_schema::*` shadow types to the
//! renderer's runtime equivalents. Only compiled when the
//! `scene-schema` feature is enabled — non-editor consumers that
//! don't need the conversion (or that already have their own bridge)
//! keep the renderer schema-free.
//!
//! Players typically use these in a one-liner after loading a project:
//!
//! ```ignore
//! let project: awsm_scene_schema::EditorProject = serde_json::from_str(&text)?;
//! renderer.set_shadows_config(project.shadows.into());
//! for (light_key, light_cfg) in light_pairs {
//!     renderer.set_light_shadow_params(
//!         light_key,
//!         light_cfg.shadow().clone().into(),
//!     )?;
//! }
//! for (mesh_key, mesh_shadow) in mesh_pairs {
//!     renderer.set_mesh_shadow_flags(mesh_key, mesh_shadow.into())?;
//! }
//! ```
//!
//! The editor frontend keeps a hand-rolled bridge for legacy reasons
//! (`scene-editor/src/renderer_bridge/node_sync.rs`); new code should
//! prefer these `From` impls because they live next to the runtime
//! struct definitions and can never drift.

use awsm_scene_schema as schema;

use crate::shadows::config::ShadowsConfig;
use crate::shadows::light_shadow::{
    CubeFaceUpdateRate, EvsmCutoff, FarCascadeUpdateRate, LightShadowHardness, LightShadowParams,
    MeshShadowFlags,
};

impl From<schema::ShadowsConfig> for ShadowsConfig {
    fn from(s: schema::ShadowsConfig) -> Self {
        Self {
            sscs_enabled: s.sscs_enabled,
            sscs_step_count: s.sscs_step_count,
            atlas_size: s.atlas_size,
            evsm_atlas_size: s.evsm_atlas_size,
            evsm_exponent: s.evsm_exponent,
            evsm_blur_radius: s.evsm_blur_radius,
            max_point_shadows: s.max_point_shadows,
            point_shadow_resolution: s.point_shadow_resolution,
            debug_cascade_colors: s.debug_cascade_colors,
        }
    }
}

impl From<schema::LightShadowConfig> for LightShadowParams {
    fn from(s: schema::LightShadowConfig) -> Self {
        Self {
            cast: s.cast,
            depth_bias: s.depth_bias,
            normal_bias: s.normal_bias,
            resolution: s.resolution,
            hardness: s.hardness.into(),
            pcss_penumbra_scale: s.pcss_penumbra_scale,
            max_distance: s.max_distance,
            cascade_count: s.cascade_count,
            cascade_split_lambda: s.cascade_split_lambda,
            evsm_cutoff: s.evsm_cutoff.into(),
            far_cascade_update_rate: s.far_cascade_update_rate.into(),
            cube_face_update_rate: s.cube_face_update_rate.into(),
        }
    }
}

impl From<schema::LightShadowHardness> for LightShadowHardness {
    fn from(s: schema::LightShadowHardness) -> Self {
        match s {
            schema::LightShadowHardness::Hard => Self::Hard,
            schema::LightShadowHardness::Soft => Self::Soft,
            schema::LightShadowHardness::Pcss => Self::Pcss,
        }
    }
}

impl From<schema::EvsmCutoff> for EvsmCutoff {
    fn from(s: schema::EvsmCutoff) -> Self {
        match s {
            schema::EvsmCutoff::Off => Self::Off,
            schema::EvsmCutoff::LastCascade => Self::LastCascade,
            schema::EvsmCutoff::LastTwoCascades => Self::LastTwoCascades,
        }
    }
}

impl From<schema::FarCascadeUpdateRate> for FarCascadeUpdateRate {
    fn from(s: schema::FarCascadeUpdateRate) -> Self {
        match s {
            schema::FarCascadeUpdateRate::EveryFrame => Self::EveryFrame,
            schema::FarCascadeUpdateRate::Every2Frames => Self::Every2Frames,
            schema::FarCascadeUpdateRate::Every4Frames => Self::Every4Frames,
            schema::FarCascadeUpdateRate::Every8Frames => Self::Every8Frames,
        }
    }
}

impl From<schema::CubeFaceUpdateRate> for CubeFaceUpdateRate {
    fn from(s: schema::CubeFaceUpdateRate) -> Self {
        match s {
            schema::CubeFaceUpdateRate::EveryFrame => Self::EveryFrame,
            schema::CubeFaceUpdateRate::Every2Frames => Self::Every2Frames,
            schema::CubeFaceUpdateRate::Every4Frames => Self::Every4Frames,
            schema::CubeFaceUpdateRate::Every8Frames => Self::Every8Frames,
        }
    }
}

impl From<schema::MeshShadowConfig> for MeshShadowFlags {
    fn from(s: schema::MeshShadowConfig) -> Self {
        Self {
            cast: s.cast,
            receive: s.receive,
        }
    }
}
