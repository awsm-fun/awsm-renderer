//! TAA resolve compute pipeline. Self-contained like SMAA / SSR: compiles its
//! own shader + pipeline when the pass is built (TAA turned on), nothing when
//! TAA is off.
//!
//! The debug views (`TaaDebugView`, the raw harness views) live in their own
//! shader variant: their branch, even never taken, costs the production
//! resolve about 0.12 ms at 1920x1435. That variant compiles lazily, the
//! first frame a view is selected; until it is ready the production variant
//! keeps running (it ignores the view).

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts};
use crate::pipelines::compute_pipeline::{
    ComputePipelineCacheKey, ComputePipelineKey, ComputePipelines,
};
use crate::render_passes::taa::{bind_group::TaaBindGroups, shader::cache_key::ShaderCacheKeyTaa};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::{ShaderCacheKey, Shaders};

/// A compile in flight: its result, written by the spawned future.
type PendingPipeline =
    Rc<RefCell<Option<std::result::Result<web_sys::GpuComputePipeline, String>>>>;

pub struct TaaPipelines {
    pub resolve: ComputePipelineKey,
    /// The resolve with the debug views compiled in: `None` until a view is
    /// first selected and its compile has finished.
    pub resolve_debug: Option<ComputePipelineKey>,
    debug_shader: ShaderCacheKeyTaa,
    layout: PipelineLayoutKey,
    debug_pending: Option<(ComputePipelineCacheKey, PendingPipeline)>,
    /// The debug variant failed to compile: never retried (the production
    /// variant keeps running).
    debug_failed: bool,
}

impl TaaPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &TaaBindGroups,
        reactive: bool,
    ) -> Result<Self> {
        let key = Self::shader_cache_key(
            ctx.features.reverse_z,
            reactive,
            ctx.post_processing.tonemapping,
            false,
        );
        let debug_shader = ShaderCacheKeyTaa {
            debug_views: true,
            ..key.clone()
        };
        ctx.shaders
            .ensure_keys(ctx.gpu, vec![ShaderCacheKey::from(key.clone())])
            .await?;
        let layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let shader = ctx.shaders.get_key(ctx.gpu, key).await?;
        let keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                vec![ComputePipelineCacheKey::new(shader, layout)],
            )
            .await?;
        Ok(Self {
            resolve: keys[0],
            resolve_debug: None,
            debug_shader,
            layout,
            debug_pending: None,
            debug_failed: false,
        })
    }

    /// Every pipeline the pass holds.
    pub(crate) fn keys(&self) -> Vec<ComputePipelineKey> {
        let mut keys = vec![self.resolve];
        keys.extend(self.resolve_debug);
        keys
    }

    /// The debug variant, compiled on first need: call every frame a debug
    /// view is selected. Starts the compile once (sync: shader module and
    /// pipeline promise; the promise resolves in a spawned future), installs
    /// it the first frame after it finished, and returns it from then on.
    /// `None` while it compiles (or if it failed): the caller dispatches the
    /// production variant.
    pub(crate) fn ensure_debug(
        &mut self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        shaders: &mut Shaders,
        pipeline_layouts: &PipelineLayouts,
        compute: &mut ComputePipelines,
    ) -> Option<ComputePipelineKey> {
        if self.resolve_debug.is_some() || self.debug_failed {
            return self.resolve_debug;
        }
        if let Some((cache_key, pending)) = self.debug_pending.as_ref() {
            let done = pending.borrow_mut().take();
            match done {
                Some(Ok(pipeline)) => {
                    let key = compute.install_resolved_pipeline(pipeline, cache_key.clone());
                    self.resolve_debug = Some(key);
                    self.debug_pending = None;
                }
                Some(Err(err)) => {
                    tracing::warn!("TAA debug-view pipeline failed to compile: {err}");
                    self.debug_failed = true;
                    self.debug_pending = None;
                }
                None => {}
            }
            return self.resolve_debug;
        }
        let started = (|| -> Result<Option<ComputePipelineKey>> {
            let shader = shaders.ensure_keys_sync_skip_validate(
                gpu,
                vec![ShaderCacheKey::from(self.debug_shader.clone())],
            )?[0];
            let cache_key = ComputePipelineCacheKey::new(shader, self.layout);
            if let Some(key) = compute.cache_lookup(&cache_key) {
                return Ok(Some(*key));
            }
            let mut prep = ComputePipelines::ensure_keys_prepare(
                gpu,
                shaders,
                pipeline_layouts,
                vec![cache_key.clone()],
            )?;
            let promise = prep
                .promises
                .pop()
                .expect("one pipeline was prepared, one promise");
            let pending: PendingPipeline = Rc::new(RefCell::new(None));
            let slot = pending.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = promise.await.map_err(|e| format!("{e:?}"));
                *slot.borrow_mut() = Some(result);
            });
            self.debug_pending = Some((cache_key, pending));
            Ok(None)
        })();
        match started {
            Ok(key) => self.resolve_debug = key,
            Err(err) => {
                tracing::warn!("TAA debug-view pipeline could not be started: {err:?}");
                self.debug_failed = true;
            }
        }
        self.resolve_debug
    }

    /// The debug variant can be dispatched from the next frame on (compiled,
    /// or compiled and waiting to be installed), or it failed and never will
    /// be. False while its compile runs, or before a view was ever selected.
    pub(crate) fn debug_settled(&self) -> bool {
        self.resolve_debug.is_some()
            || self.debug_failed
            || self
                .debug_pending
                .as_ref()
                .is_some_and(|(_, pending)| pending.borrow().is_some())
    }

    pub fn shader_cache_key(
        reverse_z: bool,
        reactive: bool,
        tonemapping: crate::post_process::ToneMapping,
        debug_views: bool,
    ) -> ShaderCacheKeyTaa {
        ShaderCacheKeyTaa {
            reverse_z,
            reactive,
            tonemapping,
            debug_views,
        }
    }
}
