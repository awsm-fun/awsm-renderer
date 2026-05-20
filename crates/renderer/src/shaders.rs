//! Shader cache and template helpers.

use std::collections::HashMap;

use awsm_renderer_core::{
    error::AwsmCoreError, renderer::AwsmRendererWebGpu, shaders::ShaderModuleDescriptor,
};
use slotmap::{new_key_type, SlotMap};
use thiserror::Error;

use awsm_renderer_core::shaders::ShaderModuleExt;

use crate::{
    picker::{ShaderCacheKeyPicker, ShaderTemplatePicker},
    render_passes::{
        shader_cache_key::ShaderCacheKeyRenderPass, shader_template::ShaderTemplateRenderPass,
    },
    shadows::shader::{cache_key::ShaderCacheKeyShadow, template::ShaderTemplateShadow},
};

/// Cached GPU shader modules keyed by template parameters.
pub struct Shaders {
    lookup: SlotMap<ShaderKey, web_sys::GpuShaderModule>,
    cache: HashMap<ShaderCacheKey, ShaderKey>,
}
impl Shaders {
    /// Creates an empty shader cache.
    pub fn new() -> Self {
        Self {
            lookup: SlotMap::with_key(),
            cache: HashMap::new(),
        }
    }

    // usually used with hooks, i.e. third-party shaders that are completely outside
    // of the normal renderer system
    /// Inserts a shader module without caching it by template key.
    pub fn insert_uncached(&mut self, shader_module: web_sys::GpuShaderModule) -> ShaderKey {
        self.lookup.insert(shader_module)
    }

    /// Returns a cached shader key, compiling and caching on demand.
    pub async fn get_key(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        cache_key: impl Into<ShaderCacheKey>,
    ) -> Result<ShaderKey> {
        let cache_key: ShaderCacheKey = cache_key.into();
        if let Some(shader_key) = self.cache.get(&cache_key) {
            return Ok(*shader_key);
        }

        let shader_descriptor = ShaderTemplate::try_from(&cache_key)?.into_descriptor()?;
        let shader_module = gpu.compile_shader(&shader_descriptor);

        if let Err(err) = shader_module
            .validate_shader()
            .await
            .map_err(AwsmShaderError::Compilation)
        {
            print_shader_source(&shader_descriptor.get_code(), true);
            return Err(err);
        }

        let shader_key = self.lookup.insert(shader_module.clone());

        self.cache.insert(cache_key.clone(), shader_key);

        Ok(shader_key)
    }

    /// Pre-warms the cache for a batch of shader keys, issuing all
    /// browser compiles concurrently.
    ///
    /// `compile_shader` is synchronous — it returns a module handle
    /// immediately and the browser begins compilation in the background.
    /// The slow part is `validate_shader().await`, which blocks until
    /// the compile finishes. By firing all `compile_shader` calls before
    /// `await`ing any validation, we let the driver compile N shaders in
    /// parallel instead of N times serially.
    ///
    /// Subsequent `get_key` calls for the same cache keys then hit the
    /// cache and complete without any further async work.
    pub async fn ensure_keys<I>(&mut self, gpu: &AwsmRendererWebGpu, cache_keys: I) -> Result<()>
    where
        I: IntoIterator<Item = ShaderCacheKey>,
    {
        // De-dupe in one pass while skipping anything already cached.
        let mut seen: HashMap<ShaderCacheKey, ()> = HashMap::new();
        let mut pending: Vec<(ShaderCacheKey, web_sys::GpuShaderModuleDescriptor)> = Vec::new();
        for cache_key in cache_keys {
            if self.cache.contains_key(&cache_key) || seen.contains_key(&cache_key) {
                continue;
            }
            seen.insert(cache_key.clone(), ());
            let descriptor = ShaderTemplate::try_from(&cache_key)?.into_descriptor()?;
            pending.push((cache_key, descriptor));
        }
        if pending.is_empty() {
            return Ok(());
        }

        // Issue every compile_shader synchronously so the browser kicks
        // off all compiles before we await anything.
        let modules: Vec<(
            ShaderCacheKey,
            web_sys::GpuShaderModule,
            web_sys::GpuShaderModuleDescriptor,
        )> = pending
            .into_iter()
            .map(|(k, desc)| {
                let module = gpu.compile_shader(&desc);
                (k, module, desc)
            })
            .collect();

        // Await every validation in parallel.
        let validate_futures = modules.iter().map(|(_, m, _)| m.validate_shader());
        let results = futures::future::join_all(validate_futures).await;
        for (i, result) in results.into_iter().enumerate() {
            if let Err(err) = result.map_err(AwsmShaderError::Compilation) {
                // Match the diagnostic behavior of `get_key`: print the
                // offending source on a failed compile.
                print_shader_source(&modules[i].2.get_code(), true);
                return Err(err);
            }
        }

        // Install everything into the cache in one go.
        for (cache_key, module, _) in modules {
            let shader_key = self.lookup.insert(module);
            self.cache.insert(cache_key, shader_key);
        }
        Ok(())
    }

