//! Coarse-grained renderer profile — one knob that flips a coordinated
//! set of defaults across anti-aliasing, shadows, post-processing,
//! features, optimization policy, edge-budget, scene-spatial cadence,
//! and depth format.
//!
//! Selected via [`crate::AwsmRendererBuilder::with_profile`] at build
//! time. After the profile applies, individual `with_*` setters can
//! still override on top — call `with_profile` first.
//!
//! ## Why a profile (not just per-knob setters)
//!
//! The renderer ships ~20 independent defaults that meaningfully
//! interact with mobile-vs-desktop fitness: MSAA, shadow atlas sizes,
//! cube-map resolutions, EVSM atlas size, bloom on/off, gpu-culling
//! engagement threshold, edge budget, BVH rebuild cadence, depth
//! format, … Picking each one individually is what frontends *can*
//! still do, but the right starting point for a mobile-class device
//! isn't "the desktop defaults with one knob tweaked" — it's a
//! self-consistent bundle. `RendererProfile` is that bundle.
//!
//! Resolved through `?mobile=…` URL params via
//! [`awsm_renderer_web_shared::perf::resolve_renderer_profile`](https://github.com/dakom/awsm-renderer/blob/main/crates/web-shared/src/perf.rs)
//! so the same code path serves the deployed-site override.

use crate::{
    anti_alias::AntiAliasing,
    features::RendererFeatures,
    optimization_policy::RendererOptimizationPolicy,
    post_process::{PostProcessing, ToneMapping},
    render_passes::material_opaque::edge_buffers::{
        DEFAULT_MAX_EDGE_BUDGET_DESKTOP, DEFAULT_MAX_EDGE_BUDGET_MOBILE,
    },
    scene_spatial::SceneSpatialConfig,
    shadows::{ShadowQualityTier, ShadowsConfig},
};

/// Coarse-grained quality / footprint preset. Picked at build time;
/// frontends can override individual knobs after.
///
/// **Order matters**: call [`crate::AwsmRendererBuilder::with_profile`]
/// **first**, then chain per-knob `with_*` setters to override.
///
/// - [`RendererProfile::Mobile`] — conservative defaults for
///   mobile-class GPUs. MSAA off, low shadow tier, no bloom, smaller
///   atlases, gpu_culling auto-engaged later, smaller edge budget.
///   Targets ~30 fps on a 2020-era Android phone in a tight indoor
///   scene.
/// - [`RendererProfile::Desktop`] — the current shipping defaults.
///   MSAA 4×, high shadow tier, full-size atlases. Default for
///   `#[derive(Default)]` paths.
/// - [`RendererProfile::Cinema`] — maximum quality. Ultra shadow tier,
///   bloom on, 8K shadow atlas. Targets discrete GPUs and
///   content-creation builds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RendererProfile {
    /// Mobile-class GPU defaults.
    Mobile,
    /// Current desktop-class defaults. Used when no profile is set.
    #[default]
    Desktop,
    /// Maximum-quality defaults — discrete GPUs / content creation.
    Cinema,
}

/// The full bundle of defaults the profile resolves to. The renderer
/// builder folds these into its own per-field state in `with_profile`;
/// callers don't usually inspect this directly, but it's `pub` so
/// downstream code (tests, schema-driven editor sliders) can peek at
/// "what would the Mobile profile do for X".
#[derive(Clone, Debug)]
pub struct RendererProfileDefaults {
    pub anti_aliasing: AntiAliasing,
    pub post_processing: PostProcessing,
    pub features: RendererFeatures,
    pub optimization_policy: RendererOptimizationPolicy,
    pub shadows_config: ShadowsConfig,
    /// Suggested shadow quality tier — frontends that read the
    /// per-light `LightShadowParams` should apply this to every
    /// shadow-casting light on registration. Not applied
    /// automatically because per-light params live on the scene,
    /// not on the renderer state.
    pub shadow_quality_tier: ShadowQualityTier,
    pub max_edge_budget: u32,
    pub scene_spatial: SceneSpatialConfig,
    /// Default render-texture formats. Frontends that hand a custom
    /// `RenderTextureFormats` via `with_render_texture_formats` win
    /// over this.
    pub render_texture_formats: RenderTextureFormatsOverride,
}

/// Subset of [`RenderTextureFormats`] the profile actually sets — just
/// `depth` today. The rest stay at their per-device defaults from
/// `RenderTextureFormats::new`. Mobile picks `Depth24Plus` for the
/// 33% per-pass depth-bandwidth saving on every render/compute pass
/// that touches the depth attachment.
#[derive(Clone, Copy, Debug)]
pub struct RenderTextureFormatsOverride {
    pub depth: awsm_renderer_core::texture::TextureFormat,
}

