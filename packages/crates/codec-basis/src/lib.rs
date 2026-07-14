//! Async client for the Basis codec Web Worker
//! (`web/workers/basis-worker.js`), which hosts the vendored Basis Universal
//! modules off the main thread. The player uses `transcode` only; the encode
//! API exists behind the editor-only `encoder` feature.
//!
//! Buffers cross the worker boundary as transferred `ArrayBuffer`s (zero-copy
//! between threads). [`BasisWorkerClient::transcode_js`] is the transferable
//! fast path (levels stay as JS buffers, viewable for GPU upload without a
//! Rust-side copy); [`BasisWorkerClient::transcode`] is the owned convenience
//! path (`Vec<u8>` per level).
//!
//! The worker never restarts itself; this client owns restart-on-fatal — a
//! fatal worker error fails all in-flight requests and the next request
//! spawns a fresh worker.

pub mod selection;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures::channel::oneshot;
use thiserror::Error;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, Worker};

/// Must match `PROTOCOL_VERSION` in `web/workers/basis-worker.js`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Transcode target formats, matching the worker's target-name table
/// (which resolves them against the transcoder's embind enum at runtime).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TranscodeTarget {
    Astc4x4,
    Bc7,
    Etc2Rgba,
    Etc1Rgb,
    Bc3,
    Bc1,
    Bc5,
    EacRg11,
    Rgba32,
}

impl TranscodeTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Astc4x4 => "astc-4x4",
            Self::Bc7 => "bc7",
            Self::Etc2Rgba => "etc2-rgba",
            Self::Etc1Rgb => "etc1-rgb",
            Self::Bc3 => "bc3",
            Self::Bc1 => "bc1",
            Self::Bc5 => "bc5",
            Self::EacRg11 => "eac-rg11",
            Self::Rgba32 => "rgba32",
        }
    }
}

#[derive(Debug, Error)]
pub enum BasisError {
    #[error("failed to create the basis worker: {0}")]
    WorkerCreate(String),
    #[error("basis worker died: {0}")]
    Fatal(String),
    #[error("basis worker protocol violation: {0}")]
    Protocol(String),
    /// A structured error from the worker (`code` is machine-readable:
    /// bad-ktx2, bad-target, bad-request, too-large, unsupported-layout,
    /// transcode-failed, encode-failed, module-load, module-unavailable, …).
    #[error("basis worker error [{code}]: {message}")]
    Worker { code: String, message: String },
    /// The watchdog fired: no reply within the configured deadline. The
    /// worker is presumed hung and has been terminated; the next request
    /// spawns a fresh one.
    #[error("basis worker request timed out after {ms}ms (worker restarted)")]
    Timeout { ms: u32 },
}

/// URLs the client needs; all resolved by the browser against the document
/// base, so root-relative defaults work for every app we serve at "/".
#[derive(Debug, Clone)]
pub struct BasisWorkerConfig {
    pub worker_url: String,
    pub transcoder_url: String,
    /// Editor-only; leave `None` in player builds so the encoder module can
    /// never even be requested.
    pub encoder_url: Option<String>,
    /// Watchdog deadline per request. A worker that doesn't reply within
    /// this window is presumed hung (wasm can't be interrupted), terminated,
    /// and lazily respawned; the request fails with [`BasisError::Timeout`].
    /// Generous by default — a large ETC1S encode on one thread is slow.
    pub request_timeout_ms: u32,
}

impl Default for BasisWorkerConfig {
    fn default() -> Self {
        Self {
            worker_url: "/workers/basis-worker.js".to_string(),
            transcoder_url: "/vendor/basis/basis_transcoder.js".to_string(),
            encoder_url: None,
            request_timeout_ms: 120_000,
        }
    }
}

impl BasisWorkerConfig {
    /// Editor configuration: transcoder + encoder.
    pub fn with_encoder() -> Self {
        Self {
            encoder_url: Some("/vendor/basis/basis_encoder.js".to_string()),
            ..Self::default()
        }
    }
}

