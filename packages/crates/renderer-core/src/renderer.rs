//! WebGPU context and builder.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::GpuSupportedLimits;

use crate::{
    configuration::CanvasConfiguration,
    error::{AwsmCoreError, Result},
};

/// WebGPU feature name for `firstInstance` in indirect draws.
/// Required for the geometry pass's `drawIndexedIndirect` calls to honor
/// non-zero `firstInstance` values written by the compaction shader.
const INDIRECT_FIRST_INSTANCE_FEATURE: &str = "indirect-first-instance";

/// GPU block-compressed texture-format families. Requested at device-create
/// whenever the adapter exposes them (they cost nothing when unused) so
/// KTX2/Basis textures can upload compressed instead of expanding to RGBA8.
/// BC ≈ desktop, ASTC ≈ modern mobile, ETC2 ≈ older mobile; WebGPU guarantees
/// at least one of the three on real hardware.
const TEXTURE_COMPRESSION_BC_FEATURE: &str = "texture-compression-bc";
const TEXTURE_COMPRESSION_ETC2_FEATURE: &str = "texture-compression-etc2";
const TEXTURE_COMPRESSION_ASTC_FEATURE: &str = "texture-compression-astc";

/// Which block-compressed texture families the active device supports.
/// Drives the KTX2/Basis transcode-target ladder (see docs/plans/compression.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextureCompressionSupport {
    pub bc: bool,
    pub etc2: bool,
    pub astc: bool,
}

impl TextureCompressionSupport {
    /// True when no block-compressed family is available and textures must
    /// fall back to RGBA8 (software WebGPU implementations, mostly).
    pub fn none(&self) -> bool {
        !self.bc && !self.etc2 && !self.astc
    }
}

/// A process-stable identity for a `GpuDevice`, used to **scope the
/// device-bound GPU caches** (blit / BRDF-LUT / mipmap / atlas / sRGB
/// pipelines + samplers + staging buffers) that renderer-core keeps in
/// `thread_local!`s.
///
/// Those caches store device-bound GPU objects (pipelines, samplers,
/// buffers). A pipeline created on device A is invalid on device B, so a
/// second `AwsmRenderer` with a different device must NOT reuse the
/// first's cached objects (doing so throws cross-device
/// `GPUValidationError`s). Keying every cache by `DeviceId` lets N
/// independent renderers coexist on one thread — each device gets its own
/// cache slot; for a single renderer the behaviour is identical to the
/// old global cache (one id, same objects).
///
/// Identity is a monotonic counter assigned once at device-creation time
/// (`build()`), so it is `Copy` and cheap to hash. `AwsmRendererWebGpu`
/// is `Clone`; clones share the underlying device **and** its id, which
/// is correct (they should hit the same cache slot). Caveat: building two
/// wrappers from the *same* pre-created device via `with_device` yields
/// two ids → the per-device resources are created twice (a one-time
/// creation cost, not a correctness bug). Every renderer in the editor
/// requests its own device, so this does not arise in practice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(u64);

impl DeviceId {
    fn next() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        DeviceId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// WebGPU device and canvas context wrapper.
/// Relatively cheap to clone.
#[derive(Clone)]
pub struct AwsmRendererWebGpu {
    pub gpu: web_sys::Gpu,
    pub adapter: web_sys::GpuAdapter,
    pub device: web_sys::GpuDevice,
    /// Cache-scoping identity for `device` — see [`DeviceId`].
    pub device_id: DeviceId,
    pub context: web_sys::GpuCanvasContext,
    /// Whether the renderer is bound to a DOM canvas
    /// (`HtmlCanvasElement`, main-thread mode) or an
    /// `OffscreenCanvas` (Phase 4.4 worker mode). Stored so
    /// downstream convenience methods on `AwsmRendererWebGpu` can
    /// branch correctly without `unchecked_into()`-ing the underlying
    /// JS handle. See [`Self::canvas_kind`] for the safe accessor;
    /// DOM-only methods on this type (`canvas()`, `canvas_size(true)`,
    /// `sync_canvas_buffer_with_css`, `pointer_event_to_canvas_coords_*`)
    /// panic with a clear message if invoked in `Offscreen` mode
    /// rather than silently mis-casting.
    canvas_kind: CanvasKind,
}

impl AwsmRendererWebGpu {
    /// Whether the active `GpuDevice` was created with the
    /// `indirect-first-instance` feature. Upper-layer renderer code
    /// consults this before engaging any path that issues
    /// `drawIndirect` / `drawIndexedIndirect` with a non-zero
    /// `firstInstance` in the indirect args — without the feature,
    /// WebGPU silently drops the call.
    pub fn has_indirect_first_instance(&self) -> bool {
        self.device.features().has(INDIRECT_FIRST_INSTANCE_FEATURE)
    }

