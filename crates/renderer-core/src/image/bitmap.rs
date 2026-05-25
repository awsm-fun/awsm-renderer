use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, ImageBitmap};

use crate::command::color::Color;
use crate::error::{AwsmCoreError, Result};
use crate::web_global;

use super::ImageBitmapOptions;

// let options = web_sys::ImageBitmapOptions::new();
// options.set_premultiply_alpha(web_sys::PremultiplyAlpha::None);
#[cfg(feature = "image")]
pub async fn load(
    url: String,
    options: Option<ImageBitmapOptions>,
) -> Result<web_sys::ImageBitmap> {
    let resp: web_sys::Response = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| AwsmCoreError::Fetch(e.to_string()))?
        .into();
    let js_value = JsFuture::from(resp.blob().map_err(AwsmCoreError::fetch)?)
        .await
        .map_err(AwsmCoreError::fetch)?;
    let blob: Blob = js_value.unchecked_into();

    load_blob(&blob, options).await
}

pub async fn load_blob(
    blob: &Blob,
    options: Option<ImageBitmapOptions>,
) -> Result<web_sys::ImageBitmap> {
    let promise = match options {
        Some(options) => web_global::create_image_bitmap_with_blob_and_image_bitmap_options(
            blob,
            &options.into(),
        ),
        None => web_global::create_image_bitmap_with_blob(blob),
    }
    .map_err(AwsmCoreError::create_image_bitmap)?;
    let js_value = JsFuture::from(promise)
        .await
        .map_err(AwsmCoreError::create_image_bitmap)?;
    Ok(js_value.unchecked_into())
}

pub async fn load_js_value(
    data: &JsValue,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    let blob_opts = BlobPropertyBag::new();
    blob_opts.set_type(mime_type);

    let blob = Blob::new_with_buffer_source_sequence_and_options(
        &js_sys::Array::of1(data).into(),
        &blob_opts,
    )
    .map_err(AwsmCoreError::create_image_bitmap)?;

    load_blob(&blob, options).await
}

pub async fn load_u8<T: AsRef<[u8]>>(
    data: T,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    load_js_value(
        // should be fine, load_js_value is just getting a blob with a new url string
        unsafe { &js_sys::Uint8Array::view(data.as_ref()).into() },
        mime_type,
        options,
    )
    .await
}

pub async fn load_u16<T: AsRef<[u16]>>(
    data: T,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    load_js_value(
        // should be fine, load_js_value is just getting a blob with a new url string
        unsafe { &js_sys::Uint16Array::view(data.as_ref()).into() },
        mime_type,
        options,
    )
    .await
}

pub async fn load_u32<T: AsRef<[u32]>>(
    data: T,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    load_js_value(
        // should be fine, load_js_value is just getting a blob with a new url string
        unsafe { &js_sys::Uint32Array::view(data.as_ref()).into() },
        mime_type,
        options,
    )
    .await
}

pub async fn load_i8<T: AsRef<[i8]>>(
    data: T,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    load_js_value(
        // should be fine, load_js_value is just getting a blob with a new url string
        unsafe { &js_sys::Int8Array::view(data.as_ref()).into() },
        mime_type,
        options,
    )
    .await
}

pub async fn load_i16<T: AsRef<[i16]>>(
    data: T,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    load_js_value(
        // should be fine, load_js_value is just getting a blob with a new url string
        unsafe { &js_sys::Int16Array::view(data.as_ref()).into() },
        mime_type,
        options,
    )
    .await
}

pub async fn load_i32<T: AsRef<[i32]>>(
    data: T,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    load_js_value(
        // should be fine, load_js_value is just getting a blob with a new url string
        unsafe { &js_sys::Int32Array::view(data.as_ref()).into() },
        mime_type,
        options,
    )
    .await
}

pub async fn load_f32<T: AsRef<[f32]>>(
    data: T,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    load_js_value(
        // should be fine, load_js_value is just getting a blob with a new url string
        unsafe { &js_sys::Float32Array::view(data.as_ref()).into() },
        mime_type,
        options,
    )
    .await
}

pub async fn load_f64<T: AsRef<[f64]>>(
    data: T,
    mime_type: &str,
    options: Option<ImageBitmapOptions>,
) -> Result<ImageBitmap> {
    load_js_value(
        // should be fine, load_js_value is just getting a blob with a new url string
        unsafe { &js_sys::Float64Array::view(data.as_ref()).into() },
        mime_type,
        options,
    )
    .await
}

