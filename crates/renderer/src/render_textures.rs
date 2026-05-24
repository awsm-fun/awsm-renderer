//! Render texture allocation and management.

use awsm_renderer_core::{
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
    texture::{
        blit::{blit_get_bind_group, blit_get_pipeline, BlitPipeline},
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
    pub opaque_to_transparent_blit_pipeline_no_anti_alias: BlitPipeline,
    pub transparent_to_composite_blit_pipeline_no_anti_alias: BlitPipeline,
    /// Feature gates picked at construction time. Threaded through to
    /// [`RenderTexturesInner`] so the gated `decal_color` allocation
    /// can be skipped when `features.decals == false`.
    features: RendererFeatures,
    frame_count: u32,
    inner: Option<RenderTexturesInner>,
}

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

    // output from display pass is whatever current gpu texture format is

    // For depth testing and transparency
    pub depth: TextureFormat,
    // note - output from the composite pass will be whatever the gpu texture format is
}

impl RenderTextureFormats {
    /// Chooses default render texture formats for the device.
    pub async fn new(_device: &web_sys::GpuDevice) -> Self {
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
            depth: TextureFormat::Depth32float, // More precision for thin/close surfaces
        }
    }
}

impl RenderTextures {
    /// Creates render texture managers and blit pipelines.
    pub async fn new(
        gpu: &AwsmRendererWebGpu,
        formats: RenderTextureFormats,
        features: &RendererFeatures,
    ) -> Result<Self> {
        // Two distinct blit pipeline variants: the `None` (single-
        // sample) variant is used by *both* the opaque→transparent
        // and the transparent→composite blits — they share the same
        // shader + color target + (no) MSAA, so a single compiled
        // GpuRenderPipeline is reused. The `Some(4)` (MSAA-4) variant
        // is used by the opaque→transparent blit only. Compile both
        // variants concurrently; the `None` result is cloned into
        // the two destination fields (wasm-bindgen JS handle clone =
        // refcount bump, not a pipeline copy).
        //
        // Earlier shape compiled the `None` variant twice inside one
        // `try_join3` — `blit_get_pipeline` has no in-flight dedupe,
        // so the two futures genuinely did the same work twice. Fixed
        // here by compiling each variant exactly once.
        let (single_sample_pipeline, msaa_4_pipeline) = futures::future::try_join(
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
        )
        .await?;

        Ok(Self {
            formats,
            features: features.clone(),
            frame_count: 0,
            inner: None,
            opaque_to_transparent_blit_pipeline_msaa_4: msaa_4_pipeline,
            opaque_to_transparent_blit_pipeline_no_anti_alias: single_sample_pipeline.clone(),
            transparent_to_composite_blit_pipeline_no_anti_alias: single_sample_pipeline,
        })
    }