    /// Which block-compressed texture families the active `GpuDevice` was
    /// created with. Texture upload paths consult this to pick a transcode
    /// target (BC/ASTC/ETC2) or fall back to RGBA8.
    pub fn texture_compression(&self) -> TextureCompressionSupport {
        let features = self.device.features();
        TextureCompressionSupport {
            bc: features.has(TEXTURE_COMPRESSION_BC_FEATURE),
            etc2: features.has(TEXTURE_COMPRESSION_ETC2_FEATURE),
            astc: features.has(TEXTURE_COMPRESSION_ASTC_FEATURE),
        }
    }

    /// The canvas the renderer was built against. Use this to branch
    /// safely between main-thread and worker-mode code paths instead
    /// of calling `canvas()` (which panics in worker mode).
    pub fn canvas_kind(&self) -> &CanvasKind {
        &self.canvas_kind
    }

    /// The cache-scoping identity of this wrapper's `GpuDevice`. The
    /// renderer-core device-bound caches key on this so independent
    /// renderers (distinct devices) never share device-bound GPU
    /// objects. See [`DeviceId`].
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Register a host callback fired (once) when **this** device is lost — the
    /// **action seam** for GPU device-loss recovery (B1a). Attaches another
    /// handler to the device's `lost` promise *alongside* the logging hook from
    /// [`install_device_lost_hook`]; the callback receives the loss `reason`
    /// (`"destroyed"` on an explicit `destroy()`, `"unknown"` on a driver reset
    /// / OOM). The host uses it to kick a cold-path recovery (reacquire device +
    /// replay the retained source-of-truth — geometry/texture CPU mirrors don't
    /// exist, so this is reload-from-source, not rebuild-from-mirror).
    ///
    /// Cold path, **one-shot** — installed once per device, never per frame, so
    /// the render hot loop pays nothing. The closure leaks (`forget`) for the
    /// device's lifetime (the device outlives nothing once lost).
    pub fn on_device_lost<F: 'static + FnOnce(String)>(&self, f: F) {
        let mut slot = Some(f);
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |info: JsValue| {
            let reason = js_sys::Reflect::get(&info, &JsValue::from_str("reason"))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            if let Some(f) = slot.take() {
                f(reason);
            }
        });
        match js_sys::Reflect::get(self.device.as_ref(), &JsValue::from_str("lost"))
            .ok()
            .and_then(|p| p.dyn_into::<js_sys::Promise>().ok())
        {
            Some(promise) => {
                let _ = promise.then(&cb);
            }
            None => {
                tracing::warn!(
                    target: "awsm_renderer_core::device_lost",
                    "on_device_lost: device.lost promise unavailable; recovery won't trigger"
                );
            }
        }
        cb.forget();
    }
}

/// Canvas kind the renderer is targeting — `HtmlCanvasElement` for
/// main-thread mode, `OffscreenCanvas` for worker-mode (Phase 4.4).
/// Both go through the same `GpuCanvasContext` API on the WebGPU side;
/// `CanvasKind` only matters at builder time (acquiring the context)
/// and on resize (the two element types have different `set_*` APIs).
#[derive(Clone)]
pub enum CanvasKind {
    Html(web_sys::HtmlCanvasElement),
    Offscreen(web_sys::OffscreenCanvas),
}

