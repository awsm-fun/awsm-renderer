//! Phase 4.3b — `GltfParseJob`, first consumer of the
//! [`awsm_renderer::workers`] worker-pool infrastructure.
//!
//! ### Scope (skeleton)
//!
//! This commit ships the job *shape* — Input/Output types, the
//! `WorkerJob` impl, and an `execute_async` helper that performs the
//! same work as `GltfLoader::load` but returns *raw* bytes so the
//! payload survives the cross-thread `postMessage` boundary
//! (web-sys `ImageBitmap` cannot be `serde`-serialised).
//!
//! ### Deferred to follow-up sprint
//!
//! - Actually dispatching glTF parses through this job in
//!   `asset_cache::load_and_populate`. Spec calls for an A/B
//!   measurement on the 27 MB robot first — if worker mode wins,
//!   wire as default; otherwise leave inline as default and expose
//!   the job as opt-in.
//! - Decoding the returned image bytes back into `ImageData` on the
//!   main thread (the consumer of `GltfParseOutput` does this).
//!   Currently `consume_into_loader` is a thin synchronous helper
//!   that re-parses the doc + lifts buffers, but image decode is
//!   left to the caller until the wiring lands.
//!
//! ### Why output bytes, not `GltfLoader`
//!
//! `GltfLoader::images: Vec<ImageData>` wraps `web_sys::ImageBitmap`
//! / `web_sys::ImageData` — neither structured-clones across worker
//! boundaries without explicit `transfer` lists, and `serde` can't
//! see through `JsValue` wrappers. The worker returns the
//! *unprocessed* encoded image bytes (PNG / JPEG / etc. as found in
//! the glb) plus their declared MIME type; the main thread runs
//! `createImageBitmap` on each to materialise the `ImageBitmap`s
//! that `ImageData::Bitmap` wraps.

use std::sync::{Arc, Mutex};

use awsm_renderer::workers::WorkerJob;
use awsm_renderer_core::image::{
    bitmap::load_u8, ColorSpaceConversion, ImageBitmapOptions, ImageData, PremultiplyAlpha,
};
use futures::future::try_join_all;
use gltf::{buffer, image, Document, Error as GltfError, Gltf};
use serde::{Deserialize, Serialize};

use crate::error::AwsmGltfError;
use crate::loader::{get_type_from_filename, GltfFileType, GltfLoader};

/// Worker-job marker.
pub struct GltfParseJob;

/// `WorkerJob::Input` — same shape as `GltfLoader::load`'s args.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GltfParseInput {
    pub url: String,
    /// Use `FileTypeHint::*` rather than the `GltfFileType` enum so
    /// the Input stays `Copy`-able strings across the postMessage
    /// boundary (enum variants serialise fine; this is just
    /// belt-and-suspenders against accidental Rust-specific shapes).
    pub file_type: Option<FileTypeHint>,
}

/// Serializable mirror of `GltfFileType` — the upstream enum lives
/// in `loader.rs` and doesn't derive `serde`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileTypeHint {
    Json,
    Glb,
    Draco,
}

impl From<&GltfFileType> for FileTypeHint {
    fn from(t: &GltfFileType) -> Self {
        match t {
            GltfFileType::Json => FileTypeHint::Json,
            GltfFileType::Glb => FileTypeHint::Glb,
            GltfFileType::Draco => FileTypeHint::Draco,
        }
    }
}

impl From<FileTypeHint> for GltfFileType {
    fn from(t: FileTypeHint) -> Self {
        match t {
            FileTypeHint::Json => GltfFileType::Json,
            FileTypeHint::Glb => GltfFileType::Glb,
            FileTypeHint::Draco => GltfFileType::Draco,
        }
    }
}

/// `WorkerJob::Output` — only raw bytes; everything serialisable.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GltfParseOutput {
    /// Re-serialised glTF JSON document — the worker's `gltf::Gltf`
    /// can't survive structured-clone (uses `serde_json::Value`
    /// internally), so we re-emit the bytes here and the main
    /// thread re-parses with `Gltf::from_slice`. Cheap (the
    /// document is rarely > a few hundred KB even for huge scenes).
    pub doc_bytes: Vec<u8>,
    /// Raw buffer-bin contents, one entry per `Document::buffers()`
    /// in index order. 4-byte padded to match what the renderer
    /// expects downstream.
    pub buffer_bytes: Vec<Vec<u8>>,
    /// Raw encoded image bytes (PNG / JPEG / KTX2 / …) one entry
    /// per `Document::images()` in index order. The main thread
    /// runs `createImageBitmap` to materialise `ImageBitmap`s once
    /// the bytes arrive.
    pub image_bytes: Vec<EncodedImage>,
}

