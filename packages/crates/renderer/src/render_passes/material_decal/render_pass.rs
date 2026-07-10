//! Material decal render pass execution.

use std::future::Future;
use std::pin::Pin;

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    decals::Decals,
    error::Result,
    pipelines::compute_pipeline::{ComputePipelines, ComputePipelinesPrep},
    render::RenderContext,
    render_passes::{
        material_decal::{
            bind_group::MaterialDecalBindGroups,
            classify::{pipeline::DecalClassifyPipelines, render_pass::DecalClassifyRenderPass},
            composite::MaterialDecalComposite,
            pipeline::MaterialDecalPipelines,
        },
        RenderPassInitContext,
    },
};

/// In-flight lazy compile kicked by [`MaterialDecalRenderPass::kick_compile`]
/// (the render-loop auto-drive for a LIVE decal insert that never reaches a
/// `commit_load`, e.g. the editor's node-kind observer calling `insert_decal`
/// directly). Mirrors `LineInflightCompile`: the promises resolve on the JS
/// event loop between frames; [`MaterialDecalRenderPass::poll_compile`]
/// installs them with a no-op-waker poll (`now_or_never`).
pub struct DecalInflightCompile {
    /// Compute batch: decal shading kernels (first `is_msaa.len()` slots) +
    /// optionally the classify cull (last slot). `None` once installed.
    compute: Option<(
        ComputePipelinesPrep,
        Pin<
            Box<
                dyn Future<
                    Output = Vec<
                        std::result::Result<web_sys::GpuComputePipeline, wasm_bindgen::JsValue>,
                    >,
                >,
            >,
        >,
    )>,
    /// MSAA slot flags for the decal shading kernels in the compute batch
    /// (empty when the kernels were already compiled and only classify /
    /// composite were missing).
    is_msaa: Vec<bool>,
    /// Whether the compute batch's tail slot is the classify cull.
    has_classify: bool,
    /// The composite's two inline render pipelines (own layout, outside the
    /// pipeline cache). `None` once installed.
    composite: Option<Pin<Box<dyn Future<Output = Result<MaterialDecalComposite>>>>>,
}

/// Material decal pass bind groups, compute pipelines, the
/// downstream composite pass, and the upstream per-tile classify pass.
pub struct MaterialDecalRenderPass {
    pub bind_groups: MaterialDecalBindGroups,
    /// Content-lazy (axis 1): `None` until `ensure_config_pipelines` compiles
    /// both MSAA variants at the first commit with a live decal. A decals
    /// feature that never places a decal compiles zero decal pipelines.
    pub pipelines: Option<MaterialDecalPipelines>,
    /// Deferred-boot: `None` until `ensure_config_pipelines` compiles the
    /// two inline-WGSL composite pipelines (they're not part of the pooled
    /// shader cache by design). Dispatch warn-skips while missing.
    pub composite: Option<MaterialDecalComposite>,
    pub classify_pass: DecalClassifyRenderPass,
    /// Render-loop auto-drive state for the content-lazy compile — `Some`
    /// while [`Self::kick_compile`]'s promises are in flight. The awaited
    /// commit path (`ensure_config_pipelines`) cancels it via
    /// [`Self::cancel_inflight`] before compiling, so the two drivers never
    /// double-install.
    inflight: Option<DecalInflightCompile>,
}

