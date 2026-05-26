//! Runtime-registered dynamic materials.
//!
//! Provides the renderer-side facade over `awsm_materials::registry::MaterialRegistry`,
//! the cache-key extension that invalidates per-shader-id pipelines when the
//! registry's [`awsm_materials::MaterialShaderId`] set
//! changes, and (Phase 6+) the extras-pool storage buffer + allocator that
//! backs `BufferSlot` data.
//!
//! Phase 0 ships the skeleton — empty [`DynamicMaterials`] struct and the
//! stub `register_material` / `unregister_material` plumbing on
//! [`AwsmRenderer`](crate::AwsmRenderer). Subsequent phases fill in the
//! registry, the template substitutions, and the extras pool.

pub mod error;
pub mod extras_pool;

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_materials::{MaterialAlphaMode, MaterialShaderId};

pub use error::AwsmDynamicMaterialError;

use awsm_materials::dynamic::{DynamicMaterialContext, DynamicTextureBinding};
use awsm_materials::dynamic_layout::MaterialLayout;
use awsm_materials::TextureContext;

/// Adapter that implements [`DynamicMaterialContext`] over the
/// renderer's [`DynamicMaterials`] registry.
///
/// `layout()` + `alpha_mode()` both look up the registry entry on
/// demand — no per-call HashMap clone. Used on the hot
/// `Materials::update` path, so this matters: every material write
/// constructs one of these and the eager-clone version we ran
/// previously copied every registered material's layout
/// (uniforms + textures + buffers Vecs) on every write.
///
/// `resolve_texture_index` resolves a bound texture key against the
/// renderer's [`TextureContext`] (when attached via
/// [`with_textures`](Self::with_textures)) to the packed
/// `array_and_layer` encoding documented on
/// [`shared_wgsl::TextureInfoRaw`]. Unbound slots and lookups that
/// can't resolve return `u32::MAX` (the WGSL helpers treat that as
/// "no texture"). `buffer_slice` resolves through the extras pool
/// when one was attached via [`with_extras`](Self::with_extras).
pub struct DynamicMaterialPackContext<'a> {
    materials: &'a DynamicMaterials,
    textures: Option<&'a dyn TextureContext>,
    extras: Option<&'a extras_pool::ExtrasPool>,
}

impl<'a> DynamicMaterialPackContext<'a> {
    /// Wraps a `&DynamicMaterials` for use as a
    /// [`DynamicMaterialContext`]. Layouts are looked up lazily from
    /// the registry — no allocation at construction time.
    pub fn new(materials: &'a DynamicMaterials) -> Self {
        Self {
            materials,
            textures: None,
            extras: None,
        }
    }

    /// Attaches a [`TextureContext`] (typically `&Textures`) so
    /// `resolve_texture_index` can look up a real `array_and_layer`
    /// encoding for each bound `DynamicTextureBinding::Pooled`.
    /// Without it, every texture slot resolves to `u32::MAX`
    /// regardless of whether the per-instance `DynamicMaterial`
    /// carries a key.
    pub fn with_textures(mut self, textures: &'a dyn TextureContext) -> Self {
        self.textures = Some(textures);
        self
    }

    /// Returns a context that also resolves `buffer_slice` lookups
    /// through the renderer's extras pool. Used by the per-frame
    /// material packer so author-side `<slot>_offset` /
    /// `<slot>_length` fields land on the right pool indices.
    pub fn with_extras(mut self, extras: &'a extras_pool::ExtrasPool) -> Self {
        self.extras = Some(extras);
        self
    }
}

impl<'a> DynamicMaterialContext for DynamicMaterialPackContext<'a> {
    fn layout(&self, shader_id: MaterialShaderId) -> Option<&MaterialLayout> {
        self.materials.get(shader_id).map(|r| &r.layout)
    }

    fn alpha_mode(&self, shader_id: MaterialShaderId) -> Option<awsm_materials::MaterialAlphaMode> {
        self.materials.get(shader_id).map(|r| r.alpha_mode)
    }