/// One transcoded mip level — transferable fast path (data still lives in the
/// JS `ArrayBuffer` that was transferred from the worker; no Rust copy).
#[derive(Debug, Clone)]
pub struct TranscodedLevelJs {
    pub level: u32,
    pub width: u32,
    pub height: u32,
    pub data: js_sys::ArrayBuffer,
}

impl TranscodedLevelJs {
    /// Byte length without copying.
    pub fn byte_length(&self) -> u32 {
        self.data.byte_length()
    }

    /// Copy out to Rust memory.
    pub fn to_vec(&self) -> Vec<u8> {
        js_sys::Uint8Array::new(&self.data).to_vec()
    }
}

/// Transcode result — transferable fast path.
#[derive(Debug, Clone)]
pub struct TranscodedTextureJs {
    pub target: TranscodeTarget,
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub is_uastc: bool,
    pub levels: Vec<TranscodedLevelJs>,
}

/// Transcode result — owned convenience path.
#[derive(Debug, Clone)]
pub struct TranscodedTexture {
    pub target: TranscodeTarget,
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub is_uastc: bool,
    pub levels: Vec<TranscodedLevel>,
}

#[derive(Debug, Clone)]
pub struct TranscodedLevel {
    pub level: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl TranscodedTextureJs {
    pub fn to_owned_texture(&self) -> TranscodedTexture {
        TranscodedTexture {
            target: self.target,
            width: self.width,
            height: self.height,
            has_alpha: self.has_alpha,
            is_uastc: self.is_uastc,
            levels: self
                .levels
                .iter()
                .map(|l| TranscodedLevel {
                    level: l.level,
                    width: l.width,
                    height: l.height,
                    data: l.to_vec(),
                })
                .collect(),
        }
    }
}

/// Parameters for the editor-only encode path.
#[cfg(feature = "encoder")]
#[derive(Debug, Clone)]
pub struct EncodeParams {
    /// UASTC (high quality, normal maps) vs ETC1S (small, color textures).
    pub uastc: bool,
    /// Perceptual/sRGB encoding for color textures; false for data textures.
    pub srgb: bool,
    /// Generate a full mip chain at encode time.
    pub mipmaps: bool,
    /// ETC1S quality 1..=255 (ignored for UASTC).
    pub quality: u8,
    /// Zstd supercompression for UASTC (ignored for ETC1S).
    pub zstd: bool,
}

#[cfg(feature = "encoder")]
impl Default for EncodeParams {
    fn default() -> Self {
        Self {
            uastc: false,
            srgb: true,
            mipmaps: true,
            quality: 128,
            zstd: true,
        }
    }
}

type PendingMap = HashMap<u32, oneshot::Sender<Result<JsValue, BasisError>>>;

struct Shared {
    worker: Option<Worker>,
    initialized: bool,
    next_id: u32,
    pending: PendingMap,
    // Keep the JS callbacks alive for the worker's lifetime.
    _onmessage: Option<Closure<dyn FnMut(MessageEvent)>>,
    _onerror: Option<Closure<dyn FnMut(web_sys::ErrorEvent)>>,
}

/// Handle to the Basis codec worker. Cheap to clone; all clones share the one
/// worker. Dropping the last clone terminates it.
#[derive(Clone)]
pub struct BasisWorkerClient {
    config: BasisWorkerConfig,
    shared: Rc<RefCell<Shared>>,
}

impl BasisWorkerClient {
    pub fn new(config: BasisWorkerConfig) -> Self {
        Self {
            config,
            shared: Rc::new(RefCell::new(Shared {
                worker: None,
                initialized: false,
                next_id: 1,
                pending: HashMap::new(),
                _onmessage: None,
                _onerror: None,
            })),
        }
    }

