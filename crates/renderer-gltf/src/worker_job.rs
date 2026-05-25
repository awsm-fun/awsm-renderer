//! Phase 4.3b — `GltfParseJob`, first consumer of the
//! [`awsm_renderer::workers`] worker-pool infrastructure.
//!
//! ### Pipeline
//!
//! The worker fetches the glb / glTF, parses buffers, AND decodes
//! every embedded image into an `ImageBitmap` via the
//! `DedicatedWorkerGlobalScope::createImageBitmap` shim. The
//! resulting handles are *transferred* (not structured-cloned)
//! across the `postMessage` boundary by overriding
//! [`WorkerJob::into_response_message`] / [`WorkerJob::from_response_message`]:
//! the trait hooks let the worker attach the handles to the
//! response object and push them into the `post_message_with_transfer`
//! transfer list. The main thread receives them in O(1) and
//! [`GltfParseOutput::into_loader`] skips its decode step entirely
//! when bitmaps are present.
//!
//! ### Earlier shape — encoded-bytes round-trip
//!
//! An earlier revision returned PNG/JPEG bytes and re-decoded on the
//! main thread. The cross-thread image-decode A/B (Corset.glb on
//! Chrome) ran ~2× slower than inline because of that re-decode —
//! the main-thread `createImageBitmap` blocked the same thread that
//! had just been freed by moving the parse off it. Moving the decode
//! into the worker (this revision) makes worker mode end-to-end
//! faster while preserving main-thread responsiveness during load —
//! the original motivation for the worker path.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use awsm_renderer::workers::WorkerJob;
use awsm_renderer_core::image::{
    bitmap::load_u8, ColorSpaceConversion, ImageBitmapOptions, ImageData, PremultiplyAlpha,
};
use futures::future::try_join_all;
use gltf::{buffer, image, Document, Error as GltfError, Gltf};
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::js_sys::{Array, Object, Reflect, Uint8Array};

use crate::error::AwsmGltfError;
use crate::loader::{get_type_from_filename, GltfFileType, GltfLoader};

// Worker-side thread-local: per-image-index slot for the
// `ImageBitmap` handle that the most recent `execute_async` run
// decoded. The vec is always exactly `image_metas.len()` entries on
// pull — `import_image_data` treats worker-side `createImageBitmap`
// rejection as fatal (no fallback; see its function doc), so every
// emitted meta carries a corresponding bitmap by construction.
//
// Pulled out by `into_response_message` (called from the worker
// dispatcher immediately after `execute` resolves): the handle
// array goes onto the response object's `bitmaps` property AND
// into the transfer list so `post_message_with_transfer` lifts
// them across the worker boundary in O(1). Main thread's
// `from_response_message` walks the array in lockstep with
// `output.image_metas` and re-attaches each handle.
//
// The `RefCell` is fine because the worker is single-threaded; the
// thread_local guarantees one cell per worker (one per pool slot).
// Each `execute_async` clears + repopulates, so a stale run can't
// leak into the next job.
thread_local! {
    static DECODED_IMAGE_HANDLES: RefCell<Vec<web_sys::ImageBitmap>> =
        const { RefCell::new(Vec::new()) };
}

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

/// Newtype wrapper for `Vec<u8>` — kept on the struct for legacy
/// direct-construction callers (non-pool consumers that build a
/// `GltfParseOutput` themselves). The pool path now ships the bytes
/// across the postMessage boundary via the transfer-list side channel
/// (`GltfParseJob::into_response_message` / `from_response_message`),
/// not through serde; see the per-field doc on `buffer_bytes`. The
/// `#[serde(transparent)]` + `serde_bytes` annotations are vestigial
/// — they only matter if someone deserialises this newtype off the
/// wire, which the pool path no longer does. Left in place so a
/// direct-construction caller that *does* hand a `GltfParseOutput`
/// to `serde_wasm_bindgen` still gets the fast Uint8Array shape.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ByteBlob(#[serde(with = "serde_bytes")] pub Vec<u8>);