/// One encoded-image entry produced by the worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncodedImage {
    pub bytes: Vec<u8>,
    /// Declared MIME type when sourced from a buffer view; `None`
    /// when sourced from a URI (the main thread re-fetches via the
    /// stored URI in that case — kept for future cleanup).
    pub mime_type: Option<String>,
    /// Source URI when the image was loaded from a separate file.
    /// Either `mime_type` or `uri` is `Some`; both being `None`
    /// indicates a programming error.
    pub uri: Option<String>,
}

impl GltfParseOutput {
    /// Bridge worker output back into a `GltfLoader`. Re-parses the
    /// doc bytes (`Gltf::from_slice`) and runs `createImageBitmap`
    /// on each encoded image — both happen on the main thread, since
    /// `web_sys::ImageBitmap` doesn't cross the worker postMessage
    /// boundary cleanly.
    ///
    /// Consumers that opt into the worker-mode gltf-parse path
    /// (Phase 4.3b) call:
    ///
    /// ```ignore
    /// let out = pool.dispatch::<GltfParseJob>(input).await?;
    /// let loader = out.into_loader().await?;
    /// renderer.populate_gltf(loader.into_data(None)?, None).await?;
    /// ```
    ///
    /// The default `asset_cache::load_and_populate` path stays on
    /// the inline `GltfLoader::load` until the A/B measurement gate
    /// in the Phase 4.3b spec confirms a real win on representative
    /// scenes (e.g. the 27 MB robot stress asset).
    pub async fn into_loader(self) -> anyhow::Result<GltfLoader> {
        let Gltf { document: doc, .. } = Gltf::from_slice(&self.doc_bytes)?;
        // Buffers are already 4-byte padded by `execute_async`.
        let buffers = self.buffer_bytes;
        // Decode each encoded image on the main thread.
        let options = Some(
            ImageBitmapOptions::new()
                .with_premultiply_alpha(PremultiplyAlpha::None)
                .with_color_space_conversion(ColorSpaceConversion::Default),
        );
        let mut images = Vec::with_capacity(self.image_bytes.len());
        for encoded in self.image_bytes {
            let mime = encoded
                .mime_type
                .as_deref()
                .unwrap_or("application/octet-stream");
            let bitmap = load_u8(&encoded.bytes, mime, options.clone()).await?;
            images.push(ImageData::Bitmap {
                image: bitmap,
                options: options.clone(),
            });
        }
        Ok(GltfLoader {
            doc,
            buffers,
            images,
        })
    }
}

impl WorkerJob for GltfParseJob {
    const NAME: &'static str = "gltf-parse";
    type Input = GltfParseInput;
    type Output = GltfParseOutput;

