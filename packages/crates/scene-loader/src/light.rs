//! Pure `LightConfig`/`LightShadowConfig` → renderer light + shadow-params
//! conversion — shared by the editor bridge (live render) and
//! `populate_awsm_scene` (player / round-trip), so a light lowers identically on
//! both sides (the round-trip compares their renders). Both maps are mechanical
//! 1:1 field copies; sharing keeps the many shadow-param fields from drifting.

use awsm_renderer::lights::Light;
use awsm_renderer_scene::{LightConfig, LightShadowConfig};
use glam::Vec3;

/// Build a renderer [`Light`] from a [`LightConfig`] plus the node's world
/// `position` + forward `direction` (the node transform supplies both; a
/// directional light uses only direction, a point light only position).
pub fn light_from_config(cfg: &LightConfig, position: Vec3, direction: Vec3) -> Light {
    match cfg {
        LightConfig::Directional {
            color, intensity, ..
        } => Light::Directional {
            color: *color,
            intensity: *intensity,
            direction: direction.to_array(),
        },
        LightConfig::Point {
            color,
            intensity,
            range,
            ..
        } => Light::Point {
            color: *color,
            intensity: *intensity,
            position: position.to_array(),
            range: *range,
        },
        LightConfig::Spot {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            ..
        } => Light::Spot {
            color: *color,
            intensity: *intensity,
            position: position.to_array(),
            direction: direction.to_array(),
            range: *range,
            inner_angle: *inner_angle,
            outer_angle: *outer_angle,
        },
    }
}

/// Authored [`LightShadowConfig`] → the renderer's `LightShadowParams` (1:1).
pub fn light_shadow_params_from_config(
    cfg: &LightShadowConfig,
) -> awsm_renderer::shadows::LightShadowParams {
    use awsm_renderer::shadows as r;
    use awsm_renderer_scene as s;
    r::LightShadowParams {
        cast: cfg.cast,
        depth_bias: cfg.depth_bias,
        normal_bias: cfg.normal_bias,
        resolution: cfg.resolution,
        hardness: match cfg.hardness {
            s::LightShadowHardness::Hard => r::LightShadowHardness::Hard,
            s::LightShadowHardness::Soft => r::LightShadowHardness::Soft,
            s::LightShadowHardness::Pcss => r::LightShadowHardness::Pcss,
        },
        pcss_penumbra_scale: cfg.pcss_penumbra_scale,
        max_distance: cfg.max_distance,
        cascade_count: cfg.cascade_count,
        cascade_split_lambda: cfg.cascade_split_lambda,
        evsm_cutoff: match cfg.evsm_cutoff {
            s::EvsmCutoff::Off => r::EvsmCutoff::Off,
            s::EvsmCutoff::LastCascade => r::EvsmCutoff::LastCascade,
            s::EvsmCutoff::LastTwoCascades => r::EvsmCutoff::LastTwoCascades,
        },
        far_cascade_update_rate: match cfg.far_cascade_update_rate {
            s::FarCascadeUpdateRate::EveryFrame => r::FarCascadeUpdateRate::EveryFrame,
            s::FarCascadeUpdateRate::Every2Frames => r::FarCascadeUpdateRate::Every2Frames,
            s::FarCascadeUpdateRate::Every4Frames => r::FarCascadeUpdateRate::Every4Frames,
            s::FarCascadeUpdateRate::Every8Frames => r::FarCascadeUpdateRate::Every8Frames,
        },
        cube_face_update_rate: match cfg.cube_face_update_rate {
            s::CubeFaceUpdateRate::EveryFrame => r::CubeFaceUpdateRate::EveryFrame,
            s::CubeFaceUpdateRate::Every2Frames => r::CubeFaceUpdateRate::Every2Frames,
            s::CubeFaceUpdateRate::Every4Frames => r::CubeFaceUpdateRate::Every4Frames,
            s::CubeFaceUpdateRate::Every8Frames => r::CubeFaceUpdateRate::Every8Frames,
        },
    }
}
