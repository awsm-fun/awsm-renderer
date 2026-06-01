//! The `DynamicMaterials` unified variant registry + its impls.
//! See the [`crate::dynamic_materials`] module docs for the design.

use super::*;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_materials::{MaterialAlphaMode, MaterialShaderId};

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
/// `shared_wgsl::TextureInfoRaw`. Unbound slots and lookups that
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

/// Hard cap on the total number of bucket entries (first-party +
/// dynamic) that the renderer accepts. Driven by the tightest
/// per-bucket-index encoding across the classify + edge pipelines:
///
/// 1. **Classify `tile_mask` (32 bits — TIGHTEST)**: the per-workgroup
///    `var<workgroup> tile_mask: atomic<u32>` accumulates a
///    `BUCKET_BIT_<NAME> = (1u << index)` per visible bucket id, then
///    fans into one atomic-incremented `args_<name>.workgroup_count_x`
///    cell per set bit. A bucket index `>= 32` would compile to a
///    WGSL `1u << 32u`, which is implementation-defined (typically `0`
///    on Dawn) — the bucket effectively wouldn't classify.
///    See `material_classify/shader/material_classify_wgsl/compute.wgsl`.
///
/// 2. **Edge `edge_slot_map` (8 bits per sample, `0xFE` / `0xFF`
///    reserved)**: looser cap at 254. Not the binding constraint here
///    but documented for context — both encodings would need widening
///    in lock-step before the bucket count could grow past 32.
///
/// The cap is now an Askama-template-driven multiple of 32: the
/// classify `tile_mask` is `array<atomic<u32>, MAX_BUCKET_WORDS>`, so
/// raising [`MAX_BUCKET_WORDS`] from 1 → 2 lifts the cap 32 → 64 at
/// near-zero cost (one extra workgroup atomic to zero per dispatch; the
/// real per-frame cost is *active-in-view* bucket fanout, not the mask
/// width). The 8-bit `edge_slot_map` encoding caps the absolute ceiling
/// at 254 (≈7 words). `register_material` rejects any registration that
/// would push past this cap with
/// [`AwsmDynamicMaterialError::BucketCapExceeded`].
///
/// Default `MAX_BUCKET_WORDS = 1` keeps the generated classify WGSL
/// semantically identical to the original single-`atomic<u32>` form.
pub const MAX_BUCKET_WORDS: u32 = 1;

/// Maximum number of co-resident material buckets. Derived from
/// [`MAX_BUCKET_WORDS`] (32 bits per word). See that const for the cost
/// model of raising it.
pub const MAX_BUCKET_ENTRIES: usize = MAX_BUCKET_WORDS as usize * 32;

/// Which built-in shading family a bucket's opaque / edge / transparent
/// template body is emitted from.
///
/// **Decouples template body-selection from the numeric
/// [`MaterialShaderId`].** Pre-pivot the opaque/edge/transparent
/// templates picked a material body with `{% if shader_id ==
/// MaterialShaderId::PBR %}` — i.e. body-selection was welded to the
/// fixed first-party id. The specialize-only design routes a PBR
/// material to a *per-feature-set* bucket whose id is registry-allocated
/// (in the dynamic range), so "which body" can no longer be read off the
/// id. The templates now branch on this `base` (which shading family)
/// while the per-pixel/per-sample `shader_id` guard uses the allocated
/// numeric id. A specialized PBR bucket carries `base = Pbr` with an
/// id `>= DYNAMIC_START`; a custom author material carries
/// `base = Custom`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShadingBase {
    Pbr,
    Unlit,
    Toon,
    Flipbook,
    /// Author-registered custom fragment (the dynamic-material wrapper
    /// path). Distinct from a registry-allocated *first-party* variant,
    /// which keeps its first-party `base`.
    Custom,
}

impl ShadingBase {
    /// Maps a shader id to its shading family. First-party ids map to
    /// their family; any dynamic-range id maps to [`ShadingBase::Custom`].
    ///
    /// Transitional helper for call sites that only know a `shader_id`
    /// (before per-feature first-party variants exist). Specialized
    /// first-party variant launch sites pass their `base` explicitly
    /// instead of deriving it here.
    pub fn for_shader_id(shader_id: MaterialShaderId) -> Self {
        if shader_id == MaterialShaderId::PBR {
            ShadingBase::Pbr
        } else if shader_id == MaterialShaderId::UNLIT {
            ShadingBase::Unlit
        } else if shader_id == MaterialShaderId::TOON {
            ShadingBase::Toon
        } else if shader_id == MaterialShaderId::FLIPBOOK {
            ShadingBase::Flipbook
        } else {
            ShadingBase::Custom
        }
    }

    /// WGSL-safe lowercase family name, used as the bucket-name prefix
    /// for a first-party variant (`pbr` → `pbr_10000`).
    pub fn wgsl_name(self) -> &'static str {
        match self {
            ShadingBase::Pbr => "pbr",
            ShadingBase::Unlit => "unlit",
            ShadingBase::Toon => "toon",
            ShadingBase::Flipbook => "flipbook",
            ShadingBase::Custom => "custom",
        }
    }

    /// The canonical first-party shader id for this base, if any. `Custom`
    /// (dynamic + scanline) has none — it conservatively gets the full set.
    pub fn canonical_shader_id(self) -> Option<MaterialShaderId> {
        match self {
            ShadingBase::Pbr => Some(MaterialShaderId::PBR),
            ShadingBase::Unlit => Some(MaterialShaderId::UNLIT),
            ShadingBase::Toon => Some(MaterialShaderId::TOON),
            ShadingBase::Flipbook => Some(MaterialShaderId::FLIPBOOK),
            ShadingBase::Custom => None,
        }
    }
}

/// The closure of shared shader modules a pipeline of this shading base needs
/// (see `docs/plans/SKINNY-MATERIALS.md`). First-party bases map to their
/// declared set; `Custom` (dynamic + scanline) conservatively gets the full set
/// since author WGSL may reference anything.
pub fn resolved_includes_for_base(base: ShadingBase) -> awsm_materials::ShaderIncludes {
    base.canonical_shader_id()
        .and_then(awsm_materials::registry::declarations_for_shader_id)
        .map(|(inc, _)| inc)
        .unwrap_or_else(awsm_materials::ShaderIncludes::all)
        .resolve()
}

/// Boolean view of [`resolved_includes_for_base`] for askama `{% if inc.x %}`
/// gating in the shading-host templates. Only the modules the host templates
/// actually gate are surfaced; add fields here as more modules become gateable.
#[derive(Clone, Copy, Debug)]
pub struct ShaderIncludeFlags {
    /// PBR BRDF lobes + IBL split-sum (`brdf.wgsl`).
    pub brdf: bool,
    /// PBR `apply_lighting*` orchestration (`apply_lighting.wgsl`).
    pub apply_lighting: bool,
    /// The PBR `PbrMaterialColor` builder — the `_pbr_*` helpers in
    /// `material_color_calc.wgsl` + their callers (`compute_material_color` in
    /// `material_shading.wgsl`, `pbr_get_gradients` in `mipmap.wgsl`). The unlit
    /// builder in the same file stays ungated.
    pub material_color_calc: bool,
}

