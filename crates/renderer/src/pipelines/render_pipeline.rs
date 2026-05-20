//! Render pipeline cache.

use std::collections::{BTreeMap, HashMap};

use awsm_renderer_core::{
    error::AwsmCoreError,
    pipeline::{
        constants::{ConstantOverrideKey, ConstantOverrideValue},
        depth_stencil::DepthStencilState,
        fragment::{ColorTargetState, FragmentState},
        layout::PipelineLayoutKind,
        multisample::MultisampleState,
        primitive::PrimitiveState,
        vertex::{VertexBufferLayout, VertexState},
        RenderPipelineDescriptor,
    },
    renderer::AwsmRendererWebGpu,
};
use slotmap::{new_key_type, SlotMap};
use thiserror::Error;

use crate::{
    bind_groups::AwsmBindGroupError,
    pipeline_layouts::{AwsmPipelineLayoutError, PipelineLayoutKey, PipelineLayouts},
    shaders::{ShaderKey, Shaders},
};

/// Cache of render pipelines by key.
pub struct RenderPipelines {
    lookup: SlotMap<RenderPipelineKey, web_sys::GpuRenderPipeline>,
    cache: HashMap<RenderPipelineCacheKey, RenderPipelineKey>,
}

impl RenderPipelines {
    /// Creates an empty render pipeline cache.
    pub fn new() -> Self {
        Self {
            lookup: SlotMap::with_key(),
            cache: HashMap::new(),
        }
    }

    /// Returns a pipeline key, creating the pipeline if needed.
    pub async fn get_key(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        shaders: &Shaders,
        pipeline_layouts: &PipelineLayouts,
        cache_key: RenderPipelineCacheKey,
    ) -> Result<RenderPipelineKey> {
        if let Some(key) = self.cache.get(&cache_key) {
            return Ok(*key);
        }

        let cache_key_clone = cache_key.clone();

        let shader_module = shaders
            .get(cache_key.shader_key)
            .ok_or(AwsmRenderPipelineError::MissingShader(cache_key.shader_key))?;

        let layout = pipeline_layouts.get(cache_key.layout_key)?;

        let mut vertex = VertexState::new(shader_module, None);
        vertex.buffer_layouts = cache_key.vertex_buffer_layouts;
        vertex.constants = cache_key.vertex_constants;

        let mut descriptor = RenderPipelineDescriptor::new(vertex, None)
            .with_primitive(cache_key.primitive)
            .with_layout(PipelineLayoutKind::Custom(layout));

        // Pipelines that want a fragment stage either have one or more
        // colour targets (regular shading) or explicitly opt in via
        // `force_fragment_stage` (depth-only fragment that writes
        // `@builtin(frag_depth)` — e.g. cube shadow generation).
        if !cache_key.fragment_targets.is_empty() || cache_key.force_fragment_stage {
            let fragment =
                FragmentState::new(shader_module, None, cache_key.fragment_targets.clone());
            descriptor = descriptor.with_fragment(fragment);
        }

        if let Some(depth_stencil) = cache_key.depth_stencil {
            descriptor = descriptor.with_depth_stencil(depth_stencil);
        }

        if let Some(multisample) = cache_key.multisample {
            descriptor = descriptor.with_multisample(multisample);
        }

        let pipeline = gpu.create_render_pipeline(&descriptor.into()).await?;

        let key = self.lookup.insert(pipeline);
        self.cache.insert(cache_key_clone, key);
        Ok(key)
    }

    /// Returns a render pipeline for a key.
    pub fn get(&self, key: RenderPipelineKey) -> Result<&web_sys::GpuRenderPipeline> {
        self.lookup
            .get(key)
            .ok_or(AwsmRenderPipelineError::NotFound(key))
    }
}

