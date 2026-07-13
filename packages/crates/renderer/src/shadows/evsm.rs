//! EVSM (Exponential Variance Shadow Maps) — moment-write compute
//! pass + separable Gaussian blur.
//!
//! For each EVSM-flagged cascade the pipeline:
//!
//! 1. Renders depth into `shadow_atlas` at the cascade's PCF rect via
//!    the standard shadow generation pass (no change there).
//! 2. Runs `cs_moments` to read the depth rect and write four
//!    exponential moments into `evsm_atlas` at the cascade's EVSM rect:
//!    `vec4(exp(c·z), exp(c·z)², -exp(-c·z), exp(-c·z)²)` packed in
//!    `.rgba`.
//! 3. Runs `cs_blur_h` then `cs_blur_v` — a separable Gaussian over
//!    the moment rect, using `evsm_blur_pingpong_texture` as the
//!    intermediate. Half-width is `config.evsm_blur_radius` clamped to
//!    `MAX_BLUR_RADIUS`.
//!
//! Receivers sample the final moments with a single bilinear fetch and
//! a Chebyshev visibility reconstruction (`sample_shadow_evsm` in
//! `shared_wgsl/shadow/bind_groups.wgsl`).
//!
//! The depth → moment remap uses `z' = 2·z − 1` (forward-Z; reverse-Z
//! uses the mirrored `z' = 1 − 2·z` — see `moment_write_wgsl`) so the
//! exponent space is symmetric around 0 — keeps `exp(c·z)` numerically tight in
//! `RGBA16F` (default `c = 20` → endpoints at `±exp(20) ≈ ±5·10⁸`, the
//! bigger end of the half-float range; lower `c` if you see moment
//! overflow on highly-contrasted depth ranges).

use std::sync::OnceLock;

use awsm_renderer_core::{
    bind_groups::{
        BindGroupLayoutResource, BufferBindingLayout, BufferBindingType, StorageTextureAccess,
        StorageTextureBindingLayout, TextureBindingLayout,
    },
    buffers::{BufferDescriptor, BufferUsage},
    renderer::AwsmRendererWebGpu,
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    texture::{TextureFormat, TextureSampleType, TextureViewDimension},
};

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey, BindGroupLayouts,
    },
    pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts},
    pipelines::{
        compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey},
        Pipelines,
    },
    shaders::{ShaderKey, Shaders},
    shadows::AwsmShadowError,
};

/// Maximum Gaussian half-width supported by the blur compute shaders.
/// The WGSL kernel array is sized to `MAX_BLUR_RADIUS + 1` entries
/// (centre tap + N side taps); each tap is a 4-channel texel fetch, so
/// keeping this modest matters for perf on mobile. 8 covers the AAA
/// default range (config defaults to 3) with headroom.
pub const MAX_BLUR_RADIUS: u32 = 8;

/// Stride for the per-cascade param slot in the uniform buffer. WebGPU
/// requires dynamic uniform offsets to be multiples of
/// `minUniformBufferOffsetAlignment` (256 B on every adapter we
/// target).
pub const EVSM_PARAMS_STRIDE: usize = 256;

/// Hard cap on simultaneously-active EVSM cascades per frame. Matches
/// `MAX_SHADOW_DESCRIPTORS / 2` headroom — in practice the cutoff only
/// promotes the last 1–2 of a 4-cascade light.
pub const MAX_EVSM_CASCADES_PER_FRAME: usize = 16;

