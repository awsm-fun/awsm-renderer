//! Fragment state descriptors and blending helpers.

use std::collections::BTreeMap;

use crate::texture::TextureFormat;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use super::constants::{ConstantOverrideKey, ConstantOverrideValue};

/// Fragment stage descriptor.
#[derive(Debug, Clone)]
pub struct FragmentState<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#fragment_object_structure
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuFragmentState.html
    pub constants: BTreeMap<ConstantOverrideKey, ConstantOverrideValue>,
    pub entry_point: Option<&'a str>,
    pub module: &'a web_sys::GpuShaderModule,
    pub targets: Vec<ColorTargetState>,
}

/// Color target state for a render pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorTargetState {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#targets
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuColorTargetState.html
    pub blend: Option<BlendState>,
    pub format: TextureFormat,
    pub write_mask_all: bool,
    pub write_mask_alpha: bool,
    pub write_mask_red: bool,
    pub write_mask_green: bool,
    pub write_mask_blue: bool,
}

impl ColorTargetState {
    /// Creates a color target state for a given format.
    pub fn new(format: TextureFormat) -> Self {
        Self {
            blend: None,
            format,
            write_mask_all: true,
            write_mask_alpha: false,
            write_mask_red: false,
            write_mask_green: false,
            write_mask_blue: false,
        }
    }

    /// Sets blending for the target.
    pub fn with_blend(mut self, blend: BlendState) -> Self {
        self.blend = Some(blend);
        self
    }

    /// Enables all color write mask bits.
    pub fn with_write_mask_all(mut self) -> Self {
        self.write_mask_all = true;
        self
    }

    /// Enables alpha channel writes.
    pub fn with_write_mask_alpha(mut self) -> Self {
        self.write_mask_alpha = true;
        self
    }

    /// Enables red channel writes.
    pub fn with_write_mask_red(mut self) -> Self {
        self.write_mask_red = true;
        self
    }

    /// Enables green channel writes.
    pub fn with_write_mask_green(mut self) -> Self {
        self.write_mask_green = true;
        self
    }

    /// Enables blue channel writes.
    pub fn with_write_mask_blue(mut self) -> Self {
        self.write_mask_blue = true;
        self
    }

    /// Returns the WebGPU write mask bitfield.
    pub fn write_mask_u32(&self) -> u32 {
        let mut mask = 0;
        if self.write_mask_all {
            mask |= web_sys::gpu_color_write::ALL;
        }

        if self.write_mask_alpha {
            mask |= web_sys::gpu_color_write::ALPHA;
        }

        if self.write_mask_red {
            mask |= web_sys::gpu_color_write::RED;
        }

        if self.write_mask_green {
            mask |= web_sys::gpu_color_write::GREEN;
        }
        if self.write_mask_blue {
            mask |= web_sys::gpu_color_write::BLUE;
        }

        mask
    }
}

impl std::hash::Hash for ColorTargetState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.blend.hash(state);
        (self.format as u32).hash(state);
        self.write_mask_u32().hash(state);
    }
}

/// Blend state for a color target.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct BlendState {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createRenderPipeline#blend
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuBlendState.html
    pub alpha: BlendComponent,
    pub color: BlendComponent,
}

impl BlendState {
    /// Creates a blend state with color and alpha components.
    pub fn new(alpha: BlendComponent, color: BlendComponent) -> Self {
        Self { alpha, color }
    }
}

/// Blend component configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlendComponent {
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuBlendComponent.html
    pub operation: Option<BlendOperation>,
    pub src_factor: Option<BlendFactor>,
    pub dst_factor: Option<BlendFactor>,
}

impl BlendComponent {
    /// Creates a default blend component.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the blend operation.
    pub fn with_operation(mut self, operation: BlendOperation) -> Self {
        self.operation = Some(operation);
        self
    }
    /// Sets the source blend factor.
    pub fn with_src_factor(mut self, src_factor: BlendFactor) -> Self {
        self.src_factor = Some(src_factor);
        self
    }
    /// Sets the destination blend factor.
    pub fn with_dst_factor(mut self, dst_factor: BlendFactor) -> Self {
        self.dst_factor = Some(dst_factor);
        self
    }
}