impl CanvasKind {
    fn get_webgpu_context(&self) -> Result<web_sys::GpuCanvasContext> {
        let ctx = match self {
            CanvasKind::Html(c) => c.get_context("webgpu"),
            CanvasKind::Offscreen(c) => c.get_context("webgpu"),
        };
        match ctx {
            Ok(Some(ctx)) => Ok(ctx.unchecked_into()),
            Err(err) => Err(AwsmCoreError::canvas_context(err)),
            Ok(None) => Err(AwsmCoreError::CanvasContext("No context found".to_string())),
        }
    }
}

/// Builder for creating an `AwsmRendererWebGpu`.
///
/// Fields are private — construct with [`Self::new`] /
/// [`Self::new_with_offscreen_canvas`] and configure via the `with_*`
/// methods. Privatising avoids a breaking change when adding new
/// inputs (e.g. the `CanvasKind` enum that replaced the original
/// `HtmlCanvasElement` `canvas` field), since callers that constructed
/// the struct with `..Default::default()` or accessed `builder.canvas`
/// directly would otherwise need to track each addition.
pub struct AwsmRendererWebGpuBuilder {
    gpu: web_sys::Gpu,
    canvas: CanvasKind,
    configuration: Option<CanvasConfiguration>,
    adapter: Option<web_sys::GpuAdapter>,
    device: Option<web_sys::GpuDevice>,
    device_req_limits: Option<DeviceRequestLimits>,
}

impl AwsmRendererWebGpuBuilder {
    /// Creates a builder for a given GPU and (main-thread) canvas.
    pub fn new(gpu: web_sys::Gpu, canvas: web_sys::HtmlCanvasElement) -> Self {
        Self {
            gpu,
            canvas: CanvasKind::Html(canvas),
            configuration: None,
            adapter: None,
            device: None,
            device_req_limits: None,
        }
    }

    /// Creates a builder for worker-mode rendering against an
    /// `OffscreenCanvas` (Phase 4.4). The caller is expected to have
    /// already called `transferControlToOffscreen()` on a DOM canvas
    /// on the main thread and posted the resulting `OffscreenCanvas`
    /// to the worker, where this is called.
    pub fn new_with_offscreen_canvas(gpu: web_sys::Gpu, canvas: web_sys::OffscreenCanvas) -> Self {
        Self {
            gpu,
            canvas: CanvasKind::Offscreen(canvas),
            configuration: None,
            adapter: None,
            device: None,
            device_req_limits: None,
        }
    }

    /// Sets the canvas configuration.
    pub fn with_configuration(mut self, configuration: CanvasConfiguration) -> Self {
        self.configuration = Some(configuration);
        self
    }

    /// Sets a pre-selected adapter.
    pub fn with_adapter(mut self, adapter: web_sys::GpuAdapter) -> Self {
        self.adapter = Some(adapter);
        self
    }

    /// Sets a pre-created device.
    pub fn with_device(mut self, device: web_sys::GpuDevice) -> Self {
        self.device = Some(device);
        self
    }

    /// Sets requested device limits.
    pub fn with_device_request_limits(mut self, device_req_limits: DeviceRequestLimits) -> Self {
        self.device_req_limits = Some(device_req_limits);
        self
    }

