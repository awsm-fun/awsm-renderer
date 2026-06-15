//! Read-only **image queries** — PNG snapshots of what the editor is showing.
//! Paired with the `EditorCommand` dispatch seam, these let an out-of-process
//! driver (a future MCP server) change state and then read back what it looks
//! like: orient a camera, snapshot the scene; re-shade a material, snapshot the
//! preview; inspect a texture.
//!
//! All three return a `data:image/png;base64,…` URL. WebGPU canvases only read
//! back reliably while they're actively presenting — both the main viewport and
//! the material preview render on a continuous RAF, so `toDataURL` returns the
//! latest frame.

use std::cell::RefCell;

use awsm_editor_protocol::{AssetSource, TextureDef};
use awsm_web_shared::prelude::Mutable;
use wasm_bindgen::{Clamped, JsCast};

use crate::controller::controller;
use crate::engine::scene::AssetId;

/// A captured scene frame: tightly packed display-space RGBA8 + dimensions.
type CapturedFrame = Result<(Vec<u8>, u32, u32), String>;

thread_local! {
    /// Pending scene-capture requests. The render loop fills these AFTER it
    /// renders a frame — the only point where the WebGPU swapchain texture holds
    /// the rendered content + is still the current texture (a `getCurrentTexture`
    /// on the next frame returns a fresh, blank one). `toDataURL`/`drawImage`
    /// return empty on a WebGPU canvas, so we GPU-copy the texture instead.
    static CAPTURE_REQUESTS: RefCell<Vec<Mutable<Option<CapturedFrame>>>> =
        const { RefCell::new(Vec::new()) };
}

/// Capture the current scene viewport as display-space RGBA8 `(rgba, w, h)`.
/// Registers a request the render loop fulfills on its next frame (`poll` below);
/// times out if no frame presents.
pub async fn capture_scene_rgba() -> CapturedFrame {
    let slot: Mutable<Option<CapturedFrame>> = Mutable::new(None);
    CAPTURE_REQUESTS.with(|q| q.borrow_mut().push(slot.clone()));
    // Poll on a frame cadence; the render loop drives `poll_scene_capture`.
    for _ in 0..120 {
        gloo_timers::future::TimeoutFuture::new(16).await;
        if let Some(result) = slot.get_cloned() {
            return result;
        }
    }
    Err("scene capture timed out (no frame presented)".to_string())
}

/// Called by the render loop right AFTER `render()` (swapchain texture valid):
/// fulfill any pending scene-capture requests by GPU-copying the current context
/// texture. The copy is submitted synchronously inside the spawned export's
/// pre-await section, before the browser presents — so it captures this frame.
pub fn poll_scene_capture(renderer: &awsm_renderer::AwsmRenderer) {
    let slots = CAPTURE_REQUESTS.with(|q| std::mem::take(&mut *q.borrow_mut()));
    if slots.is_empty() {
        return;
    }
    let gpu = renderer.gpu.clone();
    let texture = renderer.gpu.current_context_texture();
    let size = renderer.gpu.current_context_texture_size();
    let format = renderer.gpu.current_context_format();
    wasm_bindgen_futures::spawn_local(async move {
        let result: CapturedFrame = match (texture, size) {
            (Ok(texture), Ok((w, h))) => gpu
                .export_texture_as_rgba8(&texture, w, h, 0, format)
                .await
                .map(|rgba| (rgba, w, h))
                .map_err(|e| format!("{e}")),
            _ => Err("scene canvas not ready".to_string()),
        };
        for slot in slots {
            slot.set(Some(result.clone()));
        }
    });
}

/// PNG data URL of the **scene viewport** (rendered through the active camera).
/// GPU-reads the swapchain (a WebGPU canvas isn't `toDataURL`-readable), scaling
/// to `width`/`height` when given. `None` if no frame could be captured.
pub async fn scene_png(width: Option<u32>, height: Option<u32>) -> Option<String> {
    let (rgba, w, h) = match capture_scene_rgba().await {
        Ok(frame) => frame,
        Err(e) => {
            // Surfaced via console_logs — the relay's "no image available"
            // otherwise swallows the real failure (timeout vs export error).
            tracing::warn!("scene_png capture failed: {e}");
            return None;
        }
    };
    rgba_to_png_data_url_scaled(&rgba, w, h, width, height)
}

/// PNG data URL of the **material-mode preview** (the example sphere). `None`
/// when the Studio isn't mounted.
pub fn material_png(width: Option<u32>, height: Option<u32>) -> Option<String> {
    crate::engine::preview::preview_canvas().and_then(|c| canvas_png(&c, width, height))
}

