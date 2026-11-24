//! Render texture allocation and management.

use awsm_renderer_core::{
    command::CommandEncoder,
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
    texture::{
        blit::{
            blit_get_bind_group, blit_get_pipeline, blit_get_pipeline_for_depth_pass, BlitPipeline,
        },
        clear::TextureClearer,
        Extent3d, TextureDescriptor, TextureFormat, TextureUsage, TextureViewDescriptor,
        TextureViewDimension,
    },
};
use thiserror::Error;

use crate::{anti_alias::AntiAliasing, features::RendererFeatures};

/// Render textures and cached views for the renderer.
pub struct RenderTextures {
    pub formats: RenderTextureFormats,
    pub opaque_to_transparent_blit_pipeline_msaa_4: BlitPipeline,
    /// The same blit as the first draw of the world transparent pass, which
    /// has a depth attachment. Uses the `_msaa_4` blit's bind group.
    pub opaque_to_transparent_in_pass_blit_pipeline_msaa_4: BlitPipeline,
    pub opaque_to_transparent_blit_pipeline_no_anti_alias: BlitPipeline,
    pub transparent_to_composite_blit_pipeline_no_anti_alias: BlitPipeline,
    /// Feature gates picked at construction time. Threaded through to
    /// [`RenderTexturesInner`] so the gated `decal_color` allocation
    /// can be skipped when `features.decals == false`.
    features: RendererFeatures,
    /// Plan B Stage 3: number of `Rgba8unorm` layers for the prep shadow-visibility
    /// array (4 shadow slots packed per texel). From
    /// `PrepPassConfig::shadow_visibility_layers()`.
    prep_shadow_layers: u32,
    frame_count: u32,
    inner: Option<RenderTexturesInner>,
    /// Render-texture generations retired by a recreate but NOT yet
    /// `destroy()`ed, paired with the `frame_count` they were retired on. A
    /// recreate (viewport resize / AA flip / mip-grow / HUD-depth) cannot
    /// destroy the old textures immediately: a previously-submitted "Rendering"
    /// command buffer may still reference them on the GPU, and destroying a
    /// texture still bound by an in-flight submit raises a `GPUValidationError`
    /// ("Destroyed texture used in a submit") which rejects the whole frame —
    /// the cause of "black sky on cold start", where the canvas size settles
    /// over the first few frames and recreates the textures while frame 0's
    /// submit is in flight. We instead defer `destroy()` by
    /// [`DESTROY_DELAY_FRAMES`] (safely past frames-in-flight), keeping GPU
    /// memory bounded (one or two retired generations) without the hazard.
    pending_destroy: Vec<(RenderTexturesInner, u32)>,
    /// `inner` was rebuilt and the render preamble has not yet rebuilt the
    /// bind groups that reference it. Set by ANY [`Self::views`] call that
    /// rebuilds; cleared only by [`Self::acknowledge_views_recreated`]. Every
    /// `views()` call reports it as `views_recreated` until then.
    ///
    /// It has to be sticky because `views()` has more than one caller. The
    /// render preamble is the one that reacts — it marks
    /// `BindGroupCreate::TextureViewRecreate` and invalidates the opaque
    /// mipgen. The picker's compile (`ensure_picker_compiled`) also calls
    /// `views()`, for its own bind group only. When the picker's call was the
    /// one that rebuilt `inner` (it ran first after a transmissive material
    /// grew the opaque mip chain, a HUD mesh appeared, or the surface
    /// resized), a one-shot flag was reported to the picker and lost: the
    /// next frame's `views()` saw nothing new, every bind group stayed on the
    /// retired textures, and `DESTROY_DELAY_FRAMES` later each submit failed
    /// with "Destroyed texture used in a submit" — permanently, until some
    /// unrelated event (an AA toggle) happened to mark the bind groups.
    views_recreated_unacknowledged: bool,
}

/// Frames to wait before `destroy()`ing a retired render-texture generation —
/// comfortably past the 1–2 frames a submit can stay in flight. See
/// [`RenderTextures::pending_destroy`].
const DESTROY_DELAY_FRAMES: u32 = 3;

/// Formats used for render textures.
#[derive(Clone, Debug)]
pub struct RenderTextureFormats {
    // Output from geometry pass
    pub visiblity_data: TextureFormat,
    pub barycentric: TextureFormat,
    pub normal_tangent: TextureFormat, // Packed: octahedral normal + tangent angle + handedness
    pub barycentric_derivatives: TextureFormat,

    // Output from coloring passes (opaque + transparent)
    pub color: TextureFormat,

    /// Per-pixel material-owned SSR reflection descriptor (M2a). RGB =
    /// `ssr_mask * ssr_tint` (reflectivity color; 0 = surface opts out of SSR),
    /// A = `ssr_spread` (0 mirror … 1 diffuse). Written by `material_opaque`,
    /// read by the SSR pass. Rgba8unorm is enough — these are 0..1 factors.
    pub reflection_descriptor: TextureFormat,

    /// TAA reactive mask (how much to distrust the history per pixel, 03
    /// §5): the transparent pass's second target under TAA. R8unorm.
    pub reactive: TextureFormat,

    /// TAA's motion vectors come from the prep pass on this renderer
    /// (`TaaVelocityPath::PrepReconstruction`, 03 §2.4): the geometry pass
    /// has no velocity target and the velocity texture is a prep storage
    /// target. Renderer state, set from `AwsmRenderer::taa_velocity_path` by
    /// the builder and `set_anti_aliasing`; read through
    /// [`Self::taa_prep_velocity`].
    pub prep_velocity_path: bool,

    // output from display pass is whatever current gpu texture format is

    // For depth testing and transparency
    pub depth: TextureFormat,
    // note - output from the composite pass will be whatever the gpu texture format is
}

impl RenderTextureFormats {
    /// Chooses default render texture formats for the device.
    pub async fn new(_device: &web_sys::GpuDevice) -> Self {
        Self::defaults()
    }

    /// The device-independent defaults (what [`Self::new`] returns).
    pub(crate) fn defaults() -> Self {
        Self {
            visiblity_data: TextureFormat::Rgba16uint,
            // RGBA16uint: RG = bary.xy as u16 fixed-point (* 65535), BA =
            // per-fragment instance_id (split u32 via `join32`). Stays at 4
            // color attachments; barycentric precision in u16 fixed-point is
            // comparable to f16 for the [0, 1] range.
            barycentric: TextureFormat::Rgba16uint,
            normal_tangent: TextureFormat::Rgba16float,
            barycentric_derivatives: TextureFormat::Rgba16float,
            color: TextureFormat::Rgba16float, // HDR format for bloom/tonemapping
            reflection_descriptor: TextureFormat::Rgba8unorm, // SSR reflectivity + spread factors
            reactive: TextureFormat::R8unorm,  // TAA reactive mask (0..1)
            prep_velocity_path: false,
            depth: TextureFormat::Depth32float, // More precision for thin/close surfaces
        }
    }
}

impl RenderTextureFormats {
    /// TAA is on and its motion vectors come from the prep-pass fallback.
    pub fn taa_prep_velocity(&self, anti_aliasing: &crate::anti_alias::AntiAliasing) -> bool {
        anti_aliasing.taa.is_some() && self.prep_velocity_path
    }
}

impl RenderTextures {
    /// Creates render texture managers and blit pipelines.
    pub async fn new(
        gpu: &AwsmRendererWebGpu,
        formats: RenderTextureFormats,
        features: &RendererFeatures,
        prep_shadow_layers: u32,
    ) -> Result<Self> {
        // Two distinct blit pipeline variants: the `None` (single-
        // sample) variant is used by *both* the opaque→transparent
        // and the transparent→composite blits — they share the same
        // shader + color target + (no) MSAA, so a single compiled
        // GpuRenderPipeline is reused. The `Some(4)` (MSAA-4) variant
        // is used by the opaque→transparent blit only, with a twin for
        // drawing it inside the world transparent pass. Compile all
        // variants concurrently; the `None` result is cloned into
        // the two destination fields (wasm-bindgen JS handle clone =
        // refcount bump, not a pipeline copy).
        //
        // Each variant is asked for exactly once. (`blit_get_pipeline`
        // would dedupe a second request for a key that is still compiling,
        // but there is no reason to make one.)
        let (single_sample_pipeline, msaa_4_pipeline, msaa_4_in_pass_pipeline) =
            futures::future::try_join3(
                async {
                    blit_get_pipeline(gpu, formats.color, None)
                        .await
                        .map_err(AwsmRenderTextureError::BlitPipeline)
                },
                async {
                    blit_get_pipeline(gpu, formats.color, Some(4))
                        .await
                        .map_err(AwsmRenderTextureError::BlitPipeline)
                },
                async {
                    blit_get_pipeline_for_depth_pass(gpu, formats.color, Some(4), formats.depth)
                        .await
                        .map_err(AwsmRenderTextureError::BlitPipeline)
                },
            )
            .await?;

        Ok(Self {
            formats,
            features: features.clone(),
            prep_shadow_layers,
            frame_count: 0,
            inner: None,
            pending_destroy: Vec::new(),
            views_recreated_unacknowledged: false,
            opaque_to_transparent_blit_pipeline_msaa_4: msaa_4_pipeline,
            opaque_to_transparent_in_pass_blit_pipeline_msaa_4: msaa_4_in_pass_pipeline,
            opaque_to_transparent_blit_pipeline_no_anti_alias: single_sample_pipeline.clone(),
            transparent_to_composite_blit_pipeline_no_anti_alias: single_sample_pipeline,
        })
    }