    /// Builds the WebGPU context and device.
    pub async fn build(self) -> Result<AwsmRendererWebGpu> {
        tracing::info!("Building WebGPU Context");

        let context: web_sys::GpuCanvasContext = self.canvas.get_webgpu_context()?;

        let mut adapter: web_sys::GpuAdapter = match self.adapter {
            Some(adapter) => adapter,
            None => JsFuture::from(self.gpu.request_adapter())
                .await
                .map_err(AwsmCoreError::gpu_adapter)?
                .unchecked_into(),
        };

        if adapter.is_null() || adapter.is_undefined() {
            // try one more time... maybe necessary for "lost context" scenarios?
            adapter = JsFuture::from(self.gpu.request_adapter())
                .await
                .map_err(AwsmCoreError::gpu_adapter)?
                .unchecked_into();

            if adapter.is_null() || adapter.is_undefined() {
                return Err(AwsmCoreError::GpuAdapter("is null".to_string()));
            }
        }

        let device: web_sys::GpuDevice = match self.device {
            Some(device) => device,
            None => {
                let descriptor = web_sys::GpuDeviceDescriptor::new();
                // `indirect-first-instance` is required for the geometry
                // pass's `drawIndexedIndirect` to honor a non-zero
                // `firstInstance` in the args buffer. Without it, the
                // first_instance slot is treated as 0 (or fails
                // validation) and any non-instanced mesh whose meta slot
                // index is > 0 renders nothing — silently. Requested
                // only when the adapter exposes it so the device-create
                // doesn't fail on hardware that lacks the feature
                // (callers can detect and disable `RendererFeatures::gpu_culling`
                // upstream if needed).
                let features = adapter.features();
                let mut required: Vec<js_sys::JsString> = Vec::new();
                if features.has(INDIRECT_FIRST_INSTANCE_FEATURE) {
                    required.push(js_sys::JsString::from(INDIRECT_FIRST_INSTANCE_FEATURE));
                }
                // Block-compressed texture families for the KTX2/Basis
                // upload path — request every family the adapter has (free
                // when unused; the transcode ladder picks among them at
                // load time via `texture_compression()`).
                for feature in [
                    TEXTURE_COMPRESSION_BC_FEATURE,
                    TEXTURE_COMPRESSION_ETC2_FEATURE,
                    TEXTURE_COMPRESSION_ASTC_FEATURE,
                ] {
                    if features.has(feature) {
                        required.push(js_sys::JsString::from(feature));
                    }
                }
                if !required.is_empty() {
                    descriptor.set_required_features(&required);
                }
                if let Some(limits) = self.device_req_limits {
                    let adapter_limits = adapter.limits();
                    if adapter_limits.is_null() || adapter_limits.is_undefined() {
                        tracing::warn!("adapter limits are null or undefined");
                        JsFuture::from(adapter.request_device_with_descriptor(&descriptor))
                            .await
                            .map_err(AwsmCoreError::gpu_device)?
                            .unchecked_into()
                    } else {
                        descriptor.set_required_limits(
                            &limits.into_js(&adapter.limits()).unchecked_into(),
                        );
                        JsFuture::from(adapter.request_device_with_descriptor(&descriptor))
                            .await
                            .map_err(AwsmCoreError::gpu_device)?
                            .unchecked_into()
                    }
                } else {
                    JsFuture::from(adapter.request_device_with_descriptor(&descriptor))
                        .await
                        .map_err(AwsmCoreError::gpu_device)?
                        .unchecked_into()
                }
            }
        };

        if device.is_null() || device.is_undefined() {
            return Err(AwsmCoreError::GpuDevice("is null".to_string()));
        }

        // Diagnostic: one-shot dump of the effective device limits, so a
        // user reporting an init failure (especially on Android, where
        // SPIR-V / Vulkan caps differ from desktop) has the actual cap
        // values in the logs rather than us guessing from
        // hardcoded-spec-minimum assumptions.
        log_device_limits(&device);

        // Diagnostic: which block-compressed texture families this device
        // carries — tells us at a glance whether KTX2/Basis textures will
        // upload compressed (and to which family) or fall back to RGBA8.
        {
            let features = device.features();
            tracing::info!(
                "texture compression support: bc={} etc2={} astc={}",
                features.has(TEXTURE_COMPRESSION_BC_FEATURE),
                features.has(TEXTURE_COMPRESSION_ETC2_FEATURE),
                features.has(TEXTURE_COMPRESSION_ASTC_FEATURE),
            );
        }

        // Diagnostic: wire `onuncapturederror` so the JS validation /
        // OOM / internal-error channel surfaces in our tracing stream.
        // Dawn passes async pipeline failures through Promise rejection
        // (caught at the call site), but anything that fires
        // out-of-band (runtime validation, device lost, OOM under
        // memory pressure) lands here. The
        // `awsm_renderer_core::uncaptured_error` target lets the
        // operator filter for it.
        install_uncaptured_error_hook(&device);

        // Detection half of GPU device-loss recovery: wire `device.lost` so a
        // loss (explicit `destroy()`, driver reset, OOM) surfaces in tracing
        // instead of silently freezing the canvas. One-shot, cold install — no
        // per-frame cost. The recovery half (`rebuild_gpu` from CPU mirrors)
        // hangs off the same signal.
        install_device_lost_hook(&device);

        context
            .configure(
                &self
                    .configuration
                    .unwrap_or_default()
                    .into_js(&self.gpu, &device),
            )
            .map_err(AwsmCoreError::context_configuration)?;

        Ok(AwsmRendererWebGpu {
            gpu: self.gpu,
            adapter,
            device,
            device_id: DeviceId::next(),
            context,
            canvas_kind: self.canvas,
        })
    }
}