/// Owns the EVSM moment-write + blur compute pipelines, bind-group
/// layouts, and per-cascade uniform buffer. Built once at
/// `Shadows::new`; pipelines are global state, not per-frame.
pub struct EvsmPass {
    /// Bind-group layout for the moment-write compute. Bindings:
    /// 0=shadow_atlas (depth, read), 1=evsm_atlas (storage, write),
    /// 2=params (uniform, dynamic offset).
    pub moment_write_layout_key: BindGroupLayoutKey,
    /// Bind-group layout for a single Gaussian blur half-pass.
    /// Bindings: 0=src (RGBA16F, read), 1=dst (storage, write),
    /// 2=params (uniform, dynamic offset).
    pub blur_layout_key: BindGroupLayoutKey,
    /// Pipeline layout for the moment-write compute.
    pub moment_write_pipeline_layout_key: PipelineLayoutKey,
    /// Pipeline layout for either blur half-pass.
    pub blur_pipeline_layout_key: PipelineLayoutKey,
    /// Compute pipeline that reads depth, writes 4 moments. `None`
    /// until [`crate::shadows::Shadows::ensure_pipelines_compiled`]
    /// runs — deferred until the first shadow-casting light is added.
    pub moment_write_pipeline_key: Option<ComputePipelineKey>,
    /// Horizontal Gaussian blur half-pass. See `moment_write_pipeline_key`
    /// for deferred-compile semantics.
    pub blur_h_pipeline_key: Option<ComputePipelineKey>,
    /// Vertical Gaussian blur half-pass. See `moment_write_pipeline_key`
    /// for deferred-compile semantics.
    pub blur_v_pipeline_key: Option<ComputePipelineKey>,
    /// Per-cascade params uniform buffer.
    pub params_buffer: web_sys::GpuBuffer,
    /// CPU staging for `params_buffer`, re-uploaded once per frame.
    pub params_bytes: Vec<u8>,
    /// Number of cascade slots written this frame.
    pub active_cascade_count: u32,
}

/// Pre-resolved layouts + module handles for the EVSM compute pass.
/// Returned by [`EvsmPass::build_descriptors`] and consumed by
/// [`EvsmPass::from_resolved`]. The 3 inline WGSL shaders are issued
/// synchronously via `compile_shader`; their validate futures are
/// surfaced separately so the orchestrator can `try_join_all` them in
/// parallel with the cross-tail `Shaders::ensure_keys` batch.
pub struct EvsmDescriptors {
    pub moment_write_layout_key: BindGroupLayoutKey,
    pub blur_layout_key: BindGroupLayoutKey,
    pub moment_write_pipeline_layout_key: PipelineLayoutKey,
    pub blur_pipeline_layout_key: PipelineLayoutKey,
    /// 3 module handles in moment_write → blur_h → blur_v order. The
    /// orchestrator registers them into the shader cache via
    /// `Shaders::insert_uncached` after the validate join completes,
    /// and feeds the resulting `ShaderKey`s into
    /// [`EvsmDescriptors::pipeline_cache_keys`] to build the 3 compute
    /// pipeline cache keys it pools into the cross-tail batch.
    pub modules: [web_sys::GpuShaderModule; 3],
    pub params_buffer: web_sys::GpuBuffer,
    pub params_bytes: Vec<u8>,
}

impl EvsmDescriptors {
    /// Returns the 3 unawaited `validate_shader` futures for the inline
    /// EVSM shaders. The orchestrator joins them in parallel with the
    /// cross-tail `Shaders::ensure_keys` batch.
    pub fn validate_shader_futures(
        &self,
    ) -> [impl std::future::Future<Output = Result<(), awsm_renderer_core::error::AwsmCoreError>> + '_;
           3] {
        [
            self.modules[0].validate_shader(),
            self.modules[1].validate_shader(),
            self.modules[2].validate_shader(),
        ]
    }

    /// Once the orchestrator has registered the 3 modules into the
    /// shader cache via `Shaders::insert_uncached`, derive the 3
    /// compute pipeline cache keys to fold into the cross-tail batch.
    /// `shader_keys` is in the same order as `modules`.
    pub fn pipeline_cache_keys(&self, shader_keys: [ShaderKey; 3]) -> Vec<ComputePipelineCacheKey> {
        vec![
            ComputePipelineCacheKey::new(shader_keys[0], self.moment_write_pipeline_layout_key),
            ComputePipelineCacheKey::new(shader_keys[1], self.blur_pipeline_layout_key),
            ComputePipelineCacheKey::new(shader_keys[2], self.blur_pipeline_layout_key),
        ]
    }
}

impl EvsmPass {
    /// Builds layouts, compiles shaders, creates pipelines, and
    /// allocates the params uniform buffer. Thin wrapper over the
    /// split that runs the validate + ensure_keys awaits internally;
    /// the cross-tail pool path bypasses this and feeds the descriptor
    /// pieces into shared batches.
    pub async fn new(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        pipelines: &mut Pipelines,
        shaders: &mut Shaders,
        depth: crate::depth_convention::DepthConvention,
    ) -> Result<Self, AwsmShadowError> {
        let descs = Self::build_descriptors(gpu, bind_group_layouts, pipeline_layouts, depth)?;

        // Join the 3 inline-shader validations in parallel.
        let validation_results = futures::future::join_all(descs.validate_shader_futures()).await;
        for result in validation_results {
            result.map_err(AwsmShadowError::Core)?;
        }
        let shader_keys: [ShaderKey; 3] = [
            shaders.insert_uncached(descs.modules[0].clone()),
            shaders.insert_uncached(descs.modules[1].clone()),
            shaders.insert_uncached(descs.modules[2].clone()),
        ];
        let pipeline_cache_keys = descs.pipeline_cache_keys(shader_keys);

        let resolved = pipelines
            .compute
            .ensure_keys(gpu, shaders, pipeline_layouts, pipeline_cache_keys)
            .await?;

        Ok(Self::from_resolved(descs, resolved))
    }