impl ShaderIncludeFlags {
    pub fn for_base(base: ShadingBase) -> Self {
        let i = resolved_includes_for_base(base);
        use awsm_materials::ShaderIncludes as S;
        Self {
            brdf: i.contains(S::BRDF),
            apply_lighting: i.contains(S::APPLY_LIGHTING),
            material_color_calc: i.contains(S::MATERIAL_COLOR_CALC),
        }
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
    /// Which built-in shading family this bucket's templates emit
    /// (`Custom` for an author-registered fragment). A bucket *is* a
    /// registry variant: `(base, pbr_features)` for first-party,
    /// `(Custom, _)` keyed on the registration for dynamic. Carried on
    /// the entry so the opaque / edge launch + templates read it
    /// directly instead of re-deriving from the (now possibly
    /// dynamic-range) `shader_id`.
    pub base: ShadingBase,
    /// PBR (or Toon) feature mask this bucket is specialized for
    /// ([`awsm_materials::pbr::PbrFeatures::bits`]). Distinguishes two
    /// PBR variants that differ only by feature-set. `all().bits()` for
    /// the canonical all-features config and inert for
    /// `Unlit`/`Flipbook`/`Custom`.
    pub pbr_features: u32,
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
    let mut entries = first_party_bucket_entries();
    entries.reserve(dynamic.first_party_variants.len() + dynamic.len());
    // First-party feature-set variants (sorted by id for a stable layout).
    let mut fp: Vec<_> = dynamic.fp_variant_meta.iter().collect();
    fp.sort_by_key(|(id, _)| id.as_u32());
    for (id, (base, features)) in fp {
        entries.push(fp_variant_bucket_entry(*id, *base, *features));
    }
    let mut dynamics: Vec<_> = dynamic.iter().collect();
    dynamics.sort_by_key(|(id, _)| id.as_u32());
    for (shader_id, reg) in dynamics {
        entries.push(BucketEntry {
            shader_id,
            base: ShadingBase::Custom,
            pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(), // inert for Custom (own WGSL); never the uber set
            name: sanitize_wgsl_name(&reg.name),
        });
    }
    entries
}

/// Builds the [`BucketEntry`] for a first-party feature-set variant.
/// The WGSL-safe name is `<family>_<id>` (e.g. `pbr_10000`) — stable for
/// a given id within a build and guaranteed unique (ids are monotonic).
fn fp_variant_bucket_entry(id: MaterialShaderId, base: ShadingBase, features: u32) -> BucketEntry {
    BucketEntry {
        shader_id: id,
        base,
        pbr_features: features,
        name: format!("{}_{}", base.wgsl_name(), id.as_u32()),
    }
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
            base: ShadingBase::for_shader_id(e.shader_id),
            // EMPTY feature-set for the canonical base bucket. The canonical
            // PBR bucket (index 0) is only ever the skybox owner — every real
            // PBR material routes to its own per-feature-set variant bucket,
            // so nothing shades here but `sample_skybox` (gated by
            // `owns_skybox`, independent of `pbr_features`). Compiling it with
            // the full feature set would be an "uber" shader that never runs —
            // exactly what specialize-only eliminates; the empty set compiles
            // the minimal shader instead. Inert anyway for Unlit/Flipbook
            // (their bodies don't read `pbr_features`).
            pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(),
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
/// Backed by a `HashMap<MaterialShaderId, MaterialRegistration>` holding
/// registered entries plus the next-id counter. The
/// [`dispatch_hash`](Self::dispatch_hash) feeds per-shader-id pipeline cache
/// keys.
#[derive(Default)]
pub struct DynamicMaterials {
    registrations: HashMap<MaterialShaderId, MaterialRegistration>,
    /// First-party feature-set variants (the specialize-only design).
    /// A PBR/Toon material derives its [`awsm_materials::pbr::PbrFeatures`]
    /// mask and resolves it here to a per-feature-set bucket `shader_id`,
    /// deduped by `(base, features)`: the same family+feature-set always
    /// maps to the same id within a build. These ids share the
    /// `next_dynamic_id` allocation range with custom registrations (all
    /// registry ids are globally unique). Reverse lookup in
    /// [`Self::fp_variant_meta`].
    first_party_variants: HashMap<(ShadingBase, u32), MaterialShaderId>,
    /// Reverse of [`Self::first_party_variants`]: id → `(base, features)`.
    /// Lets the launch path + payload writer recover a variant's family +
    /// feature mask from its (dynamic-range) id.
    fp_variant_meta: HashMap<MaterialShaderId, (ShadingBase, u32)>,
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
            first_party_variants: HashMap::new(),
            fp_variant_meta: HashMap::new(),
            next_dynamic_id: MaterialShaderId::DYNAMIC_START,
            bucket_entries_cache: first_party_bucket_entries(),
            dispatch_hash_cache: 0,
        }
    }

    /// Resolve a first-party feature-set to its bucket `shader_id`,
    /// allocating a fresh registry id on first sight (deduped by
    /// `(base, features)`). The returned id is written as the first u32
    /// of every material payload that uses this family+feature-set, and
    /// the classify pass routes those pixels to this bucket.
    ///
    /// Only `Pbr`/`Toon` are feature-specialized; calling with another
    /// base is a programming error (debug-asserted) — Unlit/Flipbook stay
    /// single-bucket at their canonical first-party ids. Returns the
    /// allocated id; the caller is responsible for launching its pipeline
    /// compile + reconciling bucket-dependent GPU state (the
    /// `AwsmRenderer` reconcile pass).
    ///
    /// Private: the bucket cap can only be enforced if every allocation
    /// goes through [`Self::resolve_first_party_variant_or_cap_err`], so
    /// that's the only public entry point. This raw allocator is the
    /// (cap-unaware) primitive it wraps.
    fn resolve_first_party_variant(
        &mut self,
        base: ShadingBase,
        features: u32,
    ) -> MaterialShaderId {
        debug_assert!(
            !matches!(base, ShadingBase::Custom),
            "resolve_first_party_variant is for first-party families; Custom uses registrations"
        );
        if let Some(&id) = self.first_party_variants.get(&(base, features)) {
            return id;
        }
        let id = MaterialShaderId::from_dynamic_raw(self.next_dynamic_id);
        self.next_dynamic_id = self.next_dynamic_id.saturating_add(1);
        self.first_party_variants.insert((base, features), id);
        self.fp_variant_meta.insert(id, (base, features));
        self.refresh_caches();
        id
    }

    /// Returns the `(base, features)` a first-party variant id was
    /// allocated for, or `None` if `id` isn't a registered first-party
    /// variant (a custom registration or a canonical first-party id).
    pub fn first_party_variant_of(&self, id: MaterialShaderId) -> Option<(ShadingBase, u32)> {
        self.fp_variant_meta.get(&id).copied()
    }

    /// Cap-aware variant resolution (the render-loop reconcile path).
    /// Like [`Self::resolve_first_party_variant`], but allocating a *new*
    /// bucket that would push the total past `max_buckets` is a HARD ERROR
    /// ([`AwsmDynamicMaterialError::BucketCapExceeded`]) rather than a
    /// silent fallback — a wrong-but-not-crashing render is far harder to
    /// debug than a loud, actionable failure (raise [`MAX_BUCKET_WORDS`]).
    /// Existing variants always resolve (idempotent), so this only errors
    /// on the *first* genuinely-new feature-set that overflows the cap.
    pub fn resolve_first_party_variant_or_cap_err(
        &mut self,
        base: ShadingBase,
        features: u32,
        max_buckets: usize,
    ) -> std::result::Result<MaterialShaderId, AwsmDynamicMaterialError> {
        if let Some(&id) = self.first_party_variants.get(&(base, features)) {
            return Ok(id);
        }
        let would_be = self.bucket_entries_cache.len() + 1;
        if would_be > max_buckets {
            return Err(AwsmDynamicMaterialError::BucketCapExceeded {
                would_be,
                max: max_buckets,
            });
        }
        Ok(self.resolve_first_party_variant(base, features))
    }

    /// Returns the cached bucket-entries slice (first-party prefix +
    /// currently-registered dynamic materials sorted by shader_id).
    /// `O(1)` lookup; the cache is refreshed by
    /// `Self::refresh_caches` on every register / unregister.
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
        // bucket_entries: first-party defaults + first-party feature-set
        // variants (sorted by id) + sorted custom registrations.
        let mut entries: Vec<BucketEntry> = Vec::with_capacity(
            first_party_bucket_entries().len()
                + self.first_party_variants.len()
                + self.registrations.len(),
        );
        for fp in first_party_bucket_entries() {
            entries.push(fp);
        }
        let mut fp_variants: Vec<_> = self.fp_variant_meta.iter().collect();
        fp_variants.sort_by_key(|(id, _)| id.as_u32());
        for (id, (base, features)) in fp_variants {
            entries.push(fp_variant_bucket_entry(*id, *base, *features));
        }
        let mut dynamics: Vec<_> = self.registrations.iter().collect();
        dynamics.sort_by_key(|(id, _)| id.as_u32());
        for (shader_id, reg) in dynamics {
            entries.push(BucketEntry {
                shader_id: *shader_id,
                base: ShadingBase::Custom,
                pbr_features: awsm_materials::pbr::PbrFeatures::default().bits(), // inert for Custom (own WGSL); never the uber set
                name: sanitize_wgsl_name(&reg.name),
            });
        }
        self.bucket_entries_cache = entries;

