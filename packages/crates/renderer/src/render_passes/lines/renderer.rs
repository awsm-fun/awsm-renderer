use std::future::Future;
use std::pin::Pin;

use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use slotmap::SlotMap;
use wasm_bindgen::JsValue;

use awsm_renderer_core::compare::CompareFunction;

use crate::{
    bind_group_layout::BindGroupLayouts,
    error::Result,
    pipeline_layouts::PipelineLayouts,
    pipeline_scheduler::warn_pipeline_not_compiled,
    pipelines::{
        render_pipeline::{RenderPipelineKey, RenderPipelines, RenderPipelinesPrep},
        Pipelines,
    },
    render::RenderContext,
    render_textures::RenderTextureFormats,
    shaders::Shaders,
};

use super::pipelines::{LinePipelines, LinePipelinesDescriptors, LineVariantKey};
use super::shader::cache_key::ShaderCacheKeyLine;
use super::types::{GpuLineSegment, LineEntry, LineKey, LINE_UNIFORM_BYTES};

/// In-flight lazy compile of the 4 line pipeline variants (Block B.3,
/// now auto-driven). Issued by [`LineRenderer::kick_compile`] (sync,
/// non-blocking `createRenderPipelineAsync`) and installed by
/// [`LineRenderer::poll_compile`] once the promises resolve. The future
/// is `'static` — it only awaits the GPU promises — so the borrow-free
/// issue / poll / install split mirrors the material scheduler's
/// `inflight_compile` pump.
struct LineInflightCompile {
    prep: RenderPipelinesPrep,
    #[allow(clippy::type_complexity)]
    joined: Pin<
        Box<dyn Future<Output = Vec<std::result::Result<web_sys::GpuRenderPipeline, JsValue>>>>,
    >,
}

/// Renderer-side state owning the four line pipelines and every registered line strip.
pub struct LineRenderer {
    pub(super) pipelines: LinePipelines,
    pub(super) entries: SlotMap<LineKey, LineEntry>,
    /// Scratch packing buffer reused across `add_line` / `update_line`
    /// calls. The `pack_into` helper clears + extends in place so
    /// per-call allocation cost goes to zero in the steady state.
    /// Held on the renderer (not on each call site) so editor
    /// overlays that re-pack many small line strips per frame
    /// (collider wireframes, point handles, selection outlines)
    /// don't bounce the allocator.
    pub(super) pack_buf: Vec<GpuLineSegment>,
    /// Block B.3: flips to `true` the first time `add_line_*` registers
    /// a `LineEntry` against an un-compiled `LinePipelines`. The next
    /// `AwsmRenderer::wait_for_pipelines_ready` (or an explicit
    /// `ensure_line_pipelines_compiled` call) drives the actual
    /// compile via `ensure_pipelines_compiled`. Dispatch silently
    /// warn-skips between the request and the compile.
    pub(super) pipelines_compile_requested: bool,
    /// Block B.3 (auto-drive): the in-flight pipeline compile, present
    /// between `kick_compile` issuing the `createRenderPipelineAsync`
    /// promises and `poll_compile` installing the resolved pipelines.
    /// `None` in the steady state (and for the entire lifetime of any
    /// renderer that never registers a line — zero cost for non-line
    /// projects).
    inflight: Option<LineInflightCompile>,
}

/// Pre-resolved layouts + 4 pipeline cache keys for the line renderer.
/// Returned by [`LineRenderer::build_descriptors`] and consumed by
/// [`LineRenderer::from_resolved`] after the cross-system tail pool
/// resolves the keys via one batched `RenderPipelines::ensure_keys`.
pub struct LineRendererDescriptors {
    pub(super) inner: LinePipelinesDescriptors,
}

impl LineRendererDescriptors {
    /// Slice of cache keys to fold into the cross-system render-pipeline
    /// pool. 4 entries, in `LinePipelines::VARIANT_KEYS` order.
    pub fn pipeline_cache_keys(
        &self,
    ) -> &[crate::pipelines::render_pipeline::RenderPipelineCacheKey] {
        &self.inner.pipeline_cache_keys
    }
}

impl LineRenderer {
    /// Loads the four pipeline variants and creates an empty line
    /// registry. Thin wrapper over [`Self::build_descriptors`] +
    /// [`Self::from_resolved`]. The pooled startup path bypasses this
    /// and calls the two halves directly so LineRenderer's 4 pipeline
    /// compiles share the cross-system tail `RenderPipelines::ensure_keys`.
    pub async fn load(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        pipelines: &mut Pipelines,
        shaders: &mut Shaders,
        formats: &RenderTextureFormats,
        depth_compare_strict: CompareFunction,
    ) -> Result<Self> {
        let descs = Self::build_descriptors(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            shaders,
            formats,
            depth_compare_strict,
        )
        .await?;
        let resolved = pipelines
            .render
            .ensure_keys(
                gpu,
                shaders,
                pipeline_layouts,
                descs.inner.pipeline_cache_keys.clone(),
            )
            .await?;
        Ok(Self::from_resolved(descs, resolved))
    }