    fn execute(
        input: Self::Input,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Self::Output>>>> {
        Box::pin(execute_async(input))
    }
}

/// Async worker-side execution. The pool dispatcher will be taught
/// to await this directly once `WorkerJob` grows an async fn
/// variant; for now, callers fan out parses via [`execute_async`]
/// without going through the WorkerPool indirection.
pub async fn execute_async(input: GltfParseInput) -> anyhow::Result<GltfParseOutput> {
    let url = input.url;
    let file_type: GltfFileType = match input.file_type {
        Some(hint) => hint.into(),
        None => get_type_from_filename(&url).unwrap_or(GltfFileType::Json),
    };

    let (doc, blob, doc_bytes) = match file_type {
        GltfFileType::Json => {
            let text = gloo_net::http::Request::get(&url)
                .send()
                .await?
                .text()
                .await?;
            let bytes = text.into_bytes();
            let Gltf {
                document: doc,
                blob,
            } = Gltf::from_slice(&bytes)?;
            (doc, blob, bytes)
        }
        GltfFileType::Glb => {
            let bytes = gloo_net::http::Request::get(&url)
                .send()
                .await?
                .binary()
                .await?;
            // For GLB the worker keeps the original bytes — the
            // main thread can re-parse `Gltf::from_slice(&bytes)`
            // and recover both the document and the blob.
            let Gltf {
                document: doc,
                blob,
            } = Gltf::from_slice(&bytes)?;
            (doc, blob, bytes)
        }
        _ => return Err(AwsmGltfError::Load.into()),
    };

    let base_path = get_base_path(&url);
    let buffer_bytes = import_buffer_data(&doc, base_path, blob).await?;
    let image_bytes = import_image_data_as_bytes(&doc, base_path, &buffer_bytes).await?;

    Ok(GltfParseOutput {
        doc_bytes,
        buffer_bytes,
        image_bytes,
    })
}

fn get_base_path(url: &str) -> &str {
    let idx1: i32 = url.rfind('/').map(|n| n as i32).unwrap_or(-1) + 1;
    let idx2: i32 = url.rfind('\\').map(|n| n as i32).unwrap_or(-1) + 1;
    if idx1 == 0 && idx2 == 0 {
        url
    } else {
        &url[0..(std::cmp::max(idx1, idx2) as usize)]
    }
}

async fn import_buffer_data(
    document: &Document,
    base: &str,
    blob: Option<Vec<u8>>,
) -> anyhow::Result<Vec<Vec<u8>>> {
    let blob = Arc::new(Mutex::new(blob));
    let base = Arc::new(base.to_owned());

    let futures: Vec<_> = document
        .buffers()
        .map(|buffer| {
            let blob = blob.clone();
            let base = base.clone();
            async move {
                match buffer.source() {
                    buffer::Source::Uri(uri) => {
                        let url = get_url(base.as_ref(), uri)?;
                        let bytes = gloo_net::http::Request::get(&url)
                            .send()
                            .await?
                            .binary()
                            .await?;
                        Ok::<Vec<u8>, anyhow::Error>(bytes)
                    }
                    buffer::Source::Bin => blob
                        .lock()
                        .unwrap()
                        .take()
                        .ok_or_else(|| anyhow::Error::from(GltfError::MissingBlob)),
                }
            }
        })
        .collect();

    let datas: Vec<Vec<u8>> = try_join_all(futures).await?;

    let mut buffers = Vec::new();
    for (mut data, buffer) in datas.into_iter().zip(document.buffers()) {
        if data.len() < buffer.length() {
            return Err(GltfError::BufferLength {
                buffer: buffer.index(),
                expected: buffer.length(),
                actual: data.len(),
            }
            .into());
        }
        while data.len() % 4 != 0 {
            data.push(0);
        }
        buffers.push(data);
    }
    Ok(buffers)
}

async fn import_image_data_as_bytes(
    document: &Document,
    base: &str,
    buffer_data: &[Vec<u8>],
) -> anyhow::Result<Vec<EncodedImage>> {
    let base = Arc::new(base.to_owned());
    let futures: Vec<_> = document
        .images()
        .map(|image| {
            let base = Arc::clone(&base);
            async move {
                match image.source() {
                    image::Source::Uri { uri, mime_type } => {
                        let url = get_url(base.as_ref(), uri)?;
                        // Fetch the bytes so the main thread doesn't
                        // pay a second round-trip. The MIME type is
                        // either declared in the glTF or we let the
                        // browser sniff it on createImageBitmap.
                        let bytes = gloo_net::http::Request::get(&url)
                            .send()
                            .await?
                            .binary()
                            .await?;
                        Ok::<EncodedImage, anyhow::Error>(EncodedImage {
                            bytes,
                            mime_type: mime_type.map(|s| s.to_string()),
                            uri: Some(url),
                        })
                    }
                    image::Source::View { view, mime_type } => {
                        let parent = &buffer_data[view.buffer().index()];
                        let begin = view.offset();
                        let end = begin + view.length();
                        Ok(EncodedImage {
                            bytes: parent[begin..end].to_vec(),
                            mime_type: Some(mime_type.to_string()),
                            uri: None,
                        })
                    }
                }
            }
        })
        .collect();
    try_join_all(futures).await
}

fn get_url(base: &str, uri: &str) -> anyhow::Result<String> {
    if uri.contains(':') {
        if uri.starts_with("data:") || uri.starts_with("http:") || uri.starts_with("https://") {
            Ok(uri.to_owned())
        } else {
            Err(GltfError::UnsupportedScheme.into())
        }
    } else {
        Ok(format!("{base}{uri}"))
    }
}