/// One-shot dump of `device.limits()` at device-creation time.
///
/// Reaches through `js_sys::Reflect` rather than the typed
/// `GpuSupportedLimits` getters because not every limit we care to
/// surface has a typed accessor in our enabled web-sys feature set
/// (and the Reflect path is forward-compatible — newly-exposed limits
/// in Chrome stable show up automatically as the spec advances).
///
/// Logs under `target = "awsm_renderer_core::limits"` at `info`. Filter
/// via `RUST_LOG=awsm_renderer_core::limits=info`.
fn log_device_limits(device: &web_sys::GpuDevice) {
    let limits = device.limits();
    if limits.is_null() || limits.is_undefined() {
        tracing::warn!(target: "awsm_renderer_core::limits", "device.limits() is null/undefined");
        return;
    }

    // Keys worth surfacing for diagnosing renderer failures (storage
    // buffer caps, binding sizes, workgroup caps). Order roughly
    // matches the WebGPU spec's table of supported limits.
    const KEYS: &[&str] = &[
        "maxTextureDimension1D",
        "maxTextureDimension2D",
        "maxTextureDimension3D",
        "maxTextureArrayLayers",
        "maxBindGroups",
        "maxBindingsPerBindGroup",
        "maxDynamicUniformBuffersPerPipelineLayout",
        "maxDynamicStorageBuffersPerPipelineLayout",
        "maxSampledTexturesPerShaderStage",
        "maxSamplersPerShaderStage",
        "maxStorageBuffersPerShaderStage",
        "maxStorageTexturesPerShaderStage",
        "maxUniformBuffersPerShaderStage",
        "maxUniformBufferBindingSize",
        "maxStorageBufferBindingSize",
        "maxBufferSize",
        "maxVertexBuffers",
        "maxVertexAttributes",
        "maxVertexBufferArrayStride",
        "maxInterStageShaderVariables",
        "maxColorAttachments",
        "maxColorAttachmentBytesPerSample",
        "maxComputeWorkgroupStorageSize",
        "maxComputeInvocationsPerWorkgroup",
        "maxComputeWorkgroupSizeX",
        "maxComputeWorkgroupSizeY",
        "maxComputeWorkgroupSizeZ",
        "maxComputeWorkgroupsPerDimension",
    ];

    let mut parts = Vec::with_capacity(KEYS.len());
    for key in KEYS {
        let js_key = JsValue::from_str(key);
        match js_sys::Reflect::get(limits.as_ref(), &js_key) {
            Ok(v) if !v.is_undefined() && !v.is_null() => {
                if let Some(n) = v.as_f64() {
                    parts.push(format!("{}={}", key, n as u64));
                } else if let Some(s) = v.as_string() {
                    parts.push(format!("{}={}", key, s));
                }
            }
            _ => {}
        }
    }

    tracing::info!(
        target: "awsm_renderer_core::limits",
        "device limits: {}",
        parts.join(" ")
    );
}

