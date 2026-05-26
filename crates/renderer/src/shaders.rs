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
        lines::shader::{cache_key::ShaderCacheKeyLine, template::ShaderTemplateLine},
        shader_cache_key::ShaderCacheKeyRenderPass,
        shader_template::ShaderTemplateRenderPass,
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
    /// browser compiles concurrently. **Returns the resolved
    /// `ShaderKey`s in input order**, so callers can build pipeline
    /// cache keys from the batch without a follow-up `get_key`
    /// round-trip (matches the shape of
    /// [`crate::pipelines::render_pipeline::RenderPipelines::ensure_keys`] and
    /// [`crate::pipelines::compute_pipeline::ComputePipelines::ensure_keys`] —
    /// both return their respective key vectors).
    ///
    /// **Concurrency model:** `compile_shader` is synchronous — it
    /// returns a module handle immediately and the browser begins
    /// compilation in the background. The slow part is
    /// `validate_shader().await`, which blocks until the compile
    /// finishes. By firing all `compile_shader` calls before
    /// `await`ing any validation, we let the driver compile N
    /// shaders in parallel instead of N times serially.
    ///
    /// **Transaction shape:** call this with a `Vec` of every shader
    /// cache key you need; the resulting `Vec<ShaderKey>` lines up
    /// 1:1 (including for duplicate inputs and pre-cached entries —
    /// duplicates resolve to the same key, pre-cached entries
    /// resolve from the existing cache). No follow-up `get_key`
    /// needed.
    pub async fn ensure_keys<I>(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        cache_keys: I,
    ) -> Result<Vec<ShaderKey>>
    where
        I: IntoIterator<Item = ShaderCacheKey>,
    {
        let inputs: Vec<ShaderCacheKey> = cache_keys.into_iter().collect();
        // Output slot per input (cache hits + duplicates fill in
        // immediately; misses point at `pending` indices below).
        let mut slots: Vec<Option<ShaderKey>> = Vec::with_capacity(inputs.len());
        // Per-cache-key dedup table for the misses in this batch —
        // inputs `[A, B, A]` only compile A once but all three slots
        // resolve to the same key.
        let mut pending_for: HashMap<ShaderCacheKey, usize> = HashMap::new();
        let mut pending: Vec<(ShaderCacheKey, web_sys::GpuShaderModuleDescriptor)> = Vec::new();
        for cache_key in &inputs {
            if let Some(&shader_key) = self.cache.get(cache_key) {
                slots.push(Some(shader_key));
                continue;
            }
            if pending_for.contains_key(cache_key) {
                // Duplicate within this batch — fill the slot once
                // the compile finishes via the pending-index path.
                slots.push(None);
                continue;
            }
            let descriptor = ShaderTemplate::try_from(cache_key)?.into_descriptor()?;
            pending_for.insert(cache_key.clone(), pending.len());
            pending.push((cache_key.clone(), descriptor));
            slots.push(None);
        }

        if !pending.is_empty() {
            // Issue every compile_shader synchronously so the browser
            // kicks off all compiles before we await anything.
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
                    // Match the diagnostic behavior of `get_key`:
                    // print the offending source on a failed compile.
                    print_shader_source(&modules[i].2.get_code(), true);
                    return Err(err);
                }
            }

            // Install everything into the cache in one go, then
            // back-fill any unresolved slots.
            let mut pending_keys: Vec<ShaderKey> = Vec::with_capacity(modules.len());
            for (cache_key, module, _) in modules {
                let shader_key = self.lookup.insert(module);
                self.cache.insert(cache_key, shader_key);
                pending_keys.push(shader_key);
            }
            for (slot, cache_key) in slots.iter_mut().zip(&inputs) {
                if slot.is_some() {
                    continue;
                }
                if let Some(&pending_idx) = pending_for.get(cache_key) {
                    *slot = Some(pending_keys[pending_idx]);
                }
            }
        }

        Ok(slots
            .into_iter()
            .map(|s| s.expect("every input slot is filled by cache + pending"))
            .collect())
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
    /// Fat-line shader (renderer-built editor / debug overlays).
    /// Hoisted out of `RenderPass` for the same reason as `Picker` —
    /// it's a top-level renderer concern, not a per-pass variant.
    Line(ShaderCacheKeyLine),
}

/// Shader template variants for renderer-managed shaders.
pub enum ShaderTemplate {
    RenderPass(ShaderTemplateRenderPass),
    Picker(ShaderTemplatePicker),
    Shadow(ShaderTemplateShadow),
    Line(ShaderTemplateLine),
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
            ShaderCacheKey::Line(cache_key) => Ok(ShaderTemplate::Line(cache_key.try_into()?)),
        }
    }
}

impl ShaderTemplate {
    /// Builds a GPU shader module descriptor with a debug label.
    ///
    /// Labels are kept in release builds too — they're just a string
    /// crossing the WASM/JS boundary into the GPU shader module's
    /// `label` field, which Chrome's WebGPU dev tools, Spector.js,
    /// and `popErrorScope` messages all surface. The runtime cost is
    /// a single `to_string` allocation per shader-module *creation*
    /// (not per dispatch), which is unmeasurable next to the actual
    /// WGSL compile.
    pub fn into_descriptor(self) -> Result<web_sys::GpuShaderModuleDescriptor> {
        let label = self.debug_label().map(|l| l.to_string());
        Ok(ShaderModuleDescriptor::new(&self.into_source()?, label.as_deref()).into())
    }

    /// Returns an optional debug label for this shader template.
    pub fn debug_label(&self) -> Option<&str> {
        match self {
            ShaderTemplate::RenderPass(tmpl) => tmpl.debug_label(),
            ShaderTemplate::Picker(tmpl) => tmpl.debug_label(),
            ShaderTemplate::Shadow(tmpl) => tmpl.debug_label(),
            ShaderTemplate::Line(tmpl) => tmpl.debug_label(),
        }
    }

    /// Renders the template into WGSL source.
    pub fn into_source(self) -> Result<String> {
        let source = match self {
            ShaderTemplate::RenderPass(tmpl) => tmpl.into_source()?,
            ShaderTemplate::Picker(tmpl) => tmpl.into_source()?,
            ShaderTemplate::Shadow(tmpl) => tmpl.into_source()?,
            ShaderTemplate::Line(tmpl) => tmpl.into_source()?,
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