    /// Transcode a whole KTX2 file (container parse + Zstd + transcode happen
    /// in the worker). Transferable fast path — level data stays in JS
    /// buffers.
    pub async fn transcode_js(
        &self,
        ktx2: &[u8],
        target: TranscodeTarget,
    ) -> Result<TranscodedTextureJs, BasisError> {
        self.ensure_initialized().await?;

        let ktx2_js = js_sys::Uint8Array::from(ktx2);
        let buffer = ktx2_js.buffer();
        let msg = js_sys::Object::new();
        set(&msg, "op", &"transcode".into())?;
        set(&msg, "ktx2", &buffer)?;
        set(&msg, "target", &target.as_str().into())?;
        let result = self.request(&msg, &[buffer.into()]).await?;

        let levels_js: js_sys::Array = get(&result, "levels")?
            .dyn_into()
            .map_err(|_| BasisError::Protocol("transcode result levels is not an array".into()))?;
        let mut levels = Vec::with_capacity(levels_js.length() as usize);
        for level in levels_js.iter() {
            levels.push(TranscodedLevelJs {
                level: get_u32(&level, "level")?,
                width: get_u32(&level, "width")?,
                height: get_u32(&level, "height")?,
                data: get(&level, "data")?
                    .dyn_into()
                    .map_err(|_| BasisError::Protocol("level data is not an ArrayBuffer".into()))?,
            });
        }
        Ok(TranscodedTextureJs {
            target,
            width: get_u32(&result, "width")?,
            height: get_u32(&result, "height")?,
            has_alpha: get_bool(&result, "hasAlpha")?,
            is_uastc: get_bool(&result, "isUastc")?,
            levels,
        })
    }

    /// Owned convenience path: like [`Self::transcode_js`] but each level is
    /// copied out to a `Vec<u8>`.
    pub async fn transcode(
        &self,
        ktx2: &[u8],
        target: TranscodeTarget,
    ) -> Result<TranscodedTexture, BasisError> {
        Ok(self.transcode_js(ktx2, target).await?.to_owned_texture())
    }

    /// Encode RGBA8 pixels into a Basis-supercompressed KTX2 file
    /// (editor bake path).
    #[cfg(feature = "encoder")]
    pub async fn encode(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        params: &EncodeParams,
    ) -> Result<Vec<u8>, BasisError> {
        if rgba.len() != (width as usize) * (height as usize) * 4 {
            return Err(BasisError::Protocol(format!(
                "rgba length {} != {width}x{height}*4",
                rgba.len()
            )));
        }
        self.ensure_initialized().await?;

        let rgba_js = js_sys::Uint8Array::from(rgba);
        let buffer = rgba_js.buffer();
        let msg = js_sys::Object::new();
        set(&msg, "op", &"encode".into())?;
        set(&msg, "rgba", &buffer)?;
        set(&msg, "width", &(width as f64).into())?;
        set(&msg, "height", &(height as f64).into())?;
        set(&msg, "uastc", &params.uastc.into())?;
        set(&msg, "srgb", &params.srgb.into())?;
        set(&msg, "mipmaps", &params.mipmaps.into())?;
        set(&msg, "quality", &(params.quality as f64).into())?;
        set(&msg, "zstd", &params.zstd.into())?;
        let result = self.request(&msg, &[buffer.into()]).await?;

        let ktx2: js_sys::ArrayBuffer = get(&result, "ktx2")?
            .dyn_into()
            .map_err(|_| BasisError::Protocol("encode result ktx2 is not an ArrayBuffer".into()))?;
        Ok(js_sys::Uint8Array::new(&ktx2).to_vec())
    }

    /// Tear the worker down now (it also dies with the last clone).
    pub fn terminate(&self) {
        let mut shared = self.shared.borrow_mut();
        if let Some(worker) = shared.worker.take() {
            worker.terminate();
        }
        shared.initialized = false;
        fail_all_pending(&mut shared.pending, "worker terminated");
    }

