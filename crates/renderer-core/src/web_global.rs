//! Worker-safe global picker for `renderer-core`.
//!
//! Lower-level mirror of `awsm_renderer::web_global`: lives here so
//! `renderer-core` (and crates that depend on it like
//! `renderer-gltf`) can run inside an `OffscreenCanvas` worker
//! without panicking on `web_sys::window().unwrap()`.

use wasm_bindgen::{JsCast, JsValue};
use web_sys::js_sys;

/// `Some(Window)` on the main thread; `None` in a worker.
pub fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

/// `Some(DedicatedWorkerGlobalScope)` in a worker; `None` on the
/// main thread.
pub fn worker_scope() -> Option<web_sys::DedicatedWorkerGlobalScope> {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .ok()
}

/// `navigator.gpu` from whichever global is active.
pub fn navigator_gpu() -> Option<web_sys::Gpu> {
    if let Some(w) = window() {
        return Some(w.navigator().gpu());
    }
    if let Some(ws) = worker_scope() {
        return Some(ws.navigator().gpu());
    }
    None
}

/// Run `createImageBitmap(blob, options)` against whichever global is
/// active. Mirrors the `Window.create_image_bitmap_with_blob*` API on
/// `DedicatedWorkerGlobalScope`.
pub fn create_image_bitmap_with_blob(blob: &web_sys::Blob) -> Result<js_sys::Promise, JsValue> {
    if let Some(w) = window() {
        return w.create_image_bitmap_with_blob(blob);
    }
    if let Some(ws) = worker_scope() {
        return ws.create_image_bitmap_with_blob(blob);
    }
    Err(JsValue::from_str(
        "create_image_bitmap_with_blob: no main-thread Window or DedicatedWorkerGlobalScope",
    ))
}

/// Same as [`create_image_bitmap_with_blob`] with explicit
/// `ImageBitmapOptions`.
pub fn create_image_bitmap_with_blob_and_image_bitmap_options(
    blob: &web_sys::Blob,
    options: &web_sys::ImageBitmapOptions,
) -> Result<js_sys::Promise, JsValue> {
    if let Some(w) = window() {
        return w.create_image_bitmap_with_blob_and_image_bitmap_options(blob, options);
    }
    if let Some(ws) = worker_scope() {
        return ws.create_image_bitmap_with_blob_and_image_bitmap_options(blob, options);
    }
    Err(JsValue::from_str(
        "create_image_bitmap_with_blob_and_image_bitmap_options: no main-thread Window or DedicatedWorkerGlobalScope",
    ))
}

/// `createImageBitmap(imageData)`.
pub fn create_image_bitmap_with_image_data(
    image_data: &web_sys::ImageData,
) -> Result<js_sys::Promise, JsValue> {
    if let Some(w) = window() {
        return w.create_image_bitmap_with_image_data(image_data);
    }
    if let Some(ws) = worker_scope() {
        return ws.create_image_bitmap_with_image_data(image_data);
    }
    Err(JsValue::from_str(
        "create_image_bitmap_with_image_data: no main-thread Window or DedicatedWorkerGlobalScope",
    ))
}

/// `createImageBitmap(imageData, options)`.
pub fn create_image_bitmap_with_image_data_and_image_bitmap_options(
    image_data: &web_sys::ImageData,
    options: &web_sys::ImageBitmapOptions,
) -> Result<js_sys::Promise, JsValue> {
    if let Some(w) = window() {
        return w.create_image_bitmap_with_image_data_and_image_bitmap_options(image_data, options);
    }
    if let Some(ws) = worker_scope() {
        return ws
            .create_image_bitmap_with_image_data_and_image_bitmap_options(image_data, options);
    }
    Err(JsValue::from_str(
        "create_image_bitmap_with_image_data_and_image_bitmap_options: no main-thread Window or DedicatedWorkerGlobalScope",
    ))
}