impl ByteBlob {
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

/// `WorkerJob::Output` — bytes + bitmap handles travel through a
/// *side-channel* (the worker→main response object's named properties
/// plus the `post_message_with_transfer` transfer list); the serde
/// payload only carries the small `image_metas` metadata. Two reasons
/// for the split:
///
/// 1. **Zero-copy bytes.** `doc_bytes` and `buffer_bytes` get attached
///    as JS-heap `Uint8Array`s by [`GltfParseJob::into_response_message`]
///    and pushed onto the transfer list — `post_message_with_transfer`
///    detaches the `ArrayBuffer`s on the worker side and re-attaches
///    them on the main side without structured-cloning the bytes.
///    Previously the same fields rode `serde_wasm_bindgen` through
///    `#[serde(with = "serde_bytes")]`, which cost two heap-to-heap
///    memcpys (worker → structured clone → main). Now it's one memcpy
///    on the worker side (Rust `Vec<u8>` → JS `Uint8Array`) and one on
///    the main side (`Uint8Array::to_vec` back into wasm linear
///    memory) — the cross-thread hop itself is free.
///
/// 2. **`ImageBitmap` handles.** As before — bitmaps can't survive
///    structured clone, so they're transferred via the side channel
///    and stitched back into `image_metas[i].bitmap`. See the
///    `bitmap` field on `ImageMeta`.
///
/// Direct-construction callers (non-pool consumers that build a
/// `GltfParseOutput` themselves and call `into_loader` on the same
/// thread) populate `doc_bytes` / `buffer_bytes` normally; the
/// `#[serde(skip)]` only affects the postMessage round-trip.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GltfParseOutput {
    /// Re-serialised glTF JSON document — the worker's `gltf::Gltf`
    /// can't survive structured-clone (uses `serde_json::Value`
    /// internally), so we re-emit the bytes here and the main
    /// thread re-parses with `Gltf::from_slice`. Crosses the
    /// postMessage boundary via the transferred-`Uint8Array`
    /// side-channel (see struct doc), not serde.
    #[serde(skip)]
    pub doc_bytes: Vec<u8>,
    /// Raw buffer-bin contents, one entry per `Document::buffers()`
    /// in index order. 4-byte padded. Crosses the postMessage
    /// boundary via the transferred-`Uint8Array` side-channel (see
    /// struct doc), not serde.
    #[serde(skip)]
    pub buffer_bytes: Vec<ByteBlob>,
    /// One entry per `Document::images()` in index order. On the
    /// worker side `bitmap` is `None` (the handle lives in the
    /// thread_local until `into_response_message` plucks it for
    /// transfer); on the main side, `from_response_message`
    /// reattaches the handle so `into_loader` can skip its own
    /// decode. `bytes` is left empty (the worker discards it after
    /// decode) — kept on the struct only to support legacy callers
    /// that re-decode on the main thread.
    pub image_metas: Vec<ImageMeta>,
}

/// One image entry in `GltfParseOutput`. Either `bitmap` carries the
/// worker-decoded `ImageBitmap` (the fast path the pool always
/// produces) or `bytes` carries the raw encoded payload (only ever
/// populated by direct-construction callers — non-pool consumers that
/// build `GltfParseOutput` themselves and let `into_loader` decode on
/// the main thread).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageMeta {
    /// Raw encoded bytes (PNG / JPEG / …). Always empty when emitted
    /// by `GltfParseJob` — the worker either decodes successfully
    /// (handle goes via `bitmap` + the transferred side-channel) or
    /// fails fatally (no fallback; see `import_image_data`'s doc).
    /// Kept on the struct so direct-construction callers
    /// (`GltfParseOutput { … }` with bytes only) can still route
    /// through `into_loader` for the main-thread decode.
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
    /// Declared MIME type when sourced from a buffer view; `None`
    /// when sourced from a URI (the browser sniffs).
    pub mime_type: Option<String>,
    /// Source URI when the image was loaded from a separate file.
    /// Either `mime_type` or `uri` is `Some`; both being `None`
    /// indicates a programming error.
    pub uri: Option<String>,
    /// Worker-decoded `ImageBitmap` handle. Not serialised
    /// (`web_sys::ImageBitmap` doesn't implement Serialize) — set
    /// back to `Some` on the main side by
    /// `GltfParseJob::from_response_message` after picking the
    /// handles off the response object's `bitmaps` array.
    #[serde(skip)]
    pub bitmap: Option<web_sys::ImageBitmap>,
}