    /// Returns a shader module by key.
    pub fn get(&self, shader_key: ShaderKey) -> Option<&web_sys::GpuShaderModule> {
        self.lookup.get(shader_key)
    }
}

impl Default for Shaders {
    fn default() -> Self {
        Self::new()
    }
}

/// Shader cache keys for renderer-managed shader templates.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub enum ShaderCacheKey {
    RenderPass(ShaderCacheKeyRenderPass),
    Picker(ShaderCacheKeyPicker),
    Shadow(ShaderCacheKeyShadow),
}

/// Shader template variants for renderer-managed shaders.
pub enum ShaderTemplate {
    RenderPass(ShaderTemplateRenderPass),
    Picker(ShaderTemplatePicker),
    Shadow(ShaderTemplateShadow),
}

impl TryFrom<&ShaderCacheKey> for ShaderTemplate {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKey) -> Result<Self> {
        match value {
            ShaderCacheKey::RenderPass(cache_key) => {
                Ok(ShaderTemplate::RenderPass(cache_key.try_into()?))
            }
            ShaderCacheKey::Picker(cache_key) => Ok(ShaderTemplate::Picker(cache_key.into())),
            ShaderCacheKey::Shadow(cache_key) => Ok(ShaderTemplate::Shadow(cache_key.try_into()?)),
        }
    }
}

impl ShaderTemplate {
    #[cfg(debug_assertions)]
    /// Builds a GPU shader module descriptor with a debug label.
    pub fn into_descriptor(self) -> Result<web_sys::GpuShaderModuleDescriptor> {
        let label = self.debug_label().map(|l| l.to_string());
        Ok(ShaderModuleDescriptor::new(&self.into_source()?, label.as_deref()).into())
    }

    #[cfg(not(debug_assertions))]
    /// Builds a GPU shader module descriptor without debug metadata.
    pub fn into_descriptor(self) -> Result<web_sys::GpuShaderModuleDescriptor> {
        Ok(ShaderModuleDescriptor::new(&self.into_source()?, None).into())
    }

    #[cfg(debug_assertions)]
    /// Returns an optional debug label for this shader template.
    pub fn debug_label(&self) -> Option<&str> {
        match self {
            ShaderTemplate::RenderPass(tmpl) => tmpl.debug_label(),
            ShaderTemplate::Picker(tmpl) => tmpl.debug_label(),
            ShaderTemplate::Shadow(tmpl) => tmpl.debug_label(),
        }
    }

    /// Renders the template into WGSL source.
    pub fn into_source(self) -> Result<String> {
        let source = match self {
            ShaderTemplate::RenderPass(tmpl) => tmpl.into_source()?,
            ShaderTemplate::Picker(tmpl) => tmpl.into_source()?,
            ShaderTemplate::Shadow(tmpl) => tmpl.into_source()?,
        };
        //tracing::info!("{:#?}", tmpl);
        // print_shader_source(&source, true);

        Ok(source)
    }
}

#[allow(dead_code)]
/// Logs shader source to the console for debugging.
pub fn print_shader_source(source: &str, with_line_numbers: bool) {
    let mut output = "\n".to_string();
    for (line_number, line) in (1..).zip(source.lines()) {
        let formatted_line = match with_line_numbers {
            true => format!("{line_number:>4}: {line}\n"),
            false => format!("{line}\n"),
        };
        output.push_str(&formatted_line);
    }

    web_sys::console::log_1(&web_sys::wasm_bindgen::JsValue::from(output.as_str()));
}

new_key_type! {
    /// SlotMap key for cached shader modules.
    pub struct ShaderKey;
}

/// Shader result type.
pub type Result<T> = std::result::Result<T, AwsmShaderError>;
/// Shader template and compilation errors.
#[derive(Error, Debug)]
pub enum AwsmShaderError {
    #[error("[shader] source error: {0}")]
    DuplicateAttribute(String),

    #[error("[shader] Compilation error: {0:?}")]
    Compilation(AwsmCoreError),

    #[error("[shader] Template error: {0:?}")]
    Template(#[from] askama::Error),
}
