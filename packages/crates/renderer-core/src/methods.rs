//! Convenience methods for WebGPU operations.

use crate::{
    buffers::{extract_buffer_vec, BufferDescriptor, BufferUsage},
    configuration::CanvasConfiguration,
    data::JsData,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::{
    command::CommandEncoder,
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::TextureFormat,
};

impl AwsmRendererWebGpu {
    /// Returns the underlying canvas element.
    ///
    /// **Main-thread only.** Panics with a clear message when called
    /// on a renderer built via [`crate::renderer::AwsmRendererWebGpuBuilder::new_with_offscreen_canvas`]
    /// — `OffscreenCanvas` is not an `HtmlCanvasElement` and most
    /// of this type's downstream accessors (`get_bounding_client_rect`,
    /// pointer-event coord conversion, CSS sync) reach for DOM APIs
    /// that don't exist on `OffscreenCanvas`. Use
    /// [`AwsmRendererWebGpu::canvas_kind`] to branch safely.
    pub fn canvas(&self) -> web_sys::HtmlCanvasElement {
        match self.canvas_kind() {
            crate::renderer::CanvasKind::Html(c) => c.clone(),
            crate::renderer::CanvasKind::Offscreen(_) => panic!(
                "AwsmRendererWebGpu::canvas() called in worker (OffscreenCanvas) mode — \
                 this method is HtmlCanvasElement-only. Use canvas_kind() to branch."
            ),
        }
    }

    /// Returns the canvas size.
    ///
    /// # Parameters
    /// * `css_pixels`
    /// - If `true`, returns the CSS display size (the size as shown in the browser).
    /// - If `false`, returns the backing buffer size (the actual pixel buffer dimensions).
    ///
    /// # Usage
    /// - Use `canvas_size(true)` for UI layout and CSS-based calculations
    /// - Use `canvas_size(false)` (default) for rendering, transforms, and coordinate conversions
    ///   where you need the actual buffer dimensions
    ///
    /// # Worker-mode caveat
    /// `css_pixels = true` is main-thread-only (CSS pixels are a DOM
    /// concept) and will panic in worker (`OffscreenCanvas`) mode.
    /// `css_pixels = false` works in both modes — backing-buffer
    /// `width()` / `height()` exist on both canvas kinds.
    ///
    /// # Examples
    /// ```ignore
    /// // Get backing buffer size for rendering
    /// let (width, height) = renderer.canvas_size(false);
    ///
    /// // Get CSS display size for layout (main-thread only)
    /// let (css_width, css_height) = renderer.canvas_size(true);
    /// ```
    pub fn canvas_size(&self, css_pixels: bool) -> (f64, f64) {
        if css_pixels {
            // CSS pixels — DOM only.
            let canvas = self.canvas();
            let rect = canvas.get_bounding_client_rect();
            (rect.width(), rect.height())
        } else {
            // Backing buffer — works on both HtmlCanvasElement + OffscreenCanvas.
            match self.canvas_kind() {
                crate::renderer::CanvasKind::Html(c) => (c.width() as f64, c.height() as f64),
                crate::renderer::CanvasKind::Offscreen(c) => (c.width() as f64, c.height() as f64),
            }
        }
    }

    /// Syncs the canvas backing buffer size with the CSS display size.
    ///
    /// **Main-thread only.** Reaches for `get_bounding_client_rect`
    /// which doesn't exist on `OffscreenCanvas`; panics in worker mode
    /// (`canvas()` does the panic). In worker mode the host shim is
    /// responsible for the equivalent — see the
    /// [`WorkerInputEvent::Resize`][rwe] convention in the
    /// `render-worker` example.
    ///
    /// Returns true if the size was updated, false if it was already in sync
    /// or the CSS size is invalid (zero or negative).
    ///
    /// [rwe]: ../../../examples/render-worker/src/lib.rs
    pub fn sync_canvas_buffer_with_css(&self) -> bool {
        let canvas = self.canvas();
        let rect = canvas.get_bounding_client_rect();
        let css_width = rect.width();
        let css_height = rect.height();

        if css_width <= 0.0 || css_height <= 0.0 {
            return false;
        }

        let buffer_width = canvas.width() as f64;
        let buffer_height = canvas.height() as f64;

        // Check if sizes differ (with small tolerance for floating point)
        if (buffer_width - css_width).abs() > 0.5 || (buffer_height - css_height).abs() > 0.5 {
            canvas.set_width(css_width as u32);
            canvas.set_height(css_height as u32);
            true
        } else {
            false
        }
    }

    /// Returns the currently configured canvas format.
    pub fn current_context_format(&self) -> TextureFormat {
        self.context
            .get_configuration()
            .as_ref()
            .unwrap()
            .get_format()
    }

    /// Returns the current swap chain texture.
    pub fn current_context_texture(&self) -> Result<web_sys::GpuTexture> {
        // fine to call this often, from spec https://gpuweb.github.io/gpuweb/#dom-gpucanvascontext-getcurrenttexture
        // "Note: The same GPUTexture object will be returned by every call to getCurrentTexture()
        // until 'Expire the current texture' runs [...]"
        self.context
            .get_current_texture()
            .map_err(AwsmCoreError::current_context_texture)
    }

    /// Returns the current swap chain texture size.
    pub fn current_context_texture_size(&self) -> Result<(u32, u32)> {
        let texture = self.current_context_texture()?;
        Ok((texture.width(), texture.height()))
    }

    /// Returns a view for the current swap chain texture.
    pub fn current_context_texture_view(&self) -> Result<web_sys::GpuTextureView> {
        let texture = self.current_context_texture()?;

        texture
            .create_view()
            .map_err(AwsmCoreError::current_context_texture_view)
    }

    /// Example usage:
    /// let descriptor:ShaderModuleDescriptor = ...;
    /// renderer.compile_shader(&descriptor.into());
    pub fn compile_shader(
        &self,
        shader_code: &web_sys::GpuShaderModuleDescriptor,
    ) -> web_sys::GpuShaderModule {
        self.device.create_shader_module(shader_code)
    }

    /// Example usage:
    /// let descriptor:RenderPipelineDescriptor = ...;
    /// renderer.create_render_pipeline(&descriptor.into());
    pub async fn create_render_pipeline(
        &self,
        descriptor: &web_sys::GpuRenderPipelineDescriptor,
    ) -> Result<web_sys::GpuRenderPipeline> {
        let pipeline: web_sys::GpuRenderPipeline =
            JsFuture::from(self.create_render_pipeline_promise(descriptor))
                .await
                .map_err(AwsmCoreError::pipeline_creation)?
                .unchecked_into();

        Ok(pipeline)
    }

    /// Sync-issue variant of [`Self::create_render_pipeline`]. Returns
    /// the raw `js_sys::Promise` that `createRenderPipelineAsync`
    /// returned — Dawn has *already begun* compiling by the time this
    /// returns. Used by batched-prewarm paths that want to fire N
    /// `createRenderPipelineAsync` calls back-to-back (so Dawn's
    /// compile pool parallelises them) before awaiting any of them.
    /// See `RenderPipelines::ensure_keys`.
    pub fn create_render_pipeline_promise(
        &self,
        descriptor: &web_sys::GpuRenderPipelineDescriptor,
    ) -> js_sys::Promise<web_sys::GpuRenderPipeline> {
        self.device.create_render_pipeline_async(descriptor)
    }

    /// Example usage:
    /// let descriptor:ComputePipelineDescriptor = ...;
    /// renderer.create_compute_pipeline(&descriptor.into());
    pub async fn create_compute_pipeline(
        &self,
        descriptor: &web_sys::GpuComputePipelineDescriptor,
    ) -> Result<web_sys::GpuComputePipeline> {
        let pipeline: web_sys::GpuComputePipeline =
            JsFuture::from(self.create_compute_pipeline_promise(descriptor))
                .await
                .map_err(AwsmCoreError::pipeline_creation)?
                .unchecked_into();

        Ok(pipeline)
    }

    /// Sync-issue variant of [`Self::create_compute_pipeline`]. See
    /// `create_render_pipeline_promise` for the rationale.
    pub fn create_compute_pipeline_promise(
        &self,
        descriptor: &web_sys::GpuComputePipelineDescriptor,
    ) -> js_sys::Promise<web_sys::GpuComputePipeline> {
        self.device.create_compute_pipeline_async(descriptor)
    }

    /// Example usage:
    /// let descriptor:PipelineLayoutDescriptor = ...;
    /// renderer.create_pipeline_layout(&descriptor.into());
    pub fn create_pipeline_layout(
        &self,
        descriptor: &web_sys::GpuPipelineLayoutDescriptor,
    ) -> web_sys::GpuPipelineLayout {
        self.device.create_pipeline_layout(descriptor)
    }

    /// Example usage:
    /// let descriptor:BindGroupLayoutDescriptor = ...;
    /// renderer.create_bind_group_layout(&descriptor.into());
    pub fn create_bind_group_layout(
        &self,
        descriptor: &web_sys::GpuBindGroupLayoutDescriptor,
    ) -> Result<web_sys::GpuBindGroupLayout> {
        self.device
            .create_bind_group_layout(descriptor)
            .map_err(AwsmCoreError::bind_group_layout)
    }

    /// Example usage:
    /// let descriptor:BindGroupDescriptor = ...;
    /// renderer.create_bind_group(&descriptor.into());
    pub fn create_bind_group(
        &self,
        descriptor: &web_sys::GpuBindGroupDescriptor,
    ) -> web_sys::GpuBindGroup {
        #[cfg(any(debug_assertions, feature = "harden-diag"))]
        crate::CREATE_BIND_GROUP_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.device.create_bind_group(descriptor)
    }

    /// Example usage:
    /// let descriptor:SamplerDescriptor = ...;
    /// renderer.create_sampler(Some(&descriptor.into()));
    pub fn create_sampler(
        &self,
        descriptor: Option<&web_sys::GpuSamplerDescriptor>,
    ) -> web_sys::GpuSampler {
        match descriptor {
            Some(descriptor) => self.device.create_sampler_with_descriptor(descriptor),
            None => self.device.create_sampler(),
        }
    }

    /// Example usage:
    /// let descriptor:TextureDescriptor = ...;
    /// renderer.create_texture(&descriptor.into());
    /// Creates a GPU texture from a descriptor.
    pub fn create_texture(
        &self,
        descriptor: &web_sys::GpuTextureDescriptor,
    ) -> Result<web_sys::GpuTexture> {
        self.device
            .create_texture(descriptor)
            .map_err(AwsmCoreError::texture_creation)
    }

    /// Copies an external image into a texture.
    /// Typically this is called via `ImageData::to_texture(&gpu)`.
    pub fn copy_external_image_to_texture(
        &self,
        source: &web_sys::GpuCopyExternalImageSourceInfo,
        dest: &web_sys::GpuCopyExternalImageDestInfo,
        size: &web_sys::GpuExtent3dDict,
    ) -> Result<()> {
        self.device
            .queue()
            .copy_external_image_to_texture_with_gpu_extent_3d_dict(source, dest, size)
            .map_err(AwsmCoreError::copy_external_image_to_texture)
    }

    /// Example usage:
    /// let descriptor:BufferDescriptor = ...;
    /// renderer.create_buffer(&descriptor.into());
    /// Creates a GPU buffer from a descriptor.
    pub fn create_buffer(
        &self,
        descriptor: &web_sys::GpuBufferDescriptor,
    ) -> Result<web_sys::GpuBuffer> {
        // Oversized-allocation guard. A single GPU buffer near 2 GiB almost
        // certainly means a runaway size computation (a `* 0` count flipped, an
        // overflow). Cold path (buffer creation, not per frame).
        let size = js_sys::Reflect::get(
            descriptor.as_ref(),
            &wasm_bindgen::JsValue::from_str("size"),
        )
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

        // Hard cap, always on (release included): a request at/above
        // MAX_GPU_BUFFER_BYTES would hit PartitionAlloc's ~2 GiB ceiling and
        // abort the whole renderer process with a deliberate `IMMEDIATE_CRASH`
        // — which is NOT a catchable WebGPU validation error. Reject it here so
        // the load fails as a recoverable `Result` at our call site instead.
        if size >= crate::MAX_GPU_BUFFER_BYTES as f64 {
            tracing::error!(
                target: "awsm_renderer_core::oversized_alloc",
                "create_buffer refused {} bytes (>= {} hard cap) — would abort the renderer in PartitionAlloc",
                size,
                crate::MAX_GPU_BUFFER_BYTES
            );
            return Err(AwsmCoreError::oversized_buffer_allocation(
                size as u64,
                crate::MAX_GPU_BUFFER_BYTES,
            ));
        }

        // Soft diagnostic threshold: surface "getting suspiciously big" early in
        // dev (proceeds with the allocation). Tripping a `debug_assert!` here
        // points at OUR call site with a stack.
        #[cfg(any(debug_assertions, feature = "harden-diag"))]
        if size > crate::OVERSIZED_ALLOC_BYTES as f64 {
            tracing::warn!(
                target: "awsm_renderer_core::oversized_alloc",
                "create_buffer requested {} bytes (> {} soft threshold) — likely a runaway size computation",
                size,
                crate::OVERSIZED_ALLOC_BYTES
            );
            debug_assert!(
                size <= crate::OVERSIZED_ALLOC_BYTES as f64,
                "oversized GPU buffer allocation: {size} bytes"
            );
        }

        let buffer = self
            .device
            .create_buffer(descriptor)
            .map_err(AwsmCoreError::buffer_creation)?;

        // Cumulative buffer-creation census for the memory-leak soak (see
        // crate::CREATE_BUFFER_COUNT). Increment-only; two relaxed atomic adds
        // on a cold-ish path (buffer creation, not a per-byte hot loop). Gated to
        // dev/harden-diag so a release build carries zero always-on cost — the
        // soak runs a dev build, and the `create_buffer_census()` accessor stays
        // defined either way (reads 0 in release).
        #[cfg(any(debug_assertions, feature = "harden-diag"))]
        {
            use std::sync::atomic::Ordering::Relaxed;
            crate::CREATE_BUFFER_COUNT.fetch_add(1, Relaxed);
            crate::CREATE_BUFFER_BYTES.fetch_add(size as u64, Relaxed);
        }

        Ok(buffer)
    }

    /// Example usage:
    /// let encoder = renderer.create_command_encoder(Some("My Encoder"));
    /// let render_pass = command_encoder.begin_render_pass(
    ///     &RenderPassDescriptor {
    ///         color_attachments: vec![ColorAttachment::new(
    ///             &renderer.gpu.current_context_texture_view()?,
    ///             LoadOp::Clear,
    ///             StoreOp::Store,
    ///         )],
    ///         ..Default::default()
    ///     }
    ///     .into()
    /// );
    ///
    /// render_pass.set_pipeline(&pipeline);
    /// render_pass.draw(3);
    /// render_pass.end();
    /// self.gpu.submit_commands(&command_encoder.finish());
    /// Creates a command encoder with an optional label.
    pub fn create_command_encoder(&self, label: Option<&str>) -> CommandEncoder {
        #[cfg(any(debug_assertions, feature = "harden-diag"))]
        crate::CREATE_COMMAND_ENCODER_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let encoder = match label {
            None => self.device.create_command_encoder(),
            Some(label) => {
                let descriptor = web_sys::GpuCommandEncoderDescriptor::new();
                descriptor.set_label(label);
                self.device
                    .create_command_encoder_with_descriptor(&descriptor)
            }
        };

        CommandEncoder::new(encoder)
    }

    /// Record into the shared per-frame **upload** command encoder,
    /// creating it lazily on first use this frame.
    ///
    /// Per-frame buffer uploads call this instead of
    /// `create_command_encoder` + `submit_commands`: their
    /// `copyBufferToBuffer` commands all land in ONE encoder that is
    /// finished + submitted exactly once, at the next `submit_commands`
    /// (see [`Self::flush_upload_encoder`]). Collapses ~4–5 per-frame
    /// encoder-create/submit pairs into one — the WebGPU-recommended
    /// shape and the fix for the per-frame churn behind the VA leak.
    ///
    /// The closure receives the shared encoder; its return value is
    /// passed straight back so callers can propagate a `Result`.
    pub fn record_upload<R>(&self, f: impl FnOnce(&CommandEncoder) -> R) -> R {
        let mut guard = self.upload_encoder.borrow_mut();
        if guard.is_none() {
            *guard = Some(self.create_command_encoder(Some("upload-shared")));
        }
        // `unwrap` is sound: we just ensured `Some` above and hold the
        // borrow, so nothing can take it out from under us.
        f(guard.as_ref().unwrap())
    }

    /// Number of times the shared upload encoder has been flushed
    /// (finished + submitted). See [`Self::flush_upload_encoder`] and the
    /// `upload_flush_epoch` field doc — the staging ring reads this to
    /// know when a slot's copy has actually reached the queue.
    pub fn upload_flush_epoch(&self) -> u64 {
        self.upload_flush_epoch.get()
    }

    /// Finish + submit the shared upload encoder if any copies are
    /// pending, then bump [`Self::upload_flush_epoch`]. No-op when the
    /// encoder is empty (nothing recorded since the last flush), so
    /// calling it before every submit is cheap.
    ///
    /// Submits the upload buffer as its own command buffer *ahead of*
    /// whatever the caller is about to submit — WebGPU executes queue
    /// submissions in order, so the copies land before any pass that
    /// reads the uploaded data, exactly as when each upload submitted its
    /// own encoder. Submits directly via the queue (not `submit_commands`)
    /// to avoid recursing back into the flush.
    pub fn flush_upload_encoder(&self) {
        let encoder = self.upload_encoder.borrow_mut().take();
        if let Some(encoder) = encoder {
            let command_buffer = encoder.finish();
            self.device
                .queue()
                .submit(std::slice::from_ref(&command_buffer));
            self.upload_flush_epoch
                .set(self.upload_flush_epoch.get().wrapping_add(1));
        }
    }

    /// See [`Self::create_command_encoder`] for usage.
    /// Submits a single command buffer.
    pub fn submit_commands(&self, command_buffer: &web_sys::GpuCommandBuffer) {
        // Flush any pending per-frame uploads FIRST so their copies are
        // ordered ahead of the passes in `command_buffer` that read them.
        self.flush_upload_encoder();
        self.device
            .queue()
            .submit(std::slice::from_ref(command_buffer));
    }

    /// See [`Self::create_command_encoder`] for usage.
    /// Submits a batch of command buffers.
    pub fn submit_commands_batch<'a>(
        &self,
        command_buffers: impl IntoIterator<Item = &'a web_sys::GpuCommandBuffer>,
    ) {
        // Flush pending uploads ahead of the batch (see `submit_commands`).
        self.flush_upload_encoder();
        let command_buffers_js: Vec<web_sys::GpuCommandBuffer> =
            command_buffers.into_iter().cloned().collect();
        self.device.queue().submit(&command_buffers_js);
    }

    // pretty much a direct pass-through, just a bit nicer
    /// Creates a query set.
    pub fn create_query_set(
        &self,
        query_type: web_sys::GpuQueryType,
        count: u32,
        label: Option<&str>,
    ) -> Result<web_sys::GpuQuerySet> {
        let descriptor = web_sys::GpuQuerySetDescriptor::new(count, query_type);

        if let Some(label) = label {
            descriptor.set_label(label);
        }

        self.device
            .create_query_set(&descriptor)
            .map_err(AwsmCoreError::query_set_creation)
    }

    /// Example usage:
    /// let descriptor:ExternalTextureDescriptor = ...;
    /// renderer.import_external_texture(&descriptor.into());
    /// Imports an external texture.
    pub fn import_external_texture(
        &self,
        descriptor: &web_sys::GpuExternalTextureDescriptor,
    ) -> Result<web_sys::GpuExternalTexture> {
        self.device
            .import_external_texture(descriptor)
            .map_err(AwsmCoreError::external_texture_creation)
    }

    /// Example usage:
    /// let data: &[u8] = ...;
    /// renderer.write_buffer(buffer, None, data, None, None);
    /// Writes data into a GPU buffer.
    #[allow(private_bounds)]
    pub fn write_buffer<'a>(
        &self,
        buffer: &web_sys::GpuBuffer,
        buffer_offset: Option<usize>,
        data: impl Into<JsData<'a>>,
        // This value is a number of elements if data is a TypedArray, and a number of bytes if not
        data_offset: Option<usize>,
        // This value is a number of elements if data is a TypedArray, and a number of bytes if not
        data_size: Option<usize>,
    ) -> Result<()> {
        // https://developer.mozilla.org/en-US/docs/Web/API/GPUQueue/writeBuffer

        let data = data.into();

        match data {
            JsData::SliceU8(data) => match (data_offset, data_size) {
                (None, None) => self.device.queue().write_buffer_with_f64_and_u8_slice(
                    buffer,
                    buffer_offset.unwrap_or(0) as f64,
                    data,
                ),
                (Some(data_offset), Some(data_size)) => self
                    .device
                    .queue()
                    .write_buffer_with_f64_and_u8_slice_and_f64_and_f64(
                        buffer,
                        buffer_offset.unwrap_or(0) as f64,
                        data,
                        data_offset as f64,
                        data_size as f64,
                    ),
                (Some(data_offset), None) => self
                    .device
                    .queue()
                    .write_buffer_with_f64_and_u8_slice_and_f64(
                        buffer,
                        buffer_offset.unwrap_or(0) as f64,
                        data,
                        data_offset as f64,
                    ),
                (None, Some(data_size)) => self
                    .device
                    .queue()
                    .write_buffer_with_f64_and_u8_slice_and_f64_and_f64(
                        buffer,
                        buffer_offset.unwrap_or(0) as f64,
                        data,
                        0.0,
                        data_size as f64,
                    ),
            },
            _ => match (data_offset, data_size) {
                (None, None) => self.device.queue().write_buffer_with_f64_and_buffer_source(
                    buffer,
                    buffer_offset.unwrap_or(0) as f64,
                    data.as_js_value_ref().unchecked_ref(),
                ),
                (Some(data_offset), Some(data_size)) => self
                    .device
                    .queue()
                    .write_buffer_with_f64_and_buffer_source_and_f64_and_f64(
                        buffer,
                        buffer_offset.unwrap_or(0) as f64,
                        data.as_js_value_ref().unchecked_ref(),
                        data_offset as f64,
                        data_size as f64,
                    ),
                (Some(data_offset), None) => self
                    .device
                    .queue()
                    .write_buffer_with_f64_and_buffer_source_and_f64(
                        buffer,
                        buffer_offset.unwrap_or(0) as f64,
                        data.as_js_value_ref().unchecked_ref(),
                        data_offset as f64,
                    ),
                (None, Some(data_size)) => self
                    .device
                    .queue()
                    .write_buffer_with_f64_and_buffer_source_and_f64_and_f64(
                        buffer,
                        buffer_offset.unwrap_or(0) as f64,
                        data.as_js_value_ref().unchecked_ref(),
                        0.0,
                        data_size as f64,
                    ),
            },
        }
        .map_err(AwsmCoreError::buffer_write)
    }

    /// Example usage:
    /// let destination:TexelCopyTextureInfo = ...;
    /// let layout: TexelCopyBufferLayout = ...;
    /// let size: Extent3d = ...;
    /// let data: &[u8] = ...;
    /// renderer.write_texture(&destination.into(), data, &layout.into(), &size.into());
    /// Writes data into a GPU texture.
    #[allow(private_bounds)]
    pub fn write_texture<'a>(
        &self,
        destination: &web_sys::GpuTexelCopyTextureInfo,
        data: impl Into<JsData<'a>>,
        layout: &web_sys::GpuTexelCopyBufferLayout,
        size: &web_sys::GpuExtent3dDict,
    ) -> Result<()> {
        // https://developer.mozilla.org/en-US/docs/Web/API/GPUQueue/writeTexture

        let data = data.into();
        match data {
            JsData::SliceU8(data) => self
                .device
                .queue()
                .write_texture_with_u8_slice_and_gpu_extent_3d_dict(
                    destination,
                    data,
                    layout,
                    size,
                ),
            _ => self
                .device
                .queue()
                .write_texture_with_buffer_source_and_gpu_extent_3d_dict(
                    destination,
                    data.as_js_value_ref().unchecked_ref(),
                    layout,
                    size,
                ),
        }
        .map_err(AwsmCoreError::texture_write)
    }

    /// Configures the canvas with an optional configuration override.
    pub fn configure(&mut self, configuration: Option<CanvasConfiguration>) -> Result<()> {
        self.context
            .configure(
                &configuration
                    .unwrap_or_default()
                    .into_js(&self.gpu, &self.device),
            )
            .map_err(AwsmCoreError::context_configuration)?;
        Ok(())
    }

    /// Copies GPU buffer data into a new mapped buffer and returns it as a `Vec<u8>`
    pub async fn new_copy_and_extract_buffer(
        &self,
        source: &web_sys::GpuBuffer,
        size: Option<u32>,
    ) -> Result<Vec<u8>> {
        let size = size.unwrap_or(source.size() as u32);
        // Create a staging buffer with MAP_READ and COPY_DST usage
        let read_buffer = self.create_buffer(
            &BufferDescriptor::new(
                Some("buffer extractor"),
                size as usize,
                BufferUsage::new().with_map_read().with_copy_dst(),
            )
            .into(),
        )?;

        // Encode command to copy source → read_buffer
        let encoder = self.device.create_command_encoder();
        encoder
            .copy_buffer_to_buffer_with_u32_and_u32_and_u32(source, 0, &read_buffer, 0, size)
            .map_err(AwsmCoreError::buffer_copy)?;
        let command_buffer = encoder.finish();
        self.submit_commands(&command_buffer);

        extract_buffer_vec(&read_buffer, Some(size)).await
    }

    /// Converts a pointer event to canvas coordinates in backing buffer pixels (f64).
    ///
    /// This method takes pointer event coordinates (which are in CSS pixels relative to the viewport)
    /// and converts them to backing buffer pixel coordinates, accounting for the canvas's position
    /// and the scaling between CSS pixels and backing buffer pixels.
    ///
    /// **Main-thread only** — `PointerEvent` is a DOM type and the
    /// CSS-to-buffer math uses `get_bounding_client_rect`. Worker-mode
    /// consumers should forward pre-converted backing-buffer coords
    /// from the main-thread shim (see `WorkerInputEvent::PointerMove`
    /// in the `render-worker` example).
    pub fn pointer_event_to_canvas_coords_f64(&self, evt: &web_sys::PointerEvent) -> (f64, f64) {
        let canvas = self.canvas();
        let rect = canvas.get_bounding_client_rect();

        // CSS pixels relative to the canvas' top-left
        let css_x = evt.client_x() - rect.left();
        let css_y = evt.client_y() - rect.top();

        // Get CSS and backing buffer sizes
        let (css_w, css_h) = self.canvas_size(true);
        let (buffer_w, buffer_h) = self.canvas_size(false);

        // Avoid division by zero if the element is not laid out (display:none etc.)
        let css_w = css_w.max(1.0);
        let css_h = css_h.max(1.0);

        // Convert CSS pixels -> backing buffer pixels
        let scale_x = buffer_w / css_w;
        let scale_y = buffer_h / css_h;

        let x = css_x * scale_x;
        let y = css_y * scale_y;

        (x, y)
    }

    /// Converts a pointer event to canvas coordinates in backing buffer pixels (i32).
    ///
    /// This method is similar to `pointer_event_to_canvas_coords_f64` but returns integer coordinates
    /// clamped to the canvas bounds. Useful for pixel-perfect operations like reading specific pixels
    /// or texel access.
    pub fn pointer_event_to_canvas_coords_i32(&self, evt: &web_sys::PointerEvent) -> (i32, i32) {
        let (x, y) = self.pointer_event_to_canvas_coords_f64(evt);

        // Get backing buffer size for clamping bounds
        let (w, h) = self.canvas_size(false);
        let w = w.max(1.0) as i64;
        let h = h.max(1.0) as i64;

        // Floor and clamp to canvas bounds
        let mut ix = x.floor() as i64;
        let mut iy = y.floor() as i64;

        if ix < 0 {
            ix = 0;
        }
        if iy < 0 {
            iy = 0;
        }
        if ix >= w {
            ix = w - 1;
        }
        if iy >= h {
            iy = h - 1;
        }

        (ix as i32, iy as i32)
    }
}