impl GltfParseOutput {
    /// Bridge worker output back into a `GltfLoader`. Re-parses the
    /// doc bytes (`Gltf::from_slice`) — that part always happens on
    /// the main thread because `gltf::Gltf` isn't structured-clone-able.
    /// For images, the *fast path* is the worker-decoded `ImageBitmap`
    /// already attached on `entry.bitmap` (transferred zero-copy from
    /// the worker via the `bitmaps` side-channel — see
    /// `into_response_message` / `from_response_message`): when present,
    /// the handle is wrapped directly into an `ImageData::Bitmap`.
    /// The main-thread `createImageBitmap` branch only fires for
    /// direct-construction callers that built `ImageMeta` with
    /// encoded `bytes` and no `bitmap` (i.e. not via `GltfParseJob`,
    /// which always emits the bitmap path or fails fatally).
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
        let buffers: Vec<Vec<u8>> = self
            .buffer_bytes
            .into_iter()
            .map(ByteBlob::into_vec)
            .collect();
        // Worker-decoded bitmap is the fast path. The encoded-bytes
        // branch below only fires for direct-construction callers —
        // `GltfParseJob` always produces a bitmap or fails fatally
        // upstream in `import_image_data`.
        let options = Some(
            ImageBitmapOptions::new()
                .with_premultiply_alpha(PremultiplyAlpha::None)
                .with_color_space_conversion(ColorSpaceConversion::Default),
        );
        let mut images = Vec::with_capacity(self.image_metas.len());
        for entry in self.image_metas {
            if let Some(bitmap) = entry.bitmap {
                images.push(ImageData::Bitmap {
                    image: bitmap,
                    options: options.clone(),
                });
                continue;
            }
            let mime = entry
                .mime_type
                .as_deref()
                .unwrap_or("application/octet-stream");
            let bitmap = load_u8(&entry.bytes, mime, options.clone()).await?;
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

    /// Override the default `serde_wasm_bindgen::to_value`. Three
    /// classes of payload travel through the response message:
    ///
    /// 1. **Pure-data metadata** — `image_metas` (small;
    ///    `bytes`/`mime_type`/`uri`/`bitmap=None`) goes through serde
    ///    as before. `doc_bytes` and `buffer_bytes` are
    ///    `#[serde(skip)]` so serde emits no JS copy for them.
    /// 2. **Byte payloads** — `doc_bytes` and `buffer_bytes` are
    ///    moved into freshly-allocated JS-heap `Uint8Array`s (one
    ///    `Vec<u8>`-to-JS memcpy each) and attached as named
    ///    properties on the response. Each `Uint8Array.buffer`
    ///    (the underlying `ArrayBuffer`) is pushed onto the transfer
    ///    list so `post_message_with_transfer` detaches it on the
    ///    worker side and re-attaches it on the main side without
    ///    structured-cloning the bytes. On Corset-class glbs
    ///    (12.8 MB) this saves one ~12 MB heap-to-heap clone hop;
    ///    on 50+ MB assets the win scales linearly.
    /// 3. **`ImageBitmap` handles** — same as before: collected from
    ///    the per-worker thread-local, attached to the response
    ///    object's `bitmaps` array, transferred via the same list.
    ///
    /// After the transfer the worker-side `Vec<u8>` storage has been
    /// dropped (we `mem::take`'d it into the JS allocation; the
    /// transfer then detaches that JS buffer). The function returns
    /// the response immediately afterwards so no worker-side read of
    /// the bytes happens past this point.
    fn into_response_message(output: Self::Output) -> Result<(JsValue, Array), String> {
        let mut output = output; // mut so we can `mem::take` the byte fields
        let doc_bytes = std::mem::take(&mut output.doc_bytes);
        let buffer_blobs = std::mem::take(&mut output.buffer_bytes);

        // Serialise the lightweight remainder (just `image_metas` once
        // `doc_bytes`/`buffer_bytes` are `#[serde(skip)]`). This still
        // shapes the response as the Object that `from_response_message`
        // expects to walk.
        let payload = serde_wasm_bindgen::to_value(&output)
            .map_err(|err| format!("serialize output: {err}"))?;
        let response = match payload.dyn_ref::<Object>() {
            Some(_) => payload.clone(),
            None => return Err("expected output to serialise to an Object".to_string()),
        };

        let transfer = Array::new();

        // Bytes side-channel. `Uint8Array::new_with_length(n)` allocates
        // a JS-heap `ArrayBuffer` of exactly `n` bytes; `copy_from`
        // does the wasm-linear-memory → JS-heap memcpy. The resulting
        // `Uint8Array` is transferable (unlike `Uint8Array::view`,
        // which borrows wasm memory and is *not* transferable).
        let doc_u8 = make_transferable_u8(&doc_bytes);
        transfer.push(&doc_u8.buffer());
        Reflect::set(&response, &JsValue::from_str("doc_bytes"), &doc_u8)
            .map_err(|err| format!("attach doc_bytes: {err:?}"))?;
        // Drop the worker-side Vec immediately — its bytes now live in
        // the JS-heap `Uint8Array` (which is about to be transferred).
        drop(doc_bytes);

        let buffers_arr = Array::new();
        for blob in &buffer_blobs {
            let u8 = make_transferable_u8(&blob.0);
            transfer.push(&u8.buffer());
            buffers_arr.push(&u8);
        }
        Reflect::set(&response, &JsValue::from_str("buffer_bytes"), &buffers_arr)
            .map_err(|err| format!("attach buffer_bytes: {err:?}"))?;
        drop(buffer_blobs);

        // Bitmap side-channel. Drain the per-job thread-local that
        // `execute_async` filled. Every entry is a successfully-
        // decoded `ImageBitmap` (decode failure is fatal upstream in
        // `import_image_data`), so the bitmaps array is dense and the
        // transfer list always matches it 1:1.
        let handles = DECODED_IMAGE_HANDLES.with(|cell| cell.replace(Vec::new()));
        let bitmaps_arr = Array::new();
        for bitmap in handles {
            let js: JsValue = bitmap.into();
            bitmaps_arr.push(&js);
            transfer.push(&js);
        }
        Reflect::set(&response, &JsValue::from_str("bitmaps"), &bitmaps_arr)
            .map_err(|err| format!("attach bitmaps: {err:?}"))?;

        Ok((response, transfer))
    }

    /// Main-thread inverse: deserialize the lightweight serde payload
    /// (just `image_metas`), then walk the side-channel properties
    /// (`doc_bytes`, `buffer_bytes`, `bitmaps`) populated by the
    /// worker's `into_response_message` and re-attach them to the
    /// typed Output. `into_loader` then re-parses the glTF JSON and
    /// skips its own `createImageBitmap` decode entirely.
    fn from_response_message(payload: JsValue) -> Result<Self::Output, String> {
        let mut output: GltfParseOutput = serde_wasm_bindgen::from_value(payload.clone())
            .map_err(|err| format!("deserialize output: {err}"))?;

        // doc_bytes — single Uint8Array, transferred. `to_vec` does the
        // JS-heap → wasm-linear-memory memcpy (one direction only;
        // the cross-thread hop itself was free).
        let doc_val = Reflect::get(&payload, &JsValue::from_str("doc_bytes"))
            .map_err(|err| format!("read doc_bytes: {err:?}"))?;
        if doc_val.is_undefined() || doc_val.is_null() {
            return Err("doc_bytes missing from worker response — protocol violation".to_string());
        }
        let doc_u8 = doc_val
            .dyn_into::<Uint8Array>()
            .map_err(|_| "doc_bytes is not a Uint8Array".to_string())?;
        output.doc_bytes = doc_u8.to_vec();

        // buffer_bytes — array of Uint8Arrays, one per glTF buffer.
        let bufs_val = Reflect::get(&payload, &JsValue::from_str("buffer_bytes"))
            .map_err(|err| format!("read buffer_bytes: {err:?}"))?;
        if bufs_val.is_undefined() || bufs_val.is_null() {
            return Err(
                "buffer_bytes missing from worker response — protocol violation".to_string(),
            );
        }
        let bufs_arr = bufs_val
            .dyn_into::<Array>()
            .map_err(|_| "buffer_bytes is not an Array".to_string())?;
        let mut buffer_bytes: Vec<ByteBlob> = Vec::with_capacity(bufs_arr.length() as usize);
        for i in 0..bufs_arr.length() {
            let entry = bufs_arr.get(i);
            let u8 = entry
                .dyn_into::<Uint8Array>()
                .map_err(|_| format!("buffer_bytes[{i}] is not a Uint8Array"))?;
            buffer_bytes.push(ByteBlob(u8.to_vec()));
        }
        output.buffer_bytes = buffer_bytes;

        // Bitmaps side-channel. Strict: when there are images to
        // decode (`image_metas.len() > 0`), the worker MUST send a
        // dense `bitmaps` array of matching length. Anything weaker
        // (missing property, null/undefined, non-Array, length
        // mismatch, sparse slot, non-ImageBitmap entry) is a protocol
        // violation — surface it as a typed error here, where the
        // actual contract lives, instead of letting `into_loader` try
        // to decode the empty `ImageMeta.bytes` field and produce a
        // misleading "image decode failed" error several layers up.
        //
        // For a job with zero images the worker still emits an empty
        // bitmaps array (the side-channel is unconditional), but to
        // stay forgiving against e.g. an older worker bundle we
        // tolerate the property being absent / null when there is
        // nothing to decode — there's no information to mismatch in
        // that case.
        let bitmaps_val = Reflect::get(&payload, &JsValue::from_str("bitmaps"))
            .map_err(|err| format!("read bitmaps: {err:?}"))?;
        let expected = output.image_metas.len();
        if bitmaps_val.is_undefined() || bitmaps_val.is_null() {
            if expected > 0 {
                return Err(format!(
                    "bitmaps side-channel missing/null but image_metas has {expected} \
                     entries — protocol violation (likely a worker/main bundle mismatch)"
                ));
            }
        } else {
            let bitmaps_arr = bitmaps_val.dyn_into::<Array>().map_err(|_| {
                "bitmaps is not an Array — protocol violation (likely a worker/main bundle \
                 mismatch)"
                    .to_string()
            })?;
            let count = bitmaps_arr.length() as usize;
            if count != expected {
                return Err(format!(
                    "bitmaps array length mismatch: got {count}, expected {expected}"
                ));
            }
            // Worker contract: the bitmaps array is dense (one
            // `ImageBitmap` per meta in index order — see the
            // `DECODED_IMAGE_HANDLES` doc + `into_response_message`).
            for (idx, meta) in output.image_metas.iter_mut().enumerate() {
                let handle = bitmaps_arr.get(idx as u32);
                if handle.is_undefined() || handle.is_null() {
                    return Err(format!(
                        "bitmaps[{idx}] is null/undefined — worker contract requires a dense \
                         ImageBitmap array"
                    ));
                }
                match handle.dyn_into::<web_sys::ImageBitmap>() {
                    Ok(bitmap) => meta.bitmap = Some(bitmap),
                    Err(_) => {
                        return Err(format!(
                            "bitmaps[{idx}] is not an ImageBitmap — likely a worker/main \
                             bundle mismatch"
                        ));
                    }
                }
            }
        }
        Ok(output)
    }
}

/// Worker-side execution. Wired into the pool through
/// `GltfParseJob::execute`, which boxes the returned future for the
/// dispatcher's `Pin<Box<dyn Future>>` shape. Exported separately so
/// non-pool callers (legacy main-thread `GltfLoader::load` parity
/// paths, ad-hoc benches) can reuse the exact same parse without
/// constructing a `WorkerPool`.
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
    let raw_buffers = import_buffer_data(&doc, base_path, blob).await?;
    let image_metas = import_image_data(&doc, base_path, &raw_buffers).await?;
    let buffer_bytes: Vec<ByteBlob> = raw_buffers.into_iter().map(ByteBlob).collect();