    /// Advances the internal frame counter and `destroy()`s any retired
    /// render-texture generation old enough that no in-flight submit can still
    /// reference it (see [`Self::pending_destroy`]).
    pub fn next_frame(&mut self) {
        self.frame_count = self.frame_count.wrapping_add(1);
        let now = self.frame_count;
        // `destroy(self)` consumes the inner, so drain by value (not `retain`,
        // which only yields `&`). Keep the not-yet-old-enough generations.
        let mut keep = Vec::new();
        for (inner, retired_at) in std::mem::take(&mut self.pending_destroy) {
            if now.wrapping_sub(retired_at) >= DESTROY_DELAY_FRAMES {
                inner.destroy();
            } else {
                keep.push((inner, retired_at));
            }
        }
        self.pending_destroy = keep;
    }

    /// Returns the current frame counter.
    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// Returns true on even frames for ping-pong usage.
    pub fn ping_pong(&self) -> bool {
        self.frame_count() % 2 == 0
    }

    /// Returns render texture views, recreating if size or AA changed.
    ///
    /// `sizes` is the frame's [`crate::size::FrameSizes`] — the caller
    /// passes the value `AwsmRenderer::render` already snapped at the
    /// top of the frame (`AwsmRenderer::frame_sizes`), sparing this method a
    /// redundant `getCurrentTexture().getSize()` wasm↔JS hop. Render-size
    /// textures are allocated at `sizes.render`, post-size ones at
    /// `sizes.post` (see [`RenderTexturesInner`] for which is which); a change
    /// in either rebuilds.
    ///
    /// `needs_opaque_mip_chain` is `Materials::has_seen_transmission`
    /// — T2.5 lazy mip-chain allocation. When `false`, the opaque
    /// texture is created with `mip_level_count = 1` (saves ~33% of
    /// its allocation footprint). The first frame the flag flips
    /// true, this method reallocates with the full chain — the same
    /// `TextureViewRecreate` event the caller fires on size change
    /// rebuilds dependent bind groups.
    ///
    /// `needs_hud_depth` is `Meshes::has_seen_hud` — T2.6 lazy HUD
    /// depth allocation. When `false`, the HUD depth attachment is
    /// skipped entirely (the HUD passes themselves are already gated
    /// on the same condition at the call site). When the first HUD
    /// renderable registers, the flag flips and the texture is
    /// allocated on the next `views()` call.
    pub fn views(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        anti_aliasing: AntiAliasing,
        sizes: crate::size::FrameSizes,
        needs_opaque_mip_chain: bool,
        needs_hud_depth: bool,
        ssr_enabled: bool,
        ssr_half_res: bool,
        ssr_temporal: bool,
        ssr_bvh: bool,
    ) -> Result<RenderTextureViews> {
        let size_changed = match self.inner.as_ref() {
            Some(inner) => inner.render_size != sizes.render || inner.post_size != sizes.post,
            None => true,
        };

        let anti_aliasing_changed = match self.inner.as_ref() {
            // Structural only: a change inside `taa: Some(..)` (tuning,
            // sharpening, render scale) needs no new targets.
            // The motion-vector path (renderer state) is structural too: the
            // velocity target's format and usage follow it.
            Some(inner) => {
                !inner.anti_aliasing.same_structure(&anti_aliasing)
                    || inner.prep_velocity != self.formats.taa_prep_velocity(&anti_aliasing)
            }
            None => false,
        };

        // Lazy SSR-target allocation: the `ssr` + `reflection_descriptor` targets
        // are full-res only when SSR is enabled, 1×1 placeholders otherwise (so a
        // renderer that never uses SSR pays no full-res memory). Toggling SSR
        // flips this and rebuilds inner just like an AA change — the next
        // `views()` sees the new flag and re-allocates + recreates bind groups.
        let ssr_enabled_changed = match self.inner.as_ref() {
            Some(inner) => inner.ssr_enabled != ssr_enabled,
            None => false,
        };

        // Half-res SSR trace: the `ssr` target shrinks to
        // (half_extent(w), half_extent(h)) (see `crate::size`) when SSR is on
        // AND resolution_scale < 1.0 (the composite step bilinearly upsamples
        // it). Toggling it rebuilds inner + recreates the
        // SSR bind groups, exactly like the `ssr_enabled` flip above.
        let ssr_half_res_changed = match self.inner.as_ref() {
            Some(inner) => inner.ssr_half_res != ssr_half_res,
            None => false,
        };

        // SSR temporal: the accumulation output (`ssr_final`) + history pair
        // (`ssr_history`) are only allocated full-size when SSR is on AND
        // temporal is on, else 1×1 placeholders. Toggling it rebuilds inner +
        // recreates the SSR bind groups exactly like the `ssr_half_res` flip
        // above.
        let ssr_temporal_changed = match self.inner.as_ref() {
            Some(inner) => inner.ssr_temporal != ssr_temporal,
            None => false,
        };

        // SSR software-BVH: the `ssr_bvh` hit target is only allocated
        // full-size when SSR is on AND bvh_reflections is on, else a 1x1
        // placeholder. Toggling rebuilds inner exactly like `ssr_temporal`.
        let ssr_bvh_changed = match self.inner.as_ref() {
            Some(inner) => inner.ssr_bvh != ssr_bvh,
            None => false,
        };

        // T2.5: opaque mip-chain state changed (false → true). The
        // false → true direction is the only one we care about; the
        // flag is sticky so the texture never shrinks back to mip 1.
        let opaque_mips_grown = match self.inner.as_ref() {
            Some(inner) => needs_opaque_mip_chain && !inner.opaque_has_mip_chain,
            None => false,
        };

        // T2.6: HUD depth attachment appeared. Same sticky semantics
        // as the opaque mip chain — once the flag flips to true, it
        // never flips back.
        let hud_depth_appeared = match self.inner.as_ref() {
            Some(inner) => needs_hud_depth && inner.hud_depth.is_none(),
            None => false,
        };

        if size_changed
            || anti_aliasing_changed
            || opaque_mips_grown
            || hud_depth_appeared
            || ssr_enabled_changed
            || ssr_half_res_changed
            || ssr_temporal_changed
            || ssr_bvh_changed
        {
            if let Some(inner) = self.inner.take() {
                // Defer destroy: a prior frame's "Rendering" submit may still
                // reference these textures on the GPU. Destroying now would
                // raise "Destroyed texture used in a submit" and reject that
                // frame (the black-on-start cause). `next_frame` destroys it
                // once it's `DESTROY_DELAY_FRAMES` old.
                self.pending_destroy.push((inner, self.frame_count));
            }

            let inner = RenderTexturesInner::new(
                gpu,
                self.formats.clone(),
                &self.opaque_to_transparent_blit_pipeline_msaa_4,
                &self.opaque_to_transparent_blit_pipeline_no_anti_alias,
                &self.transparent_to_composite_blit_pipeline_no_anti_alias,
                sizes.render,
                sizes.post,
                anti_aliasing,
                &self.features,
                self.prep_shadow_layers,
                needs_opaque_mip_chain,
                needs_hud_depth,
                ssr_enabled,
                ssr_half_res,
                ssr_temporal,
                ssr_bvh,
            )?;
            self.inner = Some(inner);
            self.views_recreated_unacknowledged = true;
        }

        // `views_recreated` is `true` for ANY reason `inner` was rebuilt —
        // viewport resize, AA flip, T2.5 opaque-mip-chain growth, T2.6
        // HUD-depth materialization, an SSR target change — by THIS call or
        // by an earlier one the render preamble has not acknowledged yet
        // (see `views_recreated_unacknowledged`). The render preamble fires
        // `BindGroupCreate::TextureViewRecreate` and invalidates the opaque
        // mipgen cache off this flag, then acknowledges it, so every
        // recreation trigger routes its bind groups and mipgen cache through
        // the rebuild path no matter which caller of `views()` performed the
        // rebuild. (Earlier this was a one-shot computed from this call's own
        // triggers, so a rebuild performed by the picker's call was never
        // seen by the render preamble.)
        let views_recreated = self.views_recreated_unacknowledged;
        Ok(RenderTextureViews::new(
            self.inner.as_ref().unwrap(),
            self.ping_pong(),
            views_recreated,
        ))
    }

    /// `(render, post)` size of the textures as they are now (the last
    /// frame's, unless a `views()` call rebuilt them since), or `None` before
    /// the first frame. What a readback of these textures has to map into.
    pub(crate) fn current_sizes(&self) -> Option<((u32, u32), (u32, u32))> {
        self.inner
            .as_ref()
            .map(|inner| (inner.render_size, inner.post_size))
    }

    /// The render preamble calls this once it has rebuilt everything that
    /// references the render textures (the `TextureViewRecreate` bind groups,
    /// the opaque mipgen cache) for the current `inner`. Until then every
    /// [`Self::views`] call keeps reporting `views_recreated == true`.
    ///
    /// Only the render preamble may call it: it is the one consumer that
    /// reacts to the flag for the whole frame.
    pub fn acknowledge_views_recreated(&mut self) {
        self.views_recreated_unacknowledged = false;
    }

    /// Borrows the inner textures (if initialized). Used by per-frame
    /// passes that need direct GPU texture handles (e.g. mip generation).
    pub fn inner(&self) -> Option<&RenderTexturesInner> {
        self.inner.as_ref()
    }

    /// Records the opaque-render-texture clear into `encoder` (when the
    /// textures are initialized). Recorded into the frame's main
    /// "Rendering" encoder ahead of the opaque pass rather than issuing
    /// its own encoder+submit — see [`TextureClearer::clear`].
    pub fn clear_opaque(&self, encoder: &CommandEncoder) -> Result<()> {
        if let Some(inner) = self.inner.as_ref() {
            inner
                .opaque_clearer
                .clear(encoder, &inner.opaque)
                .map_err(AwsmRenderTextureError::TextureClearerClear)
        } else {
            Ok(())
        }
    }
}