        // dispatch_hash: identical algorithm to `Self::dispatch_hash`.
        self.dispatch_hash_cache = self.dispatch_hash();
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
    /// Returns `true` if inserting `registration` would NOT grow the
    /// registry (i.e., it'd be idempotent on an existing
    /// `(name, layout_hash, wgsl_hash)`). Used by
    /// [`crate::AwsmRenderer::register_material`] to skip the
    /// bucket-cap pre-check when a re-registration would just return
    /// the existing shader id.
    pub fn would_be_idempotent(&self, registration: &MaterialRegistration) -> bool {
        self.registrations.values().any(|existing| {
            existing.name == registration.name
                && existing.layout_hash == registration.layout_hash
                && existing.wgsl_hash == registration.wgsl_hash
        })
    }

    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    /// Returns true when the registry holds **no extra buckets** beyond
    /// the canonical first-party defaults — i.e. no custom registrations
    /// AND no first-party feature-set variants. In that state
    /// [`Self::dispatch_hash`] is the stable `0` sentinel and the eager
    /// first-party classify / opaque pipelines (compiled against the
    /// 4-bucket default layout) are correct. The moment a feature-set
    /// variant is allocated the bucket layout grows, so this returns
    /// `false` and the classify pass must use its dynamic
    /// (`dispatch_hash`-keyed) pipeline instead of the eager one —
    /// otherwise it would route against the stale 4-bucket layout and
    /// drop every variant's pixels (a near-black render).
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty() && self.fp_variant_meta.is_empty()
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
        // Stable empty-state sentinel: no custom registrations AND no
        // first-party feature-set variants → 0, so a canonical-only
        // build (all-features config) compiles bit-identical WGSL to the
        // pre-overhaul baseline. Either set being non-empty changes the
        // bucket layout, which the classify + opaque + edge pipelines all
        // template against, so it must invalidate their cache keys.
        if self.registrations.is_empty() && self.fp_variant_meta.is_empty() {
            return 0;
        }
        let mut hasher = DefaultHasher::new();
        let mut entries: Vec<_> = self.registrations.iter().collect();
        entries.sort_by_key(|(id, _)| id.as_u32());
        for (id, reg) in entries {
            id.as_u32().hash(&mut hasher);
            reg.name.hash(&mut hasher);
            reg.layout_hash.hash(&mut hasher);
            reg.wgsl_hash.hash(&mut hasher);
        }
        // Fold the first-party feature-set variants so adding/removing a
        // variant bucket invalidates the classify pipeline (which is keyed
        // on dispatch_hash, NOT on the full bucket list).
        let mut variants: Vec<_> = self.fp_variant_meta.iter().collect();
        variants.sort_by_key(|(id, _)| id.as_u32());
        for (id, (base, features)) in variants {
            id.as_u32().hash(&mut hasher);
            (*base as u32).hash(&mut hasher);
            features.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Validates a whole batch against the **final** bucket layout
    /// before any registration side effects run, so
    /// [`crate::AwsmRenderer::register_materials`] is all-or-nothing. Pure
    /// — no mutation, no GPU — so it's unit-testable natively.
    ///
    /// Rejects when:
    /// - two entries (within the batch, or a batch entry vs. an existing
    ///   registration) share a `name` but differ in
    ///   `(layout_hash, wgsl_hash)` → [`AwsmDynamicMaterialError::DuplicateName`];
    /// - the count of genuinely-new buckets (entries not idempotent
    ///   against the current registry, deduped within the batch) plus
    ///   the current bucket count would exceed [`MAX_BUCKET_ENTRIES`] →
    ///   [`AwsmDynamicMaterialError::BucketCapExceeded`]. The check is
    ///   against the FINAL count, not per-insert, so a batch that fits
    ///   as a whole is never rejected for a transient intermediate
    ///   overflow.
    ///
    /// Idempotent entries (same `name` + same hashes as something
    /// already present, or repeated within the batch) do not count
    /// toward the cap — they resolve to an existing/earlier shader_id.
    pub fn validate_batch(
        &self,
        registrations: &[MaterialRegistration],
    ) -> Result<(), AwsmDynamicMaterialError> {
        // name -> (layout_hash, wgsl_hash) for everything already
        // registered plus everything accepted earlier in this batch.
        let mut seen: HashMap<String, (u64, u64)> =
            HashMap::with_capacity(self.registrations.len() + registrations.len());
        for reg in self.registrations.values() {
            seen.insert(reg.name.clone(), (reg.layout_hash, reg.wgsl_hash));
        }
        let mut new_buckets = 0usize;
        for reg in registrations {
            match seen.get(&reg.name) {
                Some(&(lh, wh)) => {
                    if lh != reg.layout_hash || wh != reg.wgsl_hash {
                        return Err(AwsmDynamicMaterialError::DuplicateName(reg.name.clone()));
                    }
                    // Idempotent — reuses an existing/earlier id, no new bucket.
                }
                None => {
                    seen.insert(reg.name.clone(), (reg.layout_hash, reg.wgsl_hash));
                    new_buckets += 1;
                }
            }
        }
        let final_count = self.bucket_entries_cache.len() + new_buckets;
        if final_count > MAX_BUCKET_ENTRIES {
            return Err(AwsmDynamicMaterialError::BucketCapExceeded {
                would_be: final_count,
                max: MAX_BUCKET_ENTRIES,
            });
        }
        Ok(())
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
    /// pool's bump allocator; per-instance overrides (the per-instance
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
    /// Batch-registers custom materials.
    ///
    /// **Transactional / all-or-nothing**: the entire batch is validated
    /// against the *final* bucket layout
    /// ([`DynamicMaterials::validate_batch`]) before any registration
    /// side effects run. If any entry would collide on a name or push
    /// the batch past [`MAX_BUCKET_ENTRIES`], the call returns an error
    /// and **nothing** is registered. On success, returns the assigned
    /// [`MaterialShaderId`]s in input order (idempotent entries resolve
    /// to their existing id).
    ///
    /// Validation up front means the per-item cap re-check inside
    /// [`Self::register_material`] never fires mid-batch, so the loop
    /// can't leave the registry partially grown.
    ///
    /// **Transaction-boundary follow-up**: this still
    /// reconciles derived GPU state (classify/edge buffer resizes,
    /// scheduler relaunches) once *per accepted entry* rather than once
    /// for the final layout. Collapsing those into a single
    /// final-layout pass is the remaining optimization; the public
    /// contract (one validated, all-or-nothing batch returning ids in
    /// order) is already in place.
    pub fn register_materials(
        &mut self,
        registrations: Vec<MaterialRegistration>,
    ) -> Result<Vec<MaterialShaderId>, AwsmDynamicMaterialError> {
        // All-or-nothing: validate the whole batch BEFORE mutating
        // anything. A failure here means zero side effects.
        self.dynamic_materials.validate_batch(&registrations)?;
        let mut ids = Vec::with_capacity(registrations.len());
        for registration in registrations {
            // Cannot fail the cap check (validate_batch already proved
            // the final layout fits) nor on a name collision (validated
            // above); a DuplicateName here would only mean the batch
            // itself was internally inconsistent, which validate_batch
            // also rejects.
            ids.push(self.register_material(registration)?);
        }
        Ok(ids)
    }

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
    /// **Readiness contract (edge-resolve push):**
    /// every pipeline the new material needs to render correctly —
    /// primary opaque (4 MSAA × mipmaps variants), classify
    /// (2 MSAA variants), per-shader `edge_resolve` for the new
    /// shader_id, plus the global `skybox_edge_resolve` and
    /// `final_blend` (whose cache keys depend on `bucket_entries`
    /// and are recompiled on every register so the templated
    /// bucket constants match the classify pass) — is pushed into
    /// the scheduler's `inflight_compile` queue via
    /// [`Self::launch_dynamic_material_compile`]. The scheduler
    /// flips the material `Pending → Ready` only when the **whole**
    /// MSAA-edge chain is GPU-resident. Frontends subscribed to
    /// [`Self::drain_pipeline_status_events`] correctly observe
    /// Ready after MSAA edge resolution is fully functional for
    /// the new material — no separate `prewarm_pipelines.await` is
    /// needed for edge correctness.
    pub fn register_material(
        &mut self,
        registration: MaterialRegistration,
    ) -> Result<MaterialShaderId, AwsmDynamicMaterialError> {
        // Bucket-id cap (see [`MAX_BUCKET_ENTRIES`]). A successful
        // insert below grows `bucket_entries` by one ONLY if this is
        // a brand-new `(name, layout_hash, wgsl_hash)`; idempotent
        // re-registrations (same name + same hashes) reuse the
        // existing shader_id and don't expand the bucket list.
        //
        // Run the idempotency lookup FIRST so we don't reject a
        // re-registration of an existing entry once the registry is
        // at saturation — the registry's contract (per
        // `DynamicMaterials::insert`) is: same name + same hashes
        // returns the existing shader_id with no growth, and that
        // contract must hold even at the cap.
        if !self.dynamic_materials.would_be_idempotent(&registration) {
            let current_len = bucket_entries(&self.dynamic_materials).len();
            if current_len >= MAX_BUCKET_ENTRIES {
                return Err(AwsmDynamicMaterialError::BucketCapExceeded {
                    would_be: current_len + 1,
                    max: MAX_BUCKET_ENTRIES,
                });
            }
        }
        let buffer_defaults = registration.buffer_defaults.clone();
        let id = self.dynamic_materials.insert(registration)?;
        // Assign extras-pool slices for any buffer-slot defaults
        // declared on the registration. Per-instance overrides
        // (the per-instance CustomMaterialInstance.buffer_overrides) can
        // overwrite these per instance — the bridge calls
        // `extras_pool.assign_or_update` directly for those.
        for (slot_index, data) in buffer_defaults.iter().enumerate() {
            if data.is_empty() {
                continue;
            }
            match self
                .extras_pool
                .assign_or_update(&self.gpu, id, slot_index, data)
            {
                Ok(outcome) => {
                    if outcome.resized {
                        self.bind_groups
                            .mark_create(crate::bind_groups::BindGroupCreate::ExtrasPoolResize);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "extras_pool: failed to assign default for ({:?}, {}): {:?}",
                        id,
                        slot_index,
                        e
                    );
                }
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
        // Symmetric resize for `material_edge_buffers` (Stage 3): the
        // args_buffer + data_buffer are sized for (bucket_count, edge
        // budget). Without this, classify's binding(4) on the
        // multi-sampled bind group keeps pointing at the
        // smaller-bucket args buffer, and Dawn rejects the dispatch:
        //   [Buffer "MaterialEdgeBuffers::args"] bound with size N is
        //   too small. The pipeline requires at least M.
        // Surfaced by material-editor preview after the first dynamic
        // material registers (bucket 4 → 5).
        if let Some(edge_buffers) = self.material_edge_buffers.as_mut() {
            let edge_resized = edge_buffers.ensure_bucket_count(&self.gpu, new_count)?;
            if edge_resized {
                // The edge args/data buffers live on the classify
                // multi-sampled bind group (binding 4/5) — the same
                // recreate mark used for classify-buffers covers
                // them. Mark explicitly even though the classify
                // resize above likely already marked it: tolerant of
                // future re-sequencing.
                self.bind_groups.mark_create(
                    crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize,
                );
                // Also rebuild the edge-layout uniform so the per-frame
                // header knows the new bucket count + max_edge_budget.
                let max_edge_budget = edge_buffers.max_edge_budget;
                if let Ok((uniform, _bytes)) =
                    crate::render_passes::material_opaque::edge_buffers::build_edge_layout_uniform(
                        &self.gpu,
                        new_count,
                        max_edge_budget,
                    )
                {
                    self.material_edge_layout_uniform = Some(uniform);
                }
            }
        }
        // Scheduler bridge: also submit the material to the
        // pipeline-readiness scheduler so its lifecycle is observable
        // via the status stream / pipeline_group_status. The
        // returned MaterialId is intentionally discarded — callers
        // wanting the typed scheduler handle use the
        // `submit_dynamic_material` API which returns it explicitly.
        // The `prewarm_dynamic_pipelines` bridge marks each
        // scheduler entry `Ready` once compile resolves. Bridge
        // failures are logged but not propagated — register_material
        // succeeded and the bucket-routing path works without the
        // scheduler entry; only the observability surface degrades.
        if let Err(e) = self.submit_to_scheduler_for_shader_id(id) {
            tracing::warn!(
                target: "awsm_renderer::pipeline_readiness",
                "submit_to_scheduler_for_shader_id failed for {:?}: {:?}",
                id, e
            );
        }
        // Literal-future launch: kick off the
        // sub-pipeline compile promises sync-now, push them into
        // PipelineScheduler::inflight_compile. The renderer's
        // poll_pipeline_scheduler drains + installs them per-frame.
        // Frontends watching drain_pipeline_status_events see the
        // material light up Ready when the last sub-pipeline lands —
        // no `prewarm_pipelines().await` round-trip needed.
        //
        // ALL registered materials (not just the newly-inserted one)
        // get relaunched: this insert grew `bucket_entries`, which
        // shifts the generated `ClassifyOutput` struct field offsets
        // (every previously-registered shader_id's `<shader>_offset`
        // field now lives at a different byte position).
        //
        // **Stale typed-cache invalidation**: before relaunching, we
        // clear every (shader_id, msaa, mipmaps) entry from the
        // opaque per-pass cache and every (shader_id, mipmaps) entry
        // + globals from the edge per-pass cache. The lookup keys are
        // bucket-layout-AGNOSTIC ((shader_id, msaa, mipmaps) for
        // opaque, (shader_id, mipmaps) for edge per-shader), so
        // without this clear the dispatch path in the window between
        // relaunch and scheduler resolution would hit the OLD
        // pipeline keys — pipelines compiled against the previous
        // (smaller) bucket layout, dispatching against the newly
        // resized classify/edge buffers with WRONG offsets.
        //
        // After clearing, the dispatch site's `Option` guard returns
        // `None` and skips the draw for that material until the new
        // pipeline lands. The classify per-pass cache is
        // self-invalidating — it's keyed on `dispatch_hash` which
        // changes on every bucket mutation, so old entries become
        // unreachable orphans (no clear needed there).
        self.render_passes
            .material_opaque
            .pipelines
            .clear_dynamic_pipelines();
        self.render_passes
            .material_opaque
            .edge_pipelines
            .clear_dynamic_pipelines();

        // Opaque shader cache keys include `bucket_entries`, so the
        // launch path's `cache_lookup` correctly misses every stale
        // variant and pushes fresh compiles into the scheduler.
        //
        // Matches the pattern in `finalize_gpu_textures` after a
        // texture-pool grow — the scheduler's `registered_material_shader_ids()`
        // is the single source-of-truth for "every material currently
        // tracked by the readiness system". `launch_first_party_material_compile`
        // forwards dynamic shader_ids to `launch_dynamic_material_compile`
        // automatically, so a single iteration covers both classes.
        // Cross-call waiter dedup inside the launch path (see
        // `PipelineScheduler::register_compute_compile_waiter`)
        // ensures the global classify + edge-chain promises are
        // pushed once for the whole loop, not N times per
        // registered material; every waiter's subcompile counter is
        // still bumped so Ready transitions wait on the shared
        // compile.
        //
        // **Pending transition for existing materials**: mark every
        // PREVIOUSLY-REGISTERED material `Pending` before launching.
        // The just-submitted material `id` is already Pending from
        // `submit_to_scheduler_for_shader_id` above. Without the
        // mark, existing materials stay in `Ready` while their
        // replacement compiles are in flight — the typed pipeline
        // cache has just been cleared, dispatch skips them via the
        // Option guards, but `pipeline_group_status` /
        // `drain_pipeline_status_events` still report Ready,
        // confusing the compile modal + any consumer-driven readiness
        // gates. The generation bump inside
        // `mark_material_pending_for_relaunch` is also what makes
        // the apply_compile_resolution stale-generation gate discard
        // old in-flight resolutions (compiled against the previous
        // bucket layout).
        let registered_shader_ids = self.pipeline_scheduler.registered_material_shader_ids();
        for shader_id in &registered_shader_ids {
            if *shader_id == id {
                // Newly-inserted material: already Pending from
                // submit. Skip the redundant mark.
                continue;
            }
            if let Some(mid) = self
                .pipeline_scheduler
                .find_material_by_shader_id(*shader_id)
            {
                self.pipeline_scheduler.mark_material_pending_for_relaunch(
                    crate::pipeline_scheduler::PipelineGroupId::Material(mid),
                );
            }
        }
        // Edge-resolve pipelines are layout-level (their cache keys embed
        // the whole bucket_entries). Rebuild the full set ONCE for the new
        // layout — not once per material. Charged to the canonical PBR
        // material; runs BEFORE the per-material relaunch loop so its edge
        // subcompiles are registered before PBR's own
        // `launch_first_party_material_compile` can mark it Ready. See
        // `launch_edge_resolve_compile`.
        if let Err(e) = self.launch_edge_resolve_compile() {
            tracing::warn!(
                target: "awsm_renderer::pipeline_readiness",
                "post-register_material edge-resolve relaunch failed: {:?}", e
            );
        }
        for shader_id in registered_shader_ids {
            if let Err(e) = self.launch_first_party_material_compile(shader_id) {
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "post-register_material relaunch of material({:?}) failed: {:?}",
                    shader_id, e
                );
            }
        }
        Ok(id)
    }

    /// Per-frame reconcile: route each opaque PBR material to its
    /// per-feature-set *variant* bucket, allocating + compiling new variants
    /// and re-laying-out the bucket-dependent GPU state when the set grows.
    ///
    /// Called from the render preamble; a cheap no-op when the
    /// `variants_dirty` flag is clear (every frame after the scene's
    /// material set settles). On the frame a material enters / its
    /// feature-set changes, it:
    /// 1. derives `PbrFeatures` for every opaque PBR material,
    /// 2. resolves each to a variant `shader_id` (deduped; allocates a
    ///    new bucket on first sight of a feature-set),
    /// 3. stamps the resolved id into the material payload's first u32
    ///    (so classify routes those pixels to the variant bucket and the
    ///    variant's specialized opaque pipeline shades them), and
    /// 4. when the bucket list grew, re-lays-out classify/edge buffers
    ///    and relaunches **every** bucket's pipeline against the final
    ///    layout (the templated `ClassifyBuckets` struct depends on the
    ///    full list, so a grow invalidates all of them).
    ///
    /// Only PBR specializes per feature-set; Toon/Unlit/Flipbook render as
    /// single canonical buckets (no compile-gateable shading paths today).
    /// The canonical PBR bucket (`MaterialShaderId::PBR`, index 0) always
    /// stays present as the skybox owner even when every PBR material routes
    /// to a specialized variant.
    pub(crate) fn reconcile_material_variants(&mut self) -> Result<(), crate::error::AwsmError> {
        if !self.materials.take_variants_dirty() {
            return Ok(());
        }
        use awsm_materials::pbr::PbrFeatures;

        // 1. Derive the desired (base, features) for every PBR material
        //    (the only family that specializes per feature-set).
        let mut wants: Vec<(crate::materials::MaterialKey, ShadingBase, u32)> = Vec::new();
        for (key, mat) in self.materials.iter_for_variant_reconcile() {
            if let crate::materials::Material::Pbr(m) = mat {
                wants.push((key, ShadingBase::Pbr, PbrFeatures::from_material(m).bits()));
            }
        }
        if wants.is_empty() {
            return Ok(());
        }

        // 2. Resolve to variant ids (allocates new buckets on first sight).
        //    Exceeding the bucket cap is a HARD ERROR (no silent fallback):
        //    a material rendered with the wrong bucket is far harder to
        //    debug than a loud failure. The error names the cap + the fix
        //    (raise MAX_BUCKET_WORDS) and propagates out of the render loop.
        let before = self.dynamic_materials.bucket_entries_cached().len();
        let mut resolved: Vec<(crate::materials::MaterialKey, MaterialShaderId)> =
            Vec::with_capacity(wants.len());
        for (key, base, features) in wants {
            let id = self
                .dynamic_materials
                .resolve_first_party_variant_or_cap_err(base, features, MAX_BUCKET_ENTRIES)?;
            resolved.push((key, id));
        }
        let grew = self.dynamic_materials.bucket_entries_cached().len() != before;

        // 3. Stamp the resolved id into each material's payload (re-packs
        //    only when the id actually changed).
        for (key, id) in &resolved {
            self.materials.set_resolved_shader_id(
                *key,
                *id,
                &self.textures,
                &self.dynamic_materials,
                &self.extras_pool,
            );
        }

        // 4. A bucket-list change invalidates every bucket's templated
        //    pipelines — re-lay-out + relaunch them all against the final
        //    layout.
        if grew {
            self.relaunch_all_buckets_after_layout_change()?;
        }
        Ok(())
    }

    /// Scene-load warmup-await (`compile_materials(set).await`). Resolves
    /// every opaque PBR material's feature-set variant, launches the
    /// per-variant pipeline
    /// compiles, and resolves only when they're ALL GPU-resident. A
    /// frontend calls this once after inserting a scene's materials so the
    /// **first rendered frame is already fully specialized** — no
    /// compile-in-progress transient (the per-frame reconcile would
    /// otherwise trigger the relaunch on the first render, briefly leaving
    /// `final_blend` / opaque pipelines uncompiled). Built on the same
    /// [`Self::reconcile_material_variants`] + [`Self::wait_for_pipelines_ready`].
    pub async fn compile_material_variants(&mut self) -> Result<(), crate::error::AwsmError> {
        self.reconcile_material_variants()?;
        self.wait_for_pipelines_ready().await?;
        Ok(())
    }

    /// Re-lays-out the bucket-dependent GPU state (classify + edge
    /// buffers, edge-layout uniform) for the current bucket count, clears
    /// the layout-dependent pipeline caches, then submits + relaunches
    /// every bucket (canonical first-party, feature-set variants, custom)
    /// so each recompiles against the final bucket layout.
    ///
    /// Mirrors [`Self::register_material`]'s post-insert reconcile tail,
    /// generalized from "one new custom bucket" to "the whole bucket set
    /// changed". Used by [`Self::reconcile_material_variants`] when a new
    /// PBR/Toon feature-set variant grows the bucket list.
    fn relaunch_all_buckets_after_layout_change(&mut self) -> Result<(), crate::error::AwsmError> {
        let new_count = self.dynamic_materials.bucket_entries_cached().len() as u32;

        // Classify buffers (per-bucket indirect args + tile lists).
        if self
            .material_classify_buffers
            .ensure_bucket_count(&self.gpu, new_count)?
        {
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize);
        }
        // Edge buffers + edge-layout uniform (MSAA only).
        if let Some(edge_buffers) = self.material_edge_buffers.as_mut() {
            if edge_buffers.ensure_bucket_count(&self.gpu, new_count)? {
                self.bind_groups.mark_create(
                    crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize,
                );
                let max_edge_budget = edge_buffers.max_edge_budget;
                if let Ok((uniform, _bytes)) =
                    crate::render_passes::material_opaque::edge_buffers::build_edge_layout_uniform(
                        &self.gpu,
                        new_count,
                        max_edge_budget,
                    )
                {
                    self.material_edge_layout_uniform = Some(uniform);
                }
            }
        }

        // Clear layout-dependent typed pipeline caches (opaque `main` +
        // edge). classify's cache is self-invalidating via dispatch_hash.
        self.render_passes
            .material_opaque
            .pipelines
            .clear_dynamic_pipelines();
        self.render_passes
            .material_opaque
            .edge_pipelines
            .clear_dynamic_pipelines();

        // Ensure every bucket (incl. canonical first-party + the new
        // variants) has a scheduler entry, then relaunch them all so they
        // recompile against the final bucket layout.
        let bucket_ids: Vec<MaterialShaderId> = self
            .dynamic_materials
            .bucket_entries_cached()
            .iter()
            .map(|e| e.shader_id)
            .collect();
        for id in &bucket_ids {
            if let Err(e) = self.submit_to_scheduler_for_shader_id(*id) {
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "variant reconcile: submit_to_scheduler({:?}) failed: {:?}", id, e
                );
            }
        }
        let registered = self.pipeline_scheduler.registered_material_shader_ids();
        for shader_id in &registered {
            if let Some(mid) = self
                .pipeline_scheduler
                .find_material_by_shader_id(*shader_id)
            {
                self.pipeline_scheduler.mark_material_pending_for_relaunch(
                    crate::pipeline_scheduler::PipelineGroupId::Material(mid),
                );
            }
        }
        // Edge-resolve pipelines are layout-level (their cache keys embed
        // the whole bucket_entries). Rebuild the full set ONCE for the new
        // layout — not once per material. Charged to the canonical PBR
        // material; runs BEFORE the per-material relaunch loop so PBR (just
        // marked pending above) has its edge subcompiles registered before
        // its own `launch_first_party_material_compile` can mark it Ready.
        // See `launch_edge_resolve_compile`.
        if let Err(e) = self.launch_edge_resolve_compile() {
            tracing::warn!(
                target: "awsm_renderer::pipeline_readiness",
                "variant reconcile edge-resolve relaunch failed: {:?}", e
            );
        }
        for shader_id in registered {
            if let Err(e) = self.launch_first_party_material_compile(shader_id) {
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "variant reconcile relaunch of material({:?}) failed: {:?}", shader_id, e
                );
            }
        }
        Ok(())
    }