impl RendererProfile {
    /// Resolve the profile into its full defaults bundle.
    pub fn defaults(self) -> RendererProfileDefaults {
        use awsm_renderer_core::texture::TextureFormat;
        match self {
            RendererProfile::Mobile => RendererProfileDefaults {
                anti_aliasing: AntiAliasing {
                    msaa_sample_count: None,
                    smaa: false,
                    mipmap: true,
                },
                post_processing: PostProcessing {
                    tonemapping: ToneMapping::KhronosNeutralPbr,
                    bloom: false,
                    dof: false,
                    exposure: 0.0,
                },
                features: RendererFeatures::default(),
                optimization_policy: RendererOptimizationPolicy {
                    // Push the GPU-driven-cull engagement threshold up
                    // on mobile — the HZB+cull+compaction+drawIndirect
                    // fixed cost recovers more slowly on a TBR GPU, so
                    // wait for more meshes before paying it.
                    gpu_culling_enable_threshold: 2000,
                    gpu_culling_disable_threshold: 1200,
                    ..RendererOptimizationPolicy::default()
                },
                shadows_config: ShadowsConfig {
                    atlas_size: 1024,
                    evsm_atlas_size: 512,
                    cascade_resolution: 1024,
                    max_point_shadows: 2,
                    point_shadow_resolution: 256,
                    ..ShadowsConfig::default()
                },
                shadow_quality_tier: ShadowQualityTier::Low,
                max_edge_budget: DEFAULT_MAX_EDGE_BUDGET_MOBILE,
                scene_spatial: SceneSpatialConfig {
                    // Halve the BVH rebuild cadence — fewer per-frame
                    // CPU spikes on the smaller mobile budget.
                    rebuild_dirty_threshold: 400,
                    rebuild_period_frames: 1200,
                },
                render_texture_formats: RenderTextureFormatsOverride {
                    // 24-bit depth saves 33% of the depth attachment
                    // bandwidth per pass — material-relevant precision
                    // for the typical mobile scene size (< 100 m).
                    // Frontends that need depth32 precision (huge
                    // outdoor scenes) can still override.
                    depth: TextureFormat::Depth24plus,
                },
            },
            RendererProfile::Desktop => RendererProfileDefaults {
                anti_aliasing: AntiAliasing::default(),
                post_processing: PostProcessing::default(),
                features: RendererFeatures::default(),
                optimization_policy: RendererOptimizationPolicy::default(),
                shadows_config: ShadowsConfig::default(),
                shadow_quality_tier: ShadowQualityTier::High,
                max_edge_budget: DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
                scene_spatial: SceneSpatialConfig::default(),
                render_texture_formats: RenderTextureFormatsOverride {
                    depth: TextureFormat::Depth32float,
                },
            },
            RendererProfile::Cinema => RendererProfileDefaults {
                anti_aliasing: AntiAliasing {
                    msaa_sample_count: Some(4),
                    smaa: false,
                    mipmap: true,
                },
                post_processing: PostProcessing {
                    tonemapping: ToneMapping::KhronosNeutralPbr,
                    bloom: true,
                    dof: true,
                    exposure: 0.0,
                },
                features: RendererFeatures::default(),
                optimization_policy: RendererOptimizationPolicy::default(),
                shadows_config: ShadowsConfig {
                    atlas_size: 8192,
                    max_point_shadows: 16,
                    ..ShadowsConfig::default()
                },
                shadow_quality_tier: ShadowQualityTier::Ultra,
                max_edge_budget: DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
                scene_spatial: SceneSpatialConfig::default(),
                render_texture_formats: RenderTextureFormatsOverride {
                    depth: TextureFormat::Depth32float,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_disables_msaa() {
        assert!(RendererProfile::Mobile
            .defaults()
            .anti_aliasing
            .msaa_sample_count
            .is_none());
    }

    #[test]
    fn desktop_keeps_msaa_4x() {
        assert_eq!(
            RendererProfile::Desktop
                .defaults()
                .anti_aliasing
                .msaa_sample_count,
            Some(4)
        );
    }

    #[test]
    fn mobile_lowers_shadow_atlas_and_cube_resolution() {
        let d = RendererProfile::Mobile.defaults();
        assert_eq!(d.shadows_config.atlas_size, 1024);
        assert_eq!(d.shadows_config.point_shadow_resolution, 256);
        assert_eq!(d.shadows_config.max_point_shadows, 2);
    }

    #[test]
    fn mobile_picks_smaller_edge_budget() {
        assert_eq!(
            RendererProfile::Mobile.defaults().max_edge_budget,
            DEFAULT_MAX_EDGE_BUDGET_MOBILE
        );
        assert_eq!(
            RendererProfile::Desktop.defaults().max_edge_budget,
            DEFAULT_MAX_EDGE_BUDGET_DESKTOP
        );
    }

    #[test]
    fn mobile_halves_bvh_rebuild_cadence() {
        let m = RendererProfile::Mobile.defaults();
        let d = RendererProfile::Desktop.defaults();
        assert!(m.scene_spatial.rebuild_period_frames > d.scene_spatial.rebuild_period_frames);
        assert!(m.scene_spatial.rebuild_dirty_threshold > d.scene_spatial.rebuild_dirty_threshold);
    }

    #[test]
    fn cinema_enables_bloom_and_dof() {
        let d = RendererProfile::Cinema.defaults();
        assert!(d.post_processing.bloom);
        assert!(d.post_processing.dof);
    }

    #[test]
    fn mobile_picks_depth24() {
        use awsm_renderer_core::texture::TextureFormat;
        assert_eq!(
            RendererProfile::Mobile
                .defaults()
                .render_texture_formats
                .depth,
            TextureFormat::Depth24plus
        );
        assert_eq!(
            RendererProfile::Desktop
                .defaults()
                .render_texture_formats
                .depth,
            TextureFormat::Depth32float
        );
    }
}