/// Encode a live canvas to a PNG data URL, optionally scaling the output to
/// `width`/`height` (one given → preserve aspect; both → exact; neither → the
/// canvas's own size). Scaling samples the presented frame — it normalizes the
/// output size, it doesn't add detail beyond what the canvas rendered.
fn canvas_png(
    src: &web_sys::HtmlCanvasElement,
    width: Option<u32>,
    height: Option<u32>,
) -> Option<String> {
    if width.is_none() && height.is_none() {
        return src.to_data_url_with_type("image/png").ok();
    }
    let (sw, sh) = (src.width().max(1), src.height().max(1));
    let aspect = sw as f64 / sh as f64;
    let (w, h) = match (width, height) {
        (Some(w), Some(h)) => (w.max(1), h.max(1)),
        (Some(w), None) => (w.max(1), ((w as f64 / aspect).round() as u32).max(1)),
        (None, Some(h)) => (((h as f64 * aspect).round() as u32).max(1), h.max(1)),
        (None, None) => (sw, sh),
    };
    let document = web_sys::window()?.document()?;
    let off: web_sys::HtmlCanvasElement =
        document.create_element("canvas").ok()?.dyn_into().ok()?;
    off.set_width(w);
    off.set_height(h);
    let ctx: web_sys::CanvasRenderingContext2d = off.get_context("2d").ok()??.dyn_into().ok()?;
    ctx.draw_image_with_html_canvas_element_and_dw_and_dh(src, 0.0, 0.0, w as f64, h as f64)
        .ok()?;
    off.to_data_url_with_type("image/png").ok()
}

/// PNG data URL of a **texture asset** by id. Procedural textures are generated
/// CPU-side and encoded directly; raster (file/glTF) textures are read back from
/// the GPU and PNG-encoded by the renderer.
pub async fn texture_png(id: AssetId) -> Result<String, String> {
    enum Kind {
        Procedural(awsm_editor_protocol::ProceduralTextureDef),
        Raster,
    }
    let kind = {
        let ctrl = controller();
        let assets = ctrl.scene.assets.lock().unwrap();
        match assets.entries.get(&id).map(|e| &e.source) {
            Some(AssetSource::Texture(TextureDef::Procedural(p))) => Kind::Procedural(p.clone()),
            Some(AssetSource::Texture(TextureDef::Raster { .. })) => Kind::Raster,
            Some(_) => return Err("asset is not a texture".to_string()),
            None => return Err("no such asset".to_string()),
        }
    };
    match kind {
        Kind::Procedural(p) => {
            let (rgba, w, h) = crate::engine::bridge::material::procedural_rgba(&p);
            rgba_to_png_data_url(&rgba, w, h)
        }
        Kind::Raster => {
            let key = crate::engine::bridge::material::texture_key_for(id).ok_or_else(|| {
                "this texture isn't loaded on the GPU yet (assign it / its model first)".to_string()
            })?;
            let png = {
                let handle = crate::engine::context::renderer_handle();
                let r = handle.lock().await;
                r.texture_png_bytes(key).await.map_err(|e| format!("{e}"))?
            };
            Ok(format!("data:image/png;base64,{}", base64_encode(&png)))
        }
    }
}

/// Encode bytes to base64 via the browser's `btoa` (a "binary string" where each
/// char code is one byte). Avoids a native base64 dependency.
fn base64_encode(bytes: &[u8]) -> String {
    let bin: String = bytes.iter().map(|&b| b as char).collect();
    web_sys::window()
        .and_then(|w| w.btoa(&bin).ok())
        .unwrap_or_default()
}

/// Encode RGBA8 pixels to a PNG data URL via an offscreen 2D canvas (no native
/// PNG-encoder dependency — the browser does it).
fn rgba_to_png_data_url(rgba: &[u8], w: u32, h: u32) -> Result<String, String> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let canvas: web_sys::HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(|_| "create canvas")?
        .dyn_into()
        .map_err(|_| "canvas cast")?;
    canvas.set_width(w);
    canvas.set_height(h);
    let ctx: web_sys::CanvasRenderingContext2d = canvas
        .get_context("2d")
        .map_err(|_| "get 2d context")?
        .ok_or("no 2d context")?
        .dyn_into()
        .map_err(|_| "2d context cast")?;
    let image_data = web_sys::ImageData::new_with_u8_clamped_array_and_sh(Clamped(rgba), w, h)
        .map_err(|_| "build ImageData")?;
    ctx.put_image_data(&image_data, 0, 0)
        .map_err(|_| "put ImageData")?;
    canvas
        .to_data_url_with_type("image/png")
        .map_err(|_| "encode PNG".to_string())
}