/// Collection of texture views used by render passes.
pub struct RenderTextureViews {
    // Output from geometry pass
    pub visibility_data: web_sys::GpuTextureView,
    pub barycentric: web_sys::GpuTextureView,
    pub normal_tangent: web_sys::GpuTextureView,
    pub barycentric_derivatives: web_sys::GpuTextureView,
    /// TAA motion vectors (`rg16float`, render size, UV units `uv_now -
    /// uv_prev`): the geometry pass's fifth target. `None` unless TAA is on.
    pub velocity: Option<web_sys::GpuTextureView>,
    /// TAA reactive mask (`r8unorm`, render size; 0 = trust the history, 1 =
    /// ignore it): the world transparent pass's second target, MAX blended.
    /// `None` unless TAA is on.
    pub reactive: Option<web_sys::GpuTextureView>,
    /// TAA's motion vectors come from the prep pass (the `velocity` view is
    /// its storage target; `RenderTextureFormats::taa_prep_velocity`).
    pub prep_velocity: bool,

    // Output from opaque pass
    pub opaque: web_sys::GpuTextureView,
    /// Mip-0-only view of the opaque target. Storage bindings require a
    /// single-mip view, so the opaque compute pass and the post-pass
    /// mipgen seed both use this view.
    pub opaque_storage: web_sys::GpuTextureView,
    /// Full-mip view of the opaque target, used by the transparent pass
    /// to sample pre-blurred neighborhoods for screen-space transmission.
    pub opaque_full: web_sys::GpuTextureView,
    pub opaque_to_transparent_blit_bind_group_msaa_4: web_sys::GpuBindGroup,
    pub opaque_to_transparent_blit_bind_group_no_anti_alias: web_sys::GpuBindGroup,
    /// Number of mip levels in the opaque target (`floor(log2(max(w, h))) + 1`).
    pub opaque_mip_count: u32,

    // Output from transparent pass
    pub transparent: web_sys::GpuTextureView,

    /// Single-sample storage target the decal compute writes to when
    /// MSAA is enabled (the multisampled `transparent` can't be storage-
    /// bound). A small composite pass then alpha-blits it over the
    /// multisampled transparent target. `None` when
    /// `features.decals == false` — the texture is gated because it's
    /// ~16 MB at 4K.
    pub decal_color: Option<web_sys::GpuTextureView>,

    /// Plan B prep-pass output views (None when the prep feature is off).
    pub prep_uv: Option<web_sys::GpuTextureView>,
    pub prep_vcolor: Option<web_sys::GpuTextureView>,
    /// Plan B Stage 3: per-pixel shadow-visibility view (Rgba8unorm array).
    pub prep_shadow_visibility: Option<web_sys::GpuTextureView>,
    /// Ping-pong temp for the optional separable shadow-denoise blur (same
    /// descriptor as `prep_shadow_visibility`). H writes here, V writes back.
    pub prep_shadow_visibility_blur_tmp: Option<web_sys::GpuTextureView>,
    /// U0: per-pixel edge-id view (R32Uint). `None` when MSAA is off.
    /// Written by classify (storage texture); read by nobody in U0.
    pub edge_id: Option<web_sys::GpuTextureView>,

    // Output from composite pass
    pub composite: web_sys::GpuTextureView,
    pub transparent_to_composite_blit_bind_group_no_anti_alias: Option<web_sys::GpuBindGroup>,

    // Output from effects pass
    pub effects: web_sys::GpuTextureView,
    pub bloom: web_sys::GpuTextureView,

    /// SSR reflection target — the SSR pass storage-writes its result here.
    pub ssr: web_sys::GpuTextureView,
    /// SSR spatial-resolve output (same dims/format as `ssr`). The resolve
    /// compute reads `ssr` + depth and storage-writes the denoised reflection
    /// here; the composite upsample reads THIS instead of the raw trace —
    /// unless temporal is on, in which case the temporal pass reads this and
    /// the composite reads `ssr_final`.
    pub ssr_resolved: web_sys::GpuTextureView,
    /// SSR temporal-accumulation output (same dims/format as `ssr`). The
    /// temporal pass reads `ssr_resolved` + history, storage-writes the
    /// accumulated reflection here, and the composite reads THIS when temporal
    /// is on. 1×1 placeholder unless SSR is on AND temporal is on.
    pub ssr_final: web_sys::GpuTextureView,
    /// SSR temporal history pair (same dims as `ssr`). The temporal pass reads
    /// the previous frame's accumulated reflection from one and writes this
    /// frame's into the other, swapping by frame parity. 1×1 placeholders
    /// unless SSR is on AND temporal is on.
    pub ssr_history: [web_sys::GpuTextureView; 2],
    /// Software-BVH raw hit target (rgba16float, `ssr` dims): the bvh_trace
    /// compute storage-writes (hit color, hit flag) here; the SSR trace reads
    /// it as its miss fallback when `bvh_reflections` is on. 1x1 placeholder
    /// otherwise.
    pub ssr_bvh: web_sys::GpuTextureView,
    /// M2a material-owned reflection descriptor — `material_opaque` writes it,
    /// the SSR pass reads it.
    pub reflection_descriptor: web_sys::GpuTextureView,

    pub depth: web_sys::GpuTextureView,
    /// The depth SSR reads, always single-sample: `depth` itself without MSAA,
    /// and the SSR pass's sample-0 copy of it with (1x1 while SSR is off).
    pub ssr_depth: web_sys::GpuTextureView,
    /// T2.6: `None` until the first HUD renderable registers
    /// (`Meshes::has_seen_hud`). The HUD geometry / transparent
    /// passes are already gated on `renderables.hud.is_empty()` at
    /// their call sites (T1.10), so they never sample `hud_depth`
    /// before it materializes.
    pub hud_depth: Option<web_sys::GpuTextureView>,
    /// TAA: the depth of the HUD's lit pass, which runs after the resolve
    /// into `taa_out` at post size. `Some` only under TAA once a HUD
    /// renderable exists.
    pub hud_depth_post: Option<web_sys::GpuTextureView>,
    /// `true` when `RenderTexturesInner` was destroyed and rebuilt
    /// during this `views()` call — by viewport resize, AA flip,
    /// T2.5 opaque-mip-chain growth, or T2.6 HUD-depth
    /// materialization. Every consumer that caches anything keyed
    /// to a specific `GpuTextureView` / `GpuTexture` identity (bind
    /// groups, the opaque mipgen) MUST invalidate on this flag, not
    /// just on viewport resize — the destroyed textures and their
    /// views are no longer valid.
    pub views_recreated: bool,
    /// Scene resolution `(width, height)`: the size of every render-size
    /// texture above (geometry targets, depth, opaque, transparent,
    /// composite, prep, SSR's full-res inputs). Dispatches over those read
    /// this.
    pub render_size: (u32, u32),
    /// Post-processing resolution `(width, height)`: the size of `effects`,
    /// `bloom` (and the bloom pyramid / SMAA targets owned by their passes).
    /// Equal to `render_size` without TAA. There is deliberately no plain
    /// `width`/`height`: every consumer chooses one of the two
    /// (`size::tests::render_texture_consumers_choose_a_size`).
    pub post_size: (u32, u32),
    pub curr_index: usize,
    pub prev_index: usize,
}

impl RenderTextureViews {
    /// Builds view handles for the current render textures.
    pub fn new(inner: &RenderTexturesInner, ping_pong: bool, views_recreated: bool) -> Self {
        let curr_index = if ping_pong { 0 } else { 1 };
        let prev_index = if ping_pong { 1 } else { 0 };
        Self {
            visibility_data: inner.visibility_data_view.clone(),
            barycentric: inner.barycentric_view.clone(),
            normal_tangent: inner.normal_tangent_view.clone(),
            barycentric_derivatives: inner.barycentric_derivatives_view.clone(),
            velocity: inner.velocity_view.clone(),
            reactive: inner.reactive_view.clone(),
            prep_velocity: inner.prep_velocity,
            opaque: inner.opaque_storage_view.clone(),
            opaque_storage: inner.opaque_storage_view.clone(),
            opaque_full: inner.opaque_full_view.clone(),
            opaque_mip_count: inner.opaque_mip_count,
            opaque_to_transparent_blit_bind_group_msaa_4: inner
                .opaque_to_transparent_blit_bind_group_msaa_4
                .clone(),
            opaque_to_transparent_blit_bind_group_no_anti_alias: inner
                .opaque_to_transparent_blit_bind_group_no_anti_alias
                .clone(),
            transparent_to_composite_blit_bind_group_no_anti_alias: inner
                .transparent_to_composite_blit_bind_group_no_anti_alias
                .clone(),
            transparent: inner.transparent_view.clone(),
            decal_color: inner.decal_color_view.clone(),
            // ^ `Option::clone()` — stays `None` when decals are gated off.
            prep_uv: inner.prep_uv_view.clone(),
            prep_vcolor: inner.prep_vcolor_view.clone(),
            prep_shadow_visibility: inner.prep_shadow_visibility_view.clone(),
            prep_shadow_visibility_blur_tmp: inner.prep_shadow_visibility_blur_tmp_view.clone(),
            edge_id: inner.edge_id_view.clone(),
            depth: inner.depth_view.clone(),
            ssr_depth: inner
                .ssr_depth_view
                .as_ref()
                .unwrap_or(&inner.depth_view)
                .clone(),
            hud_depth: inner.hud_depth_view.clone(),
            hud_depth_post: inner.hud_depth_post_view.clone(),
            // ^ `Option::clone()` — `None` until T2.6's sticky flag
            // flips on the first HUD renderable insertion.
            effects: inner.effects_view.clone(),
            bloom: inner.bloom_view.clone(),
            ssr: inner.ssr_view.clone(),
            ssr_resolved: inner.ssr_resolved_view.clone(),
            ssr_final: inner.ssr_final_view.clone(),
            ssr_bvh: inner.ssr_bvh_view.clone(),
            ssr_history: [
                inner.ssr_history_views[0].clone(),
                inner.ssr_history_views[1].clone(),
            ],
            reflection_descriptor: inner.reflection_descriptor_view.clone(),
            composite: inner.composite_view.clone(),
            views_recreated,
            curr_index,
            prev_index,
            render_size: inner.render_size,
            post_size: inner.post_size,
        }
    }
}