    fn resolve_texture_index(&self, binding: Option<&DynamicTextureBinding>) -> u32 {
        // Unbound slot → "no texture" sentinel. Same convention
        // first-party materials use when an Optional<MaterialTexture>
        // is None.
        let Some(binding) = binding else {
            return u32::MAX;
        };
        // No `TextureContext` attached → can't resolve. Stay at the
        // sentinel rather than guessing. Callers that route through
        // `Materials::update` always plumb `Textures` in.
        let Some(textures) = self.textures else {
            return u32::MAX;
        };
        match binding {
            DynamicTextureBinding::Pooled(key) => {
                // Encode `array_index | (layer_index << 12)` to
                // match the bit-layout the rest of the renderer's
                // `TextureInfoRaw.array_and_layer` field uses (see
                // `shared_wgsl/textures.wgsl::convert_texture_info`).
                // Authors decode via:
                //   let array_index = idx & 0xFFFu;
                //   let layer_index = idx >> 12u;
                // for use with the texture-pool array bindings.
                // Missing entries → `u32::MAX` (the WGSL helpers
                // treat that as "no texture").
                let Some(entry) = textures.texture_entry(*key) else {
                    return u32::MAX;
                };
                let array_index = entry.array_index as u32;
                let layer_index = entry.layer_index as u32;
                debug_assert!(array_index <= 0xFFF, "array_index exceeds 12-bit field");
                debug_assert!(layer_index <= 0xFFFFF, "layer_index exceeds 20-bit field");
                (layer_index << 12) | (array_index & 0xFFF)
            }
        }
    }

    fn buffer_slice(
        &self,
        shader_id: MaterialShaderId,
        buffer_slot_index: usize,
    ) -> Option<(u32, u32)> {
        self.extras
            .and_then(|pool| pool.slice_for(shader_id, buffer_slot_index))
    }
}

/// One bucket entry — the template-rendering view of a single registered
/// material (first-party OR dynamic). Returned by [`bucket_entries`].
///
/// The classify pass + the opaque substitution template walk this list
/// to emit:
/// - one indirect-args slot + offset per entry (host-side header)
/// - one `BUCKET_BIT_<NAME>` const per entry (WGSL)
/// - one `args_<name>` + `<name>_offset` field per entry on
///   `ClassifyOutput` (WGSL)
/// - one `if shader_id == SHADER_ID_<NAME>` arm per entry (WGSL)
/// - one per-bucket extract block per entry (WGSL)
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct BucketEntry {
    /// Stable per-build shader id for this bucket.
    pub shader_id: MaterialShaderId,
    /// WGSL-safe identifier suffix used for `args_<name>` /
    /// `<name>_offset` / `BUCKET_BIT_<NAME>` / `SHADER_ID_<NAME>`. For
    /// first-party materials this is the canonical
    /// [`wgsl_const_name`](MaterialShaderId::wgsl_const_name)-derived
    /// lowercased name (`pbr`, `unlit`, `toon`, `flipbook`). For
    /// dynamic materials this is the registered material's `name`
    /// lower-cased + with non-alphanumeric chars converted to `_`.
    pub name: String,
}

impl BucketEntry {
    /// `pbr` → `BUCKET_BIT_PBR`, `irregular-atlas` → `BUCKET_BIT_IRREGULAR_ATLAS`.
    pub fn bucket_bit_const(&self) -> String {
        format!("BUCKET_BIT_{}", self.name.to_uppercase())
    }

    /// `pbr` → `SHADER_ID_PBR`.
    pub fn shader_id_const(&self) -> String {
        format!("SHADER_ID_{}", self.name.to_uppercase())
    }

    /// `pbr` → `args_pbr`. Used as a struct-field name on
    /// `ClassifyOutput`.
    pub fn args_field(&self) -> String {
        format!("args_{}", self.name)
    }

    /// `pbr` → `pbr_offset`. Per-bucket starting offset into the
    /// classify-output `tiles` array.
    pub fn offset_field(&self) -> String {
        format!("{}_offset", self.name)
    }
}

