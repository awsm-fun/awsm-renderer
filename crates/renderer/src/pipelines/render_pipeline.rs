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
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

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
    ///
    /// Thin wrapper over [`Self::ensure_keys`] — funnelling the
    /// single-key path through the same code keeps the cache-hit
    /// fast path and the cache-miss creation path in one place.
    pub async fn get_key(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        shaders: &Shaders,
        pipeline_layouts: &PipelineLayouts,
        cache_key: RenderPipelineCacheKey,
    ) -> Result<RenderPipelineKey> {
        // Fast path: cache hit, no allocation, no async.
        if let Some(key) = self.cache.get(&cache_key) {
            return Ok(*key);
        }
        let keys = self
            .ensure_keys(gpu, shaders, pipeline_layouts, std::iter::once(cache_key))
            .await?;
        Ok(keys[0])
    }

    /// Pre-warms the cache for a batch of render pipeline keys,
    /// issuing every `createRenderPipelineAsync` call back-to-back
    /// before awaiting any of them.
    ///
    /// Mirrors [`crate::shaders::Shaders::ensure_keys`]: the WebGPU
    /// driver's async-pipeline creation kicks off compilation the
    /// moment the JS Promise is constructed (synchronously, inside
    /// `createRenderPipelineAsync`). By firing all N Promises before
    /// `await`ing any, Dawn's compile pool parallelises the work —
    /// total wall-clock drops from `sum(t_i)` to roughly `max(t_i)`
    /// (bounded by the pool size, typically `num_cpus`).
    ///
    /// The returned `Vec<RenderPipelineKey>` is in input order,
    /// with duplicate cache keys resolving to the same key — so a
    /// caller can `.zip` it back against its descriptor list to
    /// fold the results into per-mesh / per-pass maps.
    pub async fn ensure_keys<I>(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        shaders: &Shaders,
        pipeline_layouts: &PipelineLayouts,
        cache_keys: I,
    ) -> Result<Vec<RenderPipelineKey>>
    where
        I: IntoIterator<Item = RenderPipelineCacheKey>,
    {
        // Collect input order; resolve cache hits and dedup the misses
        // in a single pass. `slot[i]` is `Some(key)` once we know which
        // RenderPipelineKey to return for input index `i`.
        let inputs: Vec<RenderPipelineCacheKey> = cache_keys.into_iter().collect();
        let mut slot: Vec<Option<RenderPipelineKey>> = vec![None; inputs.len()];

        // Misses (dedup'd) and the input indices that want each one.
        let mut pending_keys: Vec<RenderPipelineCacheKey> = Vec::new();
        let mut pending_indices: Vec<Vec<usize>> = Vec::new();
        let mut seen: HashMap<RenderPipelineCacheKey, usize> = HashMap::new();

        for (i, cache_key) in inputs.iter().enumerate() {
            if let Some(key) = self.cache.get(cache_key) {
                slot[i] = Some(*key);
                continue;
            }
            if let Some(&pending_idx) = seen.get(cache_key) {
                pending_indices[pending_idx].push(i);
                continue;
            }
            seen.insert(cache_key.clone(), pending_keys.len());
            pending_indices.push(vec![i]);
            pending_keys.push(cache_key.clone());
        }

        if pending_keys.is_empty() {
            return Ok(slot.into_iter().map(Option::unwrap).collect());
        }

        // Build all descriptors synchronously. Holding the descriptors
        // by value (`Vec<web_sys::GpuRenderPipelineDescriptor>`) keeps
        // them alive for the duration of the parallel await — Dawn
        // reads from the JS object the descriptor wraps.
        let mut descriptors: Vec<web_sys::GpuRenderPipelineDescriptor> =
            Vec::with_capacity(pending_keys.len());
        for cache_key in &pending_keys {
            descriptors.push(build_descriptor(cache_key, shaders, pipeline_layouts)?);
        }

        // Sync-issue every Promise. Dawn has started compiling all N
        // by the time this loop returns.
        let promises: Vec<JsFuture<web_sys::GpuRenderPipeline>> = descriptors
            .iter()
            .map(|d| JsFuture::from(gpu.create_render_pipeline_promise(d)))
            .collect();

        // Await all in parallel.
        let results = futures::future::join_all(promises).await;

        // Install in the order we issued; copy keys back into every
        // input slot that requested each cache key.
        for ((cache_key, result), input_indices) in
            pending_keys.into_iter().zip(results).zip(pending_indices)
        {
            let pipeline: web_sys::GpuRenderPipeline = result
                .map_err(|e| AwsmRenderPipelineError::Core(AwsmCoreError::pipeline_creation(e)))?
                .unchecked_into();
            let key = self.lookup.insert(pipeline);
            self.cache.insert(cache_key, key);
            for i in input_indices {
                slot[i] = Some(key);
            }
        }

        Ok(slot.into_iter().map(Option::unwrap).collect())
    }

    /// Returns a render pipeline for a key.
    pub fn get(&self, key: RenderPipelineKey) -> Result<&web_sys::GpuRenderPipeline> {
        self.lookup
            .get(key)
            .ok_or(AwsmRenderPipelineError::NotFound(key))
    }
}

/// Builds a `GpuRenderPipelineDescriptor` from a cache key. Lives
/// outside the `impl` so both [`RenderPipelines::get_key`] and
/// [`RenderPipelines::ensure_keys`] can call it without borrowing
/// `&mut self` (descriptor construction only needs `&` access to
/// the shader + pipeline-layout caches).
fn build_descriptor(
    cache_key: &RenderPipelineCacheKey,
    shaders: &Shaders,
    pipeline_layouts: &PipelineLayouts,
) -> Result<web_sys::GpuRenderPipelineDescriptor> {
    let shader_module = shaders
        .get(cache_key.shader_key)
        .ok_or(AwsmRenderPipelineError::MissingShader(cache_key.shader_key))?;

    let layout = pipeline_layouts.get(cache_key.layout_key)?;

    let mut vertex = VertexState::new(shader_module, None);
    vertex.buffer_layouts = cache_key.vertex_buffer_layouts.clone();
    vertex.constants = cache_key.vertex_constants.clone();

    let mut descriptor = RenderPipelineDescriptor::new(vertex, None)
        .with_primitive(cache_key.primitive.clone())
        .with_layout(PipelineLayoutKind::Custom(layout));

    // Pipelines that want a fragment stage either have one or more
    // colour targets (regular shading) or explicitly opt in via
    // `force_fragment_stage` (depth-only fragment that writes
    // `@builtin(frag_depth)` — e.g. cube shadow generation).
    if !cache_key.fragment_targets.is_empty() || cache_key.force_fragment_stage {
        let fragment = FragmentState::new(shader_module, None, cache_key.fragment_targets.clone());
        descriptor = descriptor.with_fragment(fragment);
    }

    if let Some(depth_stencil) = cache_key.depth_stencil.clone() {
        descriptor = descriptor.with_depth_stencil(depth_stencil);
    }

    if let Some(multisample) = cache_key.multisample.clone() {
        descriptor = descriptor.with_multisample(multisample);
    }

    Ok(descriptor.into())
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