/// Install `device.onuncapturederror` listener. The closure leaks
/// intentionally (`forget`) — it's a one-shot install for the lifetime
/// of the device, and the device itself is owned by the renderer for
/// the lifetime of the page.
///
/// The event carries a `GpuError` (validation / OOM / internal) on its
/// `error` field. We pull the message via `js_sys::Reflect` rather than
/// typed bindings — that way the hook keeps working even if the
/// enabled `web-sys` features drift, and it's robust against
/// browser-specific extras on the error type.
fn install_uncaptured_error_hook(device: &web_sys::GpuDevice) {
    let on_err = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
        // event.error is the GpuError; .message carries the human-readable string.
        let error_obj =
            js_sys::Reflect::get(&event, &JsValue::from_str("error")).unwrap_or(JsValue::UNDEFINED);
        let message = js_sys::Reflect::get(&error_obj, &JsValue::from_str("message"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "<no message>".to_string());

        // The error category is identifiable by constructor name
        // (`GPUValidationError` / `GPUInternalError` / `GPUOutOfMemoryError`).
        let category = js_sys::Reflect::get(&error_obj, &JsValue::from_str("constructor"))
            .and_then(|c| js_sys::Reflect::get(&c, &JsValue::from_str("name")))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "GpuError".to_string());

        tracing::error!(
            target: "awsm_renderer_core::uncaptured_error",
            "GPU uncaptured ({}): {}",
            category,
            message
        );
    });

    let listener: &js_sys::Function = on_err.as_ref().unchecked_ref();
    if let Err(err) = js_sys::Reflect::set(
        device.as_ref(),
        &JsValue::from_str("onuncapturederror"),
        listener,
    ) {
        tracing::warn!(
            target: "awsm_renderer_core::uncaptured_error",
            "failed to install onuncapturederror hook: {:?}",
            err
        );
    }
    on_err.forget();
}

