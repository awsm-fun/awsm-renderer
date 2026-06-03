//! Shader constant override keys and values.

use ordered_float::OrderedFloat;
use wasm_bindgen::prelude::*;

// https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#vertex_object_structure
/// Key used to identify a shader constant override.
#[derive(Debug, Clone, PartialEq, Hash, Eq, Ord, PartialOrd)]
pub enum ConstantOverrideKey {
    /// Numeric identifier.
    Id(u16),
    /// Named constant.
    Name(String),
}

impl From<ConstantOverrideKey> for JsValue {
    fn from(constant: ConstantOverrideKey) -> Self {
        match constant {
            ConstantOverrideKey::Id(value) => JsValue::from_f64(value as f64),
            ConstantOverrideKey::Name(value) => JsValue::from_str(&value),
        }
    }
}

impl From<u16> for ConstantOverrideKey {
    fn from(value: u16) -> Self {
        ConstantOverrideKey::Id(value)
    }
}

impl From<String> for ConstantOverrideKey {
    fn from(value: String) -> Self {
        ConstantOverrideKey::Name(value)
    }
}

// https://gpuweb.github.io/gpuweb/#abstract-opdef-to-wgsl-type
/// Value used for a shader constant override.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd)]
pub enum ConstantOverrideValue {
    /// Boolean constant.
    Bool(bool),
    /// Signed 32-bit integer constant.
    I32(i32),
    /// Unsigned 32-bit integer constant.
    U32(u32),
    /// 32-bit float constant.
    F32(OrderedFloat<f32>),
    //F16(f16),
}

impl From<ConstantOverrideValue> for JsValue {
    fn from(constant: ConstantOverrideValue) -> Self {
        match constant {
            ConstantOverrideValue::Bool(value) => JsValue::from_bool(value),
            ConstantOverrideValue::I32(value) => JsValue::from_f64(value as f64),
            ConstantOverrideValue::U32(value) => JsValue::from_f64(value as f64),
            ConstantOverrideValue::F32(value) => JsValue::from_f64(value.into_inner() as f64),
        }
    }
}

impl From<bool> for ConstantOverrideValue {
    fn from(value: bool) -> Self {
        ConstantOverrideValue::Bool(value)
    }
}

impl From<i32> for ConstantOverrideValue {
    fn from(value: i32) -> Self {
        ConstantOverrideValue::I32(value)
    }
}

impl From<u32> for ConstantOverrideValue {
    fn from(value: u32) -> Self {
        ConstantOverrideValue::U32(value)
    }
}

impl From<f32> for ConstantOverrideValue {
    fn from(value: f32) -> Self {
        ConstantOverrideValue::F32(OrderedFloat(value))
    }
}
