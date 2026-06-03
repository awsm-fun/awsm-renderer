//! Multisample state descriptors.

/// Multisample state for a render pipeline.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct MultisampleState {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#multisample
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuMultisampleState.html
    pub count: Option<u32>,
    pub mask: Option<u32>,
    pub alpha_to_coverage_enabled: bool,
}

impl MultisampleState {
    /// Creates a default multisample state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the sample count.
    pub fn with_count(mut self, count: u32) -> Self {
        self.count = Some(count);
        self
    }

    /// Sets the sample mask.
    pub fn with_mask(mut self, mask: u32) -> Self {
        self.mask = Some(mask);
        self
    }

    /// Enables alpha-to-coverage.
    pub fn with_alpha_to_coverage_enabled(mut self, enabled: bool) -> Self {
        self.alpha_to_coverage_enabled = enabled;
        self
    }
}

impl From<MultisampleState> for web_sys::GpuMultisampleState {
    fn from(state: MultisampleState) -> web_sys::GpuMultisampleState {
        let state_js = web_sys::GpuMultisampleState::new();
        if let Some(count) = state.count {
            state_js.set_count(count);
        }
        if let Some(mask) = state.mask {
            state_js.set_mask(mask);
        }
        state_js.set_alpha_to_coverage_enabled(state.alpha_to_coverage_enabled);

        state_js
    }
}