/// Returns the canonical bucket list — first-party materials in their
/// hard-coded order followed by every registered dynamic material in
/// shader-id-sorted order. The classify-pass + the opaque/transparent
/// substitution templates use this list as their single source of
/// truth.
///
/// Free function so the same list can be assembled from the renderer
/// side (which holds `DynamicMaterials`) and from any caller with only
/// a `&MaterialRegistry`-shaped view.
pub fn bucket_entries(dynamic: &DynamicMaterials) -> Vec<BucketEntry> {
    let mut entries = Vec::with_capacity(4 + dynamic.len());
    for first_party in awsm_materials::registry::enabled_materials() {
        entries.push(BucketEntry {
            shader_id: first_party.shader_id,
            name: first_party.name.to_string(),
        });
    }
    let mut dynamics: Vec<_> = dynamic.iter().collect();
    dynamics.sort_by_key(|(id, _)| id.as_u32());
    for (shader_id, reg) in dynamics {
        entries.push(BucketEntry {
            shader_id,
            name: sanitize_wgsl_name(&reg.name),
        });
    }
    entries
}

/// Returns the first-party-only bucket list — the `bucket_entries` value
/// the renderer uses at builder-time prewarm (before any dynamic material
/// can be registered). Stable across the program's lifetime; mid-session
/// registrations produce a different list (via [`bucket_entries`]) and
/// trigger a recompile via the dispatch-hash on affected cache keys.
pub fn first_party_bucket_entries() -> Vec<BucketEntry> {
    awsm_materials::registry::enabled_materials()
        .iter()
        .map(|e| BucketEntry {
            shader_id: e.shader_id,
            name: e.name.to_string(),
        })
        .collect()
}

/// Convert a material name into a valid WGSL identifier suffix. Replaces
/// non-alphanumeric characters with `_` and lowercases the result so the
/// emitted `BUCKET_BIT_<NAME>` / `args_<name>` / `SHADER_ID_<NAME>`
/// symbols are guaranteed to parse.
pub fn sanitize_wgsl_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out.chars().next().unwrap().is_ascii_digit() {
        out.insert(0, '_');
    }
    out
}

/// Renderer-side state for runtime-registered dynamic materials.
///
/// Phase 0 ships this as a placeholder — a `HashMap<MaterialShaderId, MaterialRegistration>`
/// holding registered entries plus the next-id counter. Phase 3 replaces the
/// inline map with `awsm_materials::registry::MaterialRegistry` and wires the
/// [`dispatch_hash`](Self::dispatch_hash) into per-shader-id pipeline cache
/// keys.
#[derive(Default)]
pub struct DynamicMaterials {
    registrations: HashMap<MaterialShaderId, MaterialRegistration>,
    next_dynamic_id: u32,
    /// Cached `bucket_entries` view of the registry, refreshed on
    /// every register / unregister. The opaque + classify render
    /// passes hit this cached slice per frame to avoid the
    /// `Vec<BucketEntry>` alloc + the dynamic-id sort that the
    /// free-function `bucket_entries()` does. Identical contents to
    /// what that function would produce — first-party prefix +
    /// dynamic suffix sorted by ascending shader_id.
    bucket_entries_cache: Vec<BucketEntry>,
    /// Cached `dispatch_hash` of the registry — keyed on the same
    /// `(shader_id, name, layout_hash, wgsl_hash)` set as
    /// [`Self::dispatch_hash`], refreshed alongside
    /// `bucket_entries_cache`. The classify pass's
    /// `dynamic_pipeline_cache` previously keyed on
    /// `(Vec<BucketEntry>, Option<u32>)` and re-built the Vec every
    /// frame; now the per-frame probe uses `(u64, Option<u32>)`
    /// instead so neither side allocates on the hot path.
    dispatch_hash_cache: u64,
}

impl DynamicMaterials {
    /// Creates an empty registry. No dynamic materials are registered until
    /// [`AwsmRenderer::register_material`](crate::AwsmRenderer::register_material)
    /// is called.
    pub fn new() -> Self {
        // Seed the cache with the first-party bucket prefix so an
        // empty-registry render-pass probe doesn't need to know the
        // registry is empty — the slice itself is correct.
        Self {
            registrations: HashMap::new(),
            next_dynamic_id: MaterialShaderId::DYNAMIC_START,
            bucket_entries_cache: first_party_bucket_entries(),
            dispatch_hash_cache: 0,
        }
    }

