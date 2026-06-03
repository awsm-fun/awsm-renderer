use wasm_bindgen::prelude::*;

// Internal-only wrapper so we can constrain the type of data used in
// write_buffer() and write_texture()
pub(crate) enum JsData<'a> {
    SliceU8(&'a [u8]),
    ArrayBuffer(&'a js_sys::ArrayBuffer),
    DataView(&'a js_sys::DataView),
    Int8Array(&'a js_sys::Int8Array),
    Uint8Array(&'a js_sys::Uint8Array),
    Uint8ClampedArray(&'a js_sys::Uint8ClampedArray),
    Int16Array(&'a js_sys::Int16Array),
    Uint16Array(&'a js_sys::Uint16Array),
    Int32Array(&'a js_sys::Int32Array),
    Uint32Array(&'a js_sys::Uint32Array),
    Float32Array(&'a js_sys::Float32Array),
    Float64Array(&'a js_sys::Float64Array),
    BigInt64Array(&'a js_sys::BigInt64Array),
    BigUint64Array(&'a js_sys::BigUint64Array),
}

impl<'a> JsData<'a> {
    /// Returns the underlying data as a `JsValue` reference when supported.
    pub fn as_js_value_ref(&'a self) -> &'a JsValue {
        match self {
            JsData::SliceU8(_) => {
                panic!("JsData::Slice should not be used as a JsValue")
            }
            JsData::ArrayBuffer(buffer) => buffer.unchecked_ref(),
            JsData::DataView(view) => view.unchecked_ref(),
            JsData::Int8Array(array) => array.unchecked_ref(),
            JsData::Uint8Array(array) => array.unchecked_ref(),
            JsData::Uint8ClampedArray(array) => array.unchecked_ref(),
            JsData::Int16Array(array) => array.unchecked_ref(),
            JsData::Uint16Array(array) => array.unchecked_ref(),
            JsData::Int32Array(array) => array.unchecked_ref(),
            JsData::Uint32Array(array) => array.unchecked_ref(),
            JsData::Float32Array(array) => array.unchecked_ref(),
            JsData::Float64Array(array) => array.unchecked_ref(),
            JsData::BigInt64Array(array) => array.unchecked_ref(),
            JsData::BigUint64Array(array) => array.unchecked_ref(),
        }
    }
}

impl<'a> From<&'a [u8]> for JsData<'a> {
    fn from(data: &'a [u8]) -> Self {
        JsData::SliceU8(data)
    }
}

impl<'a> From<&'a js_sys::ArrayBuffer> for JsData<'a> {
    fn from(data: &'a js_sys::ArrayBuffer) -> Self {
        JsData::ArrayBuffer(data)
    }
}

impl<'a> From<&'a js_sys::DataView> for JsData<'a> {
    fn from(data: &'a js_sys::DataView) -> Self {
        JsData::DataView(data)
    }
}
impl<'a> From<&'a js_sys::Int8Array> for JsData<'a> {
    fn from(data: &'a js_sys::Int8Array) -> Self {
        JsData::Int8Array(data)
    }
}
impl<'a> From<&'a js_sys::Uint8Array> for JsData<'a> {
    fn from(data: &'a js_sys::Uint8Array) -> Self {
        JsData::Uint8Array(data)
    }
}
impl<'a> From<&'a js_sys::Uint8ClampedArray> for JsData<'a> {
    fn from(data: &'a js_sys::Uint8ClampedArray) -> Self {
        JsData::Uint8ClampedArray(data)
    }
}
impl<'a> From<&'a js_sys::Int16Array> for JsData<'a> {
    fn from(data: &'a js_sys::Int16Array) -> Self {
        JsData::Int16Array(data)
    }
}
impl<'a> From<&'a js_sys::Uint16Array> for JsData<'a> {
    fn from(data: &'a js_sys::Uint16Array) -> Self {
        JsData::Uint16Array(data)
    }
}
impl<'a> From<&'a js_sys::Int32Array> for JsData<'a> {
    fn from(data: &'a js_sys::Int32Array) -> Self {
        JsData::Int32Array(data)
    }
}
impl<'a> From<&'a js_sys::Uint32Array> for JsData<'a> {
    fn from(data: &'a js_sys::Uint32Array) -> Self {
        JsData::Uint32Array(data)
    }
}
impl<'a> From<&'a js_sys::Float32Array> for JsData<'a> {
    fn from(data: &'a js_sys::Float32Array) -> Self {
        JsData::Float32Array(data)
    }
}
impl<'a> From<&'a js_sys::Float64Array> for JsData<'a> {
    fn from(data: &'a js_sys::Float64Array) -> Self {
        JsData::Float64Array(data)
    }
}
impl<'a> From<&'a js_sys::BigInt64Array> for JsData<'a> {
    fn from(data: &'a js_sys::BigInt64Array) -> Self {
        JsData::BigInt64Array(data)
    }
}
impl<'a> From<&'a js_sys::BigUint64Array> for JsData<'a> {
    fn from(data: &'a js_sys::BigUint64Array) -> Self {
        JsData::BigUint64Array(data)
    }
}