    /// Cold-boot construction (Block B.3): registers the line BGL so
    /// `add_line_*` can create per-line bind groups, but defers the 4
    /// pipeline-variant compiles until the first line primitive is
    /// inserted. The `LineRenderer` ends up with
    /// `pipelines.variants = None`; dispatch warn-skips until the next
    /// `wait_for_pipelines_ready` (or explicit
    /// `ensure_line_pipelines_compiled`) drives the compile.
    pub fn new_deferred(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
    ) -> Result<Self> {
        Ok(Self {
            pipelines: LinePipelines::register_layouts_only(gpu, bind_group_layouts)?,
            entries: SlotMap::with_key(),
            pack_buf: Vec::new(),
            pipelines_compile_requested: false,
            inflight: None,
        })
    }

    /// Idempotent lazy compile of the 4 line pipeline variants
    /// (Block B.3). Returns immediately if `pipelines.variants` is
    /// already populated. Cache hits on `bind_group_layouts` /
    /// `pipeline_layouts` / `shaders` make the descriptor build cheap;
    /// the actual GPU work is the 4 pipeline compiles inside
    /// `ensure_keys`. Called from `AwsmRenderer::ensure_line_pipelines_compiled`,
    /// which is itself driven by `wait_for_pipelines_ready` and the
    /// MSAA-toggle path in `set_anti_aliasing`.
    pub async fn ensure_pipelines_compiled(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        pipelines: &mut Pipelines,
        shaders: &mut Shaders,
        formats: &RenderTextureFormats,
        depth_compare_strict: CompareFunction,
    ) -> Result<()> {
        if self.pipelines.variants.is_some() {
            self.pipelines_compile_requested = false;
            return Ok(());
        }
        let descs = LinePipelines::build_descriptors(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            shaders,
            formats,
            depth_compare_strict,
        )
        .await?;
        let resolved = pipelines
            .render
            .ensure_keys(
                gpu,
                shaders,
                pipeline_layouts,
                descs.pipeline_cache_keys.clone(),
            )
            .await?;
        self.pipelines.install_resolved(resolved);
        self.pipelines_compile_requested = false;
        Ok(())
    }

    /// Block B.3: `true` if a `LineEntry` has been registered but the
    /// 4 pipeline variants haven't been compiled yet. The renderer's
    /// `wait_for_pipelines_ready` checks this and drives the compile
    /// on transition.
    pub fn pipelines_compile_requested(&self) -> bool {
        self.pipelines_compile_requested
    }

    /// Auto-drive step 1 (sync, non-blocking): if a line primitive has
    /// been registered but the pipelines aren't compiled and no compile
    /// is already in flight, issue the 4 `createRenderPipelineAsync`
    /// promises and stash the in-flight futures. Idempotent + cheap: a
    /// single boolean check when there's nothing to do, so a renderer
    /// that never registers a line pays effectively nothing.
    ///
    /// Pairs with [`Self::poll_compile`]; both are called from the
    /// renderer's per-frame `render()` pre-amble, mirroring how the
    /// material scheduler's `poll_pipeline_scheduler` drives compute
    /// pipeline compiles. This removes the previous footgun where a
    /// consumer had to manually call `wait_for_pipelines_ready` after
    /// registering a line for it to ever draw.
    pub fn kick_compile(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        shaders: &Shaders,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        formats: &RenderTextureFormats,
        depth_compare_strict: CompareFunction,
    ) -> Result<()> {
        if !self.pipelines_compile_requested
            || self.pipelines.variants.is_some()
            || self.inflight.is_some()
        {
            return Ok(());
        }
        // The line shader is pre-warmed at boot (`RenderPasses::new`), so
        // this sync cache peek hits. If it somehow misses (a line
        // registered before boot shaders finished), bail and retry next
        // frame — never block or compile a shader on the render path.
        let Some(shader_key) = shaders.get_cached_key(ShaderCacheKeyLine) else {
            return Ok(());
        };
        let cache_keys = self.pipelines.build_cache_keys_sync(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            shader_key,
            formats,
            depth_compare_strict,
        )?;
        let mut prepped =
            RenderPipelines::ensure_keys_prepare(gpu, shaders, pipeline_layouts, cache_keys)?;
        let promises = std::mem::take(&mut prepped.promises);
        let joined = Box::pin(futures::future::join_all(promises));
        self.inflight = Some(LineInflightCompile {
            prep: prepped.prep,
            joined,
        });
        Ok(())
    }

