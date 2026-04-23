//! Pipeline descriptor wrappers and helpers.

pub mod constants;
pub mod depth_stencil;
pub mod fragment;
pub mod layout;
pub mod multisample;
pub mod primitive;
pub mod vertex;

use std::collections::BTreeMap;

use constants::{ConstantOverrideKey, ConstantOverrideValue};
use depth_stencil::DepthStencilState;
use fragment::FragmentState;
use layout::PipelineLayoutKind;
use multisample::MultisampleState;
use primitive::PrimitiveState;
use vertex::VertexState;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// Builder for a render pipeline descriptor.
#[derive(Debug, Clone)]
pub struct RenderPipelineDescriptor<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#descriptor
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuRenderPipelineDescriptor.html
    // fill this out with a lot more detail
    depth_stencil: Option<DepthStencilState>,
    fragment: Option<FragmentState<'a>>,
    label: Option<&'a str>,
    layout: PipelineLayoutKind<'a>,
    multisample: Option<MultisampleState>,
    primitive: Option<PrimitiveState>,
    vertex: VertexState<'a>,
}

impl<'a> RenderPipelineDescriptor<'a> {
    /// Creates a descriptor with the required vertex state.
    pub fn new(vertex: VertexState<'a>, label: Option<&'a str>) -> Self {
        Self {
            depth_stencil: None,
            fragment: None,
            label,
            layout: PipelineLayoutKind::Auto,
            multisample: None,
            primitive: None,
            vertex,
        }
    }

    /// Sets the depth/stencil state.
    pub fn with_depth_stencil(mut self, depth_stencil: DepthStencilState) -> Self {
        self.depth_stencil = Some(depth_stencil);
        self
    }
    /// Sets the fragment state.
    pub fn with_fragment(mut self, fragment: FragmentState<'a>) -> Self {
        self.fragment = Some(fragment);
        self
    }

    /// Sets the pipeline layout.
    pub fn with_layout(mut self, layout: PipelineLayoutKind<'a>) -> Self {
        self.layout = layout;
        self
    }
    /// Sets the multisample state.
    pub fn with_multisample(mut self, multisample: MultisampleState) -> Self {
        self.multisample = Some(multisample);
        self
    }
    /// Sets the primitive state.
    pub fn with_primitive(mut self, primitive: PrimitiveState) -> Self {
        self.primitive = Some(primitive);
        self
    }
}

/// Builder for a compute pipeline descriptor.
pub struct ComputePipelineDescriptor<'a, 'b> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createComputePipeline#descriptor
    compute: ProgrammableStage<'a, 'b>,
    layout: PipelineLayoutKind<'a>,
    label: Option<&'a str>,
}

impl<'a, 'b> ComputePipelineDescriptor<'a, 'b> {
    /// Creates a compute pipeline descriptor.
    pub fn new(
        compute: ProgrammableStage<'a, 'b>,
        layout: PipelineLayoutKind<'a>,
        label: Option<&'a str>,
    ) -> Self {
        Self {
            label,
            layout,
            compute,
        }
    }
}

/// Describes a programmable shader stage.
pub struct ProgrammableStage<'a, 'b> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createComputePipeline#descriptor
    pub module: &'a web_sys::GpuShaderModule,
    pub entry_point: Option<&'b str>,
    pub constant_overrides: BTreeMap<ConstantOverrideKey, ConstantOverrideValue>,
}

impl<'a, 'b> ProgrammableStage<'a, 'b> {
    /// Creates a programmable stage with an optional entry point.
    pub fn new(module: &'a web_sys::GpuShaderModule, entry_point: Option<&'b str>) -> Self {
        Self {
            module,
            entry_point,
            constant_overrides: BTreeMap::new(),
        }
    }

    /// Adds a constant override for the stage.
    pub fn with_push_constant_override(
        mut self,
        key: ConstantOverrideKey,
        value: ConstantOverrideValue,
    ) -> Self {
        self.constant_overrides.insert(key, value);
        self
    }
}

// js conversions
impl From<RenderPipelineDescriptor<'_>> for web_sys::GpuRenderPipelineDescriptor {
    fn from(pipeline: RenderPipelineDescriptor) -> web_sys::GpuRenderPipelineDescriptor {
        let RenderPipelineDescriptor {
            depth_stencil,
            fragment,
            label,
            layout,
            multisample,
            primitive,
            vertex,
        } = pipeline;

        let layout_js: JsValue = layout.into();
        let pipeline_js = web_sys::GpuRenderPipelineDescriptor::new(
            layout_js.unchecked_ref(),
            &web_sys::GpuVertexState::from(vertex),
        );

        if let Some(depth_stencil) = depth_stencil {
            pipeline_js.set_depth_stencil(&web_sys::GpuDepthStencilState::from(depth_stencil));
        }

        if let Some(fragment) = fragment {
            pipeline_js.set_fragment(&web_sys::GpuFragmentState::from(fragment));
        }

        if let Some(multisample) = multisample {
            pipeline_js.set_multisample(&web_sys::GpuMultisampleState::from(multisample));
        }

        if let Some(primitive) = primitive {
            pipeline_js.set_primitive(&web_sys::GpuPrimitiveState::from(primitive));
        }

        if let Some(label) = label {
            pipeline_js.set_label(label.as_ref());
        }

        pipeline_js
    }
}

impl From<ComputePipelineDescriptor<'_, '_>> for web_sys::GpuComputePipelineDescriptor {
    fn from(pipeline: ComputePipelineDescriptor) -> web_sys::GpuComputePipelineDescriptor {
        let ComputePipelineDescriptor {
            label,
            layout,
            compute,
        } = pipeline;

        let layout_js: JsValue = layout.into();
        let pipeline_js = web_sys::GpuComputePipelineDescriptor::new(
            layout_js.unchecked_ref(),
            &compute.into(),
        );

        if let Some(label) = label {
            pipeline_js.set_label(label);
        }

        pipeline_js
    }
}

impl From<ProgrammableStage<'_, '_>> for web_sys::GpuProgrammableStage {
    fn from(compute: ProgrammableStage) -> web_sys::GpuProgrammableStage {
        let compute_js = web_sys::GpuProgrammableStage::new(compute.module);

        if let Some(entry_point) = compute.entry_point {
            compute_js.set_entry_point(entry_point);
        }

        if !compute.constant_overrides.is_empty() {
            let obj = js_sys::Object::new();
            for (binding, constant) in compute.constant_overrides {
                js_sys::Reflect::set(&obj, &JsValue::from(binding), &JsValue::from(constant))
                    .unwrap_throw();
            }
            compute_js.set_constants(&obj.unchecked_into());
        }

        compute_js
    }
}