/// Encode display-space RGBA8 to a PNG data URL via a 2D canvas, optionally
/// scaling to `target_w`/`target_h` (one given → preserve aspect).
fn rgba_to_png_data_url_scaled(
    rgba: &[u8],
    w: u32,
    h: u32,
    target_w: Option<u32>,
    target_h: Option<u32>,
) -> Option<String> {
    let document = web_sys::window()?.document()?;
    let src: web_sys::HtmlCanvasElement =
        document.create_element("canvas").ok()?.dyn_into().ok()?;
    src.set_width(w);
    src.set_height(h);
    let sctx: web_sys::CanvasRenderingContext2d = src.get_context("2d").ok()??.dyn_into().ok()?;
    let image_data =
        web_sys::ImageData::new_with_u8_clamped_array_and_sh(Clamped(rgba), w, h).ok()?;
    sctx.put_image_data(&image_data, 0, 0).ok()?;
    if target_w.is_none() && target_h.is_none() {
        return src.to_data_url_with_type("image/png").ok();
    }
    let aspect = w as f64 / h as f64;
    let (tw, th) = match (target_w, target_h) {
        (Some(a), Some(b)) => (a.max(1), b.max(1)),
        (Some(a), None) => (a.max(1), ((a as f64 / aspect).round() as u32).max(1)),
        (None, Some(b)) => (((b as f64 * aspect).round() as u32).max(1), b.max(1)),
        (None, None) => (w, h),
    };
    let off: web_sys::HtmlCanvasElement =
        document.create_element("canvas").ok()?.dyn_into().ok()?;
    off.set_width(tw);
    off.set_height(th);
    let octx: web_sys::CanvasRenderingContext2d = off.get_context("2d").ok()??.dyn_into().ok()?;
    octx.draw_image_with_html_canvas_element_and_dw_and_dh(&src, 0.0, 0.0, tw as f64, th as f64)
        .ok()?;
    off.to_data_url_with_type("image/png").ok()
}

/// Exact RGBA (0–255) at each requested canvas coordinate. Out-of-bounds coords
/// read transparent black. GPU-captures the scene (not a 2D canvas readback).
pub async fn canvas_pixels(coords: &[(u32, u32)]) -> Result<Vec<[u8; 4]>, String> {
    let (data, w, h) = capture_scene_rgba().await?;
    let mut out = Vec::with_capacity(coords.len());
    for &(x, y) in coords {
        if x >= w || y >= h {
            out.push([0, 0, 0, 0]);
            continue;
        }
        let i = ((y * w + x) * 4) as usize;
        out.push([data[i], data[i + 1], data[i + 2], data[i + 3]]);
    }
    Ok(out)
}

/// Mean / min / max luma (Rec. 709) over a region `[x, y, w, h]`, or the whole
/// canvas when `None`.
pub async fn canvas_stats(
    region: Option<[u32; 4]>,
) -> Result<crate::controller::query::StatsResult, String> {
    let (data, w, h) = capture_scene_rgba().await?;
    let [rx, ry, rw, rh] = region.unwrap_or([0, 0, w, h]);
    // The region arrives via the JSON query seam, so clamp defensively: saturate
    // the corner before `.min(w/h)` so an out-of-range `rx+rw`/`ry+rh` can't
    // overflow `u32` (which would panic in debug builds). An origin past the
    // edge yields an empty range below → the `count == 0` "empty region" error.
    let x1 = rx.saturating_add(rw).min(w);
    let y1 = ry.saturating_add(rh).min(h);
    let mut sum = 0.0f64;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut count = 0u64;
    for y in ry..y1 {
        for x in rx..x1 {
            let i = ((y * w + x) * 4) as usize;
            let r = data[i] as f64;
            let g = data[i + 1] as f64;
            let b = data[i + 2] as f64;
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            sum += luma;
            min = min.min(luma);
            max = max.max(luma);
            count += 1;
        }
    }
    if count == 0 {
        return Err("empty region".to_string());
    }
    Ok(crate::controller::query::StatsResult {
        mean_luma: sum / count as f64,
        min_luma: min,
        max_luma: max,
        pixel_count: count,
    })
}

/// Resolve an asset-id string (a UUID) to an [`AssetId`] for the query seams.
pub fn parse_asset_id(s: &str) -> Option<AssetId> {
    // `AssetId` is `serde(transparent)` over a UUID, so deserialize a quoted
    // string — avoids taking a direct `uuid` dependency here.
    serde_json::from_str::<AssetId>(&format!("\"{}\"", s.replace('"', ""))).ok()
}
