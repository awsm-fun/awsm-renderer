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
    shadows::shader::{
        cache_key::ShaderCacheKeyShadow, masked_cache_key::ShaderCacheKeyShadowMasked,
        masked_template::ShaderTemplateShadowMasked, template::ShaderTemplateShadow,
    },
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

    /// Evict a shader module by key — drops the slotmap entry (releasing the
    /// `GpuShaderModule`) AND removes any cache entry pointing at it. Used by
    /// `unregister_material` to reclaim a deleted custom material's shader
    /// modules (the pipeline-leak fix — see docs/plans/mesh-pipeline-overhaul.md).
    /// Returns true if a module was removed.
    pub fn remove(&mut self, shader_key: ShaderKey) -> bool {
        let existed = self.lookup.remove(shader_key).is_some();
        self.cache.retain(|_, v| *v != shader_key);
        existed
    }

    /// Evict every cached shader module that is STALE relative to the current
    /// dynamic-material set (see [`ShaderCacheKey::is_stale_dynamic_set`]). Drops
    /// the slotmap entries and cache rows and returns the freed [`ShaderKey`]s so
    /// the caller can sweep the pipeline pools by shader key. This reclaims the
    /// opaque / edge / classify / transparent pipelines orphaned when a bucket-set
    /// change forces a recompile under a new cache key — the core of the
    /// dynamic-material pipeline-leak fix. See docs/plans/mesh-pipeline-overhaul.md.
    pub fn take_stale_dynamic_set_shader_keys(
        &mut self,
        current_dispatch_hash: u64,
        current_bucket_entries: &[crate::dynamic_materials::BucketEntry],
    ) -> std::collections::HashSet<ShaderKey> {
        let mut removed = std::collections::HashSet::new();
        self.cache.retain(|cache_key, shader_key| {
            if cache_key.is_stale_dynamic_set(current_dispatch_hash, current_bucket_entries) {
                removed.insert(*shader_key);
                false
            } else {
                true
            }
        });
        for shader_key in &removed {
            self.lookup.remove(*shader_key);
        }
        removed
    }

    /// Sync cache-only lookup: returns the key iff the shader is already
    /// compiled + cached (e.g. pre-warmed at boot), otherwise `None`.
    /// Never compiles. Lets sync per-frame paths (e.g. the lazy line
    /// pipeline compile kick) build pipeline cache keys without awaiting.
    pub fn get_cached_key(&self, cache_key: impl Into<ShaderCacheKey>) -> Option<ShaderKey> {
        self.cache.get(&cache_key.into()).copied()
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

        if let Err(err) = shader_module.validate_shader().await {
            let code = shader_descriptor.get_code();
            print_shader_source(&code, true);
            return Err(compile_error_with_source(&code, err));
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
            let n = pending.len();
            let t_start = web_sys::js_sys::Date::now();
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
            let dt_ms = web_sys::js_sys::Date::now() - t_start;
            // One log line per batched ensure_keys call. Filter via
            // the `awsm_renderer::boot_timing` target. Counts only the
            // cache misses (cache hits + dedup'd duplicates don't
            // contribute to the compile wall-clock).
            tracing::info!(
                target: "awsm_renderer::boot_timing",
                "Shaders::ensure_keys: {n} shaders compiled in {dt_ms:.0}ms",
            );
            for (i, result) in results.into_iter().enumerate() {
                if let Err(err) = result {
                    // Match the diagnostic behavior of `get_key`:
                    // print the offending source on a failed compile + quote
                    // the failing line(s) in the returned error.
                    let code = modules[i].2.get_code();
                    print_shader_source(&code, true);
                    return Err(compile_error_with_source(&code, err));
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

    /// Sync variant of [`Self::ensure_keys`] (Block D.1 PART 2). Skips
    /// the `validate_shader().await` round-trip so the scheduler's
    /// `submit_pipeline_group_batch` can install shader modules
    /// synchronously, build pipeline descriptors using the resolved
    /// keys, and issue `create_*_pipeline_async` promises in the same
    /// sync window — pushing only the pipeline promises into
    /// `FuturesUnordered`.
    ///
    /// **Trade-off**: validation errors surface later as pipeline-
    /// creation errors. The `create_*_pipeline_async` promise's
    /// rejection includes the underlying shader-compile diagnostic,
    /// so the diagnostic path is preserved — just delayed by one
    /// async hop. For the existing async `ensure_keys` path that
    /// wants early shader-error reporting, validate is still awaited.
    pub fn ensure_keys_sync_skip_validate<I>(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        cache_keys: I,
    ) -> Result<Vec<ShaderKey>>
    where
        I: IntoIterator<Item = ShaderCacheKey>,
    {
        let inputs: Vec<ShaderCacheKey> = cache_keys.into_iter().collect();
        let mut slots: Vec<Option<ShaderKey>> = Vec::with_capacity(inputs.len());
        let mut pending_for: HashMap<ShaderCacheKey, usize> = HashMap::new();
        let mut pending: Vec<(ShaderCacheKey, web_sys::GpuShaderModule)> = Vec::new();
        for cache_key in &inputs {
            if let Some(&shader_key) = self.cache.get(cache_key) {
                slots.push(Some(shader_key));
                continue;
            }
            if pending_for.contains_key(cache_key) {
                slots.push(None);
                continue;
            }
            let descriptor = ShaderTemplate::try_from(cache_key)?.into_descriptor()?;
            let module = gpu.compile_shader(&descriptor);
            pending_for.insert(cache_key.clone(), pending.len());
            pending.push((cache_key.clone(), module));
            slots.push(None);
        }
        // Install everything sync — validate is intentionally skipped.
        let mut pending_keys: Vec<ShaderKey> = Vec::with_capacity(pending.len());
        for (cache_key, module) in pending {
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
        Ok(slots
            .into_iter()
            .map(|s| s.expect("every input slot is filled by cache + pending"))
            .collect())
    }

    /// Returns a shader module by key.
    pub fn get(&self, shader_key: ShaderKey) -> Option<&web_sys::GpuShaderModule> {
        self.lookup.get(shader_key)
    }

    /// Reverse-lookup: returns the template debug label for a given
    /// `ShaderKey`, or `None` if the key isn't in the cache.
    ///
    /// Used by the per-pipeline boot-timing log to attach a
    /// human-readable shader-template name to each pipeline. Linear
    /// scan of the cache (O(N) in shader-count) — only called during
    /// pipeline-compile logging, not on the hot path.
    pub fn get_label(&self, shader_key: ShaderKey) -> Option<String> {
        for (cache_key, &key) in &self.cache {
            if key == shader_key {
                let template = ShaderTemplate::try_from(cache_key).ok()?;
                return template.debug_label().map(|s| s.to_string());
            }
        }
        None
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
    /// Masked (alpha-tested) shadow caster — cutout / hole-shaped shadows.
    /// Per-`shader_id` specialized; see [`ShaderCacheKeyShadowMasked`].
    ShadowMasked(ShaderCacheKeyShadowMasked),
    /// Fat-line shader (renderer-built editor / debug overlays).
    /// Hoisted out of `RenderPass` for the same reason as `Picker` —
    /// it's a top-level renderer concern, not a per-pass variant.
    Line(ShaderCacheKeyLine),
}

impl ShaderCacheKey {
    /// Whether this cache key is STALE relative to the current registered
    /// dynamic-material set (its `dispatch_hash` / bucket list).
    ///
    /// The opaque, edge-resolve, transparent and classify passes specialize
    /// against the WHOLE registered set (a `dispatch_hash` for the first three,
    /// the raw `bucket_entries` for classify). Registering or unregistering ANY
    /// dynamic material changes that signature, so on the next render every
    /// affected pass recompiles under a NEW cache key — and the OLD key's GPU
    /// pipeline is orphaned in the shared pool (the typed per-pass caches drop
    /// their key, the classify/transparent keys are self-invalidating). Sweeping
    /// the shader cache by this predicate after a bucket-set change reclaims them.
    /// Set-independent keys (picker, line, shadow, masked geometry/shadow — which
    /// key on their own `shader_id`, not the registered set) return false.
    /// See docs/plans/mesh-pipeline-overhaul.md.
    pub fn is_stale_dynamic_set(
        &self,
        current_dispatch_hash: u64,
        current_bucket_entries: &[crate::dynamic_materials::BucketEntry],
    ) -> bool {
        use crate::render_passes::shader_cache_key::ShaderCacheKeyRenderPass as Rp;
        match self {
            ShaderCacheKey::RenderPass(rp) => match rp {
                Rp::MaterialOpaque(k) => k.dispatch_hash != current_dispatch_hash,
                Rp::MaterialEdgeResolve(k) => k.dispatch_hash != current_dispatch_hash,
                Rp::MaterialTransparent(k) => k.dispatch_hash != current_dispatch_hash,
                Rp::MaterialClassify(k) => k.bucket_entries.as_slice() != current_bucket_entries,
                // The global MSAA edge-resolve compositors (skybox sampler +
                // final blend) carry only the bucket list (no dispatch_hash);
                // it still drifts on every registry change, minting a new key
                // and orphaning the old compute pipeline. Sweep them by bucket set.
                Rp::MaterialSkyboxEdgeResolve(k) => {
                    k.bucket_entries.as_slice() != current_bucket_entries
                }
                Rp::MaterialFinalBlend(k) => k.bucket_entries.as_slice() != current_bucket_entries,
                _ => false,
            },
            _ => false,
        }
    }
}

/// Shader template variants for renderer-managed shaders.
///
/// `RenderPass` is `Box`'d because its inner
/// [`ShaderTemplateRenderPass`] is ~256 bytes (it carries every
/// per-pass template's askama state inline) while the other
/// variants are tens of bytes. Without the indirection the enum
/// would pay the worst-case size on every variant — `clippy::large_enum_variant`
/// catches this. The `Box` cost is one heap allocation per shader
/// template creation, which is amortized away by the actual WGSL
/// compile that follows.
pub enum ShaderTemplate {
    RenderPass(Box<ShaderTemplateRenderPass>),
    Picker(ShaderTemplatePicker),
    Shadow(ShaderTemplateShadow),
    ShadowMasked(ShaderTemplateShadowMasked),
    Line(ShaderTemplateLine),
}

impl TryFrom<&ShaderCacheKey> for ShaderTemplate {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKey) -> Result<Self> {
        match value {
            ShaderCacheKey::RenderPass(cache_key) => {
                Ok(ShaderTemplate::RenderPass(Box::new(cache_key.try_into()?)))
            }
            ShaderCacheKey::Picker(cache_key) => Ok(ShaderTemplate::Picker(cache_key.into())),
            ShaderCacheKey::Shadow(cache_key) => Ok(ShaderTemplate::Shadow(cache_key.try_into()?)),
            ShaderCacheKey::ShadowMasked(cache_key) => {
                Ok(ShaderTemplate::ShadowMasked(cache_key.try_into()?))
            }
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
            ShaderTemplate::ShadowMasked(tmpl) => tmpl.debug_label(),
            ShaderTemplate::Line(tmpl) => tmpl.debug_label(),
        }
    }

    /// Renders the template into WGSL source.
    pub fn into_source(self) -> Result<String> {
        let source = match self {
            ShaderTemplate::RenderPass(tmpl) => tmpl.into_source()?,
            ShaderTemplate::Picker(tmpl) => tmpl.into_source()?,
            ShaderTemplate::Shadow(tmpl) => tmpl.into_source()?,
            ShaderTemplate::ShadowMasked(tmpl) => tmpl.into_source()?,
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

    /// A WGSL validation failure enriched with the offending source line(s) —
    /// far more debuggable than the bare line number (which indexes the
    /// *assembled* module, not any one author file).
    #[error("[shader] Compilation error:\n{0}")]
    CompilationDetail(String),

    #[error("[shader] Template error: {0:?}")]
    Template(#[from] askama::Error),
}

/// Turn a core validation error into a shader error that quotes the offending
/// source line(s). Non-validation errors pass through as `Compilation`.
pub(crate) fn compile_error_with_source(code: &str, err: AwsmCoreError) -> AwsmShaderError {
    let AwsmCoreError::ShaderValidation(msgs) = &err else {
        return AwsmShaderError::Compilation(err);
    };
    let lines: Vec<&str> = code.lines().collect();
    let mut out = String::from("WGSL validation failed:");
    for m in msgs {
        out.push_str(&format!(
            "\n  line {}:{}: {}",
            m.line_num, m.line_pos, m.message
        ));
        let ln = m.line_num as usize;
        if ln >= 1 && ln <= lines.len() {
            out.push_str(&format!("\n    > {}", lines[ln - 1].trim_end()));
        }
    }
    AwsmShaderError::CompilationDetail(out)
}
