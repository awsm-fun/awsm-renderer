//! Error type produced by the generic web utilities (e.g. local-storage
//! access). Apps that already have a richer error type (like lockstep's
//! `FrontendError`) implement `From<WebError>` so `?` lifts these cleanly.

use thiserror::Error;
use wasm_bindgen::JsValue;

pub type WebResult<T> = Result<T, WebError>;

#[derive(Error, Debug)]
pub enum WebError {
    #[error("LocalStorage: {0:?}")]
    LocalStorage(JsValue),

    #[error("Window/Location error: {0:?}")]
    WindowLocation(JsValue),
}