    async fn ensure_initialized(&self) -> Result<(), BasisError> {
        if self.shared.borrow().initialized {
            return Ok(());
        }
        let msg = js_sys::Object::new();
        set(&msg, "op", &"init".into())?;
        let urls = js_sys::Object::new();
        set(
            &urls,
            "transcoder",
            &self.config.transcoder_url.as_str().into(),
        )?;
        if let Some(encoder_url) = &self.config.encoder_url {
            set(&urls, "encoder", &encoder_url.as_str().into())?;
        }
        set(&msg, "urls", &urls)?;
        self.request(&msg, &[]).await?;
        self.shared.borrow_mut().initialized = true;
        Ok(())
    }

    /// Send one request and await its routed reply — racing the watchdog.
    async fn request(
        &self,
        msg: &js_sys::Object,
        transfer: &[JsValue],
    ) -> Result<JsValue, BasisError> {
        let (worker, id) = {
            let mut shared = self.shared.borrow_mut();
            let worker = self.ensure_worker(&mut shared)?;
            let id = shared.next_id;
            shared.next_id = shared.next_id.wrapping_add(1);
            (worker, id)
        };

        set(msg, "v", &(PROTOCOL_VERSION as f64).into())?;
        set(msg, "id", &(id as f64).into())?;

        let (tx, rx) = oneshot::channel();
        self.shared.borrow_mut().pending.insert(id, tx);

        let transfer_array = js_sys::Array::new();
        for t in transfer {
            transfer_array.push(t);
        }
        if let Err(e) = worker.post_message_with_transfer(msg, &transfer_array) {
            self.shared.borrow_mut().pending.remove(&id);
            return Err(BasisError::Fatal(format!("postMessage failed: {e:?}")));
        }

        // Watchdog: a hung wasm module can't be interrupted — if the reply
        // doesn't arrive in time, terminate the worker (failing every other
        // in-flight request) and let the next call respawn it.
        let timeout_ms = self.config.request_timeout_ms;
        use futures::FutureExt;
        let mut reply = rx.fuse();
        let mut deadline = Box::pin(sleep_ms(timeout_ms).fuse());
        futures::select! {
            outcome = reply => outcome
                .map_err(|_| BasisError::Fatal("worker died with request in flight".into()))?,
            _ = deadline => {
                tracing::error!(
                    "basis worker request {id} exceeded {timeout_ms}ms — terminating the worker"
                );
                let mut shared = self.shared.borrow_mut();
                shared.pending.remove(&id);
                if let Some(worker) = shared.worker.take() {
                    worker.terminate();
                }
                shared.initialized = false;
                fail_all_pending(&mut shared.pending, "sibling request timed out");
                Err(BasisError::Timeout { ms: timeout_ms })
            }
        }
    }

    /// Get the live worker, creating (or re-creating after a fatal) on demand.
    fn ensure_worker(&self, shared: &mut Shared) -> Result<Worker, BasisError> {
        if let Some(worker) = &shared.worker {
            return Ok(worker.clone());
        }

        let worker = Worker::new(&self.config.worker_url)
            .map_err(|e| BasisError::WorkerCreate(format!("{e:?}")))?;

        let weak = Rc::downgrade(&self.shared);
        let onmessage = Closure::wrap(Box::new(move |event: MessageEvent| {
            let Some(shared) = weak.upgrade() else { return };
            let data = event.data();
            let Ok(id) = get_u32(&data, "id") else {
                tracing::warn!("basis worker reply without id");
                return;
            };
            let outcome = match get_bool(&data, "ok") {
                Ok(true) => get(&data, "result"),
                Ok(false) => {
                    let error = get(&data, "error").unwrap_or(JsValue::UNDEFINED);
                    Err(BasisError::Worker {
                        code: get_string(&error, "code").unwrap_or_else(|_| "unknown".into()),
                        message: get_string(&error, "message").unwrap_or_default(),
                    })
                }
                Err(e) => Err(e),
            };
            let tx = shared.borrow_mut().pending.remove(&id);
            if let Some(tx) = tx {
                let _ = tx.send(outcome);
            }
        }) as Box<dyn FnMut(MessageEvent)>);

        let weak = Rc::downgrade(&self.shared);
        let onerror = Closure::wrap(Box::new(move |event: web_sys::ErrorEvent| {
            let Some(shared) = weak.upgrade() else { return };
            let message = event.message();
            tracing::error!("basis worker fatal error: {message}");
            // Restart-on-fatal: drop the worker; the next request re-creates.
            let mut shared = shared.borrow_mut();
            if let Some(worker) = shared.worker.take() {
                worker.terminate();
            }
            shared.initialized = false;
            fail_all_pending(&mut shared.pending, &message);
        }) as Box<dyn FnMut(web_sys::ErrorEvent)>);

        worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        worker.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        shared._onmessage = Some(onmessage);
        shared._onerror = Some(onerror);
        shared.worker = Some(worker.clone());
        shared.initialized = false;
        Ok(worker)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.terminate();
        }
    }
}