    /// Internal helper: build a `MaterialDef` for a freshly-registered
    /// dynamic material and submit it to the scheduler. Idempotent —
    /// a duplicate submit for the same shader_id just adds a second
    /// scheduler entry (which the prewarm bridge marks Ready
    /// alongside the first). Kept private; the public surfaces are
    /// `register_material` and `submit_dynamic_material`.
    fn submit_to_scheduler_for_shader_id(
        &mut self,
        shader_id: awsm_materials::MaterialShaderId,
    ) -> Result<(), crate::error::AwsmError> {
        use crate::pipeline_scheduler::{
            MaterialDef, MaterialDefKind, PipelineConfigSnapshot, PipelineGroupDef,
        };
        // Skip if this shader_id already has a scheduler entry
        // (avoids ballooning the SlotMap on idempotent
        // `register_material(same_payload)` calls — these are common
        // from material-editor's debounced recompile loop hitting the
        // hash-based idempotency gate inside the registry).
        if self
            .pipeline_scheduler
            .find_material_by_shader_id(shader_id)
            .is_some()
        {
            return Ok(());
        }
        let snapshot = PipelineConfigSnapshot {
            msaa: self.anti_aliasing.clone(),
            mipmap: if self.anti_aliasing.mipmap {
                crate::render_passes::material_opaque::shader::template::MipmapMode::Gradient
            } else {
                crate::render_passes::material_opaque::shader::template::MipmapMode::None
            },
            gpu_culling: self.features.gpu_culling,
            coverage_lod: self.features.coverage_lod,
            debug_bitmask: 0,
            default_cull_mode: awsm_renderer_core::pipeline::primitive::CullMode::Back,
        };
        // FirstParty if it's a canonical first-party id (PBR/UNLIT/TOON/
        // FLIPBOOK, NOT dynamic-range) OR a first-party feature-set
        // variant (dynamic-range, but registered in `fp_variant_meta`).
        // Both compile the built-in body with params in the material_meta
        // buffer. Only a custom author registration is Dynamic.
        //
        // Submitting the canonical first-party ids matters because the
        // variant reconcile clears `main` (all opaque pipelines) on a
        // bucket-list change and relaunches only scheduler-registered
        // materials — without an entry, the canonical PBR skybox-owner +
        // the Unlit/Toon/Flipbook buckets would never recompile (black).
        let is_first_party = !shader_id.is_dynamic()
            || self
                .dynamic_materials
                .first_party_variant_of(shader_id)
                .is_some();
        let def = if is_first_party {
            MaterialDef {
                shader_id,
                alpha_mode: MaterialAlphaMode::Opaque,
                double_sided: false,
                kind: MaterialDefKind::FirstParty,
                config_snapshot: snapshot,
            }
        } else {
            // Custom author material — snapshot its registration.
            let registration = match self.dynamic_materials.get(shader_id) {
                Some(r) => r.clone(),
                None => return Ok(()), // shouldn't happen, but bail quietly
            };
            MaterialDef {
                shader_id,
                alpha_mode: registration.alpha_mode,
                double_sided: registration.double_sided,
                kind: MaterialDefKind::Dynamic(Box::new(registration)),
                config_snapshot: snapshot,
            }
        };
        self.pipeline_scheduler
            .submit_pipeline_group_batch(vec![PipelineGroupDef::Material(def)]);
        Ok(())
    }

