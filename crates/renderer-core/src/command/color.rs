//! Color helpers for command descriptors.

use wasm_bindgen::prelude::*;

/// RGBA color in linear space.
#[derive(Clone, Debug)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    /// Fully transparent black.
    pub const ZERO: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    /// Opaque black.
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    /// Opaque white.
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    /// Opaque mid-grey.
    pub const MID_GREY: Self = Self {
        r: 0.5,
        g: 0.5,
        b: 0.5,
        a: 1.0,
    };

    /// Opaque red.
    pub const RED: Self = Self {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    /// Opaque red with a u32::MAX red channel.
    pub const RED_U32: Self = Self {
        r: u32::MAX as f64,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    /// Creates a color from RGBA components.
    pub fn new_values(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }

    /// Creates a color from a 4-element slice.
    pub fn new_slice(arr: &[f64]) -> Self {
        if arr.len() != 4 {
            panic!("Array length must be 4");
        }
        Self {
            r: arr[0],
            g: arr[1],
            b: arr[2],
            a: arr[3],
        }
    }

    /// Creates a color from a 24-bit RGB hex value.
    pub fn from_hex_rgb(hex: u32) -> Self {
        let r = ((hex >> 16) & 0xFF) as f64 / 255.0;
        let g = ((hex >> 8) & 0xFF) as f64 / 255.0;
        let b = (hex & 0xFF) as f64 / 255.0;

        Self { r, g, b, a: 1.0 }
    }

    /// Creates a color from a 32-bit RGBA hex value.
    pub fn from_hex_rgba(hex: u32) -> Self {
        let r = ((hex >> 24) & 0xFF) as f64 / 255.0;
        let g = ((hex >> 16) & 0xFF) as f64 / 255.0;
        let b = ((hex >> 8) & 0xFF) as f64 / 255.0;
        let a = (hex & 0xFF) as f64 / 255.0;

        Self { r, g, b, a }
    }

    /// Converts from perceptual (sRGB-like) to linear space.
    pub fn perceptual_to_linear(self) -> Self {
        Self {
            r: perceptual_to_linear(self.r),
            g: perceptual_to_linear(self.g),
            b: perceptual_to_linear(self.b),
            a: self.a,
        }
    }

    /// Converts to a JS array `[r, g, b, a]`.
    pub fn as_js_value(&self) -> JsValue {
        let arr = js_sys::Array::new();
        arr.push(&JsValue::from_f64(self.r));
        arr.push(&JsValue::from_f64(self.g));
        arr.push(&JsValue::from_f64(self.b));
        arr.push(&JsValue::from_f64(self.a));

        arr.into()
    }

    /// Converts to `[Number; 4]` for web-sys APIs that require a typed slice.
    pub fn as_number_array(&self) -> [js_sys::Number; 4] {
        [
            js_sys::Number::from(self.r),
            js_sys::Number::from(self.g),
            js_sys::Number::from(self.b),
            js_sys::Number::from(self.a),
        ]
    }
}

fn perceptual_to_linear(perceptual: f64) -> f64 {
    // Same implementation as srgb_to_linear
    if perceptual <= 0.04045 {
        perceptual / 12.92
    } else {
        ((perceptual + 0.055) / 1.055).powf(2.4)
    }
}