fn fail_all_pending(pending: &mut PendingMap, message: &str) {
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(BasisError::Fatal(message.to_string())));
    }
}

/// Resolve after `ms` via the global `setTimeout` (works on the main thread
/// and in workers — no `Window` assumption).
async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let global = js_sys::global();
        if let Ok(set_timeout) = js_sys::Reflect::get(&global, &"setTimeout".into()) {
            if let Ok(f) = set_timeout.dyn_into::<js_sys::Function>() {
                let _ = f.call2(&global, &resolve, &JsValue::from_f64(ms as f64));
                return;
            }
        }
        // No setTimeout (never on web targets) — resolve immediately rather
        // than hang the watchdog arm.
        let _ = resolve.call0(&JsValue::UNDEFINED);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) -> Result<(), BasisError> {
    js_sys::Reflect::set(obj, &key.into(), value)
        .map(|_| ())
        .map_err(|e| BasisError::Protocol(format!("failed to set {key}: {e:?}")))
}

fn get(obj: &JsValue, key: &str) -> Result<JsValue, BasisError> {
    js_sys::Reflect::get(obj, &key.into())
        .map_err(|e| BasisError::Protocol(format!("missing {key}: {e:?}")))
}

fn get_u32(obj: &JsValue, key: &str) -> Result<u32, BasisError> {
    get(obj, key)?
        .as_f64()
        .map(|f| f as u32)
        .ok_or_else(|| BasisError::Protocol(format!("{key} is not a number")))
}

fn get_bool(obj: &JsValue, key: &str) -> Result<bool, BasisError> {
    get(obj, key)?
        .as_bool()
        .ok_or_else(|| BasisError::Protocol(format!("{key} is not a bool")))
}

fn get_string(obj: &JsValue, key: &str) -> Result<String, BasisError> {
    get(obj, key)?
        .as_string()
        .ok_or_else(|| BasisError::Protocol(format!("{key} is not a string")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_names_match_worker_table() {
        // Keep in lockstep with TARGET_TO_ENUM in web/workers/basis-worker.js.
        let all = [
            (TranscodeTarget::Astc4x4, "astc-4x4"),
            (TranscodeTarget::Bc7, "bc7"),
            (TranscodeTarget::Etc2Rgba, "etc2-rgba"),
            (TranscodeTarget::Etc1Rgb, "etc1-rgb"),
            (TranscodeTarget::Bc3, "bc3"),
            (TranscodeTarget::Bc1, "bc1"),
            (TranscodeTarget::Bc5, "bc5"),
            (TranscodeTarget::EacRg11, "eac-rg11"),
            (TranscodeTarget::Rgba32, "rgba32"),
        ];
        for (target, name) in all {
            assert_eq!(target.as_str(), name);
        }
    }

    #[test]
    fn player_default_config_has_no_encoder() {
        assert!(BasisWorkerConfig::default().encoder_url.is_none());
        assert!(BasisWorkerConfig::with_encoder().encoder_url.is_some());
    }
}