    Ok(GltfParseOutput {
        doc_bytes,
        buffer_bytes,
        image_metas,
    })
}

/// Allocate a JS-heap `Uint8Array` and memcpy `src` into it. The
/// resulting array's underlying `ArrayBuffer` is transferable —
/// unlike `Uint8Array::view(src)`, which is a borrow over wasm
/// linear memory and cannot be transferred (its backing store
/// belongs to the wasm instance, not JS).
fn make_transferable_u8(src: &[u8]) -> Uint8Array {
    let u8 = Uint8Array::new_with_length(src.len() as u32);
    // `copy_from` writes wasm-linear-memory bytes into the JS-heap
    // `ArrayBuffer`. Single memcpy per call.
    u8.copy_from(src);
    u8
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

/// Worker-side image acquisition + decode. For each `Document::images()`
/// entry we:
///   1. Fetch the encoded bytes (URI source: HTTP GET; buffer-view
///      source: slice from `buffer_data`).
///   2. Run `createImageBitmap(Blob)` via the
///      `DedicatedWorkerGlobalScope` shim. The decode happens on the
///      *worker* thread — that's the whole point: by the time the
///      job resolves, the main thread doesn't have to spend any time
///      decoding pixels.
///   3. Stash the resulting `ImageBitmap` handle in the per-worker
///      thread_local `DECODED_IMAGE_HANDLES`. The trait hook
///      `GltfParseJob::into_response_message` (called by the worker
///      dispatcher right after this function resolves) drains the
///      thread_local and attaches the handles to the response with
///      a transfer list — main thread receives them in O(1).
///   4. Emit an `ImageMeta` with `bytes` empty (encoded payload
///      discarded after decode), `bitmap` `None` (the handle lives
///      in the thread_local, not on the serialised metadata).
///
/// A worker-side `createImageBitmap` rejection is propagated as a
/// fatal error rather than wrapped in a "fall back to encoded bytes"
/// retry. Both the worker path and the main-thread parity path
/// (`GltfParseOutput::into_loader`) route through the same
/// `awsm_renderer_core::image::bitmap::load_u8` shim — which itself
/// just wraps `createImageBitmap` against a `Blob` — so a format the
/// worker browser rejects (e.g. KTX2 / Basis without a separate
/// transcoder) will fail identically on the main thread. Carrying the
/// encoded bytes across the postMessage boundary just to lose them
/// again on the main side was pure overhead; failing fast keeps the
/// telemetry honest. A real KTX2 / Basis path would need a Rust-side
/// decoder (e.g. the `image` crate's basis support behind a feature
/// flag) — out of scope here.
async fn import_image_data(
    document: &Document,
    base: &str,
    buffer_data: &[Vec<u8>],
) -> anyhow::Result<Vec<ImageMeta>> {
    let base = Arc::new(base.to_owned());
    let options = Arc::new(
        ImageBitmapOptions::new()
            .with_premultiply_alpha(PremultiplyAlpha::None)
            .with_color_space_conversion(ColorSpaceConversion::Default),
    );
    // Reset the thread_local at start so a previous job's stale
    // handles can't leak into this run.
    DECODED_IMAGE_HANDLES.with(|cell| cell.borrow_mut().clear());

    let futures: Vec<_> = document
        .images()
        .map(|image| {
            let base = Arc::clone(&base);
            let options = Arc::clone(&options);
            async move {
                let (bytes, mime_type, uri): (Vec<u8>, Option<String>, Option<String>) =
                    match image.source() {
                        image::Source::Uri { uri, mime_type } => {
                            let url = get_url(base.as_ref(), uri)?;
                            let bytes = gloo_net::http::Request::get(&url)
                                .send()
                                .await?
                                .binary()
                                .await?;
                            (bytes, mime_type.map(|s| s.to_string()), Some(url))
                        }
                        image::Source::View { view, mime_type } => {
                            let parent = &buffer_data[view.buffer().index()];
                            let begin = view.offset();
                            let end = begin + view.length();
                            (parent[begin..end].to_vec(), Some(mime_type.to_string()), None)
                        }
                    };
                // Worker-side decode via the `load_u8` shim — its
                // `web_global::create_image_bitmap_with_blob` already
                // routes to `DedicatedWorkerGlobalScope::createImageBitmap`
                // when called from the worker thread. Decode failure
                // is fatal here (see the function-level doc): the
                // main-thread parity path uses the same shim, so a
                // retry there would fail identically and the
                // intermediate bytes-round-trip would just be
                // bandwidth wasted. Drop the encoded bytes
                // unconditionally on success so the worker→main
                // payload stays as small as the transferred handle.
                let decode_mime = mime_type.as_deref().unwrap_or("application/octet-stream");
                let bitmap = load_u8(&bytes, decode_mime, Some((*options).clone()))
                    .await
                    .with_context(|| {
                        format!(
                            "GltfParseJob: createImageBitmap failed for {decode_mime} (uri={uri:?}) — \
                             the main-thread fallback uses the same shim, so this is fatal"
                        )
                    })?;
                Ok::<ImageMeta, anyhow::Error>(ImageMeta {
                    bytes: Vec::new(),
                    mime_type,
                    uri,
                    bitmap: Some(bitmap),
                })
            }
        })
        .collect();
    let mut metas: Vec<ImageMeta> = try_join_all(futures).await?;
    // Move bitmap handles into the thread_local in image-index order
    // — `into_response_message` walks both side-by-side. Every meta
    // emitted above carries `bitmap: Some(_)` (decode failure is
    // fatal in the `try_join_all` above), so the `expect` matches
    // the worker-contract invariant. The bitmaps are transferred
    // (not cloned) by `into_response_message`, so `meta.bitmap.take()`
    // is the right way to hand off ownership; the meta's bitmap slot
    // is None from here onward.
    DECODED_IMAGE_HANDLES.with(|cell| {
        let mut cell = cell.borrow_mut();
        cell.clear();
        cell.reserve(metas.len());
        for meta in metas.iter_mut() {
            let bitmap = meta
                .bitmap
                .take()
                .expect("import_image_data emits Some(bitmap) for every meta or errors fatally");
            cell.push(bitmap);
        }
    });
    Ok(metas)
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