/// Internal texture storage and GPU objects.
///
/// Two sizes (see [`crate::size::FrameSizes`]); without TAA they are equal.
///
/// - **Render size** (the scene; TAA's input): `visibility_data`,
///   `barycentric`, `normal_tangent`, `barycentric_derivatives`, `velocity`
///   and `reactive` (TAA only), `depth`,
///   `ssr_depth`, `hud_depth`, `opaque` (+ mips, `opaque_clearer`),
///   `transparent`, `decal_color`, `prep_*`, `edge_id`, `composite`,
///   `reflection_descriptor`, and the SSR targets (`ssr`, `ssr_resolved`,
///   `ssr_final`, `ssr_history`, `ssr_bvh`; half of render size when SSR runs
///   half-res). Outside this struct: the HZB and the volumetrics froxel grid
///   (aligned with light culling) follow render size too.
/// - **Post size** (after the TAA resolve): `effects`, `bloom`,
///   `hud_depth_post` (TAA only: the HUD's lit pass runs after the resolve).
///   Outside this struct: the bloom pyramid, the SMAA edges/weights (SMAA is
///   rejected together with TAA, so for SMAA the two sizes are always equal;
///   it is post-side because its weights are consumed by the effects pass
///   blend) and TAA's history / output targets.
#[allow(dead_code)]
pub struct RenderTexturesInner {
    pub visibility_data: web_sys::GpuTexture,
    pub visibility_data_view: web_sys::GpuTextureView,

    pub barycentric: web_sys::GpuTexture,
    pub barycentric_view: web_sys::GpuTextureView,

    // pub taa_clip_positions: [web_sys::GpuTexture; 2],
    // pub taa_clip_position_views: [web_sys::GpuTextureView; 2],
    pub normal_tangent: web_sys::GpuTexture,
    pub normal_tangent_view: web_sys::GpuTextureView,

    pub barycentric_derivatives: web_sys::GpuTexture,
    pub barycentric_derivatives_view: web_sys::GpuTextureView,

    /// TAA motion-vector target. `None` unless `anti_aliasing.taa` is set
    /// (an AA change rebuilds the whole set, so it appears / disappears with
    /// TAA).
    pub velocity: Option<web_sys::GpuTexture>,
    pub velocity_view: Option<web_sys::GpuTextureView>,

    /// TAA reactive mask. `None` unless `anti_aliasing.taa` is set.
    pub reactive: Option<web_sys::GpuTexture>,
    pub reactive_view: Option<web_sys::GpuTextureView>,
    /// See [`RenderTextureViews::prep_velocity`].
    pub prep_velocity: bool,

    pub opaque: web_sys::GpuTexture,
    pub opaque_clearer: TextureClearer,
    /// Storage-binding-friendly mip-0 view; also what the blit shaders
    /// sample from. WebGPU requires storage views to cover exactly one
    /// mip level, so we keep this separate from the full-mip view below.
    pub opaque_storage_view: web_sys::GpuTextureView,
    pub opaque_full_view: web_sys::GpuTextureView,
    pub opaque_mip_count: u32,
    /// T2.5: `true` when the opaque texture carries the full
    /// `floor(log2(max(W,H))) + 1` mip chain (allocated because a
    /// transmissive material is registered); `false` when allocated
    /// with `mip_level_count = 1`. Read by `RenderTextures::views`
    /// to detect a false → true transition that needs reallocation.
    pub opaque_has_mip_chain: bool,
    pub opaque_to_transparent_blit_bind_group_msaa_4: web_sys::GpuBindGroup,
    pub opaque_to_transparent_blit_bind_group_no_anti_alias: web_sys::GpuBindGroup,

    pub transparent: web_sys::GpuTexture,
    pub transparent_view: web_sys::GpuTextureView,

    /// Single-sample storage target used as the decal compute's output
    /// when MSAA is on (the multisampled `transparent` can't be
    /// storage-bound). A small composite pass alpha-blits it over the
    /// multisampled `transparent` target. `None` when
    /// `features.decals == false` — the bind-group shape stays stable
    /// because the decal pass's bind groups are also skipped in that
    /// mode.
    pub decal_color: Option<web_sys::GpuTexture>,
    pub decal_color_view: Option<web_sys::GpuTextureView>,

    /// Plan B prep-pass outputs (always allocated — prep is unconditional).
    /// Kept `Option` so the bind-group `.as_ref()` read sites stay unchanged.
    pub prep_uv: Option<web_sys::GpuTexture>,
    pub prep_uv_view: Option<web_sys::GpuTextureView>,
    pub prep_vcolor: Option<web_sys::GpuTexture>,
    pub prep_vcolor_view: Option<web_sys::GpuTextureView>,
    /// Plan B Stage 3: per-pixel shadow-visibility (Rgba8unorm array, 4 packed
    /// slots/texel). Always allocated (prep is unconditional).
    pub prep_shadow_visibility: Option<web_sys::GpuTexture>,
    pub prep_shadow_visibility_view: Option<web_sys::GpuTextureView>,
    /// Ping-pong temp for the optional separable shadow-denoise blur (same
    /// descriptor as `prep_shadow_visibility`).
    pub prep_shadow_visibility_blur_tmp: Option<web_sys::GpuTexture>,
    pub prep_shadow_visibility_blur_tmp_view: Option<web_sys::GpuTextureView>,

    /// U0 (`docs/plans/unified-edge-shading.md`): per-pixel edge-id texture
    /// (R32Uint, one word/pixel). Classify writes the compact edge_pixel_id /
    /// U32_MAX sentinel; the future unified kernel reads it. `None` when
    /// MSAA is off.
    pub edge_id: Option<web_sys::GpuTexture>,
    pub edge_id_view: Option<web_sys::GpuTextureView>,

    pub depth: web_sys::GpuTexture,
    pub depth_view: web_sys::GpuTextureView,
    /// Sample-0 copy of `depth` for SSR. `None` without MSAA.
    pub ssr_depth: Option<web_sys::GpuTexture>,
    pub ssr_depth_view: Option<web_sys::GpuTextureView>,

    /// T2.6: lazy HUD depth attachment — `None` while
    /// `Meshes::has_seen_hud` is false. The texture sits on the inner
    /// rather than `RenderTextures` so it participates in the same
    /// destroy-and-recreate lifecycle as everything else on
    /// viewport resize / AA flip / mip-chain growth.
    pub hud_depth: Option<web_sys::GpuTexture>,
    pub hud_depth_view: Option<web_sys::GpuTextureView>,
    /// TAA only: the post-size depth of the HUD's lit pass (after the
    /// resolve). See [`RenderTextureViews::hud_depth_post`].
    pub hud_depth_post: Option<web_sys::GpuTexture>,
    pub hud_depth_post_view: Option<web_sys::GpuTextureView>,

    pub composite: web_sys::GpuTexture,
    pub composite_view: web_sys::GpuTextureView,
    pub transparent_to_composite_blit_bind_group_no_anti_alias: Option<web_sys::GpuBindGroup>,

    pub effects: web_sys::GpuTexture,
    pub effects_view: web_sys::GpuTextureView,

    pub bloom: web_sys::GpuTexture,
    pub bloom_view: web_sys::GpuTextureView,

    /// SSR reflection target — the SSR trace storage-writes reflection-only
    /// premultiplied color here (half-res by default); `SsrComposite`
    /// additively blends it onto `composite`. HDR, same storage+sample usage
    /// as bloom.
    pub ssr: web_sys::GpuTexture,
    pub ssr_view: web_sys::GpuTextureView,
    /// SSR spatial-resolve output — same dims + format + usage as `ssr`
    /// (storage-write from the resolve compute + texture read by the
    /// composite). Follows the same lazy sizing: 1×1 placeholder when SSR off.
    pub ssr_resolved: web_sys::GpuTexture,
    pub ssr_resolved_view: web_sys::GpuTextureView,
    /// SSR temporal-accumulation output — same dims + format + usage as `ssr`
    /// (storage-write from the temporal compute + texture read by the
    /// composite). 1×1 placeholder unless `ssr_enabled && ssr_temporal` (the
    /// temporal-off composite reads `ssr_resolved` directly).
    pub ssr_final: web_sys::GpuTexture,
    pub ssr_final_view: web_sys::GpuTextureView,
    /// SSR temporal history pair — same dims + format as `ssr`. Ping-ponged
    /// by frame parity: one is read (previous frame), the other written (this
    /// frame). 1×1 placeholders unless `ssr_enabled && ssr_temporal`.
    pub ssr_history: [web_sys::GpuTexture; 2],
    pub ssr_history_views: [web_sys::GpuTextureView; 2],
    /// Software-BVH hit target. 1×1 placeholder unless `ssr_enabled && ssr_bvh`.
    pub ssr_bvh_tex: web_sys::GpuTexture,
    pub ssr_bvh_view: web_sys::GpuTextureView,

    /// M2a material-owned reflection descriptor (Rgba8unorm). `material_opaque`
    /// storage-writes it; the SSR pass texture-reads it. See
    /// [`RenderTextureFormats::reflection_descriptor`].
    pub reflection_descriptor: web_sys::GpuTexture,
    pub reflection_descriptor_view: web_sys::GpuTextureView,

    /// Size of every render-size texture (see the type docs).
    pub render_size: (u32, u32),
    /// Size of every post-size texture (`effects`, `bloom`).
    pub post_size: (u32, u32),