impl Default for RenderPipelines {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache key for render pipeline creation.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct RenderPipelineCacheKey {
    pub shader_key: ShaderKey,
    pub layout_key: PipelineLayoutKey,
    pub primitive: PrimitiveState,
    pub depth_stencil: Option<DepthStencilState>,
    pub fragment_targets: Vec<ColorTargetState>,
    pub vertex_buffer_layouts: Vec<VertexBufferLayout>,
    pub vertex_constants: BTreeMap<ConstantOverrideKey, ConstantOverrideValue>,
    pub multisample: Option<MultisampleState>,
    /// Force a fragment stage even with no colour targets. Used by
    /// shadow-generation pipelines whose fragment writes only
    /// `@builtin(frag_depth)` (cube shadows store linear radial depth
    /// computed in the fragment, not perspective NDC.z).
    pub force_fragment_stage: bool,
}

impl RenderPipelineCacheKey {
    /// Creates a cache key with shader and layout keys.
    pub fn new(shader_key: ShaderKey, layout_key: PipelineLayoutKey) -> Self {
        Self {
            shader_key,
            layout_key,
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            fragment_targets: Vec::new(),
            vertex_buffer_layouts: Vec::new(),
            vertex_constants: BTreeMap::new(),
            multisample: None,
            force_fragment_stage: false,
        }
    }

    /// Forces the pipeline to include a fragment stage even with no
    /// colour targets. Used for depth-only fragments that override
    /// `@builtin(frag_depth)`.
    pub fn with_force_fragment_stage(mut self) -> Self {
        self.force_fragment_stage = true;
        self
    }

    /// Sets the multisample state for the pipeline.
    pub fn with_multisample(mut self, multisample: MultisampleState) -> Self {
        self.multisample = Some(multisample);
        self
    }

    /// Appends a vertex buffer layout to the pipeline.
    pub fn with_push_vertex_buffer_layout(
        mut self,
        vertex_buffer_layout: VertexBufferLayout,
    ) -> Self {
        self.vertex_buffer_layouts.push(vertex_buffer_layout);
        self
    }

    /// Appends a single fragment target to the pipeline.
    pub fn with_push_fragment_target(mut self, target: ColorTargetState) -> Self {
        self.fragment_targets.push(target);
        self
    }

    /// Appends multiple fragment targets to the pipeline.
    pub fn with_push_fragment_targets(
        mut self,
        targets: impl IntoIterator<Item = ColorTargetState>,
    ) -> Self {
        for target in targets.into_iter() {
            self.fragment_targets.push(target);
        }
        self
    }

    /// Sets the primitive state for the pipeline.
    pub fn with_primitive(mut self, primitive: PrimitiveState) -> Self {
        self.primitive = primitive;
        self
    }

    /// Sets the depth-stencil state for the pipeline.
    pub fn with_depth_stencil(mut self, depth_stencil: DepthStencilState) -> Self {
        self.depth_stencil = Some(depth_stencil);
        self
    }

    #[allow(dead_code)]
    /// Sets a vertex constant override for the pipeline.
    pub fn with_vertex_constant(
        mut self,
        key: ConstantOverrideKey,
        value: ConstantOverrideValue,
    ) -> Self {
        self.vertex_constants.insert(key, value);
        self
    }
}

new_key_type! {
    /// Opaque key for render pipelines.
    pub struct RenderPipelineKey;
}

/// Result type for render pipeline operations.
type Result<T> = std::result::Result<T, AwsmRenderPipelineError>;

/// Render pipeline errors.
#[derive(Error, Debug)]
pub enum AwsmRenderPipelineError {
    #[error("[render pipeline] missing pipeline: {0:?}")]
    NotFound(RenderPipelineKey),

    #[error("[render pipeline] missing shader: {0:?}")]
    MissingShader(ShaderKey),

    #[error("[render pipeline] bind group: {0:?}")]
    BindGroup(#[from] AwsmBindGroupError),

    #[error("[render pipeline]: {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[render pipeline] {0:?}")]
    Layout(#[from] AwsmPipelineLayoutError),
}
