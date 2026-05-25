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

/// WebGPU device and canvas context wrapper.
/// Relatively cheap to clone.
#[derive(Clone)]
pub struct AwsmRendererWebGpu {
    pub gpu: web_sys::Gpu,
    pub adapter: web_sys::GpuAdapter,
    pub device: web_sys::GpuDevice,
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

    /// The canvas the renderer was built against. Use this to branch
    /// safely between main-thread and worker-mode code paths instead
    /// of calling `canvas()` (which panics in worker mode).
    pub fn canvas_kind(&self) -> &CanvasKind {
        &self.canvas_kind
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
                if features.has(INDIRECT_FIRST_INSTANCE_FEATURE) {
                    let required: [js_sys::JsString; 1] =
                        [js_sys::JsString::from(INDIRECT_FIRST_INSTANCE_FEATURE)];
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
            context,
            canvas_kind: self.canvas,
        })
    }
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