    /// Removes a previously-registered dynamic material.
    ///
    /// Note: the registration is dropped from the registry but the
    /// pipeline cache is not currently invalidated. Returns
    /// [`AwsmDynamicMaterialError::UnknownShaderId`] if the id was never
    /// registered or has already been removed.
    ///
    /// Releases any extras-pool slices owned by the shader_id back to
    /// the free list — the next equal-length `assign_or_update` will
    /// reclaim the bytes. This is the editor's edit→re-register cycle
    /// closing back on itself; without it every recompile leaks the
    /// previous slice into the pool's used region until the bump
    /// allocator grows past the leak.
    pub fn unregister_material(
        &mut self,
        shader_id: MaterialShaderId,
    ) -> Result<(), AwsmDynamicMaterialError> {
        let dropped = self.extras_pool.drop_shader(shader_id);
        if dropped > 0 {
            tracing::debug!(
                target: "awsm_renderer::extras_pool",
                "unregister_material({shader_id:?}): reclaimed {dropped} extras-pool slice(s)",
            );
        }
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
    /// Per the architecture in `https://github.com/dakom/awsm-renderer/pull/99`,
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
    /// **Readiness flow**: `register_material` pushes real compile
    /// futures into the scheduler's `inflight_compile` set via
    /// [`Self::launch_dynamic_material_compile`] (with the
    /// edge-resolve extension). The promise set covers every
    /// pipeline the material needs to render correctly — opaque,
    /// classify, per-shader edge_resolve, skybox edge_resolve,
    /// final_blend. The scheduler's [`Self::poll_pipeline_scheduler`]
    /// (called each render-frame preamble) drains those futures
    /// and marks the corresponding material `Ready` when its last
    /// sub-pipeline resolves. Frontends that need to block until
    /// ready use [`Self::wait_for_pipelines_ready`], which polls
    /// until no further transitions are applied; frontends that
    /// just want a progress signal subscribe to
    /// [`Self::drain_pipeline_status_events`] and render fall-back
    /// content (loading modal / placeholder mesh) until Ready
    /// arrives. Either approach yields full MSAA-edge correctness
    /// for the new material on the next render after Ready.
    ///
    /// [`Self::prewarm_pipelines`] still exists as the lower-level
    /// compile-drive surface; the scheduler bridge inside it marks
    /// the scheduler entries Ready when its `ensure_keys` resolves,
    /// so the two surfaces are interchangeable for the
    /// readiness-state contract.
    pub fn submit_dynamic_material(
        &mut self,
        registration: MaterialRegistration,
    ) -> Result<(MaterialShaderId, crate::pipeline_scheduler::MaterialId), crate::error::AwsmError>
    {
        // `register_material` bridges the scheduler internally.
        // After it returns, we just look up the
        // scheduler-side MaterialId that was allocated for this
        // shader_id and return it alongside.
        let shader_id = self.register_material(registration)?;
        let material_id = self
            .pipeline_scheduler
            .find_material_by_shader_id(shader_id)
            .ok_or_else(|| {
                crate::error::AwsmError::PipelineVariantNotCompiled(
                    "register_material did not populate scheduler",
                )
            })?;
        Ok((shader_id, material_id))
    }
}

// ─────────────────────────────────────────────────────────────────────
// Regression tests for the registry's pure (non-GPU)
// bucket-layout mutation behaviour. These run natively via
// `cargo test --all-features` and lock the contract for the
// batch-registration / transaction-boundary behaviour and
// the feature-hash bucketing.
//
// Cap-exceeded (`BucketCapExceeded`), MSAA edge-buffer resize, and
// Pending/Ready transitions are enforced one level up in
// `AwsmRenderer::register_material` (which needs a live GPU) — those are
// covered by the GPU/browser path, not here. What's tested here is the
// GPU-free `DynamicMaterials` surface the refactor must preserve.
#[cfg(test)]
mod tests {
    use super::*;

