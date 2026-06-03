//! Shader module helpers and compilation info.

use crate::error::{AwsmCoreError, Result};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// Builder for a shader module descriptor.
pub struct ShaderModuleDescriptor<'a> {
    pub code: &'a str,
    pub label: Option<&'a str>,
}

impl<'a> ShaderModuleDescriptor<'a> {
    /// Creates a shader module descriptor.
    pub fn new(code: &'a str, label: Option<&'a str>) -> Self {
        Self { code, label }
    }
}

impl From<ShaderModuleDescriptor<'_>> for web_sys::GpuShaderModuleDescriptor {
    fn from(shader_code: ShaderModuleDescriptor) -> Self {
        let descriptor = web_sys::GpuShaderModuleDescriptor::new(shader_code.code);

        if let Some(label) = shader_code.label {
            descriptor.set_label(label);
        }

        descriptor
    }
}

/// Extension trait for shader compilation info.
pub trait ShaderModuleExt {
    fn get_compilation_info_ext(
        &self,
    ) -> impl std::future::Future<Output = Result<ShaderCompilationInfo>>;

    fn validate_shader(&self) -> impl std::future::Future<Output = Result<()>>;
}

impl ShaderModuleExt for web_sys::GpuShaderModule {
    async fn get_compilation_info_ext(&self) -> Result<ShaderCompilationInfo> {
        let compilation_info = JsFuture::from(self.get_compilation_info())
            .await
            .map_err(AwsmCoreError::shader_compilation_info)?;
        Ok(ShaderCompilationInfo::new(
            compilation_info.unchecked_into(),
        ))
    }

    async fn validate_shader(&self) -> Result<()> {
        let compilation_info = self.get_compilation_info_ext().await?;

        if compilation_info.errors.is_empty() {
            Ok(())
        } else {
            Err(AwsmCoreError::ShaderValidation(compilation_info.errors))
        }
    }
}

/// Compilation info for a shader module.
#[derive(Clone, Debug)]
pub struct ShaderCompilationInfo {
    pub errors: Vec<ShaderCompilationMessage>,
    pub warnings: Vec<ShaderCompilationMessage>,
    pub infos: Vec<ShaderCompilationMessage>,
}

/// Single shader compilation message.
#[derive(Clone, Debug)]
pub struct ShaderCompilationMessage {
    pub length: u64,
    pub line_num: u64,
    pub line_pos: u64,
    pub message: String,
    pub offset: u64,
}

impl ShaderCompilationInfo {
    /// Parses compilation info into categorized messages.
    pub fn new(info: web_sys::GpuCompilationInfo) -> Self {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut infos = Vec::new();
        let js_messages = info.messages();

        for i in 0..js_messages.length() {
            let js_message: web_sys::GpuCompilationMessage = js_messages.get(i).unchecked_into();

            let message = ShaderCompilationMessage {
                length: js_message.length() as u64,
                line_num: js_message.line_num() as u64,
                line_pos: js_message.line_pos() as u64,
                message: js_message.message(),
                offset: js_message.offset() as u64,
            };

            match js_message.type_() {
                web_sys::GpuCompilationMessageType::Error => errors.push(message),
                web_sys::GpuCompilationMessageType::Warning => warnings.push(message),
                web_sys::GpuCompilationMessageType::Info => infos.push(message),
                _ => {
                    // Handle unknown message types if necessary
                    tracing::warn!(
                        "Unknown shader compilation message type: {:?}",
                        js_message.type_()
                    );
                }
            }
        }

        Self {
            errors,
            warnings,
            infos,
        }
    }
}
