//! Per-frame box-filter mipmap generator for the opaque render target.
//!
//! Used by the screen-space transmission path so we can ask the GPU for a
//! pre-blurred neighborhood color via a single hardware texture fetch at
//! the right mip level, instead of running a multi-tap Gaussian blur per
//! fragment.

use std::{borrow::Cow, sync::Mutex};

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
        BindGroupLayoutResource, BindGroupResource, StorageTextureAccess,
        StorageTextureBindingLayout, TextureBindingLayout,
    },
    command::{
        compute_pass::{ComputePassDescriptor, ComputePassEncoder},
        CommandEncoder,
    },
    error::AwsmCoreError,
    pipeline::{
        layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
        ComputePipelineDescriptor, ProgrammableStage,
    },
    renderer::AwsmRendererWebGpu,
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    texture::{TextureFormat, TextureSampleType, TextureViewDescriptor, TextureViewDimension},
};
use thiserror::Error;

const SHADER_SOURCE: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_size = textureDimensions(dst);
    if (gid.x >= dst_size.x || gid.y >= dst_size.y) { return; }
    let coord = vec2<i32>(gid.xy);
    let src_size = vec2<i32>(textureDimensions(src));
    let src_max = src_size - vec2<i32>(1);
    let base = coord * 2;
    let s00 = textureLoad(src, clamp(base + vec2<i32>(0, 0), vec2<i32>(0), src_max), 0);
    let s10 = textureLoad(src, clamp(base + vec2<i32>(1, 0), vec2<i32>(0), src_max), 0);
    let s01 = textureLoad(src, clamp(base + vec2<i32>(0, 1), vec2<i32>(0), src_max), 0);
    let s11 = textureLoad(src, clamp(base + vec2<i32>(1, 1), vec2<i32>(0), src_max), 0);
    textureStore(dst, coord, (s00 + s10 + s01 + s11) * 0.25);
}
"#;

/// Compute pipeline + bind-group layout for the opaque-RT mipgen.
///
/// One instance is built at renderer construction time and reused every
/// frame. Per-mip texture views and bind groups are built once per
/// (width, height, mip_count) combination and cached — they only need
/// to be rebuilt when the viewport resizes (or AA changes, which
/// reallocates the opaque texture). The pipeline itself never changes.
pub struct OpaqueMipgen {
    pipeline: web_sys::GpuComputePipeline,
    bind_group_layout: web_sys::GpuBindGroupLayout,
    // Interior-mutable so `record` can stay `&self`. The frame's
    // `renderables` borrow holds an immutable handle on `AwsmRenderer`,
    // so the mipgen is called while parts of `self` are already borrowed
    // — a `&mut self` here would force a lot of unrelated restructuring
    // for what is effectively a private cache.
    //
    // `Mutex` (not `RefCell`) for renderer-wide consistency — every
    // owned interior-mutability slot in the renderer uses `Mutex`/
    // atomics so the convention stays uniform regardless of whether
    // a given container actually gets `Sync`. (`MipgenCache` holds
    // `web_sys::GpuBindGroup`, which is `!Send`, so the `Mutex` here
    // doesn't *grant* `Sync` to `OpaqueMipgen`; the inner types
    // would have to become Send first.) The lock is one atomic CAS
    // and is uncontested.
    cache: Mutex<Option<MipgenCache>>,
}

/// Cached per-(width, height, mip_count) work for the mipgen pass. The
/// entries hold the bind groups, which transitively pin the per-mip
/// texture views, so we don't need to also keep view handles around.
struct MipgenCache {
    width: u32,
    height: u32,
    mip_count: u32,
    entries: Vec<MipgenCacheEntry>,
}

struct MipgenCacheEntry {
    dst_width: u32,
    dst_height: u32,
    bind_group: web_sys::GpuBindGroup,
}

impl OpaqueMipgen {
    /// Builds the pipeline. Async because shader/pipeline construction is.
    pub async fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let shader_module = gpu.compile_shader(
            &ShaderModuleDescriptor::new(SHADER_SOURCE, Some("Opaque Mipgen Shader")).into(),
        );
        shader_module.validate_shader().await?;

        let bind_group_layout = gpu.create_bind_group_layout(
            &BindGroupLayoutDescriptor::new(Some("Opaque Mipgen Layout"))
                .with_entries(vec![
                    BindGroupLayoutEntry::new(
                        0,
                        BindGroupLayoutResource::Texture(
                            TextureBindingLayout::new()
                                .with_sample_type(TextureSampleType::UnfilterableFloat)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_multisampled(false),
                        ),
                    )
                    .with_visibility_compute(),
                    BindGroupLayoutEntry::new(
                        1,
                        BindGroupLayoutResource::StorageTexture(
                            StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_access(StorageTextureAccess::WriteOnly),
                        ),
                    )
                    .with_visibility_compute(),
                ])
                .into(),
        )?;

        let pipeline_layout = gpu.create_pipeline_layout(
            &PipelineLayoutDescriptor::new(
                Some("Opaque Mipgen Pipeline Layout"),
                vec![bind_group_layout.clone()],
            )
            .into(),
        );

        let compute = ProgrammableStage::new(&shader_module, None);
        let pipeline = gpu
            .create_compute_pipeline(
                &ComputePipelineDescriptor::new(
                    compute,
                    PipelineLayoutKind::Custom(&pipeline_layout),
                    Some("Opaque Mipgen Pipeline"),
                )
                .into(),
            )
            .await?;

