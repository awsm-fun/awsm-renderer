//! WebGPU compatibility checks and reporting.

use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::renderer::DeviceRequestLimits;

/// Summary of WebGPU compatibility or failure reason.
#[derive(Clone, Debug)]
pub enum Compatibility {
    /// WebGPU is available and meets requirements.
    Compatible,
    /// The browser does not expose WebGPU.
    MissingGpu,
    /// Failed to request a GPU adapter.
    AdapterError(String),
    /// No adapter was returned by the browser.
    MissingAdapter,
    /// Failed to create a GPU device.
    DeviceError(String),
    /// No device was returned by the adapter.
    MissingDevice,
    /// Failed to fetch adapter limits.
    MissingLimits,
    /// The adapter does not meet required storage buffer limits.
    NotEnoughStorageBuffers { required: u32, available: u32 },
}

impl Compatibility {
    /// Returns a primary user-facing message describing the status.
    pub fn main_text(&self) -> String {
        match self {
            Compatibility::Compatible => "Compatible".to_string(),
            Compatibility::MissingGpu => "WebGPU is not supported in this browser.".to_string(),
            Compatibility::AdapterError(err) => format!("Error requesting GPU adapter: {}", err),
            Compatibility::MissingAdapter => "No suitable GPU adapter found.".to_string(),
            Compatibility::DeviceError(err) => format!("Error creating GPU device: {}", err),
            Compatibility::MissingDevice => "Failed to create a GPU device.".to_string(),
            Compatibility::MissingLimits => "Failed to retrieve GPU adapter limits.".to_string(),
            Compatibility::NotEnoughStorageBuffers {
                required,
                available,
            } => format!(
                "Insufficient storage buffers: required {}, available {}.",
                required, available
            ),
        }
    }

    /// Returns extra guidance for certain failure cases.
    pub fn extra_text(&self) -> Option<String> {
        match self {
            Compatibility::AdapterError(_) | Compatibility::MissingAdapter => {
                Some("This can sometimes be fixed by restarting the browser".to_string())
            }
            Compatibility::NotEnoughStorageBuffers { .. } => {
                Some("Please consider using a device with higher capabilities".to_string())
            }
            _ => None,
        }
    }
}

/// Requirements used when evaluating compatibility.
#[derive(Clone, Debug, Default)]
pub struct CompatibilityRequirements {
    /// Required number of storage buffers per shader stage.
    pub storage_buffers: Option<u32>,
}

impl Compatibility {
    /// Runs compatibility checks against the current browser and adapter.
    pub async fn check(requirements: Option<CompatibilityRequirements>) -> Self {
        let requirements = requirements.unwrap_or_default();

        let gpu = web_sys::window().unwrap().navigator().gpu();

        if gpu.is_null() || gpu.is_undefined() {
            return Self::MissingGpu;
        }

        let adapter: web_sys::GpuAdapter = match JsFuture::from(gpu.request_adapter()).await {
            Ok(adapter) => adapter.unchecked_into(),
            Err(err) => {
                return Self::AdapterError(
                    err.as_string()
                        .unwrap_or_else(|| "Unknown error".to_string()),
                );
            }
        };

        if adapter.is_null() || adapter.is_undefined() {
            return Self::MissingAdapter;
        }

        let adapter_limits = adapter.limits();
        if adapter_limits.is_null() || adapter_limits.is_undefined() {
            return Self::MissingLimits;
        }

        let descriptor = web_sys::GpuDeviceDescriptor::new();
        descriptor.set_required_limits(
            &DeviceRequestLimits::typical()
                .into_js(&adapter.limits())
                .unchecked_into(),
        );
        let device: web_sys::GpuDevice =
            match JsFuture::from(adapter.request_device_with_descriptor(&descriptor)).await {
                Ok(device) => device.unchecked_into(),
                Err(err) => {
                    return Self::DeviceError(
                        err.as_string()
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    );
                }
            };

        if device.is_null() || device.is_undefined() {
            return Self::MissingDevice;
        }

        let limits = device.limits();

        if let Some(required) = requirements.storage_buffers {
            if limits.max_storage_buffers_per_shader_stage() < required {
                return Self::NotEnoughStorageBuffers {
                    required,
                    available: limits.max_storage_buffers_per_shader_stage(),
                };
            }
        }

        //web_sys::console::log_1(&limits);

        Self::Compatible
    }
}