    /// Returns the cached bucket-entries slice (first-party prefix +
    /// currently-registered dynamic materials sorted by shader_id).
    /// `O(1)` lookup; the cache is refreshed by
    /// [`Self::refresh_caches`] on every register / unregister.
    ///
    /// Replaces per-frame `bucket_entries(&materials)` allocations
    /// on the opaque + classify hot paths.
    pub fn bucket_entries_cached(&self) -> &[BucketEntry] {
        &self.bucket_entries_cache
    }

    /// Returns the cached `dispatch_hash` (same value
    /// [`Self::dispatch_hash`] would compute, but `O(1)`). Used by
    /// the classify pass's per-frame pipeline-cache probe so the
    /// key stays a plain `(u64, Option<u32>)` instead of a freshly-
    /// allocated `Vec<BucketEntry>`.
    pub fn dispatch_hash_cached(&self) -> u64 {
        self.dispatch_hash_cache
    }

    /// Recomputes `bucket_entries_cache` + `dispatch_hash_cache`
    /// from the current `registrations`. Called by `insert` /
    /// `remove` after they mutate the registry — never by external
    /// code on the hot path.
    fn refresh_caches(&mut self) {
        // bucket_entries: first-party prefix + sorted dynamic.
        let mut entries: Vec<BucketEntry> =
            Vec::with_capacity(first_party_bucket_entries().len() + self.registrations.len());
        for fp in first_party_bucket_entries() {
            entries.push(fp);
        }
        let mut dynamics: Vec<_> = self.registrations.iter().collect();
        dynamics.sort_by_key(|(id, _)| id.as_u32());
        for (shader_id, reg) in dynamics {
            entries.push(BucketEntry {
                shader_id: *shader_id,
                name: sanitize_wgsl_name(&reg.name),
            });
        }
        self.bucket_entries_cache = entries;

        // dispatch_hash: identical algorithm to `Self::dispatch_hash`.
        if self.registrations.is_empty() {
            self.dispatch_hash_cache = 0;
        } else {
            let mut entries: Vec<_> = self.registrations.iter().collect();
            entries.sort_by_key(|(id, _)| id.as_u32());
            let mut hasher = DefaultHasher::new();
            for (id, reg) in entries {
                id.as_u32().hash(&mut hasher);
                reg.name.hash(&mut hasher);
                reg.layout_hash.hash(&mut hasher);
                reg.wgsl_hash.hash(&mut hasher);
            }
            self.dispatch_hash_cache = hasher.finish();
        }
    }