    pub anti_aliasing: AntiAliasing,
    /// Whether the `ssr` + `reflection_descriptor` targets are full-res (SSR on)
    /// or 1×1 placeholders (SSR off). `views()` rebuilds inner when this flips.
    pub ssr_enabled: bool,
    /// Whether the `ssr` target is half-res (SSR on AND resolution_scale < 1.0).
    /// `reflection_descriptor` stays full-res regardless. `views()` rebuilds
    /// inner when this flips.
    pub ssr_half_res: bool,
    /// Whether the temporal targets (`ssr_final` + the `ssr_history` pair) are
    /// allocated at the `ssr` dims (SSR on AND temporal on) vs. 1×1
    /// placeholders. `views()` rebuilds inner when this flips.
    pub ssr_temporal: bool,
    /// Whether the `ssr_bvh` hit target is full-size.
    pub ssr_bvh: bool,
}

impl RenderTexturesInner {
    /// Creates render textures and views for the given size.
    ///
    /// `needs_opaque_mip_chain` controls T2.5's lazy mip allocation:
    /// `false` → opaque texture allocated with `mip_level_count = 1`
    /// (the only mip the opaque pass writes); `true` → full
    /// `floor(log2(max(W,H))) + 1` chain for transmission sampling.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        render_texture_formats: RenderTextureFormats,
        opaque_to_transparent_blit_pipeline_msaa_4: &BlitPipeline,
        opaque_to_transparent_blit_pipeline_no_anti_alias: &BlitPipeline,
        transparent_to_composite_blit_pipeline_no_anti_alias: &BlitPipeline,
        render_size: (u32, u32),
        post_size: (u32, u32),
        anti_aliasing: AntiAliasing,
        features: &RendererFeatures,
        prep_shadow_layers: u32,
        needs_opaque_mip_chain: bool,
        needs_hud_depth: bool,
        ssr_enabled: bool,
        ssr_half_res: bool,
        ssr_temporal: bool,
        ssr_bvh: bool,
    ) -> Result<Self> {
        let (render_w, render_h) = render_size;
        let (post_w, post_h) = post_size;
        // Lazy SSR-target sizing: full-res only when SSR is on, else a 1×1
        // placeholder (the material_opaque layout always binds `reflection_descriptor`
        // at binding 24, so a resource must exist — but the SSR-off kernel never
        // writes it, so 1×1 is enough and costs no full-res memory).
        //
        // `reflection_descriptor` is written full-res by `material_opaque` and
        // read full-res by the SSR trace, so it stays full-res whenever SSR is
        // on. Only the `ssr` reflection TARGET shrinks to half-res (the trace
        // writes it, the composite step bilinearly upsamples it).
        let (refl_w, refl_h) = if ssr_enabled {
            (render_w, render_h)
        } else {
            (1u32, 1u32)
        };
        let (ssr_w, ssr_h) = if ssr_enabled && ssr_half_res {
            (
                crate::size::half_extent(render_w),
                crate::size::half_extent(render_h),
            )
        } else {
            (refl_w, refl_h)
        };
        let maybe_multisample_texture =
            |format: TextureFormat, label: &'static str| -> TextureDescriptor<'static> {
                let mut descriptor = TextureDescriptor::new(
                    format,
                    Extent3d::new(render_w, Some(render_h), Some(1)),
                    TextureUsage::new()
                        .with_render_attachment()
                        .with_texture_binding(),
                )
                .with_label(label);

                if let Some(sample_count) = anti_aliasing.msaa_sample_count {
                    descriptor = descriptor.with_sample_count(sample_count);
                }

                descriptor
            };

        // 1. Create all textures
        let visibility_data = gpu
            .create_texture(
                &maybe_multisample_texture(
                    render_texture_formats.visiblity_data,
                    "Visibility Data",
                )
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let barycentric = gpu
            .create_texture(
                &maybe_multisample_texture(render_texture_formats.barycentric, "Barycentric")
                    .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let normal_tangent = gpu
            .create_texture(
                &maybe_multisample_texture(render_texture_formats.normal_tangent, "Normal Tangent")
                    .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let barycentric_derivatives = gpu
            .create_texture(
                &maybe_multisample_texture(
                    render_texture_formats.barycentric_derivatives,
                    "Barycentric Derivatives",
                )
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        // TAA motion vectors: geometry-pass target, render size, single-sampled
        // (TAA turns MSAA off). COPY_SRC for the harness readback
        // (`debug_read_velocity`). Zero cost when TAA is off: not allocated.
        // With the prep-pass fallback (03 §2.4) the prep compute pass writes
        // it instead: a storage texture, `rg32float` (`rg16float` is not a
        // core storage format). The resolve reads either the same way.
        let velocity = if anti_aliasing.taa.is_some() {
            let prep = render_texture_formats.taa_prep_velocity(&anti_aliasing);
            Some(
                gpu.create_texture(
                    &TextureDescriptor::new(
                        if prep {
                            crate::render_passes::material_prep::PREP_VELOCITY_FORMAT
                        } else {
                            crate::render_passes::geometry::pipeline::VELOCITY_FORMAT
                        },
                        Extent3d::new(render_w, Some(render_h), Some(1)),
                        if prep {
                            TextureUsage::new().with_storage_binding()
                        } else {
                            TextureUsage::new().with_render_attachment()
                        }
                        .with_texture_binding()
                        .with_copy_src(),
                    )
                    .with_label("Velocity")
                    .into(),
                )
                .map_err(AwsmRenderTextureError::CreateTexture)?,
            )
        } else {
            None
        };
        let velocity_view = match velocity.as_ref() {
            Some(tex) => Some(tex.create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("velocity: {e:?}"))
            })?),
            None => None,
        };

        // TAA reactive mask: the world transparent pass's second target
        // (render size, single-sampled), read by the resolve. COPY_SRC for
        // the harness readback. Zero cost when TAA is off: not allocated.
        let reactive = if anti_aliasing.taa.is_some() {
            Some(
                gpu.create_texture(
                    &TextureDescriptor::new(
                        render_texture_formats.reactive,
                        Extent3d::new(render_w, Some(render_h), Some(1)),
                        TextureUsage::new()
                            .with_render_attachment()
                            .with_texture_binding()
                            .with_copy_src(),
                    )
                    .with_label("TAA Reactive")
                    .into(),
                )
                .map_err(AwsmRenderTextureError::CreateTexture)?,
            )
        } else {
            None
        };
        let reactive_view = match reactive.as_ref() {
            Some(tex) => Some(tex.create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("reactive: {e:?}"))
            })?),
            None => None,
        };

        // NEVER multisampled, used as a storage texture. Allocate a full
        // mip chain so the transparent pass can do hardware-filtered
        // background sampling for rough/refractive materials. Mip 0 is
        // populated by the opaque pass; subsequent mips are filled in by
        // `OpaqueMipgen` between the opaque pass and the transparent pass
        // when the frame uses transmission.
        //
        // RENDER_ATTACHMENT
        // dropped — this texture is never used as a render-pass color
        // attachment. The frame-start clear runs via `TextureClearer::clear`
        // which uses `copy_buffer_to_texture` (needs COPY_DST, not
        // RENDER_ATTACHMENT). On a TBR mobile GPU, RENDER_ATTACHMENT on a
        // storage-bound texture forces the driver to evict its on-chip tile
        // cache across compute pass boundaries — dropping the flag here is
        // the single biggest TBR-friendly change.
        //
        // T2.5: mip chain allocation is now lazy. `needs_opaque_mip_chain`
        // is `Materials::has_seen_transmission` — when no transmissive
        // material has ever been registered, only mip 0 exists (the only
        // level the opaque pass actually writes). The first transmissive
        // material registration flips that flag and reallocates with the
        // full chain on the next `views()` call.
        let opaque_mip_count = if needs_opaque_mip_chain {
            mip_levels_for(render_w, render_h)
        } else {
            1
        };
        let opaque = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(render_w, Some(render_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding()
                        .with_copy_dst(),
                )
                .with_label("Opaque")
                .with_mip_level_count(opaque_mip_count)
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        // maybe multisampled, but a bit differnt since we need to resolve it later
        // and it has copy_dst
        let transparent = {
            // The decal compute pass writes overlaid pixels into this
            // texture after the opaque→transparent blit has primed it
            // with the opaque shading result; it needs
            // `STORAGE_BINDING` to bind the view as a storage texture
            // write. MSAA textures can't be storage-bound, so the
            // usage flag is conditional. (`Decals::write_gpu` CPU-side
            // gates the render-graph slot on MSAA too.)
            let mut usage = TextureUsage::new()
                .with_render_attachment()
                .with_texture_binding()
                .with_copy_dst();
            if anti_aliasing.msaa_sample_count.is_none() {
                usage = usage.with_storage_binding();
            }
            let mut descriptor = TextureDescriptor::new(
                render_texture_formats.color,
                Extent3d::new(render_w, Some(render_h), Some(1)),
                usage,
            )
            .with_label("Transparent");

            if let Some(sample_count) = anti_aliasing.msaa_sample_count {
                descriptor = descriptor.with_sample_count(sample_count);
            }

            gpu.create_texture(&descriptor.into())
                .map_err(AwsmRenderTextureError::CreateTexture)?
        };

        // Single-sample storage-write target the decal compute uses
        // when MSAA is on; the composite step alpha-blits it onto the
        // multisampled transparent target. Gated by `features.decals`
        // — the texture is ~16 MB at 4K, and skipping it is the
        // largest single allocation behind the decals feature flag.
        //
        // T2.3: RENDER_ATTACHMENT + COPY_DST dropped — written via
        // storage by the decal compute pass, read as a texture in the
        // composite. Neither flag has any current consumer. Same TBR
        // tile-cache benefit as `opaque` above.
        let decal_color = if features.decals {
            Some(
                gpu.create_texture(
                    &TextureDescriptor::new(
                        render_texture_formats.color,
                        Extent3d::new(render_w, Some(render_h), Some(1)),
                        TextureUsage::new()
                            .with_storage_binding()
                            .with_texture_binding(),
                    )
                    .with_label("DecalColor")
                    .into(),
                )
                .map_err(AwsmRenderTextureError::CreateTexture)?,
            )
        } else {
            None
        };

        let depth = gpu
            .create_texture(
                &{
                    let mut descriptor =
                        maybe_multisample_texture(render_texture_formats.depth, "Depth");
                    // TAA: depth is also a copy source (last frame's depth for
                    // the resolve's disocclusion test, and the harness's
                    // `debug_read_depth`). Usage flag only, and only with TAA.
                    if anti_aliasing.taa.is_some() {
                        descriptor.usage = descriptor.usage.with_copy_src();
                    }
                    descriptor
                }
                .into(),
            )
            // Keeping the depth buffer bindable allows later passes (e.g. compute shading) to
            // sample it directly for world-position reconstruction.
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let ssr_depth = match anti_aliasing.msaa_sample_count {
            Some(_) => Some(
                gpu.create_texture(
                    &TextureDescriptor::new(
                        render_texture_formats.depth,
                        Extent3d::new(refl_w, Some(refl_h), Some(1)),
                        TextureUsage::new()
                            .with_render_attachment()
                            .with_texture_binding(),
                    )
                    .with_label("SSR Depth")
                    .into(),
                )
                .map_err(AwsmRenderTextureError::CreateTexture)?,
            ),
            None => None,
        };

        // T2.6: HUD depth attachment is allocated only after the first
        // HUD renderable has registered. Saves a full-screen depth
        // texture (Depth32 = 4 bytes/texel, Depth24Plus also 4
        // bytes/texel; ~5 MB at 400×800 mobile, ~33 MB at 4K) on the
        // common library/game builds that never use the HUD pass.
        let hud_depth = if needs_hud_depth {
            Some(
                gpu.create_texture(
                    &maybe_multisample_texture(render_texture_formats.depth, "Hud Depth").into(),
                )
                .map_err(AwsmRenderTextureError::CreateTexture)?,
            )
        } else {
            None
        };
        // TAA: the HUD's lit pass runs after the resolve, at post size,
        // single-sampled (TAA excludes MSAA). Only with a HUD.
        let hud_depth_post = if needs_hud_depth && anti_aliasing.taa.is_some() {
            Some(
                gpu.create_texture(
                    &TextureDescriptor::new(
                        render_texture_formats.depth,
                        Extent3d::new(post_w, Some(post_h), Some(1)),
                        TextureUsage::new().with_render_attachment(),
                    )
                    .with_label("Hud Depth (post)")
                    .into(),
                )
                .map_err(AwsmRenderTextureError::CreateTexture)?,
            )
        } else {
            None
        };

        // NEVER multisampled, that's the point.
        //
        // T2.3: STORAGE_BINDING dropped — composite is only ever bound as
        // a sampled `texture_2d<f32>` (in effects + the
        // material_decal composite + the non-AA blit path), never as a
        // storage texture. RENDER_ATTACHMENT stays — it's the MSAA
        // resolve target for the transparent / HUD passes.
        let composite = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(render_w, Some(render_h), Some(1)),
                    TextureUsage::new()
                        .with_texture_binding()
                        .with_render_attachment(),
                )
                .with_label("Composite")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        // T2.3: RENDER_ATTACHMENT dropped on both `effects` and `bloom`
        // — these are pure compute-pass outputs (storage write from the
        // effects compute pass, texture read in display + effects). No
        // render attachment consumer in any pass.
        let effects = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(post_w, Some(post_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("Effects")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let bloom = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(post_w, Some(post_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("Bloom")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let ssr = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(ssr_w, Some(ssr_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("SSR")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        // SSR spatial-resolve output — same dims + format + usage as the `ssr`
        // trace target (including the SSR-off 1×1 placeholder). The resolve
        // compute storage-writes the denoised reflection here; the composite
        // upsample texture-reads it.
        let ssr_resolved = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(ssr_w, Some(ssr_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("SSR Resolved")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        // SSR temporal targets — same dims + format (RGBA16F) + usage as the
        // `ssr` target above. `ssr_final` is the temporal pass's accumulated
        // output (the composite's source when temporal is on); the history
        // pair is storage-written by the temporal pass + sampled (filtered)
        // as the previous frame. Full-size only when SSR is on AND temporal is
        // on; otherwise 1×1 placeholders (the temporal-off pipeline binds none
        // of them, but the render-texture struct always carries valid resources).
        let (hist_w, hist_h) = if ssr_enabled && ssr_temporal {
            (ssr_w, ssr_h)
        } else {
            (1u32, 1u32)
        };
        let ssr_final = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(hist_w, Some(hist_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("SSR Final")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;
        let make_history = |label: &'static str| -> Result<web_sys::GpuTexture> {
            gpu.create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(hist_w, Some(hist_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label(label)
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)
        };
        let ssr_history = [
            make_history("SSR History 0")?,
            make_history("SSR History 1")?,
        ];

        // Software-BVH hit target — same dims/format/usage as `ssr`; the
        // bvh_trace compute storage-writes it, the SSR trace texture-reads
        // it. Full-size only when SSR is on AND bvh_reflections is on.
        let (bvh_w, bvh_h) = if ssr_enabled && ssr_bvh {
            (ssr_w, ssr_h)
        } else {
            (1u32, 1u32)
        };
        let ssr_bvh_tex = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(bvh_w, Some(bvh_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("SSR BVH")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        // M2a: material-owned SSR reflection descriptor. `material_opaque`
        // storage-writes it per pixel; the SSR pass texture-reads it. Always
        // allocated (a small Rgba8unorm target) — the zero-cost gate lives on
        // the SSR PASS, not this producer-side target.
        let reflection_descriptor = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.reflection_descriptor,
                    Extent3d::new(refl_w, Some(refl_h), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("SSR Reflection Descriptor")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        // 2. Create views for all textures

        let visibility_data_view = visibility_data.create_view().map_err(|e| {
            AwsmRenderTextureError::CreateTextureView(format!("visibility_data: {e:?}"))
        })?;

        let barycentric_view = barycentric.create_view().map_err(|e| {
            AwsmRenderTextureError::CreateTextureView(format!("barycentric: {e:?}"))
        })?;

        // let taa_clip_position_views = [
        //     taa_clip_positions[0].create_view().map_err(|e| {
        //         AwsmRenderTextureError::CreateTextureView(format!("taa_clip_positions[0]: {e:?}"))
        //     })?,
        //     taa_clip_positions[1].create_view().map_err(|e| {
        //         AwsmRenderTextureError::CreateTextureView(format!("taa_clip_positions[1]: {e:?}"))
        //     })?,
        // ];

        let normal_tangent_view = normal_tangent.create_view().map_err(|e| {
            AwsmRenderTextureError::CreateTextureView(format!("normal_tangent: {e:?}"))
        })?;

        let barycentric_derivatives_view = barycentric_derivatives.create_view().map_err(|e| {
            AwsmRenderTextureError::CreateTextureView(format!("barycentric: {e:?}"))
        })?;

        // Storage views must cover a single mip level. The opaque pass
        // writes only mip 0; the full-mip view is used by the transparent
        // pass when sampling for transmission.
        let opaque_storage_view = opaque
            .create_view_with_descriptor(
                &TextureViewDescriptor::new(Some("Opaque (mip 0)"))
                    .with_dimension(TextureViewDimension::N2d)
                    .with_base_mip_level(0)
                    .with_mip_level_count(1)
                    .into(),
            )
            .map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("opaque storage: {e:?}"))
            })?;

        let opaque_full_view = opaque
            .create_view_with_descriptor(
                &TextureViewDescriptor::new(Some("Opaque (full mips)"))
                    .with_dimension(TextureViewDimension::N2d)
                    .with_base_mip_level(0)
                    .with_mip_level_count(opaque_mip_count)
                    .into(),
            )
            .map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("opaque full: {e:?}"))
            })?;

        let decal_color_view = match decal_color.as_ref() {
            Some(tex) => Some(tex.create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("decal_color: {e:?}"))
            })?),
            None => None,
        };

        // Plan B prep-pass outputs (always allocated — the shared prep pass is
        // unconditional): interpolated UV + vertex color, storage-written by
        // the prep compute pass and texture-read by the slim per-material shader.
        // Stage 2a: array textures — one layer per UV / color set. `cs_prep`
        // writes layers `0..min(set_count, cap)`; the slim shader reads
        // `prep_*[set_index]`.
        let prep_uv = Some(
            gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Rg32float,
                    Extent3d::new(
                        render_w,
                        Some(render_h),
                        Some(crate::render_passes::material_prep::MAX_PREP_UV_SETS),
                    ),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("PrepUv")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?,
        );
        let prep_uv_view = match prep_uv.as_ref() {
            Some(tex) => Some(
                tex.create_view_with_descriptor(
                    &TextureViewDescriptor::new(Some("PrepUv"))
                        .with_dimension(TextureViewDimension::N2dArray)
                        .with_array_layer_count(
                            crate::render_passes::material_prep::MAX_PREP_UV_SETS,
                        )
                        .into(),
                )
                .map_err(|e| {
                    AwsmRenderTextureError::CreateTextureView(format!("prep_uv: {e:?}"))
                })?,
            ),
            None => None,
        };
        let prep_vcolor = Some(
            gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Rgba32float,
                    Extent3d::new(
                        render_w,
                        Some(render_h),
                        Some(crate::render_passes::material_prep::MAX_PREP_COLOR_SETS),
                    ),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("PrepVColor")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?,
        );
        let prep_vcolor_view = match prep_vcolor.as_ref() {
            Some(tex) => Some(
                tex.create_view_with_descriptor(
                    &TextureViewDescriptor::new(Some("PrepVColor"))
                        .with_dimension(TextureViewDimension::N2dArray)
                        .with_array_layer_count(
                            crate::render_passes::material_prep::MAX_PREP_COLOR_SETS,
                        )
                        .into(),
                )
                .map_err(|e| {
                    AwsmRenderTextureError::CreateTextureView(format!("prep_vcolor: {e:?}"))
                })?,
            ),
            None => None,
        };
        // Stage 3a: per-pixel shadow-visibility buffer — Rgba8unorm array, 4
        // shadow slots packed per texel (channel = slot % 4, layer = slot / 4).
        // Inert until Stage 3b binds it + cs_prep writes it.
        let prep_shadow_visibility = Some(
            gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Rgba8unorm,
                    Extent3d::new(render_w, Some(render_h), Some(prep_shadow_layers.max(1))),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("PrepShadowVisibility")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?,
        );
        let prep_shadow_visibility_view = match prep_shadow_visibility.as_ref() {
            Some(tex) => Some(
                tex.create_view_with_descriptor(
                    &TextureViewDescriptor::new(Some("PrepShadowVisibility"))
                        .with_dimension(TextureViewDimension::N2dArray)
                        .with_array_layer_count(prep_shadow_layers.max(1))
                        .into(),
                )
                .map_err(|e| {
                    AwsmRenderTextureError::CreateTextureView(format!(
                        "prep_shadow_visibility: {e:?}"
                    ))
                })?,
            ),
            None => None,
        };
        // Ping-pong temp for the optional separable shadow-denoise blur. Same
        // descriptor as `prep_shadow_visibility` (storage write + sampled read).
        // DELIBERATELY always allocated (it shares prep_shadow_visibility's exact
        // lifecycle: resize/AA rebuild only). Denoise defaults ON, so the temp is
        // needed in the common case; making it conditional would couple the cheap
        // live `denoise` toggle to a full render-texture rebuild for no benefit
        // when on. Standing cost when denoise is OFF: 4 bytes/px × ceil(K/4)
        // layers (~8 MB @1080p K≤4, ~133 MB @4K K=16) — revisit only if a
        // high-K, denoise-off configuration proves common.
        let prep_shadow_visibility_blur_tmp = Some(
            gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Rgba8unorm,
                    Extent3d::new(render_w, Some(render_h), Some(prep_shadow_layers.max(1))),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding(),
                )
                .with_label("PrepShadowVisibilityBlurTmp")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?,
        );
        let prep_shadow_visibility_blur_tmp_view = match prep_shadow_visibility_blur_tmp.as_ref() {
            Some(tex) => Some(
                tex.create_view_with_descriptor(
                    &TextureViewDescriptor::new(Some("PrepShadowVisibilityBlurTmp"))
                        .with_dimension(TextureViewDimension::N2dArray)
                        .with_array_layer_count(prep_shadow_layers.max(1))
                        .into(),
                )
                .map_err(|e| {
                    AwsmRenderTextureError::CreateTextureView(format!(
                        "prep_shadow_visibility_blur_tmp: {e:?}"
                    ))
                })?,
            ),
            None => None,
        };

        // U0 (`docs/plans/unified-edge-shading.md`): per-pixel edge-id texture
        // — R32Uint, one word/pixel, NEVER multisampled (one value per pixel,
        // not per sample). Classify binds it as a `texture_storage_2d<r32uint,
        // write>` to write the compact edge_pixel_id / U32_MAX sentinel, so it
        // needs STORAGE_BINDING; TEXTURE_BINDING is added for the future
        // unified kernel's read. Gated on MSAA: the edge-id texture only feeds
        // the MSAA `cs_shade` edge branch, so a no-MSAA build allocates nothing.
        // classify only declares/binds the edge_id texture under MSAA
        // (`emit_edge_data` ≡ MSAA), so this stays in lockstep with the shader.
        let edge_id = if anti_aliasing.msaa_sample_count.is_some() {
            Some(
                gpu.create_texture(
                    &TextureDescriptor::new(
                        TextureFormat::R32uint,
                        Extent3d::new(render_w, Some(render_h), Some(1)),
                        TextureUsage::new()
                            .with_storage_binding()
                            .with_texture_binding(),
                    )
                    .with_label("EdgeId")
                    .into(),
                )
                .map_err(AwsmRenderTextureError::CreateTexture)?,
            )
        } else {
            None
        };
        let edge_id_view = match edge_id.as_ref() {
            Some(tex) => Some(tex.create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("edge_id: {e:?}"))
            })?),
            None => None,
        };

        let transparent_view = transparent.create_view().map_err(|e| {
            AwsmRenderTextureError::CreateTextureView(format!("transparent: {e:?}"))
        })?;

        let depth_view = depth
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("depth: {e:?}")))?;

        let ssr_depth_view = match ssr_depth.as_ref() {
            Some(tex) => Some(tex.create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("ssr_depth: {e:?}"))
            })?),
            None => None,
        };

        let hud_depth_view = match hud_depth.as_ref() {
            Some(tex) => Some(tex.create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("hud_depth: {e:?}"))
            })?),
            None => None,
        };
        let hud_depth_post_view = match hud_depth_post.as_ref() {
            Some(tex) => Some(tex.create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("hud_depth_post: {e:?}"))
            })?),
            None => None,
        };

        let composite_view = composite
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("composite: {e:?}")))?;

        let effects_view = effects
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("effects: {e:?}")))?;

        let bloom_view = bloom
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("bloom: {e:?}")))?;

        let ssr_view = ssr
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("ssr: {e:?}")))?;

        let ssr_resolved_view = ssr_resolved.create_view().map_err(|e| {
            AwsmRenderTextureError::CreateTextureView(format!("ssr_resolved: {e:?}"))
        })?;

        let ssr_final_view = ssr_final
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("ssr_final: {e:?}")))?;

        let ssr_history_views = [
            ssr_history[0].create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("ssr_history[0]: {e:?}"))
            })?,
            ssr_history[1].create_view().map_err(|e| {
                AwsmRenderTextureError::CreateTextureView(format!("ssr_history[1]: {e:?}"))
            })?,
        ];

        let ssr_bvh_view = ssr_bvh_tex
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("ssr_bvh: {e:?}")))?;

        let reflection_descriptor_view = reflection_descriptor.create_view().map_err(|e| {
            AwsmRenderTextureError::CreateTextureView(format!("reflection_descriptor: {e:?}"))
        })?;

        // The blit shader uses `textureLoad(_, _, 0)` against mip 0, so
        // either of the opaque views technically works; bind the storage
        // (mip-0-only) view to keep its intent obvious.
        let opaque_to_transparent_blit_bind_group_msaa_4 = blit_get_bind_group(
            gpu,
            opaque_to_transparent_blit_pipeline_msaa_4,
            &opaque_storage_view,
        );

        let opaque_to_transparent_blit_bind_group_no_anti_alias = blit_get_bind_group(
            gpu,
            opaque_to_transparent_blit_pipeline_no_anti_alias,
            &opaque_storage_view,
        );

        let transparent_to_composite_blit_bind_group_no_anti_alias =
            if anti_aliasing.msaa_sample_count.is_none() {
                Some(blit_get_bind_group(
                    gpu,
                    transparent_to_composite_blit_pipeline_no_anti_alias,
                    &transparent_view,
                ))
            } else {
                None
            };

        Ok(Self {
            visibility_data,
            visibility_data_view,

            barycentric,
            barycentric_view,

            normal_tangent,
            normal_tangent_view,

            barycentric_derivatives,
            barycentric_derivatives_view,

            velocity,
            velocity_view,

            reactive,
            reactive_view,
            prep_velocity: render_texture_formats.taa_prep_velocity(&anti_aliasing),

            opaque,
            opaque_storage_view,
            opaque_full_view,
            opaque_mip_count,
            opaque_has_mip_chain: needs_opaque_mip_chain,
            opaque_clearer: TextureClearer::new(
                gpu,
                render_texture_formats.color,
                render_w,
                render_h,
            )
            .map_err(AwsmRenderTextureError::CreateTextureClearer)?,
            opaque_to_transparent_blit_bind_group_msaa_4,
            opaque_to_transparent_blit_bind_group_no_anti_alias,

            transparent,
            transparent_view,

            decal_color,
            decal_color_view,
            prep_uv,
            prep_uv_view,
            prep_vcolor,
            prep_vcolor_view,
            prep_shadow_visibility,
            prep_shadow_visibility_view,
            prep_shadow_visibility_blur_tmp,
            prep_shadow_visibility_blur_tmp_view,

            edge_id,
            edge_id_view,

            depth,
            depth_view,
            ssr_depth,
            ssr_depth_view,

            hud_depth,
            hud_depth_view,
            hud_depth_post,
            hud_depth_post_view,

            composite,
            composite_view,
            transparent_to_composite_blit_bind_group_no_anti_alias,

            effects,
            effects_view,

            bloom,
            bloom_view,

            ssr,
            ssr_view,
            ssr_resolved,
            ssr_resolved_view,
            ssr_final,
            ssr_final_view,
            ssr_history,
            ssr_history_views,
            ssr_bvh_tex,
            ssr_bvh_view,

            reflection_descriptor,
            reflection_descriptor_view,

            render_size,
            post_size,

            anti_aliasing,
            ssr_enabled,
            ssr_half_res,
            ssr_temporal,
            ssr_bvh,
        })
    }

    /// Destroys all GPU textures.
    pub fn destroy(self) {
        self.visibility_data.destroy();
        self.barycentric.destroy();
        // for texture in self.taa_clip_positions {
        //     texture.destroy();
        // }
        self.normal_tangent.destroy();
        self.barycentric_derivatives.destroy();
        if let Some(tex) = self.velocity {
            tex.destroy();
        }
        if let Some(tex) = self.reactive {
            tex.destroy();
        }
        self.opaque.destroy();
        self.transparent.destroy();
        if let Some(tex) = self.decal_color {
            tex.destroy();
        }
        if let Some(tex) = self.prep_uv {
            tex.destroy();
        }
        if let Some(tex) = self.prep_vcolor {
            tex.destroy();
        }
        if let Some(tex) = self.prep_shadow_visibility {
            tex.destroy();
        }
        if let Some(tex) = self.prep_shadow_visibility_blur_tmp {
            tex.destroy();
        }
        if let Some(tex) = self.edge_id {
            tex.destroy();
        }
        self.depth.destroy();
        if let Some(tex) = self.ssr_depth {
            tex.destroy();
        }
        if let Some(tex) = self.hud_depth {
            tex.destroy();
        }
        if let Some(tex) = self.hud_depth_post {
            tex.destroy();
        }
        self.composite.destroy();
        self.effects.destroy();
        self.bloom.destroy();
        self.ssr.destroy();
        self.ssr_resolved.destroy();
        self.ssr_bvh_tex.destroy();
        self.ssr_final.destroy();
        self.ssr_history[0].destroy();
        self.ssr_history[1].destroy();
        self.reflection_descriptor.destroy();
    }
}