        Ok(Self {
            pipeline,
            bind_group_layout,
            cache: Mutex::new(None),
        })
    }

    /// Drops the cached views/bind groups. Call this when the opaque
    /// texture is recreated (viewport resize, AA change) — the next
    /// `record` will rebuild against the new texture.
    pub fn invalidate(&self) {
        *self.cache.lock().unwrap() = None;
    }

    /// True when the cached per-mip bind groups need (re)building for this
    /// `(width, height, mip_count)` — nothing is cached yet, `invalidate()`
    /// cleared it (opaque-texture identity changed at the same size, e.g. an
    /// AA flip), or the viewport dimensions / mip count drifted.
    ///
    /// The render loop calls this in the early (pre-`recreate`) phase and
    /// fires `BindGroupCreate::OpaqueMipgen` when it returns true; the build
    /// itself then runs in `recreate` via [`Self::rebuild`]. So — like every
    /// other bind group in the renderer — this pass's creation only ever
    /// flows through the central `mark_create` → `recreate` ledger, never
    /// inline on the per-frame path.
    pub fn needs_rebuild(&self, width: u32, height: u32, mip_count: u32) -> bool {
        match self.cache.lock().unwrap().as_ref() {
            Some(cache) => {
                cache.width != width || cache.height != height || cache.mip_count != mip_count
            }
            None => true,
        }
    }

    /// (Re)builds the cached per-mip views + bind groups against
    /// `opaque_texture`. Dispatched from `BindGroups::recreate` in response to
    /// `BindGroupCreate::OpaqueMipgen`. Interior-mutable (`&self`) so it shares
    /// [`Self::record`]'s borrow discipline.
    pub fn rebuild(
        &self,
        gpu: &AwsmRendererWebGpu,
        opaque_texture: &web_sys::GpuTexture,
        mip_count: u32,
    ) -> Result<()> {
        let built = self.build_cache(gpu, opaque_texture, mip_count)?;
        *self.cache.lock().unwrap() = Some(built);
        Ok(())
    }

    /// Records the cached mipgen compute passes (mips `1..mip_count` filled
    /// from mip 0 of the opaque RT). The base mip is assumed to already hold
    /// the just-rendered opaque output.
    ///
    /// Dispatch-only: the bind groups were built ahead of time by
    /// [`Self::rebuild`] via the `mark_create` → `recreate` path, so nothing
    /// is created on the per-frame path here. No-op until the cache exists
    /// (mip chain inactive, i.e. `mip_count < 2`, or the frame transmission
    /// first appears — the cache lands next frame).
    pub fn record(&self, encoder: &CommandEncoder) -> Result<()> {
        let cache_ref = self.cache.lock().unwrap();
        let Some(cache) = cache_ref.as_ref() else {
            return Ok(());
        };

        for entry in &cache.entries {
            let descriptor: web_sys::GpuComputePassDescriptor =
                ComputePassDescriptor::new(Some("Opaque Mipgen Pass")).into();
            let pass: ComputePassEncoder = encoder.begin_compute_pass(Some(&descriptor));
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &entry.bind_group, None)?;
            pass.dispatch_workgroups(
                entry.dst_width.div_ceil(8),
                Some(entry.dst_height.div_ceil(8)),
                None,
            );
            pass.end();
        }

        Ok(())
    }

    fn build_cache(
        &self,
        gpu: &AwsmRendererWebGpu,
        opaque_texture: &web_sys::GpuTexture,
        mip_count: u32,
    ) -> Result<MipgenCache> {
        let width = opaque_texture.width();
        let height = opaque_texture.height();

        let mut entries = Vec::with_capacity((mip_count - 1) as usize);
        let mut src_width = width;
        let mut src_height = height;

        for mip in 1..mip_count {
            let src_view = opaque_texture
                .create_view_with_descriptor(
                    &TextureViewDescriptor::new(Some("Opaque Mipgen Src"))
                        .with_dimension(TextureViewDimension::N2d)
                        .with_base_mip_level(mip - 1)
                        .with_mip_level_count(1)
                        .into(),
                )
                .map_err(AwsmCoreError::create_texture_view)?;

            let dst_view = opaque_texture
                .create_view_with_descriptor(
                    &TextureViewDescriptor::new(Some("Opaque Mipgen Dst"))
                        .with_dimension(TextureViewDimension::N2d)
                        .with_base_mip_level(mip)
                        .with_mip_level_count(1)
                        .into(),
                )
                .map_err(AwsmCoreError::create_texture_view)?;

            let bind_group = gpu.create_bind_group(
                &BindGroupDescriptor::new(
                    &self.bind_group_layout,
                    Some("Opaque Mipgen Bind Group"),
                    vec![
                        BindGroupEntry::new(
                            0,
                            BindGroupResource::TextureView(Cow::Borrowed(&src_view)),
                        ),
                        BindGroupEntry::new(
                            1,
                            BindGroupResource::TextureView(Cow::Borrowed(&dst_view)),
                        ),
                    ],
                )
                .into(),
            );

            let dst_width = (src_width / 2).max(1);
            let dst_height = (src_height / 2).max(1);

            entries.push(MipgenCacheEntry {
                dst_width,
                dst_height,
                bind_group,
            });

            src_width = dst_width;
            src_height = dst_height;
        }

        Ok(MipgenCache {
            width,
            height,
            mip_count,
            entries,
        })
    }
}

#[derive(Debug, Error)]
pub enum AwsmOpaqueMipgenError {
    #[error("[opaque mipgen] core: {0:?}")]
    Core(#[from] AwsmCoreError),
}

type Result<T> = std::result::Result<T, AwsmOpaqueMipgenError>;
