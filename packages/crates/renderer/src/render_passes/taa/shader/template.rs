//! TAA resolve shader template. The structural axes are askama flags, so a
//! variant without the reactive mask contains no reactive binding or read.

use askama::Template;

use crate::{
    post_process::ToneMapping,
    render_passes::taa::shader::cache_key::{ShaderCacheKeyTaa, ShaderCacheKeyTaaReactiveSeed},
    shaders::{AwsmShaderError, Result},
};

/// The TAA resolve compute shader.
#[derive(Template, Debug)]
#[template(path = "taa_wgsl/resolve.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateTaa {
    /// Depth convention (003).
    pub reverse_z: bool,
    /// Reactive-mask binding (plan phase 3).
    pub reactive: bool,
    /// The display tonemapper (the working space).
    pub tonemapping: ToneMapping,
    /// See [`ShaderCacheKeyTaa::debug_views`].
    pub debug_views: bool,
    /// The motion test and the previous-velocity binding
    /// ([`ShaderCacheKeyTaa::disocclusion`]).
    pub motion_test: bool,
    /// The object-id test, its visibility bindings and same-surface
    /// reconstruction ([`ShaderCacheKeyTaa::disocclusion`]).
    pub id_test: bool,
    /// Binding numbers of the optional resources (`TaaBindings`; 0 when the
    /// resource is not bound).
    pub binding_prev_velocity: u32,
    pub binding_reactive: u32,
    pub binding_visibility: u32,
    pub binding_prev_visibility: u32,
}

impl TryFrom<&ShaderCacheKeyTaa> for ShaderTemplateTaa {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyTaa) -> Result<Self> {
        let b = crate::render_passes::taa::bind_group::TaaBindings::new(
            value.reactive,
            value.disocclusion,
        );
        Ok(Self {
            binding_prev_velocity: b.prev_velocity.unwrap_or(0),
            binding_reactive: b.reactive.unwrap_or(0),
            binding_visibility: b.visibility.unwrap_or(0),
            binding_prev_visibility: b.prev_visibility.unwrap_or(0),
            reverse_z: value.reverse_z,
            reactive: value.reactive,
            tonemapping: value.tonemapping,
            debug_views: value.debug_views,
            motion_test: value.disocclusion.motion(),
            id_test: value.disocclusion.id(),
        })
    }
}

impl ShaderTemplateTaa {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("TAA Resolve")
    }
}

/// The TAA reactive seed (fullscreen draw in the world transparent pass).
#[derive(Template, Debug, Default)]
#[template(path = "taa_wgsl/reactive_seed.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateTaaReactiveSeed {
    /// See [`ShaderCacheKeyTaaReactiveSeed::ssr`].
    pub ssr: bool,
}

impl TryFrom<&ShaderCacheKeyTaaReactiveSeed> for ShaderTemplateTaaReactiveSeed {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyTaaReactiveSeed) -> Result<Self> {
        Ok(Self { ssr: value.ssr })
    }
}

impl ShaderTemplateTaaReactiveSeed {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some(if self.ssr {
            "TAA Reactive Seed (SSR)"
        } else {
            "TAA Reactive Seed"
        })
    }
}