    /// Advances the internal frame counter.
    pub fn next_frame(&mut self) {
        self.frame_count = self.frame_count.wrapping_add(1);
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
    pub fn views(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        anti_aliasing: AntiAliasing,
    ) -> Result<RenderTextureViews> {
        let current_size = gpu
            .current_context_texture_size()
            .map_err(AwsmRenderTextureError::CurrentScreenSize)?;

        let size_changed = match self.inner.as_ref() {
            Some(inner) => (inner.width, inner.height) != current_size,
            None => true,
        };

        let anti_aliasing_changed = match self.inner.as_ref() {
            Some(inner) => inner.anti_aliasing != anti_aliasing,
            None => false,
        };

        if size_changed || anti_aliasing_changed {
            if let Some(inner) = self.inner.take() {
                inner.destroy();
            }

            let inner = RenderTexturesInner::new(
                gpu,
                self.formats.clone(),
                &self.opaque_to_transparent_blit_pipeline_msaa_4,
                &self.opaque_to_transparent_blit_pipeline_no_anti_alias,
                &self.transparent_to_composite_blit_pipeline_no_anti_alias,
                current_size.0,
                current_size.1,
                anti_aliasing,
                &self.features,
            )?;
            self.inner = Some(inner);
        }

        Ok(RenderTextureViews::new(
            self.inner.as_ref().unwrap(),
            self.ping_pong(),
            current_size.0,
            current_size.1,
            size_changed,
        ))
    }

    /// Borrows the inner textures (if initialized). Used by per-frame
    /// passes that need direct GPU texture handles (e.g. mip generation).
    pub fn inner(&self) -> Option<&RenderTexturesInner> {
        self.inner.as_ref()
    }

    /// Clears the opaque render texture when initialized.
    pub fn clear_opaque(&self, gpu: &AwsmRendererWebGpu) -> Result<()> {
        if let Some(inner) = self.inner.as_ref() {
            inner
                .opaque_clearer
                .clear(gpu, &inner.opaque)
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

    // Output from composite pass
    pub composite: web_sys::GpuTextureView,
    pub transparent_to_composite_blit_bind_group_no_anti_alias: Option<web_sys::GpuBindGroup>,

    // Output from effects pass
    pub effects: web_sys::GpuTextureView,
    pub bloom: web_sys::GpuTextureView,

    pub depth: web_sys::GpuTextureView,
    pub hud_depth: web_sys::GpuTextureView,
    pub size_changed: bool,
    pub width: u32,
    pub height: u32,
    pub curr_index: usize,
    pub prev_index: usize,
}

impl RenderTextureViews {
    /// Builds view handles for the current render textures.
    pub fn new(
        inner: &RenderTexturesInner,
        ping_pong: bool,
        width: u32,
        height: u32,
        size_changed: bool,
    ) -> Self {
        let curr_index = if ping_pong { 0 } else { 1 };
        let prev_index = if ping_pong { 1 } else { 0 };
        Self {
            visibility_data: inner.visibility_data_view.clone(),
            barycentric: inner.barycentric_view.clone(),
            normal_tangent: inner.normal_tangent_view.clone(),
            barycentric_derivatives: inner.barycentric_derivatives_view.clone(),
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
            depth: inner.depth_view.clone(),
            hud_depth: inner.hud_depth_view.clone(),
            effects: inner.effects_view.clone(),
            bloom: inner.bloom_view.clone(),
            composite: inner.composite_view.clone(),
            size_changed,
            curr_index,
            prev_index,
            width,
            height,
        }
    }
}

/// Internal texture storage and GPU objects.
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

    pub opaque: web_sys::GpuTexture,
    pub opaque_clearer: TextureClearer,
    /// Storage-binding-friendly mip-0 view; also what the blit shaders
    /// sample from. WebGPU requires storage views to cover exactly one
    /// mip level, so we keep this separate from the full-mip view below.
    pub opaque_storage_view: web_sys::GpuTextureView,
    pub opaque_full_view: web_sys::GpuTextureView,
    pub opaque_mip_count: u32,
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

    pub depth: web_sys::GpuTexture,
    pub depth_view: web_sys::GpuTextureView,

    pub hud_depth: web_sys::GpuTexture,
    pub hud_depth_view: web_sys::GpuTextureView,

    pub composite: web_sys::GpuTexture,
    pub composite_view: web_sys::GpuTextureView,
    pub transparent_to_composite_blit_bind_group_no_anti_alias: Option<web_sys::GpuBindGroup>,

    pub effects: web_sys::GpuTexture,
    pub effects_view: web_sys::GpuTextureView,

    pub bloom: web_sys::GpuTexture,
    pub bloom_view: web_sys::GpuTextureView,

    pub width: u32,
    pub height: u32,

    pub anti_aliasing: AntiAliasing,
}

impl RenderTexturesInner {
    /// Creates render textures and views for the given size.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        render_texture_formats: RenderTextureFormats,
        opaque_to_transparent_blit_pipeline_msaa_4: &BlitPipeline,
        opaque_to_transparent_blit_pipeline_no_anti_alias: &BlitPipeline,
        transparent_to_composite_blit_pipeline_no_anti_alias: &BlitPipeline,
        width: u32,
        height: u32,
        anti_aliasing: AntiAliasing,
        features: &RendererFeatures,
    ) -> Result<Self> {
        let maybe_multisample_texture =
            |format: TextureFormat, label: &'static str| -> TextureDescriptor<'static> {
                let mut descriptor = TextureDescriptor::new(
                    format,
                    Extent3d::new(width, Some(height), Some(1)),
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

        // NEVER multisampled, used as a storage texture. Allocate a full
        // mip chain so the transparent pass can do hardware-filtered
        // background sampling for rough/refractive materials. Mip 0 is
        // populated by the opaque pass; subsequent mips are filled in by
        // `OpaqueMipgen` between the opaque pass and the transparent pass
        // when the frame uses transmission.
        let opaque_mip_count = mip_levels_for(width, height);
        let opaque = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(width, Some(height), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding()
                        .with_render_attachment()
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
                Extent3d::new(width, Some(height), Some(1)),
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
        let decal_color = if features.decals {
            Some(
                gpu.create_texture(
                    &TextureDescriptor::new(
                        render_texture_formats.color,
                        Extent3d::new(width, Some(height), Some(1)),
                        TextureUsage::new()
                            .with_storage_binding()
                            .with_texture_binding()
                            .with_render_attachment()
                            .with_copy_dst(),
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
                &maybe_multisample_texture(render_texture_formats.depth, "Depth").into(),
            )
            // Keeping the depth buffer bindable allows later passes (e.g. compute shading) to
            // sample it directly for world-position reconstruction.
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let hud_depth = gpu
            .create_texture(
                &maybe_multisample_texture(render_texture_formats.depth, "Hud Depth").into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        // NEVER multisampled, that's the point
        let composite = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(width, Some(height), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding()
                        .with_render_attachment(),
                )
                .with_label("Composite")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let effects = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(width, Some(height), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding()
                        .with_render_attachment(),
                )
                .with_label("Effects")
                .into(),
            )
            .map_err(AwsmRenderTextureError::CreateTexture)?;

        let bloom = gpu
            .create_texture(
                &TextureDescriptor::new(
                    render_texture_formats.color,
                    Extent3d::new(width, Some(height), Some(1)),
                    TextureUsage::new()
                        .with_storage_binding()
                        .with_texture_binding()
                        .with_render_attachment(),
                )
                .with_label("Bloom")
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

        let transparent_view = transparent.create_view().map_err(|e| {
            AwsmRenderTextureError::CreateTextureView(format!("transparent: {e:?}"))
        })?;

        let depth_view = depth
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("depth: {e:?}")))?;

        let hud_depth_view = hud_depth
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("hud_depth: {e:?}")))?;

        let composite_view = composite
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("composite: {e:?}")))?;

        let effects_view = effects
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("effects: {e:?}")))?;

        let bloom_view = bloom
            .create_view()
            .map_err(|e| AwsmRenderTextureError::CreateTextureView(format!("bloom: {e:?}")))?;

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

            opaque,
            opaque_storage_view,
            opaque_full_view,
            opaque_mip_count,
            opaque_clearer: TextureClearer::new(gpu, render_texture_formats.color, width, height)
                .map_err(AwsmRenderTextureError::CreateTextureClearer)?,
            opaque_to_transparent_blit_bind_group_msaa_4,
            opaque_to_transparent_blit_bind_group_no_anti_alias,

            transparent,
            transparent_view,

            decal_color,
            decal_color_view,

            depth,
            depth_view,

            hud_depth,
            hud_depth_view,

            composite,
            composite_view,
            transparent_to_composite_blit_bind_group_no_anti_alias,

            effects,
            effects_view,

            bloom,
            bloom_view,

            width,
            height,

            anti_aliasing,
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
        self.opaque.destroy();
        self.transparent.destroy();
        if let Some(tex) = self.decal_color {
            tex.destroy();
        }
        self.depth.destroy();
        self.composite.destroy();
        self.effects.destroy();
        self.bloom.destroy();
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
}