/// Install a `device.lost` handler. `GPUDevice.lost` is a `Promise` that
/// resolves **exactly once** when the device is lost — after an explicit
/// `device.destroy()` (`reason = "destroyed"`) or a driver reset / OOM
/// (`reason = "unknown"`). We attach a `.then` and log the reason + message
/// under the `awsm_renderer_core::device_lost` target so a loss is never silent
/// (it surfaces in `get_logs`). The closure leaks (`forget`) like the
/// uncaptured-error hook — one-shot for the device's lifetime.
///
/// Pulled via `js_sys::Reflect` rather than a typed getter so the hook keeps
/// working regardless of the enabled `web-sys` feature set. This is the
/// **detection** seam; GPU-device-loss recovery (`rebuild_gpu` from CPU
/// mirrors) builds its action onto this same promise.
fn install_device_lost_hook(device: &web_sys::GpuDevice) {
    let on_lost = Closure::<dyn FnMut(JsValue)>::new(move |info: JsValue| {
        let reason = js_sys::Reflect::get(&info, &JsValue::from_str("reason"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        let message = js_sys::Reflect::get(&info, &JsValue::from_str("message"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "<no message>".to_string());
        tracing::error!(
            target: "awsm_renderer_core::device_lost",
            "GPU device lost ({}): {}",
            reason,
            message
        );
    });

    match js_sys::Reflect::get(device.as_ref(), &JsValue::from_str("lost"))
        .ok()
        .and_then(|p| p.dyn_into::<js_sys::Promise>().ok())
    {
        Some(promise) => {
            let _ = promise.then(&on_lost);
        }
        None => {
            tracing::warn!(
                target: "awsm_renderer_core::device_lost",
                "device.lost promise unavailable; a device loss would be silent"
            );
        }
    }
    on_lost.forget();
}

/// Requested device limits to increase WebGPU caps.
#[derive(Debug, Clone, Default)]
pub struct DeviceRequestLimits {
    pub max_texture_dimension_2d: bool,
    pub max_texture_array_layers: bool,
    pub max_bindings_per_bind_group: bool,
    pub max_sampled_textures_per_shader_stage: bool,
    pub max_storage_buffers_per_shader_stage: bool,
    pub max_buffer_size: bool,
    pub max_bind_groups: bool,
    pub max_storage_buffer_binding_size: bool,
    pub max_color_attachment_bytes_per_sample: bool,
}

impl DeviceRequestLimits {
    /// Requests maximum supported limits for all tracked fields.
    pub fn max_all() -> Self {
        Self {
            max_texture_dimension_2d: true,
            max_texture_array_layers: true,
            max_bindings_per_bind_group: true,
            max_sampled_textures_per_shader_stage: true,
            max_storage_buffers_per_shader_stage: true,
            max_buffer_size: true,
            max_bind_groups: true,
            max_storage_buffer_binding_size: true,
            max_color_attachment_bytes_per_sample: true,
        }
    }

    /// Requests a typical set of limits for awsm-renderer.
    pub fn typical() -> Self {
        Self::default()
            .with_max_storage_buffer_binding_size()
            .with_max_storage_buffers_per_shader_stage()
    }

    /// Enables requesting max storage buffer binding size.
    pub fn with_max_storage_buffer_binding_size(mut self) -> Self {
        self.max_storage_buffer_binding_size = true;
        self
    }

    /// Enables requesting max color attachments bytes per sample
    pub fn with_max_color_attachment_bytes_per_sample(mut self) -> Self {
        self.max_color_attachment_bytes_per_sample = true;
        self
    }

    /// Enables requesting max storage buffers per shader stage.
    pub fn with_max_storage_buffers_per_shader_stage(mut self) -> Self {
        self.max_storage_buffers_per_shader_stage = true;
        self
    }

    pub fn with_max_sampled_textures_per_shader_stage(mut self) -> Self {
        self.max_sampled_textures_per_shader_stage = true;
        self
    }

    /// Converts requested limits into a WebGPU limits object.
    pub fn into_js(self, limits: &GpuSupportedLimits) -> js_sys::Object {
        let obj = js_sys::Object::new();

        if self.max_texture_dimension_2d {
            js_sys::Reflect::set(
                &obj,
                &"maxTextureDimension2D".into(),
                &JsValue::from_f64(limits.max_texture_dimension_2d() as f64),
            )
            .unwrap();
        }
        if self.max_texture_array_layers {
            js_sys::Reflect::set(
                &obj,
                &"maxTextureArrayLayers".into(),
                &JsValue::from_f64(limits.max_texture_array_layers() as f64),
            )
            .unwrap();
        }
        if self.max_bindings_per_bind_group {
            js_sys::Reflect::set(
                &obj,
                &"maxBindingsPerBindGroup".into(),
                &JsValue::from_f64(limits.max_bindings_per_bind_group() as f64),
            )
            .unwrap();
        }
        if self.max_bind_groups {
            js_sys::Reflect::set(
                &obj,
                &"maxBindGroups".into(),
                &JsValue::from_f64(limits.max_bind_groups() as f64),
            )
            .unwrap();
        }
        if self.max_sampled_textures_per_shader_stage {
            js_sys::Reflect::set(
                &obj,
                &"maxSampledTexturesPerShaderStage".into(),
                &JsValue::from_f64(limits.max_sampled_textures_per_shader_stage() as f64),
            )
            .unwrap();
        }

        if self.max_storage_buffers_per_shader_stage {
            js_sys::Reflect::set(
                &obj,
                &"maxStorageBuffersPerShaderStage".into(),
                &JsValue::from_f64(limits.max_storage_buffers_per_shader_stage() as f64),
            )
            .unwrap();
        }
        if self.max_buffer_size {
            js_sys::Reflect::set(
                &obj,
                &"maxBufferSize".into(),
                &JsValue::from_f64(limits.max_buffer_size()),
            )
            .unwrap();
        }
        if self.max_storage_buffer_binding_size {
            js_sys::Reflect::set(
                &obj,
                &"maxStorageBufferBindingSize".into(),
                &JsValue::from_f64(limits.max_storage_buffer_binding_size()),
            )
            .unwrap();
        }

        if self.max_color_attachment_bytes_per_sample {
            js_sys::Reflect::set(
                &obj,
                &"maxColorAttachmentBytesPerSample".into(),
                &JsValue::from(limits.max_color_attachment_bytes_per_sample()),
            )
            .unwrap();
        }

        obj
    }
}
