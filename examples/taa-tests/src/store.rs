//! Capture store: PNGs go to / come from the tiny local HTTP store
//! (`examples/taa-tests/capture-server.py`, default `http://localhost:9095`),
//! which maps `PUT/GET /<set>/<file>.png` to `<root>/<set>/<file>.png` on disk.
//! PNG is lossless, so a stored capture round-trips bit-exactly.

use anyhow::{anyhow, Result};
use image::ImageEncoder;

use crate::metrics::Image;

pub fn encode_png(img: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            &img.rgba,
            img.width,
            img.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| anyhow!("png encode: {e}"))?;
    Ok(out)
}

pub fn decode_png(bytes: &[u8]) -> Result<Image> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| anyhow!("png decode: {e}"))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Ok(Image::from_rgba(w, h, img.into_raw()))
}

pub async fn put(store: &str, path: &str, img: &Image) -> Result<()> {
    let bytes = encode_png(img)?;
    let url = format!("{store}/{path}");
    let body = js_sys::Uint8Array::from(bytes.as_slice());
    let resp = gloo_net::http::Request::put(&url)
        .header("content-type", "image/png")
        .body(body)
        .map_err(|e| anyhow!("PUT {url}: {e}"))?
        .send()
        .await
        .map_err(|e| anyhow!("PUT {url}: {e}"))?;
    if !resp.ok() {
        return Err(anyhow!("PUT {url}: HTTP {}", resp.status()));
    }
    Ok(())
}

/// `Ok(None)` when the store has no such file.
pub async fn get(store: &str, path: &str) -> Result<Option<Image>> {
    let url = format!("{store}/{path}");
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("GET {url}: {e}"))?;
    if resp.status() == 404 {
        return Ok(None);
    }
    if !resp.ok() {
        return Err(anyhow!("GET {url}: HTTP {}", resp.status()));
    }
    let bytes = resp.binary().await.map_err(|e| anyhow!("GET {url}: {e}"))?;
    Ok(Some(decode_png(&bytes)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_is_bit_exact() {
        let mut img = Image::filled(5, 3, [0, 0, 0, 255]);
        for (i, b) in img.rgba.iter_mut().enumerate() {
            if i % 4 != 3 {
                *b = (i * 37 % 256) as u8;
            }
        }
        let back = decode_png(&encode_png(&img).unwrap()).unwrap();
        assert_eq!(back, img);
    }
}