    /// Sync. Builds the 2 bind-group layouts, the 2 pipeline layouts,
    /// the params uniform buffer, and issues `compile_shader` for the
    /// 3 inline WGSL shaders. Returns the modules + the params buffer
    /// in [`EvsmDescriptors`] for the orchestrator to interleave with
    /// the cross-tail shader / pipeline batches.
    pub fn build_descriptors(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        depth: crate::depth_convention::DepthConvention,
    ) -> Result<EvsmDescriptors, AwsmShadowError> {
        // ── moment-write layout ───────────────────────────────────────
        let moment_write_layout_key = bind_group_layouts.get_key(
            gpu,
            BindGroupLayoutCacheKey::new(vec![
                BindGroupLayoutCacheKeyEntry {
                    // Source depth lives in the cascade-array (one
                    // layer per cascade). The compute shader picks
                    // the layer via `params.cascade_layer`.
                    resource: BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_sample_type(TextureSampleType::Depth)
                            .with_view_dimension(TextureViewDimension::N2dArray),
                    ),
                    visibility_vertex: false,
                    visibility_fragment: false,
                    visibility_compute: true,
                },
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::StorageTexture(
                        StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                            .with_access(StorageTextureAccess::WriteOnly)
                            .with_view_dimension(TextureViewDimension::N2d),
                    ),
                    visibility_vertex: false,
                    visibility_fragment: false,
                    visibility_compute: true,
                },
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::Uniform)
                            .with_dynamic_offset(true),
                    ),
                    visibility_vertex: false,
                    visibility_fragment: false,
                    visibility_compute: true,
                },
            ]),
        )?;

        let moment_write_pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![moment_write_layout_key]),
        )?;

        // ── blur layout ────────────────────────────────────────────────
        let blur_layout_key = bind_group_layouts.get_key(
            gpu,
            BindGroupLayoutCacheKey::new(vec![
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_sample_type(TextureSampleType::Float)
                            .with_view_dimension(TextureViewDimension::N2d),
                    ),
                    visibility_vertex: false,
                    visibility_fragment: false,
                    visibility_compute: true,
                },
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::StorageTexture(
                        StorageTextureBindingLayout::new(TextureFormat::Rgba16float)
                            .with_access(StorageTextureAccess::WriteOnly)
                            .with_view_dimension(TextureViewDimension::N2d),
                    ),
                    visibility_vertex: false,
                    visibility_fragment: false,
                    visibility_compute: true,
                },
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::Uniform)
                            .with_dynamic_offset(true),
                    ),
                    visibility_vertex: false,
                    visibility_fragment: false,
                    visibility_compute: true,
                },
            ]),
        )?;

        let blur_pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![blur_layout_key]),
        )?;

        // Issue all 3 inline `compile_shader` calls synchronously. The
        // validate futures are exposed via
        // `EvsmDescriptors::validate_shader_futures` so the
        // orchestrator can interleave them with the cross-tail shader
        // ensure_keys.
        let moment_write_src = moment_write_wgsl(depth);
        let inline_specs: [(&str, &str); 3] = [
            ("Shadow EVSM Moment Write", moment_write_src.as_str()),
            ("Shadow EVSM Blur H", blur_h_wgsl()),
            ("Shadow EVSM Blur V", blur_v_wgsl()),
        ];
        let mods: Vec<web_sys::GpuShaderModule> = inline_specs
            .iter()
            .map(|(label, code)| {
                let desc: web_sys::GpuShaderModuleDescriptor =
                    ShaderModuleDescriptor::new(code, Some(label)).into();
                gpu.compile_shader(&desc)
            })
            .collect();
        let modules: [web_sys::GpuShaderModule; 3] =
            [mods[0].clone(), mods[1].clone(), mods[2].clone()];

        // ── params buffer ──────────────────────────────────────────────
        let params_buffer_size = EVSM_PARAMS_STRIDE * MAX_EVSM_CASCADES_PER_FRAME;
        let params_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow EVSM Params"),
                params_buffer_size,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        Ok(EvsmDescriptors {
            moment_write_layout_key,
            blur_layout_key,
            moment_write_pipeline_layout_key,
            blur_pipeline_layout_key,
            modules,
            params_buffer,
            params_bytes: vec![0u8; params_buffer_size],
        })
    }

    /// Folds resolved compute pipeline keys back into the typed
    /// `EvsmPass`. Sync; the orchestrator has already run the
    /// validate join + the cross-tail `ComputePipelines::ensure_keys`.
    ///
    /// Pass an empty `resolved` slice to construct an `EvsmPass` with
    /// pipeline keys deferred (`None`). The orchestrator's lazy path
    /// (Block B.1) uses this — pipelines compile on first shadow-caster
    /// via `Shadows::ensure_pipelines_compiled`.
    pub fn from_resolved(descs: EvsmDescriptors, resolved: Vec<ComputePipelineKey>) -> Self {
        let (moment_write, blur_h, blur_v) = match resolved.len() {
            3 => (Some(resolved[0]), Some(resolved[1]), Some(resolved[2])),
            0 => (None, None, None),
            other => {
                debug_assert!(
                    other == 0 || other == 3,
                    "EvsmPass::from_resolved expects 0 or 3 resolved keys, got {other}"
                );
                (None, None, None)
            }
        };
        Self {
            moment_write_layout_key: descs.moment_write_layout_key,
            blur_layout_key: descs.blur_layout_key,
            moment_write_pipeline_layout_key: descs.moment_write_pipeline_layout_key,
            blur_pipeline_layout_key: descs.blur_pipeline_layout_key,
            moment_write_pipeline_key: moment_write,
            blur_h_pipeline_key: blur_h,
            blur_v_pipeline_key: blur_v,
            params_buffer: descs.params_buffer,
            params_bytes: descs.params_bytes,
            active_cascade_count: 0,
        }
    }

    /// Returns the dynamic-offset for cascade slot `index`.
    pub fn params_dynamic_offset(index: u32) -> u32 {
        index * EVSM_PARAMS_STRIDE as u32
    }

    /// Writes one cascade's params into the CPU staging buffer at
    /// slot `index`. Layout (stride = 256 B; first 48 B used):
    ///
    /// ```text
    /// [ 0.. 8] src_offset    (u32×2 — texels into cascade-array layer)
    /// [ 8..16] src_size      (u32×2 — texels)
    /// [16..24] dst_offset    (u32×2 — texels into evsm_atlas)
    /// [24..32] dst_size      (u32×2 — texels)
    /// [32..36] exponent      (f32)
    /// [36..40] blur_radius   (u32 — clamped to MAX_BLUR_RADIUS)
    /// [40..44] cascade_layer (u32 — index into cascade_array)
    /// [44..48] _pad          (u32)
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn write_params_slot(
        &mut self,
        index: usize,
        src_offset: [u32; 2],
        src_size: [u32; 2],
        dst_offset: [u32; 2],
        dst_size: [u32; 2],
        exponent: f32,
        blur_radius: u32,
        cascade_layer: u32,
    ) {
        let base = index * EVSM_PARAMS_STRIDE;
        let dst = &mut self.params_bytes[base..base + 48];
        dst[0..4].copy_from_slice(&src_offset[0].to_ne_bytes());
        dst[4..8].copy_from_slice(&src_offset[1].to_ne_bytes());
        dst[8..12].copy_from_slice(&src_size[0].to_ne_bytes());
        dst[12..16].copy_from_slice(&src_size[1].to_ne_bytes());
        dst[16..20].copy_from_slice(&dst_offset[0].to_ne_bytes());
        dst[20..24].copy_from_slice(&dst_offset[1].to_ne_bytes());
        dst[24..28].copy_from_slice(&dst_size[0].to_ne_bytes());
        dst[28..32].copy_from_slice(&dst_size[1].to_ne_bytes());
        dst[32..36].copy_from_slice(&exponent.to_ne_bytes());
        let radius = blur_radius.min(MAX_BLUR_RADIUS);
        dst[36..40].copy_from_slice(&radius.to_ne_bytes());
        dst[40..44].copy_from_slice(&cascade_layer.to_ne_bytes());
        dst[44..48].copy_from_slice(&0u32.to_ne_bytes());
    }

    /// Flushes the staging buffer to GPU. Called once at the end of
    /// `Shadows::write_gpu` after `active_cascade_count` is finalised.
    pub fn upload_params(&self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmShadowError> {
        if self.active_cascade_count == 0 {
            return Ok(());
        }
        let used = self.active_cascade_count as usize * EVSM_PARAMS_STRIDE;
        gpu.write_buffer(
            &self.params_buffer,
            None,
            &self.params_bytes[..used],
            None,
            None,
        )?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────
// Compute shader sources
// ─────────────────────────────────────────────────────────────────────

const MOMENT_WRITE_WGSL: &str = r#"
struct Params {
    src_offset: vec2<u32>,
    src_size: vec2<u32>,
    dst_offset: vec2<u32>,
    dst_size: vec2<u32>,
    exponent: f32,
    blur_radius: u32,
    cascade_layer: u32,
    _pad: u32,
}

@group(0) @binding(0) var src_depth: texture_depth_2d_array;
@group(0) @binding(1) var dst_moments: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.dst_size.x || gid.y >= params.dst_size.y) {
        return;
    }
    // Map dst texel center back to src texel. Same-size atlases give
    // 1:1; if PCF is larger, pick nearest source texel — depth is a
    // sharp signal, bilinear averages across discontinuities.
    let dst_uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5, 0.5))
        / vec2<f32>(f32(params.dst_size.x), f32(params.dst_size.y));
    let src_xy = vec2<u32>(
        params.src_offset.x + u32(dst_uv.x * f32(params.src_size.x)),
        params.src_offset.y + u32(dst_uv.y * f32(params.src_size.y)),
    );
    let depth = textureLoad(
        src_depth,
        vec2<i32>(i32(src_xy.x), i32(src_xy.y)),
        i32(params.cascade_layer),
        0,
    );
    // Remap [0,1] → [-1,1] so the exponent space is symmetric. Substituted
    // per depth convention by `moment_write_wgsl` — the receiver's remap in
    // `shared_wgsl/shadow/bind_groups.wgsl` (`sample_shadow_evsm`) MUST match:
    //   forward: z = 2·depth − 1        (near → −1, far → +1)
    //   reverse: z = 1 − 2·depth        (near → −1, far → +1)
    // Both keep "farther ⇒ larger warped value", preserving the one-tailed
    // Chebyshev semantics; the reverse ortho cascade depth is exactly
    // 1 − forward depth, so the warped values are identical.
    let z = /*EVSM_DEPTH_REMAP*/;
    let pos_exp = exp(params.exponent * z);
    let neg_exp = -exp(-params.exponent * z);
    let moments = vec4<f32>(pos_exp, pos_exp * pos_exp, neg_exp, neg_exp * neg_exp);
    let store_xy = vec2<i32>(
        i32(params.dst_offset.x + gid.x),
        i32(params.dst_offset.y + gid.y),
    );
    textureStore(dst_moments, store_xy, moments);
}
"#;