impl std::hash::Hash for BlendComponent {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.operation.map(|x| x as u32).hash(state);
        self.src_factor.map(|x| x as u32).hash(state);
        self.dst_factor.map(|x| x as u32).hash(state);
    }
}

// https://docs.rs/web-sys/latest/web_sys/enum.GpuBlendFactor.html
/// WebGPU blend factor.
pub type BlendFactor = web_sys::GpuBlendFactor;
// https://docs.rs/web-sys/latest/web_sys/enum.GpuBlendOperation.html
/// WebGPU blend operation.
pub type BlendOperation = web_sys::GpuBlendOperation;

// js conversions

impl<'a> FragmentState<'a> {
    /// Creates a fragment state for a module and entry point.
    pub fn new(
        module: &'a web_sys::GpuShaderModule,
        entry_point: Option<&'a str>,
        targets: Vec<ColorTargetState>,
    ) -> Self {
        Self {
            constants: BTreeMap::new(),
            entry_point,
            module,
            targets,
        }
    }

    /// Adds a constant override.
    pub fn with_constant(
        mut self,
        binding: ConstantOverrideKey,
        constant: ConstantOverrideValue,
    ) -> Self {
        self.constants.insert(binding, constant);
        self
    }
    /// Adds a color target.
    pub fn with_target(mut self, target: ColorTargetState) -> Self {
        self.targets.push(target);
        self
    }
}

impl From<FragmentState<'_>> for web_sys::GpuFragmentState {
    fn from(state: FragmentState) -> web_sys::GpuFragmentState {
        let targets: Vec<js_sys::JsOption<web_sys::GpuColorTargetState>> = state
            .targets
            .into_iter()
            .map(|target| js_sys::JsOption::wrap(web_sys::GpuColorTargetState::from(target)))
            .collect();

        let state_js = web_sys::GpuFragmentState::new(state.module, &targets);

        if let Some(entry_point) = state.entry_point {
            state_js.set_entry_point(entry_point);
        }

        if !state.constants.is_empty() {
            let obj = js_sys::Object::new();
            for (binding, constant) in state.constants {
                js_sys::Reflect::set(&obj, &JsValue::from(binding), &JsValue::from(constant))
                    .unwrap_throw();
            }
            state_js.set_constants(&obj.unchecked_into());
        }

        state_js
    }
}

impl From<ColorTargetState> for web_sys::GpuColorTargetState {
    fn from(state: ColorTargetState) -> web_sys::GpuColorTargetState {
        let state_js = web_sys::GpuColorTargetState::new(state.format);

        let write_mask = state.write_mask_u32();
        if write_mask != 0 {
            state_js.set_write_mask(write_mask);
        }

        if let Some(blend) = state.blend {
            state_js.set_blend(&web_sys::GpuBlendState::from(blend));
        }

        state_js
    }
}

impl From<BlendState> for web_sys::GpuBlendState {
    fn from(state: BlendState) -> web_sys::GpuBlendState {
        web_sys::GpuBlendState::new(
            // not sure why these are reversed compared to opengl, but they are:
            // https://docs.rs/web-sys/latest/web_sys/struct.GpuBlendState.html#method.new
            // vs. opengl's https://registry.khronos.org/OpenGL-Refpages/gl4/html/glBlendFuncSeparate.xhtml
            &web_sys::GpuBlendComponent::from(state.alpha),
            &web_sys::GpuBlendComponent::from(state.color),
        )
    }
}

impl From<BlendComponent> for web_sys::GpuBlendComponent {
    fn from(component: BlendComponent) -> web_sys::GpuBlendComponent {
        let component_js = web_sys::GpuBlendComponent::new();

        if let Some(operation) = component.operation {
            component_js.set_operation(operation);
        }
        if let Some(src_factor) = component.src_factor {
            component_js.set_src_factor(src_factor);
        }
        if let Some(dst_factor) = component.dst_factor {
            component_js.set_dst_factor(dst_factor);
        }

        component_js
    }
}