    fn reg(name: &str, layout_hash: u64, wgsl_hash: u64) -> MaterialRegistration {
        MaterialRegistration {
            name: name.to_string(),
            alpha_mode: MaterialAlphaMode::Opaque,
            double_sided: false,
            layout: MaterialLayout::default(),
            layout_hash,
            wgsl_hash,
            wgsl_fragment: String::new(),
            buffer_defaults: Vec::new(),
            uniform_defaults: Vec::new(),
        }
    }

    fn first_party_len() -> usize {
        first_party_bucket_entries().len()
    }

    #[test]
    fn register_grows_bucket_count_by_one_per_distinct_material() {
        let mut dm = DynamicMaterials::new();
        let fp = first_party_len();
        assert_eq!(dm.bucket_entries_cached().len(), fp);
        assert!(dm.is_empty());

        dm.insert(reg("a", 1, 1)).unwrap();
        assert_eq!(dm.len(), 1);
        assert_eq!(dm.bucket_entries_cached().len(), fp + 1);

        dm.insert(reg("b", 2, 2)).unwrap();
        assert_eq!(dm.len(), 2);
        assert_eq!(dm.bucket_entries_cached().len(), fp + 2);
    }

    #[test]
    fn reregister_identical_is_idempotent() {
        let mut dm = DynamicMaterials::new();
        let id1 = dm.insert(reg("a", 1, 1)).unwrap();
        let hash_after_first = dm.dispatch_hash_cached();

        // Same (name, layout_hash, wgsl_hash) → same id, no growth, no
        // dispatch_hash change.
        let id2 = dm.insert(reg("a", 1, 1)).unwrap();
        assert_eq!(id1, id2);
        assert_eq!(dm.len(), 1);
        assert_eq!(dm.bucket_entries_cached().len(), first_party_len() + 1);
        assert_eq!(dm.dispatch_hash_cached(), hash_after_first);
        assert!(dm.would_be_idempotent(&reg("a", 1, 1)));
    }

