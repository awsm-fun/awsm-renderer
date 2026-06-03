//! Vertex state descriptors.

use std::collections::BTreeMap;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use super::constants::{ConstantOverrideKey, ConstantOverrideValue};

/// Vertex stage descriptor.
#[derive(Debug, Clone)]
pub struct VertexState<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#vertex_object_structure
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuVertexState.html
    pub constants: BTreeMap<ConstantOverrideKey, ConstantOverrideValue>,
    pub entry_point: Option<&'a str>,
    pub module: &'a web_sys::GpuShaderModule,
    pub buffer_layouts: Vec<VertexBufferLayout>,
}

/// Vertex buffer layout descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VertexBufferLayout {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#buffers
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuVertexBufferLayout.html
    pub array_stride: u64,
    pub attributes: Vec<VertexAttribute>,
    pub step_mode: Option<VertexStepMode>,
}

impl std::hash::Hash for VertexBufferLayout {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.array_stride.hash(state);
        self.step_mode.map(|x| x as u32).hash(state);
        self.attributes.hash(state);
    }
}

/// Vertex attribute descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexAttribute {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#attributes
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuVertexAttribute.html
    pub format: VertexFormat,
    pub offset: u64,
    pub shader_location: u32,
}

impl std::hash::Hash for VertexAttribute {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.format as u32).hash(state);
        self.offset.hash(state);
        self.shader_location.hash(state);
    }
}

// https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#stepmode
/// WebGPU vertex step mode.
pub type VertexStepMode = web_sys::GpuVertexStepMode;

// https://docs.rs/web-sys/latest/web_sys/enum.GpuVertexFormat.html
/// WebGPU vertex format.
pub type VertexFormat = web_sys::GpuVertexFormat;

// JS conversion

impl<'a> VertexState<'a> {
    /// Creates a vertex state for a module and entry point.
    pub fn new(module: &'a web_sys::GpuShaderModule, entry_point: Option<&'a str>) -> Self {
        Self {
            constants: BTreeMap::new(),
            entry_point,
            module,
            buffer_layouts: Vec::new(),
        }
    }

    /// Adds a vertex buffer layout.
    pub fn with_buffer_layout(mut self, buffer_layout: VertexBufferLayout) -> Self {
        self.buffer_layouts.push(buffer_layout);
        self
    }

    /// Adds a constant override.
    pub fn with_constant(mut self, key: ConstantOverrideKey, value: ConstantOverrideValue) -> Self {
        self.constants.insert(key, value);
        self
    }
}

impl From<VertexState<'_>> for web_sys::GpuVertexState {
    fn from(state: VertexState) -> Self {
        let state_js = web_sys::GpuVertexState::new(state.module);

        if let Some(entry_point) = &state.entry_point {
            state_js.set_entry_point(entry_point);
        }

        if !state.constants.is_empty() {
            let obj = js_sys::Object::new();
            for (key, value) in state.constants.into_iter() {
                js_sys::Reflect::set(&obj, &JsValue::from(key), &JsValue::from(value))
                    .unwrap_throw();
            }
            state_js.set_constants(&obj.unchecked_into());
        }

        if !state.buffer_layouts.is_empty() {
            let buffers: Vec<js_sys::JsOption<web_sys::GpuVertexBufferLayout>> = state
                .buffer_layouts
                .into_iter()
                .map(|buffer| js_sys::JsOption::wrap(web_sys::GpuVertexBufferLayout::from(buffer)))
                .collect();
            state_js.set_buffers(&buffers);
        }

        state_js
    }
}

impl From<VertexBufferLayout> for web_sys::GpuVertexBufferLayout {
    fn from(buffer_layout: VertexBufferLayout) -> web_sys::GpuVertexBufferLayout {
        let attributes: Vec<web_sys::GpuVertexAttribute> = buffer_layout
            .attributes
            .iter()
            .map(|attribute| web_sys::GpuVertexAttribute::from(*attribute))
            .collect();

        let buffer_layout_js = web_sys::GpuVertexBufferLayout::new_with_f64(
            buffer_layout.array_stride as f64,
            &attributes,
        );

        if let Some(step_mode) = buffer_layout.step_mode {
            buffer_layout_js.set_step_mode(step_mode);
        }

        buffer_layout_js
    }
}

impl From<VertexAttribute> for web_sys::GpuVertexAttribute {
    fn from(attribute: VertexAttribute) -> web_sys::GpuVertexAttribute {
        web_sys::GpuVertexAttribute::new_with_f64(
            attribute.format,
            attribute.offset as f64,
            attribute.shader_location,
        )
    }
}
