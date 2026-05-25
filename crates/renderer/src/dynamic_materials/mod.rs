//! Runtime-registered dynamic materials.
//!
//! Provides the renderer-side facade over `awsm_materials::registry::MaterialRegistry`,
//! the cache-key extension that invalidates per-shader-id pipelines when the
//! registry's [`MaterialShaderId`](awsm_materials::MaterialShaderId) set
//! changes, and (Phase 6+) the extras-pool storage buffer + allocator that
//! backs `BufferSlot` data.
//!
//! Phase 0 ships the skeleton — empty [`DynamicMaterials`] struct and the
//! stub `register_material` / `unregister_material` plumbing on
//! [`AwsmRenderer`](crate::AwsmRenderer). Subsequent phases fill in the
//! registry, the template substitutions, and the extras pool.

pub mod error;

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_materials::{MaterialAlphaMode, MaterialShaderId};

pub use error::AwsmDynamicMaterialError;

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
}

impl DynamicMaterials {
    /// Creates an empty registry. No dynamic materials are registered until
    /// [`AwsmRenderer::register_material`](crate::AwsmRenderer::register_material)
    /// is called.
    pub fn new() -> Self {
        Self {
            registrations: HashMap::new(),
            next_dynamic_id: MaterialShaderId::DYNAMIC_START,
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
            .map(|_| ())
            .ok_or(AwsmDynamicMaterialError::UnknownShaderId(shader_id))
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
    /// **Phase 0**: the stub returns the next id but does not yet wire the
    /// material into the renderer's template substitution machinery — see
    /// Phase 3 for the registry + cache-hash plumbing and Phase 4 / Phase 7
    /// for the opaque + transparent template substitution.
    pub fn register_material(
        &mut self,
        registration: MaterialRegistration,
    ) -> Result<MaterialShaderId, AwsmDynamicMaterialError> {
        self.dynamic_materials.insert(registration)
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
}