/// Returns the natural mip-chain length for a 2D texture of the given size.
fn mip_levels_for(width: u32, height: u32) -> u32 {
    let max_dim = width.max(height).max(1);
    32 - max_dim.leading_zeros()
}

/// Result type for render texture operations.
type Result<T> = std::result::Result<T, AwsmRenderTextureError>;
/// Render texture related errors.
#[derive(Debug, Error)]
pub enum AwsmRenderTextureError {
    #[error("[render_texture] Error creating texture: {0:?}")]
    CreateTexture(AwsmCoreError),

    #[error("[render_texture] Error creating texture view: {0}")]
    CreateTextureView(String),

    #[error("[render_texture] Error getting current screen size: {0:?}")]
    CurrentScreenSize(AwsmCoreError),

    #[error("[render_texture] Error getting current texture view: {0:?}")]
    CurrentTextureView(AwsmCoreError),

    #[error("[render_texture] Error creating texture clearer: {0:?}")]
    CreateTextureClearer(AwsmCoreError),

    #[error("[render_texture] Error clearing texture: {0:?}")]
    TextureClearerClear(AwsmCoreError),

    #[error("[render_texture] Blit pipeline: {0:?}")]
    BlitPipeline(AwsmCoreError),

    #[error("[render_texture] debug readback unavailable: {0}")]
    DebugReadback(String),
}

