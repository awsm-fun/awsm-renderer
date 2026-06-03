//! Canvas configuration helpers for WebGPU.

use crate::texture::{TextureFormat, TextureUsage};

// device is _not_ included in the configuration, we get that at build time
// https://developer.mozilla.org/en-US/docs/Web/API/GPUCanvasContext/configure#configuration
// https://docs.rs/web-sys/latest/web_sys/struct.GpuCanvasConfiguration.html
/// Canvas configuration wrapper for WebGPU.
#[derive(Default)]
pub struct CanvasConfiguration {
    // if not set, will be derived at build time via get_preferred_canvas_format()
    /// Preferred texture format for the canvas.
    pub format: Option<TextureFormat>,
    /// Alpha compositing mode for the canvas.
    pub alpha_mode: Option<CanvasAlphaMode>,
    /// Tone mapping mode for the canvas.
    pub tone_mapping: Option<CanvasToneMappingMode>,
    /// Usage flags for the canvas texture.
    pub usage: Option<TextureUsage>,
}

impl CanvasConfiguration {
    /// Returns a default configuration with no overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the preferred canvas texture format.
    pub fn with_format(mut self, format: TextureFormat) -> Self {
        self.format = Some(format);
        self
    }

    /// Sets the alpha mode for the canvas.
    pub fn with_alpha_mode(mut self, alpha_mode: CanvasAlphaMode) -> Self {
        self.alpha_mode = Some(alpha_mode);
        self
    }

    /// Sets the tone mapping mode for the canvas.
    pub fn with_tone_mapping(mut self, tone_mapping: CanvasToneMappingMode) -> Self {
        self.tone_mapping = Some(tone_mapping);
        self
    }
    /// Sets the usage flags for the canvas texture.
    pub fn with_usage(mut self, usage: TextureUsage) -> Self {
        self.usage = Some(usage);
        self
    }
}

/// WebGPU canvas alpha mode.
// https://docs.rs/web-sys/latest/web_sys/enum.GpuCanvasAlphaMode.html
/// WebGPU canvas alpha mode.
pub type CanvasAlphaMode = web_sys::GpuCanvasAlphaMode;
/// WebGPU canvas tone mapping mode.
// https://docs.rs/web-sys/latest/web_sys/enum.GpuCanvasToneMappingMode.html
/// WebGPU canvas tone mapping mode.
pub type CanvasToneMappingMode = web_sys::GpuCanvasToneMappingMode;

impl CanvasConfiguration {
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuCanvasConfiguration.html
    /// Converts to a WebGPU canvas configuration.
    pub fn into_js(
        self,
        gpu: &web_sys::Gpu,
        device: &web_sys::GpuDevice,
    ) -> web_sys::GpuCanvasConfiguration {
        let format = self
            .format
            .unwrap_or_else(|| gpu.get_preferred_canvas_format());

        let configuration_js = web_sys::GpuCanvasConfiguration::new(device, format);

        if let Some(alpha_mode) = self.alpha_mode {
            configuration_js.set_alpha_mode(alpha_mode);
        }
        if let Some(usage) = self.usage {
            configuration_js.set_usage(usage.as_u32());
        }
        if let Some(mode) = self.tone_mapping {
            let tone_mapping_js = web_sys::GpuCanvasToneMapping::new();
            tone_mapping_js.set_mode(mode);
            configuration_js.set_tone_mapping(&tone_mapping_js);
        }

        configuration_js
    }
}