impl MaterialDecalRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialDecalBindGroups::new(ctx).await?;
        let pipelines = Some(MaterialDecalPipelines::new(ctx, &bind_groups).await?);
        let composite = Some(MaterialDecalComposite::new(ctx).await?);
        let classify_pass = DecalClassifyRenderPass::new(ctx).await?;
        Ok(Self {
            bind_groups,
            pipelines,
            composite,
            classify_pass,
            inflight: None,
        })
    }

    /// Assemble the content-lazy boot shape: bind groups only, every pipeline
    /// `None` (compiled by `ensure_config_pipelines` at the first commit with
    /// a live decal, or by the render-loop [`Self::kick_compile`] for a live
    /// insert that never commits).
    pub fn from_parts(
        bind_groups: MaterialDecalBindGroups,
        classify_pass: DecalClassifyRenderPass,
    ) -> Self {
        Self {
            bind_groups,
            pipelines: None,
            composite: None,
            classify_pass,
            inflight: None,
        }
    }

    /// Drop any render-loop in-flight compile (the browser still resolves the
    /// orphaned promises; the results are simply discarded). Called by the
    /// awaited commit path before it compiles, so only one driver installs.
    pub fn cancel_inflight(&mut self) {
        self.inflight = None;
    }

    /// Auto-drive step 1 (sync, non-blocking) — the decal analog of
    /// `LineRenderer::kick_compile`. If a decal is live but any of the
    /// content-lazy pipelines (shading kernels / classify cull / composite)
    /// is missing and nothing is in flight, issue the compile promises and
    /// stash them. This is what makes a LIVE `insert_decal` (editor node
    /// observer — no `commit_load` afterwards, §5b pending) project within a
    /// frame or two; the scene-load path still lands through
    /// `ensure_config_pipelines` at commit. Idempotent + cheap: boolean
    /// checks when there's nothing to do.
    ///
    /// Caller gates on `decals.len() > 0` (the renderer-side wrapper
    /// `AwsmRenderer::kick_decal_pipelines_compile`).
    pub fn kick_compile(&mut self, ctx: &mut RenderPassInitContext<'_>) -> Result<()> {
        if self.inflight.is_some() {
            return Ok(());
        }
        let needs_pipelines = self.pipelines.is_none();
        let needs_classify = self.classify_pass.pipelines.is_none();
        let needs_composite = self.composite.is_none();
        if !needs_pipelines && !needs_classify && !needs_composite {
            return Ok(());
        }

        let mut cache_keys = Vec::new();
        let mut is_msaa = Vec::new();
        if needs_pipelines {
            let (keys, msaa) =
                MaterialDecalPipelines::build_cache_keys_sync(ctx, &self.bind_groups)?;
            cache_keys.extend(keys);
            is_msaa = msaa;
        }
        if needs_classify {
            cache_keys.push(DecalClassifyPipelines::build_cache_key_sync(
                ctx,
                &self.classify_pass.bind_groups,
            )?);
        }
        let compute = if cache_keys.is_empty() {
            None
        } else {
            let mut prepped = ComputePipelines::ensure_keys_prepare(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                cache_keys,
            )?;
            let promises = std::mem::take(&mut prepped.promises);
            let joined: Pin<
                Box<
                    dyn Future<
                        Output = Vec<
                            std::result::Result<web_sys::GpuComputePipeline, wasm_bindgen::JsValue>,
                        >,
                    >,
                >,
            > = Box::pin(futures::future::join_all(promises));
            Some((prepped.prep, joined))
        };
        let composite = needs_composite.then(|| {
            Box::pin(MaterialDecalComposite::new_static(
                ctx.gpu.clone(),
                ctx.render_texture_formats.color,
            )) as Pin<Box<dyn Future<Output = Result<MaterialDecalComposite>>>>
        });
        self.inflight = Some(DecalInflightCompile {
            compute,
            is_msaa,
            has_classify: needs_classify,
            composite,
        });
        Ok(())
    }

    /// Auto-drive step 2 (sync, non-blocking): poll the in-flight compile
    /// with a no-op waker. Installs the shading kernels + classify cull into
    /// the pass (via the shared compute-pipeline cache) and the composite
    /// when their promises have resolved; no-op otherwise.
    ///
    /// Returns `true` when the COMPOSITE was installed this poll — the caller
    /// must then mark `BindGroupCreate::TextureViewRecreate` so the fresh
    /// composite's bind group is created (the composite arm in
    /// `bind_groups.rs` only binds on that event; without the mark it would
    /// skip-render forever).
    pub fn poll_compile(&mut self, compute_pipelines: &mut ComputePipelines) -> Result<bool> {
        use futures::FutureExt;
        let Some(mut inflight) = self.inflight.take() else {
            return Ok(false);
        };
        let mut composite_installed = false;

        if let Some((_, joined)) = inflight.compute.as_mut() {
            if let Some(results) = joined.as_mut().now_or_never() {
                let (prep, _) = inflight
                    .compute
                    .take()
                    .expect("compute inflight present (just polled Some)");
                let keys = compute_pipelines.ensure_keys_install(prep, results)?;
                let n_decal = inflight.is_msaa.len();
                if n_decal > 0 {
                    self.pipelines = Some(MaterialDecalPipelines::from_resolved(
                        std::mem::take(&mut inflight.is_msaa),
                        keys[..n_decal].to_vec(),
                    ));
                }
                if inflight.has_classify {
                    self.classify_pass.pipelines = Some(DecalClassifyPipelines::from_resolved(
                        keys[n_decal..].to_vec(),
                    ));
                }
            }
        }

        if let Some(fut) = inflight.composite.as_mut() {
            if let Some(result) = fut.as_mut().now_or_never() {
                inflight.composite = None;
                self.composite = Some(result?);
                composite_installed = true;
            }
        }

        // Put the state back while anything is still resolving.
        if inflight.compute.is_some() || inflight.composite.is_some() {
            self.inflight = Some(inflight);
        }
        Ok(composite_installed)
    }

    /// Rebuilds the texture-pool layout + dependent pipelines after the
    /// texture pool changes. Mirrors the opaque / transparent passes;
    /// without this the cached `texture_pool_layout_key` stays pinned
    /// to the empty layout captured at init time and the populated
    /// texture-pool bind group fails validation.
    ///
    /// Content-lazy: only recompiles the compute pipelines when they were
    /// already compiled (a decal is or was live); otherwise the eventual
    /// `ensure_config_pipelines` compile builds against the then-current pool.
    pub async fn texture_pool_changed(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
    ) -> Result<()> {
        self.bind_groups = self.bind_groups.clone_because_texture_pool_changed(ctx)?;
        if self.pipelines.is_some() {
            self.pipelines = Some(MaterialDecalPipelines::new(ctx, &self.bind_groups).await?);
        }
        Ok(())
    }

    /// Dispatches: classify → compute → composite. Skipped when no
    /// decals are active.
    pub fn render(&self, ctx: &RenderContext, decals: &Decals) -> Result<()> {
        if decals.is_empty() {
            return Ok(());
        }

        // Content-lazy: `None` only between a runtime decal insert and the
        // next commit's `ensure_config_pipelines` — skip (the decal simply
        // doesn't project that frame).
        let Some(pipelines) = self.pipelines.as_ref() else {
            return Ok(());
        };

        // Tile-bucket classify must run before the shading compute so
        // per-pixel iteration reads from a fresh per-tile decal list.
        self.classify_pass.render(ctx, decals.len() as u32)?;

        let pipeline_key = if ctx.anti_aliasing.msaa_sample_count.is_some() {
            pipelines.multisampled_pipeline_key
        } else {
            pipelines.singlesampled_pipeline_key
        };

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Decal Pass")).into(),
        ));

        compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
        compute_pass.set_bind_group(0, self.bind_groups.get_main()?, None)?;
        compute_pass.set_bind_group(1, self.bind_groups.get_texture_pool()?, None)?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
        compute_pass.end();

        // Composite pass — blit decal_color onto transparent. Cheap
        // fullscreen-tri with per-fragment discard; per-frame cost is
        // negligible vs the compute that just ran. `None` only between a
        // runtime decal insert and the next commit's config ensure — skip
        // (the decal simply doesn't composite that frame).
        if let Some(composite) = self.composite.as_ref() {
            composite.render(ctx)?;
        }

        Ok(())
    }
}
