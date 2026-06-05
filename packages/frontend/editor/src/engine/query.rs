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

use awsm_scene_schema::{AssetSource, TextureDef};
use wasm_bindgen::{Clamped, JsCast};

use crate::controller::controller;
use crate::engine::scene::AssetId;

/// PNG data URL of the **scene viewport** (rendered through the active camera —
/// built-in or a scene camera). `None` if the canvas isn't mounted yet.
pub fn scene_png() -> Option<String> {
    crate::engine::context::with_canvas(|c| c.to_data_url_with_type("image/png").ok())
}

/// PNG data URL of the **material-mode preview** (the example sphere). `None`
/// when the Studio isn't mounted.
pub fn material_png() -> Option<String> {
    crate::engine::preview::preview_canvas()
        .and_then(|c| c.to_data_url_with_type("image/png").ok())
}

/// PNG data URL of a **texture asset** by id. Procedural textures are generated
/// CPU-side and encoded directly; raster (file/glTF) textures are read back from
/// the GPU and PNG-encoded by the renderer.
pub async fn texture_png(id: AssetId) -> Result<String, String> {
    enum Kind {
        Procedural(awsm_scene_schema::ProceduralTextureDef),
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
    let image_data =
        web_sys::ImageData::new_with_u8_clamped_array_and_sh(Clamped(rgba), w, h)
            .map_err(|_| "build ImageData")?;
    ctx.put_image_data(&image_data, 0, 0)
        .map_err(|_| "put ImageData")?;
    canvas
        .to_data_url_with_type("image/png")
        .map_err(|_| "encode PNG".to_string())
}

/// Resolve an asset-id string (a UUID) to an [`AssetId`] for the query seams.
pub fn parse_asset_id(s: &str) -> Option<AssetId> {
    // `AssetId` is `serde(transparent)` over a UUID, so deserialize a quoted
    // string — avoids taking a direct `uuid` dependency here.
    serde_json::from_str::<AssetId>(&format!("\"{}\"", s.replace('"', ""))).ok()
}