    /// Auto-drive step 2 (sync, non-blocking): poll the in-flight compile
    /// future once with a no-op waker (`now_or_never`). When every
    /// `createRenderPipelineAsync` promise has resolved (which happens on
    /// the JS event loop between frames), install the resolved pipelines
    /// into the cross-pass pool + populate `variants`. Until then it's a
    /// no-op. No-op when nothing is in flight.
    pub fn poll_compile(&mut self, render_pipelines: &mut RenderPipelines) -> Result<()> {
        use futures::FutureExt;
        let ready = match self.inflight.as_mut() {
            Some(inflight) => inflight.joined.as_mut().now_or_never(),
            None => return Ok(()),
        };
        let Some(results) = ready else {
            return Ok(());
        };
        let inflight = self
            .inflight
            .take()
            .expect("inflight present (just polled Some)");
        let keys = render_pipelines.ensure_keys_install(inflight.prep, results)?;
        self.pipelines.install_resolved(keys);
        self.pipelines_compile_requested = false;
        Ok(())
    }

    /// Builds layouts + 4 pipeline cache keys. Cache-hit on the line
    /// shader (pre-warmed by `RenderPasses::new`); otherwise sync.
    pub async fn build_descriptors(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        shaders: &mut Shaders,
        formats: &RenderTextureFormats,
        depth_compare_strict: CompareFunction,
    ) -> Result<LineRendererDescriptors> {
        let inner = LinePipelines::build_descriptors(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            shaders,
            formats,
            depth_compare_strict,
        )
        .await?;
        Ok(LineRendererDescriptors { inner })
    }

    /// Folds resolved pipeline keys back into the typed `LineRenderer`.
    pub fn from_resolved(descs: LineRendererDescriptors, resolved: Vec<RenderPipelineKey>) -> Self {
        Self {
            pipelines: LinePipelines::from_resolved(descs.inner, resolved),
            entries: SlotMap::with_key(),
            pack_buf: Vec::new(),
            pipelines_compile_requested: false,
            inflight: None,
        }
    }

    /// Returns the number of registered lines.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if there are no registered lines.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl LineRenderer {
    /// Executes the line render pass: re-writes each line's uniform buffer
    /// with the current viewport size + width, then draws all registered lines
    /// against the world-space transparent target. Safe to call with zero
    /// registered lines (it returns early).
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        // Lazy-pool guard (Block B.3): a line primitive was registered
        // before pipelines were compiled. `add_line_*` sets
        // `pipelines_compile_requested = true`; the renderer's
        // `wait_for_pipelines_ready` drives the actual compile. Warn
        // once per session and skip the pass until then.
        if self.pipelines.variants.is_none() {
            warn_pipeline_not_compiled("line_pass", "all_variants");
            return Ok(());
        }
        let msaa = ctx.anti_aliasing.has_msaa_checked()?;
        let viewport_w = ctx.render_texture_views.width as f32;
        let viewport_h = ctx.render_texture_views.height as f32;

        let render_pass = ctx.begin_world_transparent_pass(Some("Line Render Pass"))?;
        let mut current_variant: Option<LineVariantKey> = None;

        for entry in self.entries.values() {
            if entry.segment_count == 0 {
                continue;
            }
            let mut uniform_bytes = [0u8; LINE_UNIFORM_BYTES];
            uniform_bytes[0..4].copy_from_slice(&entry.width_px.to_le_bytes());
            uniform_bytes[4..8].copy_from_slice(&viewport_w.to_le_bytes());
            uniform_bytes[8..12].copy_from_slice(&viewport_h.to_le_bytes());
            entry.uniform_uploader.lock().unwrap().write_dirty_ranges(
                ctx.gpu,
                &entry.uniform_buffer,
                LINE_UNIFORM_BYTES,
                &uniform_bytes[..],
                &[(0, LINE_UNIFORM_BYTES)],
            )?;

            let variant = LineVariantKey {
                depth_test_always: entry.depth_test_always,
                msaa,
            };
            if current_variant != Some(variant) {
                let Some(pipeline_key) = self.pipelines.get(variant) else {
                    warn_pipeline_not_compiled("line_pass", "variant");
                    continue;
                };
                render_pass.set_pipeline(ctx.pipelines.render.get(pipeline_key)?);
                current_variant = Some(variant);
            }
            render_pass.set_bind_group(0, &entry.bind_group, None)?;
            // 4 vertices per instance (triangle strip quad), N-1 instances.
            // Web GPU instanced non-indexed draw.
            render_pass.draw_with_instance_count(4, entry.segment_count);
        }
        render_pass.end();
        Ok(())
    }
}