// Shared prefix for both blur half-pass shaders: bindings + kernel.
const BLUR_COMMON_PREFIX: &str = r#"
struct Params {
    src_offset: vec2<u32>,
    src_size: vec2<u32>,
    dst_offset: vec2<u32>,
    dst_size: vec2<u32>,
    exponent: f32,
    blur_radius: u32,
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: Params;

// 9-tap Gaussian (centre + 8 sides), σ ≈ 8/3 covering ~99.7%. Shaders
// pick the first `radius+1` weights and re-normalise via `kernel_sum`.
const GAUSSIAN_W: array<f32, 9> = array<f32, 9>(
    0.150946,
    0.139148,
    0.108878,
    0.072448,
    0.040951,
    0.019696,
    0.008049,
    0.002800,
    0.000829,
);

fn kernel_sum(radius: u32) -> f32 {
    var s = GAUSSIAN_W[0];
    for (var i = 1u; i <= radius; i = i + 1u) {
        s = s + 2.0 * GAUSSIAN_W[i];
    }
    return s;
}
"#;

const BLUR_H_BODY: &str = r#"
@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.dst_size.x || gid.y >= params.dst_size.y) {
        return;
    }
    let centre_xy = vec2<i32>(
        i32(params.dst_offset.x + gid.x),
        i32(params.dst_offset.y + gid.y),
    );
    let radius = min(params.blur_radius, 8u);
    let inv_sum = 1.0 / kernel_sum(radius);
    var acc = textureLoad(src_tex, centre_xy, 0) * GAUSSIAN_W[0];
    let lo = i32(params.dst_offset.x);
    let hi = i32(params.dst_offset.x + params.dst_size.x) - 1;
    for (var i = 1u; i <= radius; i = i + 1u) {
        let w = GAUSSIAN_W[i];
        let off_pos = clamp(centre_xy.x + i32(i), lo, hi);
        let off_neg = clamp(centre_xy.x - i32(i), lo, hi);
        acc = acc + textureLoad(src_tex, vec2<i32>(off_pos, centre_xy.y), 0) * w;
        acc = acc + textureLoad(src_tex, vec2<i32>(off_neg, centre_xy.y), 0) * w;
    }
    textureStore(dst_tex, centre_xy, acc * inv_sum);
}
"#;

