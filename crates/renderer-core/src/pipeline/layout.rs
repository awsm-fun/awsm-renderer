//! Pipeline layout helpers.

use wasm_bindgen::prelude::*;

/// Pipeline layout selection for pipeline descriptors.
#[derive(Default, Debug, Clone)]
pub enum PipelineLayoutKind<'a> {
    /// Let WebGPU infer layout.
    #[default]
    Auto,
    /// Use an explicit pipeline layout.
    Custom(&'a web_sys::GpuPipelineLayout),
}

impl From<PipelineLayoutKind<'_>> for JsValue {
    fn from(layout_kind: PipelineLayoutKind) -> JsValue {
        match layout_kind {
            PipelineLayoutKind::Auto => JsValue::from_str("auto"),
            PipelineLayoutKind::Custom(layout) => layout.into(),
        }
    }
}

/// Builder for a pipeline layout descriptor.
#[derive(Debug, Clone)]
pub struct PipelineLayoutDescriptor<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createPipelineLayout
    pub label: Option<&'a str>,
    pub bind_group_layouts: Vec<web_sys::GpuBindGroupLayout>,
}

impl<'a> PipelineLayoutDescriptor<'a> {
    /// Creates a pipeline layout descriptor.
    pub fn new(
        label: Option<&'a str>,
        bind_group_layouts: Vec<web_sys::GpuBindGroupLayout>,
    ) -> Self {
        Self {
            label,
            bind_group_layouts,
        }
    }
}

impl From<PipelineLayoutDescriptor<'_>> for web_sys::GpuPipelineLayoutDescriptor {
    fn from(layout: PipelineLayoutDescriptor) -> Self {
        let bind_group_layouts: Vec<js_sys::JsOption<web_sys::GpuBindGroupLayout>> = layout
            .bind_group_layouts
            .into_iter()
            .map(js_sys::JsOption::wrap)
            .collect();

        let layout_js = web_sys::GpuPipelineLayoutDescriptor::new(&bind_group_layouts);

        if let Some(label) = layout.label {
            layout_js.set_label(label);
        }

        layout_js
    }
}
