//! Volumetrics shader templates.

use askama::Template;

use crate::{
    render_passes::volumetrics::shader::cache_key::{ShaderCacheKeyVolumetrics, VolumetricsStage},
    shaders::{AwsmShaderError, Result},
};

/// Bind-group declarations + the shared lighting/shadow includes. Rendered
/// ahead of whichever stage body follows it.
#[derive(Template, Debug)]
#[template(path = "volumetrics_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateVolumetricsBindGroups {
    /// Compile the shadow-SAMPLING bodies. Always true — lighting the medium
    /// without shadowing it is what makes beams look like solid cones.
    pub needs_shadow_sampling: bool,
    /// The cascade-debug overlay is a shading-time colour aid; the volume has
    /// no use for it.
    pub needs_cascade_debug: bool,
    /// SSCS is a screen-space *surface* contact term and this pass binds no
    /// depth buffer, so the shared body compiles out to `return 1.0`.
    pub sscs_available: bool,
    pub sscs_step_count: u32,
    /// Shadows sit at group 2 (0 = volume, 1 = lights).
    pub shadow_group_index: u32,
    /// Collapse shadow filtering to the 1-tap hard path (see the shared
    /// `shadow/bind_groups.wgsl`). Only the volumetrics pass sets this.
    pub shadow_force_hard: bool,
    /// Z-slice count for `froxel_walk.wgsl`.
    pub froxel_slice_count: u32,
    /// Depth convention (003) — read by the shared shadow include.
    pub reverse_z: bool,
    /// The shared shadow include branches on this for its depth reads; the
    /// volume has no MSAA target of its own.
    pub multisampled_geometry: bool,
    /// Declare the history volume + its sampler. Structural: it changes the
    /// bind-group layout, so it must match the pass's own `temporal`.
    pub temporal: bool,
}

impl ShaderTemplateVolumetricsBindGroups {
    fn new(key: &ShaderCacheKeyVolumetrics) -> Self {
        Self {
            needs_shadow_sampling: true,
            needs_cascade_debug: false,
            sscs_available: false,
            sscs_step_count: 1,
            shadow_group_index: 2,
            shadow_force_hard: true,
            froxel_slice_count: crate::render_passes::light_culling::DEFAULT_SLICE_COUNT,
            reverse_z: key.reverse_z,
            multisampled_geometry: false,
            temporal: key.temporal,
        }
    }
}

/// Per-froxel medium + in-scattered light.
#[derive(Template, Debug, Default)]
#[template(path = "volumetrics_wgsl/inject.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateVolumetricsInject {
    /// Jitter the sample point and blend against reprojected history.
    pub temporal: bool,
}

/// Front-to-back accumulation down each froxel column.
#[derive(Template, Debug, Default)]
#[template(path = "volumetrics_wgsl/integrate.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateVolumetricsIntegrate;

/// A volumetrics stage shader: shared bind groups + one stage body.
#[derive(Debug)]
pub struct ShaderTemplateVolumetrics {
    bind_groups: ShaderTemplateVolumetricsBindGroups,
    stage: VolumetricsStage,
    temporal: bool,
}

impl TryFrom<&ShaderCacheKeyVolumetrics> for ShaderTemplateVolumetrics {
    type Error = AwsmShaderError;

    fn try_from(key: &ShaderCacheKeyVolumetrics) -> Result<Self> {
        Ok(Self {
            bind_groups: ShaderTemplateVolumetricsBindGroups::new(key),
            stage: key.stage,
            temporal: key.temporal,
        })
    }
}

impl ShaderTemplateVolumetrics {
    pub fn into_source(self) -> Result<String> {
        let bind_groups = self.bind_groups.render()?;
        let body = match self.stage {
            VolumetricsStage::Inject => ShaderTemplateVolumetricsInject {
                temporal: self.temporal,
            }
            .render()?,
            VolumetricsStage::Integrate => ShaderTemplateVolumetricsIntegrate.render()?,
        };
        Ok(format!("{bind_groups}\n{body}"))
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some(match self.stage {
            VolumetricsStage::Inject => "Volumetrics Inject",
            VolumetricsStage::Integrate => "Volumetrics Integrate",
        })
    }
}