const BLUR_V_BODY: &str = r#"
@compute @workgroup_size(1, 64, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.dst_size.x || gid.y >= params.dst_size.y) {
        return;
    }
    let centre_xy = vec2<i32>(
        i32(params.dst_offset.x + gid.x),
        i32(params.dst_offset.y + gid.y),
    );
    let radius = min(params.blur_radius, 8u);
    let inv_sum = 1.0 / kernel_sum(radius);
    var acc = textureLoad(src_tex, centre_xy, 0) * GAUSSIAN_W[0];
    let lo = i32(params.dst_offset.y);
    let hi = i32(params.dst_offset.y + params.dst_size.y) - 1;
    for (var i = 1u; i <= radius; i = i + 1u) {
        let w = GAUSSIAN_W[i];
        let off_pos = clamp(centre_xy.y + i32(i), lo, hi);
        let off_neg = clamp(centre_xy.y - i32(i), lo, hi);
        acc = acc + textureLoad(src_tex, vec2<i32>(centre_xy.x, off_pos), 0) * w;
        acc = acc + textureLoad(src_tex, vec2<i32>(centre_xy.x, off_neg), 0) * w;
    }
    textureStore(dst_tex, centre_xy, acc * inv_sum);
}
"#;

static BLUR_H_ONCE: OnceLock<String> = OnceLock::new();
static BLUR_V_ONCE: OnceLock<String> = OnceLock::new();