    #[test]
    fn duplicate_name_different_hash_errors() {
        let mut dm = DynamicMaterials::new();
        dm.insert(reg("a", 1, 1)).unwrap();
        let err = dm.insert(reg("a", 1, 2)).unwrap_err();
        matches!(err, AwsmDynamicMaterialError::DuplicateName(_))
            .then_some(())
            .expect("same name + different wgsl_hash must be a DuplicateName error");
        // The failed insert must not have grown the registry.
        assert_eq!(dm.len(), 1);
        assert!(!dm.would_be_idempotent(&reg("a", 1, 2)));
    }

    #[test]
    fn dispatch_hash_zero_when_empty_nonzero_when_registered_resets_on_empty() {
        let mut dm = DynamicMaterials::new();
        assert_eq!(dm.dispatch_hash(), 0);
        assert_eq!(dm.dispatch_hash_cached(), 0);

        let id = dm.insert(reg("a", 1, 1)).unwrap();
        assert_ne!(dm.dispatch_hash(), 0);
        assert_eq!(dm.dispatch_hash_cached(), dm.dispatch_hash());

        dm.remove(id).unwrap();
        assert!(dm.is_empty());
        assert_eq!(dm.dispatch_hash(), 0);
        assert_eq!(dm.dispatch_hash_cached(), 0);
    }

    #[test]
    fn dispatch_hash_changes_when_a_distinct_material_is_added() {
        let mut dm = DynamicMaterials::new();
        dm.insert(reg("a", 1, 1)).unwrap();
        let h1 = dm.dispatch_hash_cached();
        dm.insert(reg("b", 2, 2)).unwrap();
        let h2 = dm.dispatch_hash_cached();
        assert_ne!(
            h1, h2,
            "adding a distinct material must change dispatch_hash"
        );
    }

    #[test]
    fn unregister_refreshes_caches_back_to_first_party() {
        let mut dm = DynamicMaterials::new();
        let fp = first_party_len();
        let id = dm.insert(reg("a", 1, 1)).unwrap();
        assert_eq!(dm.bucket_entries_cached().len(), fp + 1);

        dm.remove(id).unwrap();
        assert_eq!(dm.len(), 0);
        assert_eq!(dm.bucket_entries_cached().len(), fp);
        assert_eq!(dm.dispatch_hash_cached(), 0);
    }

    #[test]
    fn remove_unknown_or_non_dynamic_errors() {
        let mut dm = DynamicMaterials::new();
        // Never-registered dynamic id.
        let unknown = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START + 42);
        matches!(
            dm.remove(unknown).unwrap_err(),
            AwsmDynamicMaterialError::UnknownShaderId(_)
        )
        .then_some(())
        .expect("removing an unregistered dynamic id must error");
        // A first-party (non-dynamic) id is rejected before the map lookup.
        matches!(
            dm.remove(MaterialShaderId::PBR).unwrap_err(),
            AwsmDynamicMaterialError::UnknownShaderId(_)
        )
        .then_some(())
        .expect("removing a non-dynamic id must error");
    }

    #[test]
    fn bucket_entries_first_party_prefix_then_sorted_dynamic_suffix() {
        let mut dm = DynamicMaterials::new();
        let fp = first_party_len();
        // Insert in a non-sorted-by-future-id order; ids are allocated
        // ascending, and the suffix must come out ascending by shader_id.
        let id_a = dm.insert(reg("a", 1, 1)).unwrap();
        let id_b = dm.insert(reg("b", 2, 2)).unwrap();
        let id_c = dm.insert(reg("c", 3, 3)).unwrap();
        assert!(id_a.as_u32() < id_b.as_u32() && id_b.as_u32() < id_c.as_u32());

        let entries = dm.bucket_entries_cached();
        assert_eq!(entries.len(), fp + 3);
        // First-party prefix is unchanged.
        let fp_entries = first_party_bucket_entries();
        assert_eq!(&entries[..fp], &fp_entries[..]);
        // Dynamic suffix sorted ascending by shader_id.
        let suffix_ids: Vec<u32> = entries[fp..].iter().map(|e| e.shader_id.as_u32()).collect();
        let mut sorted = suffix_ids.clone();
        sorted.sort_unstable();
        assert_eq!(suffix_ids, sorted);
    }

    #[test]
    fn reregister_after_removal_allocates_fresh_id_no_reuse() {
        let mut dm = DynamicMaterials::new();
        let id1 = dm.insert(reg("a", 1, 1)).unwrap();
        dm.remove(id1).unwrap();
        // A fresh registration (even same name/hashes) after removal gets
        // a new id — ids are monotonic, never reused, so stale pipeline
        // keys from the removed id can't collide with the new one.
        let id2 = dm.insert(reg("a", 1, 1)).unwrap();
        assert_ne!(id1, id2);
        assert!(id2.as_u32() > id1.as_u32());
    }