    /// Iterates over `(shader_id, registration)` pairs in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = (MaterialShaderId, &MaterialRegistration)> {
        self.registrations.iter().map(|(id, reg)| (*id, reg))
    }

    /// Returns the registration record for a previously-registered id.
    pub fn get(&self, shader_id: MaterialShaderId) -> Option<&MaterialRegistration> {
        self.registrations.get(&shader_id)
    }

    /// Returns the count of currently-registered dynamic materials.
    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    /// Returns true if no dynamic materials are registered. When this is the
    /// case, [`Self::dispatch_hash`] returns a stable constant identical to
    /// today's implicit "no dynamic materials" value, and first-party
    /// per-shader-id pipelines' compiled WGSL is bit-identical to the
    /// pre-feature build.
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    /// Stable hash over the current registry's
    /// `[(shader_id, name, layout_hash, wgsl_hash)]` (sorted by id).
    ///
    /// Wired into per-shader-id pipeline cache keys so registering /
    /// unregistering a dynamic material invalidates affected pipelines on
    /// next render. Returns `0` (a stable sentinel) when the registry is
    /// empty so first-party-only builds compile bit-identical WGSL to the
    /// pre-feature baseline.
    pub fn dispatch_hash(&self) -> u64 {
        if self.registrations.is_empty() {
            return 0;
        }
        let mut entries: Vec<_> = self.registrations.iter().collect();
        entries.sort_by_key(|(id, _)| id.as_u32());
        let mut hasher = DefaultHasher::new();
        for (id, reg) in entries {
            id.as_u32().hash(&mut hasher);
            reg.name.hash(&mut hasher);
            reg.layout_hash.hash(&mut hasher);
            reg.wgsl_hash.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Allocates the next dynamic shader id and inserts the registration.
    /// Returns [`AwsmDynamicMaterialError::DuplicateName`] if a registration
    /// with the same `name` already exists at a different id.
    pub(crate) fn insert(
        &mut self,
        registration: MaterialRegistration,
    ) -> Result<MaterialShaderId, AwsmDynamicMaterialError> {
        // Idempotency: same `(name, layout_hash, wgsl_hash)` returns the
        // existing id without bumping the counter or changing the hash.
        for (id, existing) in &self.registrations {
            if existing.name == registration.name {
                if existing.layout_hash == registration.layout_hash
                    && existing.wgsl_hash == registration.wgsl_hash
                {
                    return Ok(*id);
                }
                return Err(AwsmDynamicMaterialError::DuplicateName(registration.name));
            }
        }
        let id = MaterialShaderId::from_dynamic_raw(self.next_dynamic_id);
        self.next_dynamic_id = self.next_dynamic_id.saturating_add(1);
        self.registrations.insert(id, registration);
        // Refresh the bucket-entries + dispatch-hash caches so the
        // next render frame's hot-path probe sees the new entry
        // without re-allocating per-frame.
        self.refresh_caches();
        Ok(id)
    }

    /// Removes a previously-registered dynamic material.
    pub(crate) fn remove(
        &mut self,
        shader_id: MaterialShaderId,
    ) -> Result<(), AwsmDynamicMaterialError> {
        if !shader_id.is_dynamic() {
            return Err(AwsmDynamicMaterialError::UnknownShaderId(shader_id));
        }
        self.registrations
            .remove(&shader_id)
            .ok_or(AwsmDynamicMaterialError::UnknownShaderId(shader_id))?;
        // Refresh caches even on the empty-after-removal case so the
        // dispatch_hash collapses back to the `0` sentinel that
        // first-party-only builds compile against.
        self.refresh_caches();
        Ok(())
    }
}

/// Runtime registration payload for a custom material.
///
/// The renderer's counterpart to `awsm_scene_schema::MaterialDefinition` +
/// the loaded WGSL fragment. Consumers (`scene-editor`, `material-editor`,
/// game runtimes) convert their on-disk format into a
/// [`MaterialRegistration`] before calling
/// [`AwsmRenderer::register_material`](crate::AwsmRenderer::register_material);
/// the renderer never depends on `awsm-scene-schema`.
#[derive(Clone, Debug)]
pub struct MaterialRegistration {
    /// Author-facing name. Must be unique across registered materials.
    pub name: String,
    /// Alpha mode — drives whether the material routes through the opaque
    /// compute kernel ([`MaterialAlphaMode::Opaque`]) or the transparent
    /// fragment shader ([`MaterialAlphaMode::Mask`] / [`MaterialAlphaMode::Blend`]).
    pub alpha_mode: MaterialAlphaMode,
    /// True when the material renders both front- and back-facing
    /// triangles. Plumbed onto the mesh's `double_sided` flag the same
    /// way first-party materials' `double_sided()` does.
    pub double_sided: bool,
    /// The material's uniform + texture + buffer layout. Drives both
    /// the auto-generated `struct CustomMaterialData_<id>` WGSL
    /// declaration and the per-instance byte packing.
    pub layout: MaterialLayout,
    /// Stable hash over the layout (uniforms + textures + buffers).
    /// Drives the renderer's per-shader-id pipeline cache invalidation.
    /// Set by the consumer; the renderer never computes this itself.
    pub layout_hash: u64,
    /// Stable hash over the WGSL fragment source. Same role as
    /// [`Self::layout_hash`].
    pub wgsl_hash: u64,
    /// The WGSL fragment the renderer injects into the per-shader-id
    /// pipeline template at the `{% match shader_id %}` site.
    pub wgsl_fragment: String,
    /// Default buffer-slot data, one `Vec<u32>` per `BufferSlot` in
    /// declaration order. Passed at registration time to the extras
    /// pool's bump allocator; per-instance overrides (Phase 5's
    /// `CustomMaterialInstance::buffer_overrides`) can also override.
    /// Empty Vec for slots without a registration default.
    pub buffer_defaults: Vec<Vec<u32>>,
    /// Default values for each uniform in `layout.uniforms`, in the
    /// same declaration order. Consumers (`material-editor`,
    /// scene-editor's per-mesh instance picker, game-runtime loaders)
    /// use these to seed a fresh `DynamicMaterial`'s `values` array
    /// — empty `Vec` means "fall back to a zero of each field's
    /// type". When `len()` doesn't match `layout.uniforms.len()`,
    /// the consumer falls back to the zero default for any
    /// missing entry.
    pub uniform_defaults: Vec<awsm_materials::dynamic_layout::UniformValue>,
}

impl crate::AwsmRenderer {
    /// Registers a custom material.
    ///
    /// Returns an opaque [`MaterialShaderId`] in the dynamic range
    /// (`>= MaterialShaderId::DYNAMIC_START`). Takes effect on the next
    /// `render()` call (the shader cache key changes; the affected
    /// per-shader-id pipeline recompiles on first dispatch).
    ///
    /// Idempotent on `(name, layout_hash, wgsl_hash)`: re-registering the
    /// same material returns the same id without recompiling.
    ///
    /// **Phase 4**: the new shader_id is inserted; the next render call's
    /// classify-pass cache lookup misses (since `bucket_entries` changed)
    /// and triggers a recompile of the classify shader. The opaque-compute
    /// pipeline for the new shader_id is compiled on first dispatch (or
    /// eagerly via [`Self::prewarm_pipelines`]).
    pub fn register_material(
        &mut self,
        registration: MaterialRegistration,
    ) -> Result<MaterialShaderId, AwsmDynamicMaterialError> {
        let buffer_defaults = registration.buffer_defaults.clone();
        let id = self.dynamic_materials.insert(registration)?;
        // Assign extras-pool slices for any buffer-slot defaults
        // declared on the registration. Per-instance overrides
        // (Phase 5's CustomMaterialInstance.buffer_overrides) can
        // overwrite these per instance — the bridge calls
        // `extras_pool.assign_or_update` directly for those.
        for (slot_index, data) in buffer_defaults.iter().enumerate() {
            if data.is_empty() {
                continue;
            }
            if let Err(e) = self.extras_pool.assign_or_update(id, slot_index, data) {
                tracing::warn!(
                    "extras_pool: failed to assign default for ({:?}, {}): {:?}",
                    id,
                    slot_index,
                    e
                );
            }
        }
        // Ensure the classify buffer has capacity for the (possibly
        // larger) bucket count. The mid-session header writer
        // re-emits the per-bucket offsets at the new layout.
        //
        // When this returns `true`, the underlying GPU buffer was
        // reallocated, which means every bind group that referenced
        // the old buffer is now stale. WITHOUT this mark, the
        // classify-output binding on the opaque + transparent compute
        // bind groups silently keeps pointing at the deallocated
        // buffer and the dispatch produces no observable output —
        // exactly the symptom of "preview canvas stays black after
        // registering the first dynamic material". The next render
        // frame's `BindGroups::flush_create` path picks up this mark
        // and rebuilds the affected groups.
        let new_count = bucket_entries(&self.dynamic_materials).len() as u32;
        let resized = self
            .material_classify_buffers
            .ensure_bucket_count(&self.gpu, new_count)?;
        if resized {
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize);
        }
        Ok(id)
    }

    /// Removes a previously-registered dynamic material.
    ///
    /// Phase 0 stub: the registration is dropped from the registry but
    /// pipeline-cache invalidation lands in Phase 3. Returns
    /// [`AwsmDynamicMaterialError::UnknownShaderId`] if the id was never
    /// registered or has already been removed.
    pub fn unregister_material(
        &mut self,
        shader_id: MaterialShaderId,
    ) -> Result<(), AwsmDynamicMaterialError> {
        self.dynamic_materials.remove(shader_id)
    }

    /// Returns the registration record for a previously-registered id.
    pub fn dynamic_material_registration(
        &self,
        shader_id: MaterialShaderId,
    ) -> Option<&MaterialRegistration> {
        self.dynamic_materials.get(shader_id)
    }

    /// Iterator over all currently-registered dynamic materials.
    pub fn dynamic_materials(
        &self,
    ) -> impl Iterator<Item = (MaterialShaderId, &MaterialRegistration)> {
        self.dynamic_materials.iter()
    }

    /// Submit a dynamic material via the new pipeline-readiness API.
    ///
    /// Per the architecture in `docs/plans/more-optimizations.md`,
    /// this is the non-blocking submission entry point that registers
    /// the material AND submits a `PipelineGroupDef::Material(Dynamic)`
    /// to the renderer's scheduler. Returns:
    ///
    /// - `MaterialShaderId` — same as `register_material`, for the
    ///   bucket-routing path and `Material::Custom { shader_id }` field.
    /// - `MaterialId` — new scheduler-side handle to a
    ///   `PipelineGroupId::Material(_)`. Frontends watch this id via
    ///   [`Self::pipeline_group_status`] /
    ///   [`Self::drain_pipeline_status_events`] for the Ready transition.
    ///
    /// **Stage 1 status**: today the scheduler's compile future is a
    /// stub (resolves immediately to Ready); the actual compile work
    /// still requires a follow-up `.await` of either
    /// [`Self::prewarm_pipelines`] (the legacy surface) or a Stage 1.8-fully
    /// completion that wires real compile through the scheduler.
    /// Frontends migrating from `register_material + prewarm.await`
    /// to this new API gain the readiness-state contract surface
    /// now and the real-async compile when Stage 1.8 lands.
    pub fn submit_dynamic_material(
        &mut self,
        registration: MaterialRegistration,
    ) -> Result<(MaterialShaderId, crate::pipeline_scheduler::MaterialId), crate::error::AwsmError>
    {
        use crate::pipeline_scheduler::{
            MaterialDef, MaterialDefKind, PipelineConfigSnapshot, PipelineGroupDef, PipelineGroupId,
        };

        // 1. Existing sync registration (returns the bucket-routing id).
        let shader_id = self.register_material(registration.clone())?;

        // 2. Snapshot the current config so the scheduler can detect
        //    config-drift later (Stage 1.8 / 1.14 fully).
        let snapshot = PipelineConfigSnapshot {
            msaa: self.anti_aliasing.clone(),
            mipmap: if self.anti_aliasing.mipmap {
                crate::render_passes::material_opaque::shader::template::MipmapMode::Gradient
            } else {
                crate::render_passes::material_opaque::shader::template::MipmapMode::None
            },
            use_mesh_light_slices: false,
            gpu_culling: self.features.gpu_culling,
            coverage_lod: self.features.coverage_lod,
            debug_bitmask: 0,
            default_cull_mode: awsm_renderer_core::pipeline::primitive::CullMode::Back,
        };

        // 3. Submit to the scheduler.
        let def = MaterialDef {
            shader_id,
            alpha_mode: registration.alpha_mode,
            double_sided: registration.double_sided,
            kind: MaterialDefKind::Dynamic(Box::new(registration)),
            config_snapshot: snapshot,
        };
        let ids = self
            .pipeline_scheduler
            .submit_pipeline_group_batch(vec![PipelineGroupDef::Material(def)]);
        let material_id = match ids[0] {
            PipelineGroupId::Material(mid) => mid,
            PipelineGroupId::Pass(_) => unreachable!("submit returned Pass for Material def"),
        };

        Ok((shader_id, material_id))
    }
}