/// Moment-write WGSL for the active depth convention — substitutes the
/// depth → warp remap (see the marker comment inside [`MOMENT_WRITE_WGSL`]).
/// Forward and reverse both warp to a symmetric [-1, 1] with "farther ⇒
/// larger", so the Chebyshev reconstruction on the receiver is unchanged.
fn moment_write_wgsl(depth: crate::depth_convention::DepthConvention) -> String {
    let remap = if depth.reverse_z {
        "1.0 - 2.0 * depth"
    } else {
        "2.0 * depth - 1.0"
    };
    MOMENT_WRITE_WGSL.replace("/*EVSM_DEPTH_REMAP*/", remap)
}

fn blur_h_wgsl() -> &'static str {
    BLUR_H_ONCE
        .get_or_init(|| format!("{}{}", BLUR_COMMON_PREFIX, BLUR_H_BODY))
        .as_str()
}

fn blur_v_wgsl() -> &'static str {
    BLUR_V_ONCE
        .get_or_init(|| format!("{}{}", BLUR_COMMON_PREFIX, BLUR_V_BODY))
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depth_convention::DepthConvention;

    /// 003 stage 7 guard: the moment-write remap is substituted via a marker
    /// string — if the marker drifts, `replace` silently no-ops and the shader
    /// ships `let z = ;` (a browser-only compile failure). Also pins the two
    /// remap arms to the receiver's (`sample_shadow_evsm`) exactly.
    #[test]
    fn moment_write_remap_substitutes_per_convention() {
        let forward = moment_write_wgsl(DepthConvention::FORWARD);
        assert!(forward.contains("let z = 2.0 * depth - 1.0;"));
        let reverse = moment_write_wgsl(DepthConvention { reverse_z: true });
        assert!(reverse.contains("let z = 1.0 - 2.0 * depth;"));
        for src in [&forward, &reverse] {
            assert!(
                !src.contains("EVSM_DEPTH_REMAP"),
                "marker must be fully substituted"
            );
        }
    }
}