pub async fn create_color(
    color: Color,
    width: u32,
    height: u32,
    options: Option<ImageBitmapOptions>,
) -> Result<web_sys::ImageBitmap> {
    // Convert f64 color values (0.0-1.0) to u8 (0-255)
    let r = (color.r.clamp(0.0, 1.0) * 255.0) as u8;
    let g = (color.g.clamp(0.0, 1.0) * 255.0) as u8;
    let b = (color.b.clamp(0.0, 1.0) * 255.0) as u8;
    let a = (color.a.clamp(0.0, 1.0) * 255.0) as u8;

    // Create RGBA pixel data
    let pixel_count = (width * height) as usize;
    let mut data = Vec::with_capacity(pixel_count * 4);

    for _ in 0..pixel_count {
        data.push(r);
        data.push(g);
        data.push(b);
        data.push(a);
    }

    // Create ImageData from the pixel data
    let uint8_array = js_sys::Uint8ClampedArray::from(data.as_slice());
    let image_data = web_sys::ImageData::new_with_js_u8_clamped_array(&uint8_array, width)
        .map_err(AwsmCoreError::create_image_bitmap)?;

    // Create ImageBitmap from ImageData
    let promise = match options {
        Some(options) => web_global::create_image_bitmap_with_image_data_and_image_bitmap_options(
            &image_data,
            &options.into(),
        ),
        None => web_global::create_image_bitmap_with_image_data(&image_data),
    }
    .map_err(AwsmCoreError::create_image_bitmap)?;

    let js_value = JsFuture::from(promise)
        .await
        .map_err(AwsmCoreError::create_image_bitmap)?;

    Ok(js_value.unchecked_into())
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn color_channel_to_u8(channel: f64) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub async fn create_vertical_gradient(
    top: Color,
    bottom: Color,
    width: u32,
    height: u32,
    options: Option<ImageBitmapOptions>,
) -> Result<web_sys::ImageBitmap> {
    let pixel_count = (width * height) as usize;
    let mut data = Vec::with_capacity(pixel_count * 4);

    let height_f = if height > 1 { (height - 1) as f64 } else { 1.0 };

    for y in 0..height {
        let t = if height_f == 0.0 {
            0.0
        } else {
            y as f64 / height_f
        };

        let r = color_channel_to_u8(lerp(top.r, bottom.r, t));
        let g = color_channel_to_u8(lerp(top.g, bottom.g, t));
        let b = color_channel_to_u8(lerp(top.b, bottom.b, t));
        let a = color_channel_to_u8(lerp(top.a, bottom.a, t));

        for _ in 0..width {
            data.push(r);
            data.push(g);
            data.push(b);
            data.push(a);
        }
    }

    let uint8_array = js_sys::Uint8ClampedArray::from(data.as_slice());
    let image_data = web_sys::ImageData::new_with_js_u8_clamped_array(&uint8_array, width)
        .map_err(AwsmCoreError::create_image_bitmap)?;

    let promise = match options {
        Some(options) => web_global::create_image_bitmap_with_image_data_and_image_bitmap_options(
            &image_data,
            &options.into(),
        ),
        None => web_global::create_image_bitmap_with_image_data(&image_data),
    }
    .map_err(AwsmCoreError::create_image_bitmap)?;

    let js_value = JsFuture::from(promise)
        .await
        .map_err(AwsmCoreError::create_image_bitmap)?;

    Ok(js_value.unchecked_into())
}

#[cfg(feature = "image")]
fn _same_origin(url: &str) -> Result<bool> {
    if url.starts_with("http://") || url.starts_with("https://") {
        // Worker scopes don't expose a `location.origin` directly; on
        // main-thread we read it from `window.location`. Same-origin
        // worker-mode check is deferred (returns true so the caller's
        // CORS branch doesn't reject unconditionally) — the worker
        // ships with its own origin anyway, and the consumer build
        // controls cross-origin fetches.
        let location_origin = match web_global::window() {
            Some(w) => w
                .location()
                .origin()
                .map_err(AwsmCoreError::location_origin)?,
            None => return Ok(true),
        };
        let url_origin = web_sys::Url::new(url)
            .map_err(AwsmCoreError::url_parse)?
            .origin();
        Ok(url_origin == location_origin)
    } else {
        Ok(true)
    }
}
