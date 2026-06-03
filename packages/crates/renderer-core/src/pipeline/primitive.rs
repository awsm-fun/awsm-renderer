//! Primitive state descriptors for render pipelines.

/// Primitive state for a render pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrimitiveState {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#primitive
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuPrimitiveState.html
    pub cull_mode: Option<CullMode>,
    pub front_face: Option<FrontFace>,
    pub strip_index_format: Option<IndexFormat>,
    pub topology: Option<PrimitiveTopology>,
    pub unclipped_depth: Option<bool>,
}

impl std::hash::Hash for PrimitiveState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.cull_mode.map(|x| x as u32).hash(state);
        self.front_face.map(|x| x as u32).hash(state);
        self.strip_index_format.map(|x| x as u32).hash(state);
        self.topology.map(|x| x as u32).hash(state);
        self.unclipped_depth.hash(state);
    }
}

/// WebGPU primitive topology.
pub type PrimitiveTopology = web_sys::GpuPrimitiveTopology;
// https://docs.rs/web-sys/latest/web_sys/enum.GpuIndexFormat.html
/// WebGPU index format.
pub type IndexFormat = web_sys::GpuIndexFormat;
/// WebGPU front face winding.
pub type FrontFace = web_sys::GpuFrontFace;
// https://docs.rs/web-sys/latest/web_sys/enum.GpuCullMode.html
/// WebGPU cull mode.
pub type CullMode = web_sys::GpuCullMode;

impl PrimitiveState {
    /// Creates an empty primitive state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the cull mode.
    pub fn with_cull_mode(mut self, cull_mode: CullMode) -> Self {
        self.cull_mode = Some(cull_mode);
        self
    }

    /// Sets the front face winding.
    pub fn with_front_face(mut self, front_face: FrontFace) -> Self {
        self.front_face = Some(front_face);
        self
    }

    /// Sets the strip index format.
    pub fn with_strip_index_format(mut self, strip_index_format: IndexFormat) -> Self {
        self.strip_index_format = Some(strip_index_format);
        self
    }

    /// Sets the primitive topology.
    pub fn with_topology(mut self, topology: PrimitiveTopology) -> Self {
        self.topology = Some(topology);
        self
    }

    /// Enables or disables unclipped depth.
    pub fn with_unclipped_depth(mut self, unclipped_depth: bool) -> Self {
        self.unclipped_depth = Some(unclipped_depth);
        self
    }
}

impl From<PrimitiveState> for web_sys::GpuPrimitiveState {
    fn from(state: PrimitiveState) -> web_sys::GpuPrimitiveState {
        let state_js = web_sys::GpuPrimitiveState::new();
        if let Some(cull_mode) = state.cull_mode {
            state_js.set_cull_mode(cull_mode);
        }
        if let Some(front_face) = state.front_face {
            state_js.set_front_face(front_face);
        }
        if let Some(strip_index_format) = state.strip_index_format {
            state_js.set_strip_index_format(strip_index_format);
        }
        if let Some(topology) = state.topology {
            state_js.set_topology(topology);
        }

        if let Some(unclipped_depth) = state.unclipped_depth {
            state_js.set_unclipped_depth(unclipped_depth);
        }

        state_js
    }
}