    // ── batch-registration validation ──────────────────

    // ── First-party feature-set variant allocation ──────────────────────────

    #[test]
    fn resolve_first_party_variant_dedups_by_base_and_features() {
        let mut dm = DynamicMaterials::new();
        let fp = first_party_len();

        let a = dm.resolve_first_party_variant(ShadingBase::Pbr, 0b001);
        let b = dm.resolve_first_party_variant(ShadingBase::Pbr, 0b001);
        assert_eq!(a, b, "same (base, features) must dedup to the same id");
        assert!(a.is_dynamic(), "variant ids live in the dynamic range");
        assert_eq!(dm.bucket_entries_cached().len(), fp + 1);

        // Different feature mask → distinct bucket.
        let c = dm.resolve_first_party_variant(ShadingBase::Pbr, 0b010);
        assert_ne!(a, c);
        assert_eq!(dm.bucket_entries_cached().len(), fp + 2);

        // Same feature mask, different base → distinct bucket.
        let d = dm.resolve_first_party_variant(ShadingBase::Toon, 0b001);
        assert_ne!(a, d);
        assert_ne!(c, d);
        assert_eq!(dm.bucket_entries_cached().len(), fp + 3);
    }

    #[test]
    fn resolve_first_party_variant_hard_errors_at_cap() {
        let mut dm = DynamicMaterials::new();
        // Fill to exactly the cap with distinct PBR feature-set variants.
        let mut i = 0u32;
        while dm.bucket_entries_cached().len() < MAX_BUCKET_ENTRIES {
            let id = dm
                .resolve_first_party_variant_or_cap_err(ShadingBase::Pbr, i, MAX_BUCKET_ENTRIES)
                .expect("should resolve below the cap");
            assert!(id.is_dynamic());
            i += 1;
        }
        assert_eq!(dm.bucket_entries_cached().len(), MAX_BUCKET_ENTRIES);

        // An already-known feature-set still resolves (idempotent) even at
        // saturation — returns the existing variant id, no allocation.
        let existing = dm
            .resolve_first_party_variant_or_cap_err(ShadingBase::Pbr, 0, MAX_BUCKET_ENTRIES)
            .expect("existing feature-set must resolve at saturation");
        assert!(existing.is_dynamic());

        // A brand-new feature-set past the cap is a HARD ERROR — no silent
        // fallback to the canonical bucket — and does NOT grow the list.
        let err = dm
            .resolve_first_party_variant_or_cap_err(ShadingBase::Pbr, 999_999, MAX_BUCKET_ENTRIES)
            .expect_err("a new variant past the cap must error");
        assert!(matches!(
            err,
            AwsmDynamicMaterialError::BucketCapExceeded { .. }
        ));
        assert_eq!(dm.bucket_entries_cached().len(), MAX_BUCKET_ENTRIES);
    }

    #[test]
    fn first_party_variant_meta_round_trips() {
        let mut dm = DynamicMaterials::new();
        let id = dm.resolve_first_party_variant(ShadingBase::Pbr, 0b101);
        assert_eq!(
            dm.first_party_variant_of(id),
            Some((ShadingBase::Pbr, 0b101))
        );
        // Canonical first-party + never-allocated ids return None.
        assert_eq!(dm.first_party_variant_of(MaterialShaderId::PBR), None);
    }

    #[test]
    fn fp_variant_buckets_appear_after_defaults_before_custom() {
        let mut dm = DynamicMaterials::new();
        let fp = first_party_len();
        let var = dm.resolve_first_party_variant(ShadingBase::Pbr, 0b001);
        let cust = dm.insert(reg("c", 1, 1)).unwrap();

        let entries = dm.bucket_entries_cached();
        assert_eq!(entries.len(), fp + 2);
        // Index 0 stays the canonical PBR bucket (classify routes skybox
        // to bit 0 → it must remain the PBR skybox-owner).
        assert_eq!(entries[0].shader_id, MaterialShaderId::PBR);
        assert_eq!(entries[0].base, ShadingBase::Pbr);
        // The fp variant comes after the defaults, the custom last.
        assert_eq!(entries[fp].shader_id, var);
        assert_eq!(entries[fp].base, ShadingBase::Pbr);
        assert!(entries[fp].name.starts_with("pbr_"));
        assert_eq!(entries[fp + 1].shader_id, cust);
        assert_eq!(entries[fp + 1].base, ShadingBase::Custom);
    }

    #[test]
    fn validate_batch_accepts_distinct_new_materials() {
        let dm = DynamicMaterials::new();
        let batch = vec![reg("a", 1, 1), reg("b", 2, 2), reg("c", 3, 3)];
        dm.validate_batch(&batch)
            .expect("distinct names within budget must validate");
    }

    #[test]
    fn validate_batch_empty_is_ok() {
        let dm = DynamicMaterials::new();
        dm.validate_batch(&[]).unwrap();
    }

    #[test]
    fn validate_batch_rejects_internal_name_collision_with_different_hash() {
        let dm = DynamicMaterials::new();
        // Same name, different hashes, within one batch → DuplicateName.
        let batch = vec![reg("dup", 1, 1), reg("dup", 9, 9)];
        matches!(
            dm.validate_batch(&batch).unwrap_err(),
            AwsmDynamicMaterialError::DuplicateName(_)
        )
        .then_some(())
        .expect("name collision with differing hashes must be DuplicateName");
    }

    #[test]
    fn validate_batch_allows_idempotent_repeat_within_batch() {
        let dm = DynamicMaterials::new();
        // Same name AND same hashes repeated → idempotent, one bucket.
        let batch = vec![reg("a", 1, 1), reg("a", 1, 1)];
        dm.validate_batch(&batch)
            .expect("byte-identical repeat in a batch is idempotent, not a conflict");
    }

    #[test]
    fn validate_batch_counts_only_new_buckets_against_cap() {
        let mut dm = DynamicMaterials::new();
        let fp = first_party_len();
        // Fill to exactly the cap with distinct dynamic materials.
        let dynamic_slots = MAX_BUCKET_ENTRIES - fp;
        for i in 0..dynamic_slots {
            dm.insert(reg(&format!("m{i}"), i as u64, i as u64))
                .unwrap();
        }
        assert_eq!(dm.bucket_entries_cached().len(), MAX_BUCKET_ENTRIES);

        // A batch of purely idempotent re-registrations adds no buckets
        // → must validate even at saturation.
        let idempotent = vec![reg("m0", 0, 0), reg("m1", 1, 1)];
        dm.validate_batch(&idempotent)
            .expect("idempotent re-registrations must validate even at the cap");

        // A single genuinely-new material at saturation → cap exceeded.
        let overflow = vec![reg("brand_new", 999, 999)];
        matches!(
            dm.validate_batch(&overflow).unwrap_err(),
            AwsmDynamicMaterialError::BucketCapExceeded { .. }
        )
        .then_some(())
        .expect("a new bucket past the cap must be BucketCapExceeded");
    }

    #[test]
    fn validate_batch_checks_final_count_not_per_insert() {
        let mut dm = DynamicMaterials::new();
        let fp = first_party_len();
        // Leave room for exactly 2 more buckets.
        let dynamic_slots = MAX_BUCKET_ENTRIES - fp - 2;
        for i in 0..dynamic_slots {
            dm.insert(reg(&format!("m{i}"), i as u64, i as u64))
                .unwrap();
        }
        // A batch of exactly 2 new materials fits the final layout.
        let fits = vec![reg("x", 100, 100), reg("y", 101, 101)];
        dm.validate_batch(&fits)
            .expect("batch that fits the final count must pass");
        // 3 new would overflow by one.
        let over = vec![reg("x", 100, 100), reg("y", 101, 101), reg("z", 102, 102)];
        matches!(
            dm.validate_batch(&over).unwrap_err(),
            AwsmDynamicMaterialError::BucketCapExceeded { would_be, .. }
                if would_be == MAX_BUCKET_ENTRIES + 1
        )
        .then_some(())
        .expect("over-budget batch must report would_be = cap + 1");
    }
}
