//! Depth/stencil state descriptors.

use ordered_float::OrderedFloat;

use crate::texture::TextureFormat;

use crate::compare::CompareFunction;

/// Depth and stencil state for a render pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthStencilState {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#depthstencil_object_structure
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuDepthStencilState.html
    pub depth_bias: Option<i32>,
    pub depth_bias_clamp: Option<OrderedFloat<f32>>,
    pub depth_bias_slope_scale: Option<OrderedFloat<f32>>,
    pub depth_compare: Option<CompareFunction>,
    pub depth_write_enabled: Option<bool>,
    pub format: TextureFormat,
    pub stencil_back: Option<StencilFaceState>,
    pub stencil_front: Option<StencilFaceState>,
    pub stencil_read_mask: Option<u32>,
    pub stencil_write_mask: Option<u32>,
}

impl DepthStencilState {
    /// Creates a depth/stencil state for the given format.
    pub fn new(format: TextureFormat) -> Self {
        Self {
            format,
            depth_bias: None,
            depth_bias_clamp: None,
            depth_bias_slope_scale: None,
            depth_compare: None,
            depth_write_enabled: None,
            stencil_back: None,
            stencil_front: None,
            stencil_read_mask: None,
            stencil_write_mask: None,
        }
    }

    /// Sets depth bias.
    pub fn with_depth_bias(mut self, depth_bias: i32) -> Self {
        self.depth_bias = Some(depth_bias);
        self
    }
    /// Sets depth bias clamp.
    pub fn with_depth_bias_clamp(mut self, depth_bias_clamp: impl Into<OrderedFloat<f32>>) -> Self {
        self.depth_bias_clamp = Some(depth_bias_clamp.into());
        self
    }
    /// Sets depth bias slope scale.
    pub fn with_depth_bias_slope_scale(
        mut self,
        depth_bias_slope_scale: impl Into<OrderedFloat<f32>>,
    ) -> Self {
        self.depth_bias_slope_scale = Some(depth_bias_slope_scale.into());
        self
    }
    /// Sets the depth compare function.
    pub fn with_depth_compare(mut self, depth_compare: CompareFunction) -> Self {
        self.depth_compare = Some(depth_compare);
        self
    }
    /// Enables or disables depth writes.
    pub fn with_depth_write_enabled(mut self, depth_write_enabled: bool) -> Self {
        self.depth_write_enabled = Some(depth_write_enabled);
        self
    }
    /// Sets the stencil back face state.
    pub fn with_stencil_back(mut self, stencil_back: StencilFaceState) -> Self {
        self.stencil_back = Some(stencil_back);
        self
    }
    /// Sets the stencil front face state.
    pub fn with_stencil_front(mut self, stencil_front: StencilFaceState) -> Self {
        self.stencil_front = Some(stencil_front);
        self
    }
    /// Sets the stencil read mask.
    pub fn with_stencil_read_mask(mut self, stencil_read_mask: u32) -> Self {
        self.stencil_read_mask = Some(stencil_read_mask);
        self
    }
    /// Sets the stencil write mask.
    pub fn with_stencil_write_mask(mut self, stencil_write_mask: u32) -> Self {
        self.stencil_write_mask = Some(stencil_write_mask);
        self
    }
}

impl std::hash::Hash for DepthStencilState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.depth_bias.hash(state);
        self.depth_bias_clamp.hash(state);
        self.depth_bias_slope_scale.hash(state);
        self.depth_compare.map(|x| x as u32).hash(state);
        self.depth_write_enabled.hash(state);
        (self.format as u32).hash(state);
        self.stencil_back.hash(state);
        self.stencil_front.hash(state);
        self.stencil_read_mask.hash(state);
        self.stencil_write_mask.hash(state);
    }
}

/// Stencil face state for front or back faces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StencilFaceState {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#stencilback
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuStencilFaceState.html
    pub compare: Option<CompareFunction>,
    pub fail_op: Option<StencilOperation>,
    pub depth_fail_op: Option<StencilOperation>,
    pub pass_op: Option<StencilOperation>,
}

impl std::hash::Hash for StencilFaceState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.compare.map(|x| x as u32).hash(state);
        self.fail_op.map(|x| x as u32).hash(state);
        self.depth_fail_op.map(|x| x as u32).hash(state);
        self.pass_op.map(|x| x as u32).hash(state);
    }
}

/// WebGPU stencil operation.
pub type StencilOperation = web_sys::GpuStencilOperation;

impl From<DepthStencilState> for web_sys::GpuDepthStencilState {
    fn from(state: DepthStencilState) -> web_sys::GpuDepthStencilState {
        let state_js = web_sys::GpuDepthStencilState::new(state.format);
        if let Some(depth_bias) = state.depth_bias {
            state_js.set_depth_bias(depth_bias);
        }
        if let Some(depth_bias_clamp) = state.depth_bias_clamp {
            state_js.set_depth_bias_clamp(*depth_bias_clamp);
        }
        if let Some(depth_bias_slope_scale) = state.depth_bias_slope_scale {
            state_js.set_depth_bias_slope_scale(*depth_bias_slope_scale);
        }
        if let Some(depth_compare) = state.depth_compare {
            state_js.set_depth_compare(depth_compare);
        }
        if let Some(depth_write_enabled) = state.depth_write_enabled {
            state_js.set_depth_write_enabled(depth_write_enabled);
        }
        if let Some(stencil_back) = state.stencil_back {
            state_js.set_stencil_back(&web_sys::GpuStencilFaceState::from(stencil_back));
        }
        if let Some(stencil_front) = state.stencil_front {
            state_js.set_stencil_front(&web_sys::GpuStencilFaceState::from(stencil_front));
        }
        if let Some(stencil_read_mask) = state.stencil_read_mask {
            state_js.set_stencil_read_mask(stencil_read_mask);
        }
        if let Some(stencil_write_mask) = state.stencil_write_mask {
            state_js.set_stencil_write_mask(stencil_write_mask);
        }
        state_js
    }
}

impl From<StencilFaceState> for web_sys::GpuStencilFaceState {
    fn from(state: StencilFaceState) -> web_sys::GpuStencilFaceState {
        let state_js = web_sys::GpuStencilFaceState::new();

        if let Some(compare) = state.compare {
            state_js.set_compare(compare);
        }
        if let Some(fail_op) = state.fail_op {
            state_js.set_fail_op(fail_op);
        }
        if let Some(depth_fail_op) = state.depth_fail_op {
            state_js.set_depth_fail_op(depth_fail_op);
        }
        if let Some(pass_op) = state.pass_op {
            state_js.set_pass_op(pass_op);
        }

        state_js
    }
}