/// What a TAA debug readback reads.
#[derive(Clone, Copy)]
enum DebugReadTarget {
    Velocity,
    Depth,
    Reactive,
}

/// `(width, height, values)` of a TAA debug readback: render size, rows top
/// to bottom, tightly packed.
pub type DebugReadback = (u32, u32, Vec<f32>);

impl crate::AwsmRenderer {
    /// TAA test hook: the motion vectors of the LAST rendered frame, as
    /// `(width, height, data)`. Render size; `data` holds `width * height`
    /// `(x, y)` pairs (`2 * width * height` floats), rows top to bottom, in UV
    /// units: `uv_now - uv_prev` of the surface at that pixel (`x` right, `y`
    /// down; multiply by the render size for pixels). Sky / nothing drawn is
    /// `(0, 0)`; a point that was behind last frame's camera is `(2, 2)`.
    ///
    /// Requires TAA on (`anti_aliasing.taa`); errors otherwise. The copy is
    /// submitted synchronously (call it between frames, after `render()`);
    /// only the map + decode is awaited, so the caller can drop its borrow of
    /// the renderer before awaiting. Not for per-frame use (one buffer
    /// allocation + a GPU sync per call).
    pub fn debug_read_velocity(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<DebugReadback>>>>
    {
        self.debug_read(DebugReadTarget::Velocity)
    }

    /// TAA test hook: the raw depth buffer of the LAST rendered frame, same
    /// shape as [`Self::debug_read_velocity`] with one float per pixel (the
    /// depth-buffer value: reverse-Z when `features.reverse_z`, so 0 = far).
    /// Requires TAA on (single-sampled depth with copy usage).
    pub fn debug_read_depth(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<DebugReadback>>>>
    {
        self.debug_read(DebugReadTarget::Depth)
    }

    /// TAA test hook: the reactive mask of the LAST rendered frame (one
    /// 0..1 value per render pixel; exactly 1.0 = an opaque-path HUD pixel),
    /// same shape as [`Self::debug_read_velocity`]. Requires TAA on.
    pub fn debug_read_reactive(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<DebugReadback>>>>
    {
        self.debug_read(DebugReadTarget::Reactive)
    }

    fn debug_read(
        &self,
        target: DebugReadTarget,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<DebugReadback>>>>
    {
        use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
        use awsm_renderer_core::command::copy_texture::{
            TexelCopyBufferInfo, TexelCopyTextureInfo,
        };

        let submitted = (|| -> crate::error::Result<(web_sys::GpuBuffer, u32, u32, u32, u32)> {
            let unavailable = |what: &str| {
                crate::error::AwsmError::from(AwsmRenderTextureError::DebugReadback(format!(
                    "{what}: TAA must be on and a frame rendered"
                )))
            };
            let inner = self
                .render_textures
                .inner()
                .filter(|inner| inner.anti_aliasing.taa.is_some())
                .ok_or_else(|| unavailable("debug readback"))?;
            let texture = match target {
                DebugReadTarget::Velocity => inner
                    .velocity
                    .as_ref()
                    .ok_or_else(|| unavailable("velocity"))?,
                DebugReadTarget::Depth => &inner.depth,
                DebugReadTarget::Reactive => inner
                    .reactive
                    .as_ref()
                    .ok_or_else(|| unavailable("reactive"))?,
            };
            // All targets are render size.
            let (render_w, render_h) = inner.render_size;
            // rg16float and depth32float are 4 bytes per pixel, r8unorm 1.
            let texel_bytes = match target {
                DebugReadTarget::Reactive => 1,
                // The prep-pass fallback's velocity is rg32float.
                DebugReadTarget::Velocity if inner.prep_velocity => 8,
                _ => 4,
            };
            let bytes_per_row = (render_w * texel_bytes).div_ceil(256) * 256;
            let buffer = self.gpu.create_buffer(
                &BufferDescriptor::new(
                    Some("TAA Debug Readback"),
                    (bytes_per_row * render_h) as usize,
                    BufferUsage::new().with_copy_dst().with_map_read(),
                )
                .into(),
            )?;
            let encoder = self.gpu.create_command_encoder(Some("TAA Debug Readback"));
            encoder.copy_texture_to_buffer(
                &TexelCopyTextureInfo::new(texture).into(),
                &TexelCopyBufferInfo::new(&buffer)
                    .with_bytes_per_row(bytes_per_row)
                    .with_rows_per_image(render_h)
                    .into(),
                &Extent3d::new(render_w, Some(render_h), Some(1)).into(),
            )?;
            self.gpu.submit_commands(&encoder.finish());
            Ok((buffer, render_w, render_h, bytes_per_row, texel_bytes))
        })();

        Box::pin(async move {
            let (buffer, width, height, bytes_per_row, texel_bytes) = submitted?;
            let texel_bytes = texel_bytes as usize;
            let bytes = awsm_renderer_core::buffers::extract_buffer_vec(&buffer, None).await?;
            buffer.destroy();
            let mut out = Vec::with_capacity((width * height * 2) as usize);
            for row in 0..height as usize {
                let start = row * bytes_per_row as usize;
                let row_bytes = &bytes[start..start + width as usize * texel_bytes];
                match target {
                    DebugReadTarget::Velocity if texel_bytes == 8 => {
                        for word in row_bytes.chunks_exact(4) {
                            out.push(f32::from_le_bytes([word[0], word[1], word[2], word[3]]));
                        }
                    }
                    DebugReadTarget::Velocity => {
                        for half in row_bytes.chunks_exact(2) {
                            out.push(f16_to_f32(u16::from_le_bytes([half[0], half[1]])));
                        }
                    }
                    DebugReadTarget::Depth => {
                        for word in row_bytes.chunks_exact(4) {
                            out.push(f32::from_le_bytes([word[0], word[1], word[2], word[3]]));
                        }
                    }
                    DebugReadTarget::Reactive => {
                        out.extend(row_bytes.iter().map(|&b| b as f32 / 255.0));
                    }
                }
            }
            Ok((width, height, out))
        })
    }
}

/// IEEE 754 half -> single (exact; subnormals, infinities and NaN included).
fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exponent = ((h >> 10) & 0x1f) as u32;
    let mantissa = (h & 0x3ff) as u32;
    let bits = match (exponent, mantissa) {
        (0, 0) => sign << 31,
        (0, m) => {
            // Subnormal: normalise.
            let mut e: i32 = -14;
            let mut m = m;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            (sign << 31) | (((e + 127) as u32) << 23) | ((m & 0x3ff) << 13)
        }
        (0x1f, m) => (sign << 31) | (0xff << 23) | (m << 13),
        (e, m) => (sign << 31) | ((e + 127 - 15) << 23) | (m << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod debug_readback_tests {
    use super::f16_to_f32;

    #[test]
    fn half_floats_decode_exactly() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x4000), 2.0);
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95);
        // Smallest subnormal and the largest finite value.
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert!(f16_to_f32(0x7c00).is_infinite());
        assert!(f16_to_f32(0x7e00).is_nan());
    }
}
