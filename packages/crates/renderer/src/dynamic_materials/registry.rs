//! The `DynamicMaterials` unified variant registry + its impls.
//! See the [`crate::dynamic_materials`] module docs for the design.

use super::*;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use awsm_renderer_materials::{MaterialAlphaMode, MaterialShaderId};

use awsm_renderer_materials::dynamic::{DynamicMaterialContext, DynamicTextureBinding};
use awsm_renderer_materials::dynamic_layout::MaterialLayout;
use awsm_renderer_materials::TextureContext;

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

    fn alpha_mode(
        &self,
        shader_id: MaterialShaderId,
    ) -> Option<awsm_renderer_materials::MaterialAlphaMode> {
        self.materials.get(shader_id).map(|r| r.alpha_mode)
    }

    fn resolve_texture_index(&self, binding: Option<&DynamicTextureBinding>) -> [u32; 2] {
        // Unbound slot → "no texture" sentinel in the first word. Same
        // convention first-party materials use when an
        // Optional<MaterialTexture> is None; the generated
        // `material_sample_<name>` helper treats first-word `u32::MAX` as
        // "no texture".
        let Some(binding) = binding else {
            return [u32::MAX, 0];
        };
        // No `TextureContext` attached → can't resolve. Stay at the
        // sentinel rather than guessing. Callers that route through
        // `Materials::update` always plumb `Textures` in.
        let Some(textures) = self.textures else {
            return [u32::MAX, 0];
        };
        match binding {
            DynamicTextureBinding::Pooled { texture, sampler } => {
                // Word 0 — `array_and_layer`: `array_index |
                // (layer_index << 12)`, matching the bit-layout of
                // `TextureInfoRaw.array_and_layer` (see
                // `shared_wgsl/textures.wgsl::convert_texture_info`).
                // Missing entries → `u32::MAX` (the WGSL helpers treat
                // that as "no texture").
                let Some(entry) = textures.texture_entry(*texture) else {
                    return [u32::MAX, 0];
                };
                let array_index = entry.array_index as u32;
                let layer_index = entry.layer_index as u32;
                debug_assert!(array_index <= 0xFFF, "array_index exceeds 12-bit field");
                debug_assert!(layer_index <= 0xFFFFF, "layer_index exceeds 20-bit field");
                let array_and_layer = (layer_index << 12) | (array_index & 0xFFF);

                // Word 1 — `uv_and_sampler`: `uv_set | (sampler_index <<
                // 8)`, mirroring `TextureInfoRaw.uv_and_sampler`. Dynamic
                // pooled bindings carry no UV-set selector (the author
                // passes `uv` explicitly), so uv_set is 0. A sampler that
                // isn't pooled resolves to index 0 (the pool default).
                let sampler_index = textures.sampler_index(*sampler).unwrap_or(0);
                let uv_and_sampler = sampler_index << 8;

                [array_and_layer, uv_and_sampler]
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
        if shader_id == MaterialShaderId::SKYBOX {
            // The skybox bucket shades no geometry; its pipeline is the
            // `skybox_primary` writer (gated to `skybox_only` includes via
            // `owns_skybox`). `Pbr` is just the base it nominally carries — the
            // owns_skybox path overrides the include set regardless.
            ShadingBase::Pbr
        } else if shader_id == MaterialShaderId::PBR {
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
    /// (dynamic materials) has none — it conservatively gets the full set.
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

/// The closure of shared shader modules a pipeline of this shading base needs.
/// First-party bases map to their
/// declared set; `Custom` (dynamic materials) conservatively gets the full set
/// since author WGSL may reference anything.
pub fn resolved_includes_for_base(base: ShadingBase) -> awsm_renderer_materials::ShaderIncludes {
    base.canonical_shader_id()
        .and_then(awsm_renderer_materials::registry::declarations_for_shader_id)
        .map(|(inc, _)| inc)
        .unwrap_or_else(awsm_renderer_materials::ShaderIncludes::all)
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
    /// `extras.wgsl` — the `extras_load_*` accessors over the extras storage
    /// pool. Tier A (generic): only author/custom WGSL that declares `EXTRAS`
    /// calls these; no first-party shading or kernel scaffolding does. The
    /// `extras_pool` *binding* stays declared regardless (ABI); only the WGSL
    /// accessor bodies are gated. (Phase 4)
    pub extras: bool,
    /// `skybox.wgsl` helper — `sample_skybox`. Tier A: in the opaque pass only
    /// the skybox-owner bucket (`skybox_primary`) calls it; the material kernel
    /// (`compute.wgsl`) never does. A custom material that declares `SKYBOX` may
    /// sample it too. (Phase 4)
    pub skybox: bool,
    /// `light_access.wgsl` accessor FUNCTIONS (get_lights_info / get_light /
    /// light_sample / …). Tier A: a material/scene with no lighting opts out
    /// completely. The structs (`light_access_types.wgsl`) stay always-included
    /// (bind-group ABI); only the accessor bodies + the kernel's
    /// `get_lights_info()` calls + `shade_sample` `lights_info` param gate on
    /// this. PBR/Toon declare LIGHT_ACCESS (and APPLY_LIGHTING resolves to it).
    /// (Phase 4)
    pub light_access: bool,
    /// Texture sampling code: `texture_uvs.wgsl` (UV computation + texture-pool
    /// sampling) + the generic UV-derivative half of `mipmap.wgsl` + the Custom
    /// `material_uv` wrapper accessor. Tier A: a material that samples no textures
    /// opts out. NOTE: `textures.wgsl` itself (the `TextureInfo`/`TextureInfoRaw`
    /// descriptor structs) stays ALWAYS-included — the always-present
    /// `material.wgsl` storage accessor (`material_load_texture_info -> TextureInfo`)
    /// references the types (ABI-like). PBR/Unlit/Toon/Flipbook all declare
    /// TEXTURES. (Phase 4)
    pub textures: bool,
    /// Per-vertex COLOR attribute code: `vertex_color.wgsl` (the
    /// `VertexColorInfo` struct) + `vertex_color_attrib.wgsl` (the `vertex_color`
    /// fetch fn) + the Custom `material_vertex_color` accessor. Tier A. The only
    /// first-party caller is the PBR builder's `{% if pbr_features.vertex_color %}`
    /// block (PBR declares VERTEX_COLOR so always keeps it); Unlit/Toon/Flipbook
    /// never read vertex colour. (Phase 4)
    pub vertex_color: bool,
    /// `lighting/ibl.wgsl` — the `sample_ibl(...)` image-based-lighting primitive.
    /// Tier A: a custom material opts in to be lit by the scene environment
    /// (irradiance + prefiltered specular + BRDF LUT) without re-deriving PBR.
    /// The IBL cubemap/LUT bindings stay always-declared (ABI); only the WGSL
    /// helper body gates on this.
    pub ibl: bool,
    /// `normal_map` helpers (`apply_normal_map` / `material_tbn`) — Tier A: a
    /// custom material perturbs its normal from a normal-map sample using the
    /// engine's reconstructed per-pixel tangent frame. The frame fields on
    /// `OpaqueShadingInput` are always present; only these helpers gate on this.
    pub normal_map: bool,
}

impl ShaderIncludeFlags {
    pub fn for_base(base: ShadingBase) -> Self {
        Self::from_includes(resolved_includes_for_base(base))
    }

    /// Build the gate flags from an explicit (resolved or unresolved) include
    /// set. Used for `Custom`-base dynamic materials, which carry their own
    /// author-declared [`awsm_renderer_materials::ShaderIncludes`] per registration
    /// rather than sharing the base's canonical set.
    pub fn from_includes(includes: awsm_renderer_materials::ShaderIncludes) -> Self {
        let i = includes.resolve();
        use awsm_renderer_materials::ShaderIncludes as S;
        Self {
            brdf: i.contains(S::BRDF),
            apply_lighting: i.contains(S::APPLY_LIGHTING),
            material_color_calc: i.contains(S::MATERIAL_COLOR_CALC),
            extras: i.contains(S::EXTRAS),
            skybox: i.contains(S::SKYBOX),
            light_access: i.contains(S::LIGHT_ACCESS),
            textures: i.contains(S::TEXTURES),
            vertex_color: i.contains(S::VERTEX_COLOR),
            ibl: i.contains(S::IBL),
            normal_map: i.contains(S::NORMAL_MAP),
        }
    }

    /// Build the gate flags for a CUSTOM (dynamic) material from its declared
    /// include set, with every Tier-B (PBR-internal) module FORCED OFF. A custom
    /// material can never enable `brdf` / `apply_lighting` / `material_color_calc`
    /// — those are welded to the `PbrMaterial` / `PbrMaterialColor` types and are
    /// emitted only for the built-in PBR base. This is defense beyond `all()`
    /// being Tier-A-only (Phase 3 item 1): even an explicit `S::BRDF` in a
    /// registration is ignored on the custom path. A custom material that wants
    /// PBR-like shading supplies its own WGSL (optionally built on the generic
    /// `brdf_primitives` Tier-A helpers).
    pub fn for_custom(includes: awsm_renderer_materials::ShaderIncludes) -> Self {
        let mut f = Self::from_includes(includes);
        f.brdf = false;
        f.apply_lighting = false;
        f.material_color_calc = false;
        f
    }

    /// The canonical skybox-owner bucket (#13): it only writes the skybox on
    /// skybox/uncovered pixels — its material-shading body is gated out — so it
    /// needs none of the PBR shading modules even though its `base` is `Pbr`.
    pub fn skybox_only() -> Self {
        Self {
            brdf: false,
            apply_lighting: false,
            material_color_calc: false,
            extras: false,
            // The skybox-owner bucket (skybox_primary) is the one place that
            // actually calls `sample_skybox`, so it keeps the skybox helper.
            skybox: true,
            // Skybox-owner shades no geometry → no light accessors.
            light_access: false,
            // Skybox-owner samples only the skybox cubemap (via sample_skybox),
            // never the texture pool → no texture sampling code.
            textures: false,
            // Skybox-owner reads no per-vertex colour.
            vertex_color: false,
            // Skybox-owner shades no geometry → no IBL surface term.
            ibl: false,
            // Skybox-owner shades no geometry → no normal mapping.
            normal_map: false,
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
    /// ([`awsm_renderer_materials::pbr::PbrFeatures::bits`]). Distinguishes two
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
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(), // inert for Custom (own WGSL); never the uber set
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
    // Bucket 0 is the dedicated SKYBOX bucket — NOT a material. classify routes
    // every fully-uncovered ("sky") pixel here, and its opaque pipeline is the
    // `skybox_primary` writer (owns_skybox → `skybox_only` includes; shades no
    // geometry). Reserved by TYPE at index 0 (its id is 0, which sorts ahead of
    // every real material) instead of the old "the empty-feature PBR bucket is
    // secretly the skybox" convention — so no real PBR material can ever collide
    // with the sky slot. It rides a `Pbr` base purely so it shares the opaque
    // kernel preamble; `owns_skybox` overrides the include set regardless.
    let skybox = BucketEntry {
        shader_id: MaterialShaderId::SKYBOX,
        base: ShadingBase::Pbr,
        pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
        name: "skybox".to_string(),
    };
    std::iter::once(skybox)
        .chain(
            awsm_renderer_materials::registry::enabled_materials()
                .iter()
                .map(|e| {
                    BucketEntry {
                        shader_id: e.shader_id,
                        base: ShadingBase::for_shader_id(e.shader_id),
                        // EMPTY feature-set for the canonical base bucket. Every real
                        // first-party material routes to its own per-feature-set variant
                        // bucket, so these canonical buckets shade nothing — the empty
                        // set compiles the minimal shader (never an "uber" all-features
                        // one). Inert anyway for Unlit/Flipbook (their bodies don't read
                        // `pbr_features`).
                        pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
                        name: e.name.to_string(),
                    }
                }),
        )
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
    /// A PBR/Toon material derives its [`awsm_renderer_materials::pbr::PbrFeatures`]
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
    /// Registration ceiling — how many co-resident buckets the registry
    /// accepts (the cap-check sites compare against this). Defaults to
    /// [`MAX_BUCKET_ENTRIES`] (32) so behavior is identical to today unless
    /// the builder sets a [`BucketConfig`](crate::dynamic_materials::BucketConfig).
    /// Does NOT size any per-frame shader/buffer — those follow the live
    /// count (§0). Validated `1..=65534` by the builder before it lands here.
    max_bucket_entries: usize,
}

/// Parse + validate an assembled WGSL module with `naga` (Capabilities::all).
/// Returns the compile error message(s) — empty = valid. Shared by every
/// `validate_dynamic_*_wgsl` accessor so they all surface errors identically.
#[cfg(feature = "dynamic-material-validation")]
fn naga_validate_module(src: &str) -> Vec<String> {
    match naga::front::wgsl::parse_str(src) {
        Err(e) => vec![e.emit_to_string(src)],
        Ok(module) => {
            let mut validator = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            match validator.validate(&module) {
                Ok(_) => Vec::new(),
                Err(e) => vec![e.emit_to_string(src)],
            }
        }
    }
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
            max_bucket_entries: MAX_BUCKET_ENTRIES,
        }
    }

    /// Sets the registration ceiling (from the builder's resolved
    /// [`BucketConfig`](crate::dynamic_materials::BucketConfig)). The caller
    /// must have validated `1..=65534` already. Does not resize anything —
    /// per-frame widths follow the live count (§0).
    pub fn set_max_bucket_entries(&mut self, cap: u32) {
        self.max_bucket_entries = cap as usize;
    }

    /// The configured registration ceiling (default 32). Cap-check sites
    /// compare the prospective bucket count against this.
    pub fn max_bucket_entries(&self) -> usize {
        self.max_bucket_entries
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
                pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(), // inert for Custom (own WGSL); never the uber set
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

    /// Overwrite ONE uniform's authored default on a registration in place.
    /// Live uniform edits push the new value into every ALREADY-inserted
    /// material, but consumers that seed a fresh material from the
    /// registration (`uniform_defaults`) would otherwise keep reading the
    /// register-time snapshot — a mesh (re)materialized after the edit came
    /// up with stale values. Returns `false` (no-op) on an unknown id, an
    /// out-of-range index, or a type mismatch against the layout.
    pub fn set_uniform_default(
        &mut self,
        shader_id: MaterialShaderId,
        index: usize,
        value: awsm_renderer_materials::dynamic_layout::UniformValue,
    ) -> bool {
        let Some(reg) = self.registrations.get_mut(&shader_id) else {
            return false;
        };
        let Some(slot) = reg.layout.uniforms.get(index) else {
            return false;
        };
        if value.field_type() != slot.ty {
            return false;
        }
        if reg.uniform_defaults.len() < reg.layout.uniforms.len() {
            let pad = reg.layout.uniforms.len();
            reg.uniform_defaults.resize_with(pad, || value.clone());
        }
        reg.uniform_defaults[index] = value;
        true
    }

    /// Builds the [`DynamicShaderInfo`] (the `MaterialData` struct decl +
    /// `material_data_load` accessor + author fragment) for a registered
    /// dynamic material, or `None` for a first-party / unregistered id.
    ///
    /// The per-mesh transparent pipeline path needs this so a `Custom`-base
    /// transparent mesh emits the same dynamic-material wrapper the opaque
    /// prewarm does — without it the transparent fragment references
    /// `MaterialData` but never defines it (WGSL "unresolved type").
    pub fn shader_info_for(
        &self,
        shader_id: MaterialShaderId,
    ) -> Option<crate::render_passes::material_opaque::shader::cache_key::DynamicShaderInfo> {
        let reg = self.registrations.get(&shader_id)?;
        Some(
            crate::render_passes::material_opaque::shader::cache_key::DynamicShaderInfo {
                shader_includes: reg.shader_includes.resolve(),
                struct_decl: awsm_renderer_materials::dynamic_layout::generate_wgsl_struct(
                    "MaterialData",
                    &reg.layout,
                ),
                loader_decl: awsm_renderer_materials::dynamic_layout::generate_wgsl_loader(
                    "MaterialData",
                    "material_data_load",
                    &reg.layout,
                ),
                wgsl_fragment: reg.wgsl_fragment.clone(),
            },
        )
    }

    /// Cheap existence check: does this registered material declare a non-empty
    /// custom-vertex body? Equivalent to `vertex_shader_info_for(..).is_some()`
    /// but WITHOUT building the `DynamicVertexShaderInfo` (which allocates the
    /// generated `MaterialData` struct/loader WGSL). `collect_renderables` runs
    /// per-frame and only needs the bool to route a mesh to the custom-vertex
    /// pipeline, so it must not allocate per frame (per the no-per-frame-alloc
    /// standard). The full info is built once at pipeline-compile time.
    pub fn has_vertex_shader(&self, shader_id: MaterialShaderId) -> bool {
        self.registrations
            .get(&shader_id)
            .and_then(|reg| reg.wgsl_vertex.as_deref())
            .is_some_and(|w| !w.trim().is_empty())
    }

    /// Returns the [`DynamicVertexShaderInfo`] (the `MaterialData` struct decl +
    /// loader + author vertex body) for a registered custom-vertex material, or
    /// `None` when the material declared no `wgsl_vertex` (→ shared fast vertex
    /// pipeline). The struct/loader are byte-identical to the fragment hook's,
    /// so both stages read the same uniform buffer. Sibling of
    /// [`Self::shader_info_for`] / [`Self::alpha_info_for`]. NOTE: this
    /// allocates — for a per-frame existence check use [`Self::has_vertex_shader`].
    pub fn vertex_shader_info_for(
        &self,
        shader_id: MaterialShaderId,
    ) -> Option<crate::render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo> {
        let reg = self.registrations.get(&shader_id)?;
        let wgsl_vertex = reg.wgsl_vertex.as_ref()?.clone();
        if wgsl_vertex.trim().is_empty() {
            return None;
        }
        Some(
            crate::render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo {
                shader_includes: reg.shader_includes.resolve(),
                struct_decl: awsm_renderer_materials::dynamic_layout::generate_wgsl_struct(
                    "MaterialData",
                    &reg.layout,
                ),
                loader_decl: awsm_renderer_materials::dynamic_layout::generate_wgsl_loader(
                    "MaterialData",
                    "material_data_load",
                    &reg.layout,
                ),
                wgsl_vertex,
            },
        )
    }

    /// Returns the **alpha-only** dynamic-shader info for a registered MASK
    /// custom material — the generated `MaterialData` struct + loader + texture
    /// helpers, plus the author's alpha-only WGSL fragment — so the masked
    /// visibility-raster variant can wrap it into `custom_alpha_dynamic`.
    /// `None` unless the material is registered, `alpha_mode == Mask`, and a
    /// non-empty `alpha_wgsl` was provided.
    pub fn alpha_info_for(
        &self,
        shader_id: MaterialShaderId,
    ) -> Option<crate::render_passes::geometry::shader::masked_cache_key::DynamicAlphaShaderInfo>
    {
        let reg = self.registrations.get(&shader_id)?;
        if !matches!(reg.alpha_mode, MaterialAlphaMode::Mask { .. }) {
            return None;
        }
        let alpha_wgsl = reg.alpha_wgsl.as_ref()?.clone();
        if alpha_wgsl.trim().is_empty() {
            return None;
        }
        Some(
            crate::render_passes::geometry::shader::masked_cache_key::DynamicAlphaShaderInfo {
                struct_decl: awsm_renderer_materials::dynamic_layout::generate_wgsl_struct(
                    "MaterialData",
                    &reg.layout,
                ),
                loader_decl: awsm_renderer_materials::dynamic_layout::generate_wgsl_loader(
                    "MaterialData",
                    "material_data_load",
                    &reg.layout,
                ),
                texture_helpers:
                    awsm_renderer_materials::dynamic_layout::generate_wgsl_texture_helpers(
                        "MaterialData",
                        &reg.layout,
                    ),
                alpha_wgsl,
            },
        )
    }

    /// Synchronously validate a registered custom-vertex material's ASSEMBLED
    /// **geometry custom-vertex** module with `naga` — the vertex-stage sibling
    /// of [`crate::AwsmRenderer::validate_dynamic_material_wgsl`]. Returns the
    /// compile error message(s) (empty = valid).
    ///
    /// Assembles the same WGSL the live custom-vertex geometry pipeline would
    /// compile: the masked geometry bind groups (they declare `materials` + the
    /// texture pool the hook's `material_data_load` / `material_sample_<name>`
    /// read) + the geometry vertex shader built with the `custom_displace_vertex`
    /// hook + the plain geometry fragment. Representative pool/AA config (lens
    /// 1/1, single-sampled, non-instanced, uniform meta) — validation depends
    /// only on the dynamic struct/loader/body, not the array lengths.
    ///
    /// `None` registration / no `wgsl_vertex` → empty (nothing to validate).
    /// naga line numbers index the assembled module (not the author's snippet),
    /// so callers surface the error message without a per-snippet line.
    #[cfg(feature = "dynamic-material-validation")]
    pub fn validate_dynamic_vertex_wgsl(&self, shader_id: MaterialShaderId) -> Vec<String> {
        use crate::render_passes::geometry::shader::custom_vertex_cache_key::ShaderCacheKeyGeometryCustomVertex;
        use crate::render_passes::geometry::shader::custom_vertex_template::ShaderTemplateGeometryCustomVertex;

        let Some(dynamic_vertex) = self.vertex_shader_info_for(shader_id) else {
            return Vec::new();
        };
        let key = ShaderCacheKeyGeometryCustomVertex {
            shader_id,
            dynamic_vertex,
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_samples: None,
            instancing_transforms: false,
            meta_storage_array: false,
        };
        let template = match ShaderTemplateGeometryCustomVertex::try_from(&key) {
            Ok(t) => t,
            Err(e) => return vec![format!("shader template build failed: {e:?}")],
        };
        let src = match template.into_source() {
            Ok(s) => s,
            Err(e) => return vec![format!("shader render failed: {e:?}")],
        };
        naga_validate_module(&src)
    }

    /// Synchronously validate a registered custom-vertex material's ASSEMBLED
    /// **custom-vertex SHADOW** module with `naga` — the shadow-pass sibling of
    /// [`Self::validate_dynamic_vertex_wgsl`]. Returns the compile error
    /// message(s) (empty = valid).
    ///
    /// Assembles the same WGSL the live custom-vertex shadow pipeline compiles:
    /// the augmented custom-vertex shadow bind groups (shadow_view + materials +
    /// frame_globals + texture pool + the minimal material-load helpers) + the
    /// depth-only shadow vertex shader built with the `custom_displace_vertex`
    /// hook (no fragment). Representative pool config (1/1, non-instanced).
    /// Catches a custom-vertex body that the geometry pass accepts but the shadow
    /// assembly would reject (different surrounding bindings / helper set), so the
    /// displaced shadow can never silently fail to compile.
    ///
    /// `None` registration / no `wgsl_vertex` → empty (nothing to validate).
    #[cfg(feature = "dynamic-material-validation")]
    pub fn validate_dynamic_vertex_shadow_wgsl(&self, shader_id: MaterialShaderId) -> Vec<String> {
        use crate::shadows::shader::custom_vertex_cache_key::ShaderCacheKeyShadowCustomVertex;
        use crate::shadows::shader::custom_vertex_template::ShaderTemplateShadowCustomVertex;

        let Some(dynamic_vertex) = self.vertex_shader_info_for(shader_id) else {
            return Vec::new();
        };
        let key = ShaderCacheKeyShadowCustomVertex {
            shader_id,
            dynamic_vertex,
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            instancing_transforms: false,
        };
        let template = match ShaderTemplateShadowCustomVertex::try_from(&key) {
            Ok(t) => t,
            Err(e) => return vec![format!("shader template build failed: {e:?}")],
        };
        let src = match template.into_source() {
            Ok(s) => s,
            Err(e) => return vec![format!("shader render failed: {e:?}")],
        };
        naga_validate_module(&src)
    }

    /// Synchronously validate a registered custom-vertex material's ASSEMBLED
    /// **transparent custom-vertex** module with `naga` — the transparent-pass
    /// sibling of [`Self::validate_dynamic_vertex_wgsl`]. Returns the compile
    /// error message(s) (empty = valid).
    ///
    /// Assembles the same WGSL the live transparent custom-vertex pipeline would
    /// compile: the per-material transparent template (`base == Custom`, the
    /// fragment hook + the `custom_displace_vertex` vertex hook) — same
    /// `MaterialData` struct/loader the geometry path uses, but paired with the
    /// transparent bind groups (which already declare `materials` + the texture
    /// pool VERTEX-visible). Catches a custom-vertex body the geometry pass
    /// accepts but the transparent assembly would reject (different surrounding
    /// bindings / fragment contract), so the displaced transparent can never
    /// silently fail to compile. Representative pool/AA config (1/1, single-
    /// sampled, non-instanced).
    ///
    /// `None` registration / no `wgsl_vertex` → empty (nothing to validate).
    /// naga line numbers index the assembled module (not the author's snippet).
    #[cfg(feature = "dynamic-material-validation")]
    pub fn validate_dynamic_vertex_transparent_wgsl(
        &self,
        shader_id: MaterialShaderId,
    ) -> Vec<String> {
        use crate::render_passes::material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent;
        use crate::render_passes::material_transparent::shader::template::ShaderTemplateMaterialTransparent;
        use crate::render_passes::shared::material::cache_key::ShaderMaterialVertexAttributes;

        let Some(dynamic_vertex) = self.vertex_shader_info_for(shader_id) else {
            return Vec::new();
        };
        // A custom transparent material is `base == Custom`; the fragment
        // template references `MaterialData` inside the `base == Custom` wrapper,
        // so the fragment hook (struct/loader + author fragment) must be present.
        let Some(dynamic_shader) = self.shader_info_for(shader_id) else {
            return vec![
                "custom-vertex transparent validation requires a fragment hook \
                 (shader_info_for returned None)"
                    .to_string(),
            ];
        };
        let key = ShaderCacheKeyMaterialTransparent {
            instancing_transforms: false,
            attributes: ShaderMaterialVertexAttributes {
                normals: true,
                tangents: true,
                color_sets: None,
                uv_sets: Some(1),
            },
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_sample_count: None,
            mipmaps: true,
            base: crate::dynamic_materials::ShadingBase::Custom,
            pbr_features: awsm_renderer_materials::pbr::PbrFeatures::default().bits(),
            dispatch_hash: 1,
            dynamic_shader_id: Some(shader_id),
            dynamic_shader: Some(dynamic_shader),
            dynamic_vertex_shader: Some(dynamic_vertex),
            froxel_slice_count: crate::render_passes::light_culling::buffers::DEFAULT_SLICE_COUNT,
        };
        let template = match ShaderTemplateMaterialTransparent::try_from(&key) {
            Ok(t) => t,
            Err(e) => return vec![format!("shader template build failed: {e:?}")],
        };
        let src = match template.into_source() {
            Ok(s) => s,
            Err(e) => return vec![format!("shader render failed: {e:?}")],
        };
        naga_validate_module(&src)
    }

    /// Synchronously validate a registered material's ASSEMBLED **combined
    /// masked + custom-vertex GEOMETRY** module with `naga` — the union of
    /// [`Self::validate_dynamic_vertex_wgsl`] (displacement) +
    /// [`crate::AwsmRenderer::validate_dynamic_material_wgsl`] (alpha cutout).
    /// Returns the compile error message(s) (empty = valid).
    ///
    /// Assembles the same WGSL the live combined pipeline compiles: the masked
    /// geometry bind groups + the geometry vertex built with the
    /// `custom_displace_vertex` hook + the masked fragment (alpha test). The
    /// shared `material_load_*` / `texture_pool_sample` helpers come from the
    /// fragment's `masked_alpha.wgsl` (single copy); the vertex hook's generated
    /// `material_data_load` resolves against it — proving the combine is
    /// redefinition-free. For a Custom material the fragment's struct/loader are
    /// suppressed (the vertex emits them).
    ///
    /// `None` registration / no `wgsl_vertex` → empty. A material with no
    /// `alpha_wgsl` (built-in MASK base) validates the base-color alpha path.
    #[cfg(feature = "dynamic-material-validation")]
    pub fn validate_dynamic_masked_vertex_wgsl(&self, shader_id: MaterialShaderId) -> Vec<String> {
        use crate::render_passes::geometry::shader::masked_custom_vertex_cache_key::ShaderCacheKeyGeometryMaskedCustomVertex;
        use crate::render_passes::geometry::shader::masked_custom_vertex_template::ShaderTemplateGeometryMaskedCustomVertex;

        let Some(dynamic_vertex) = self.vertex_shader_info_for(shader_id) else {
            return Vec::new();
        };
        // A registered material with a `wgsl_vertex` body is a dynamic (Custom)
        // material; its MASK cutout takes the Custom alpha path.
        let key = ShaderCacheKeyGeometryMaskedCustomVertex {
            shader_id,
            base: crate::dynamic_materials::ShadingBase::Custom,
            dynamic_vertex,
            dynamic_alpha: self.alpha_info_for(shader_id),
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
            msaa_samples: None,
        };
        let template = match ShaderTemplateGeometryMaskedCustomVertex::try_from(&key) {
            Ok(t) => t,
            Err(e) => return vec![format!("shader template build failed: {e:?}")],
        };
        let src = match template.into_source() {
            Ok(s) => s,
            Err(e) => return vec![format!("shader render failed: {e:?}")],
        };
        naga_validate_module(&src)
    }

    /// Synchronously validate a registered material's ASSEMBLED **combined
    /// masked + custom-vertex SHADOW** module with `naga` — the shadow sibling of
    /// [`Self::validate_dynamic_masked_vertex_wgsl`]. Returns the compile error
    /// message(s) (empty = valid).
    ///
    /// Assembles the same WGSL the live combined shadow pipeline compiles: the
    /// masked-shadow WGSL with `has_custom_vertex` on (depth-only displaced
    /// vertex + alpha-test fragment). Catches a body the geometry combine accepts
    /// but the shadow combine would reject (different surrounding bindings), so the
    /// displaced cutout shadow can never silently fail to compile.
    ///
    /// `None` registration / no `wgsl_vertex` → empty.
    #[cfg(feature = "dynamic-material-validation")]
    pub fn validate_dynamic_masked_vertex_shadow_wgsl(
        &self,
        shader_id: MaterialShaderId,
    ) -> Vec<String> {
        use crate::shadows::shader::masked_custom_vertex_cache_key::ShaderCacheKeyShadowMaskedCustomVertex;
        use crate::shadows::shader::masked_custom_vertex_template::ShaderTemplateShadowMaskedCustomVertex;

        let Some(dynamic_vertex) = self.vertex_shader_info_for(shader_id) else {
            return Vec::new();
        };
        let key = ShaderCacheKeyShadowMaskedCustomVertex {
            shader_id,
            base: crate::dynamic_materials::ShadingBase::Custom,
            dynamic_vertex,
            dynamic_alpha: self.alpha_info_for(shader_id),
            texture_pool_arrays_len: 1,
            texture_pool_samplers_len: 1,
        };
        let template = match ShaderTemplateShadowMaskedCustomVertex::try_from(&key) {
            Ok(t) => t,
            Err(e) => return vec![format!("shader template build failed: {e:?}")],
        };
        let src = match template.into_source() {
            Ok(s) => s,
            Err(e) => return vec![format!("shader render failed: {e:?}")],
        };
        naga_validate_module(&src)
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
            // Author-declared skinny-material declarations: changing them
            // re-specializes the Custom host shader's gated includes, so they
            // must invalidate the per-shader-id pipeline cache.
            reg.shader_includes.bits().hash(&mut hasher);
            reg.fragment_inputs.bits().hash(&mut hasher);
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
        if final_count > self.max_bucket_entries {
            return Err(AwsmDynamicMaterialError::BucketCapExceeded {
                would_be: final_count,
                max: self.max_bucket_entries,
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
/// The renderer's counterpart to `awsm_renderer_scene::MaterialDefinition` +
/// the loaded WGSL fragment. Consumers (`scene-editor`, `material-editor`,
/// game runtimes) convert their on-disk format into a
/// [`MaterialRegistration`] before calling
/// [`AwsmRenderer::register_material`](crate::AwsmRenderer::register_material);
/// the renderer never depends on `awsm-renderer-scene`.
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
    /// `MaterialInstance::buffer_overrides`) can also override.
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
    pub uniform_defaults: Vec<awsm_renderer_materials::dynamic_layout::UniformValue>,
    /// Author-declared optional shared shader modules this material's WGSL
    /// uses ("skinny materials" — see [`awsm_renderer_materials::ShaderIncludes`]).
    /// The renderer compiles the transitive closure and gates the shared
    /// shading modules (BRDF / apply_lighting / material_color_calc) on it,
    /// so a custom material that declares less gets a leaner Custom host
    /// shader. Defaults to [`ShaderIncludes::all`] — i.e. the pre-skinny
    /// "may reference anything" behaviour — until the author narrows it via
    /// the material editor's Pass Dependencies UI.
    pub shader_includes: awsm_renderer_materials::ShaderIncludes,
    /// Author-declared pre-shade fragment inputs this material consumes
    /// ([`awsm_renderer_materials::FragmentInputs`]). Carried for the cache key +
    /// future scaffolding gating; defaults to `all()`.
    pub fragment_inputs: awsm_renderer_materials::FragmentInputs,
    /// The **second** ("alpha-only") WGSL fragment, present only when
    /// `alpha_mode` is [`MaterialAlphaMode::Mask`]. Wrapped into
    /// `fn custom_alpha_dynamic(input: MaskAlphaInput) -> f32` and compiled into
    /// the masked visibility-raster variant so the material's cutout is
    /// alpha-tested (and casts hole-shaped shadows / shows through to
    /// transmission). `None` → the masked variant isn't built and the mesh
    /// renders solid through the opaque path. The body returns an `f32` alpha in
    /// `[0,1]`; the raster `discard`s below the per-mesh cutoff. Optional even
    /// for Mask materials (a procedural cutout can be tiny; a textured one
    /// samples via the generated `material_sample_<name>` helpers).
    pub alpha_wgsl: Option<String>,
    /// The **third** ("vertex") WGSL window — the programmable vertex-
    /// displacement body. Wrapped into `fn custom_displace_vertex(input:
    /// VertexDisplaceInput) -> VertexDisplaceOutput` and compiled into the
    /// rasterizing passes' custom-vertex pipeline variants (geometry / shadow /
    /// transparent / masked), so the material controls its vertices the same way
    /// `wgsl_fragment` controls its shading. `None` → the material uses the
    /// shared fast vertex pipeline (zero cost). Folded into [`Self::wgsl_hash`]
    /// by the consumer so an edit recompiles. See `DynamicVertexShaderInfo`.
    pub wgsl_vertex: Option<String>,
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
    /// **Readiness contract:** register is a pure deferred ADD — it submits a
    /// scheduler group for the new material (for observability) and flags the
    /// reconcile (`mark_variants_dirty`); it does NOT compile. The embedder's
    /// next [`AwsmRenderer::commit_load`] (the one compile path) runs
    /// [`Self::reconcile_material_variants`] → [`Self::ensure_scene_pipelines`],
    /// which detects the bucket-SET change, re-lays-out the bucket buffers, and
    /// compiles every pipeline the material needs (opaque, classify, per-shader,
    /// skybox + final_blend edge resolve) for the ACTIVE AA config, charging
    /// the new material's compile to its scheduler group so a WGSL error
    /// surfaces via `dynamic_material_compile_status`. The scheduler flips the
    /// material `Pending → Ready` when its last sub-pipeline resolves; frontends
    /// subscribed to [`Self::drain_pipeline_status_events`] observe that
    /// transition.
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
            let cap = self.dynamic_materials.max_bucket_entries();
            if current_len >= cap {
                return Err(AwsmDynamicMaterialError::BucketCapExceeded {
                    would_be: current_len + 1,
                    max: cap,
                });
            }
        }
        let buffer_defaults = registration.buffer_defaults.clone();
        let id = self.dynamic_materials.insert(registration)?;
        // A new/changed registration may carry a 2nd alpha-only WGSL → its masked
        // variant must (re)build on the next finalize even with no texture change.
        self.masked_dynamic_dirty = true;
        // Assign extras-pool slices for any buffer-slot defaults
        // declared on the registration. Per-instance overrides
        // (the per-instance MaterialInstance.buffer_overrides) can
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
        // Scheduler bridge: submit the material to the pipeline-readiness
        // scheduler so its lifecycle is observable via the status stream /
        // `pipeline_group_status` (the editor polls
        // `dynamic_material_compile_status` for the Pending→Ready/Failed
        // transition right after register). The returned `MaterialId` is
        // intentionally discarded — callers wanting the typed scheduler
        // handle use `submit_dynamic_material`. Failures are logged but
        // not propagated: register succeeded + the bucket-routing path
        // works without the scheduler entry; only observability degrades.
        if let Err(e) = self.submit_to_scheduler_for_shader_id(id) {
            tracing::warn!(
                target: "awsm_renderer::pipeline_readiness",
                "submit_to_scheduler_for_shader_id failed for {:?}: {:?}",
                id, e
            );
        }

        // Deferred compile: this insert grew `bucket_entries`, so flag the
        // reconcile. The embedder's next `commit_load` runs
        // `ensure_scene_pipelines`, which detects the bucket-SET change (its
        // `dispatch_hash` + count signature drifted), resizes the classify /
        // edge buffers + rebuilds the edge-layout uniform + clears the stale
        // layout-keyed pipeline caches, then compiles every bucket against the
        // new layout (this new one charged to its freshly-submitted scheduler
        // group, so its WGSL compile error — if any — surfaces via
        // `dynamic_material_compile_status`). The material lights up
        // Ready/Failed as the commit's compile drain resolves.
        self.materials.mark_variants_dirty();
        Ok(id)
    }

    /// Upload a custom material instance's per-slot BUFFER data into the extras
    /// pool (keyed by its `shader_id`), so the packed `MaterialData.<slot>_offset`
    /// / `_length` resolve to those bytes. The registration-time `buffer_defaults`
    /// path only covers material-level defaults; per-instance `buffer_overrides`
    /// flow through HERE — without this call the slice stays `(0, 0)` and the
    /// shader's `extras_load_*` reads pool[0] (zero → black).
    ///
    /// Call BEFORE [`Materials::insert`]/`update`: `insert` packs the payload by
    /// reading `extras_pool.slice_for`, so the slice must exist first.
    ///
    /// Keyed per-shader, not per-instance: two meshes sharing one custom material
    /// with DIFFERENT buffer data collide (last write wins). The common
    /// single-assignment case (and the bundle round-trip) is correct.
    pub fn upload_dynamic_material_buffers(&mut self, material: &crate::materials::Material) {
        let crate::materials::Material::Custom(dm) = material else {
            return;
        };
        for (slot_index, data) in dm.buffers.iter().enumerate() {
            let Some(words) = data else { continue };
            if words.is_empty() {
                continue;
            }
            match self
                .extras_pool
                .assign_or_update(&self.gpu, dm.shader_id, slot_index, words)
            {
                Ok(outcome) => {
                    if outcome.resized {
                        self.bind_groups
                            .mark_create(crate::bind_groups::BindGroupCreate::ExtrasPoolResize);
                    }
                }
                Err(e) => tracing::warn!(
                    "extras_pool: per-instance buffer assign failed (slot {slot_index}): {e:?}"
                ),
            }
        }
    }

    /// Per-frame reconcile: route each opaque PBR material to its
    /// per-feature-set *variant* bucket, allocating + compiling new variants
    /// and re-laying-out the bucket-dependent GPU state when the set grows.
    ///
    /// Called ONLY from `commit_load` (the one compile path) — never per render
    /// frame; a cheap no-op when the `variants_dirty` flag is clear (a commit
    /// with no material change since the last one). When a material entered / a
    /// feature-set changed since the last commit, it:
    /// 1. derives `PbrFeatures` for every opaque PBR material,
    /// 2. resolves each to a variant `shader_id` (deduped; allocates a
    ///    new bucket on first sight of a feature-set),
    /// 3. stamps the resolved id into the material payload's first u32
    ///    (so classify routes those pixels to the variant bucket and the
    ///    variant's specialized opaque pipeline shades them), and
    /// 4. drives [`Self::ensure_scene_pipelines`], which compiles exactly
    ///    what the live scene needs at the active AA config — handling any
    ///    bucket-SET grow (re-lay-out classify/edge buffers + clear the
    ///    stale layout-keyed caches, then recompile every bucket against
    ///    the final layout) internally.
    ///
    /// Only PBR specializes per feature-set; Toon/Unlit/Flipbook render as
    /// single canonical buckets (no compile-gateable shading paths today).
    /// The dedicated SKYBOX bucket (index 0) always stays present as the skybox
    /// writer, independent of any material.
    pub(crate) fn reconcile_material_variants(&mut self) -> Result<(), crate::error::AwsmError> {
        // ── Warm-path fast-out: ONE consuming bool check. Drives BOTH the
        //    PBR feature-set variant resolution below AND the
        //    `ensure_scene_pipelines` compile. Nothing iterates / allocates /
        //    builds a key before this gate — a no-change commit is a single
        //    `bool`.
        if !self.materials.take_variants_dirty() {
            return Ok(());
        }
        use awsm_renderer_materials::pbr::PbrFeatures;

        // ── PBR feature-set variant resolution. Only PBR specializes
        //    per feature-set; a custom-only / unlit-only scene has no
        //    `wants`, but the ensure pass below still runs.
        let mut wants: Vec<(crate::materials::MaterialKey, ShadingBase, u32)> = Vec::new();
        for (key, mat) in self.materials.iter_for_variant_reconcile() {
            if let crate::materials::Material::Pbr(m) = mat {
                wants.push((key, ShadingBase::Pbr, PbrFeatures::from_material(m).bits()));
            }
        }
        if !wants.is_empty() {
            // Resolve to variant ids (allocates new buckets on first sight).
            // Exceeding the bucket cap is a HARD ERROR (no silent fallback):
            // a material rendered with the wrong bucket is far harder to
            // debug than a loud failure. The error names the cap + the fix
            // (raise MAX_BUCKET_WORDS) and propagates out of the render loop.
            let mut resolved: Vec<(crate::materials::MaterialKey, MaterialShaderId)> =
                Vec::with_capacity(wants.len());
            let cap = self.dynamic_materials.max_bucket_entries();
            for (key, base, features) in wants {
                let id = self
                    .dynamic_materials
                    .resolve_first_party_variant_or_cap_err(base, features, cap)?;
                resolved.push((key, id));
            }
            // Stamp the resolved id into each material's payload (re-packs
            // only when the id actually changed) so classify routes those
            // pixels to the variant bucket on this same frame.
            for (key, id) in &resolved {
                self.materials.set_resolved_shader_id(
                    *key,
                    *id,
                    &self.textures,
                    &self.dynamic_materials,
                    &self.extras_pool,
                );
            }
        }

        // ── Render-driven compile of exactly what the live scene needs at
        //    the active AA config. Handles bucket-SET changes (resize
        //    buffers + clear stale caches BEFORE compiling) internally.
        //    The same dirty read that gated this method gates the compile —
        //    one flag, both consumers, no double-consume.
        self.ensure_scene_pipelines()?;
        Ok(())
    }

    // (Removed: the public `compile_material_variants` (= reconcile + wait).
    // Its job IS `commit_load` for materials — every former caller now goes
    // through the one compile path.)

    /// Internal helper: build a `MaterialDef` for a freshly-registered
    /// dynamic material and submit it to the scheduler. Idempotent —
    /// a duplicate submit for the same shader_id just adds a second
    /// scheduler entry (which the prewarm bridge marks Ready
    /// alongside the first). Kept private; the public surfaces are
    /// `register_material` and `submit_dynamic_material`.
    pub(crate) fn submit_to_scheduler_for_shader_id(
        &mut self,
        shader_id: awsm_renderer_materials::MaterialShaderId,
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
        // materials — without an entry, the SKYBOX bucket + the
        // PBR/Unlit/Toon/Flipbook canonical buckets would never recompile.
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
        // The masked pool may hold a now-stale variant for this id; flag a
        // rebuild so the next finalize clears + recompiles the live set.
        self.masked_dynamic_dirty = true;
        // Flag the variant reconcile so THIS delete (not just the next register)
        // drives `ensure_scene_pipelines` → `relayout_bucket_buffers` on the next
        // render. Without it the bucket-SET shrank but no reconcile ran, so the
        // deleted material's per-bucket opaque/edge pipelines lingered in the typed
        // caches until some later edit happened to mark variants dirty — the
        // pipeline-leak residual (it never self-cleaned under delete-only or
        // faster-than-compile churn). Mirrors `register_material`'s dirty flag.
        self.materials.mark_variants_dirty();
        let dropped = self.extras_pool.drop_shader(shader_id);
        if dropped > 0 {
            tracing::debug!(
                target: "awsm_renderer::extras_pool",
                "unregister_material({shader_id:?}): reclaimed {dropped} extras-pool slice(s)",
            );
        }
        // Retire the material's scheduler group. Without this the group
        // lingers after its registration is gone — but a group whose
        // registration was just removed has nothing to compile, so it
        // could never reach `Ready` and the compile-status modal would
        // hang forever ("Compiling N pipeline…"). This is exactly the
        // hot-reload cleanup `drop_material_group` exists for (e.g. a
        // material-editor starter switch). In-flight compiles for the
        // dropped id resolve and self-discard via the
        // generation/existence check.
        if let Some(mid) = self
            .pipeline_scheduler
            .find_material_by_shader_id(shader_id)
        {
            self.pipeline_scheduler.drop_material_group(mid);
        }

        // NOTE: the deleted material's compiled GPU pipelines + shader modules are
        // reclaimed by `relayout_bucket_buffers` at the next `commit_load`, NOT
        // here. The remove() below drops it from the registry, which changes
        // `dispatch_hash` + the bucket count → the next commit's
        // `ensure_scene_pipelines` detects the bucket-SET change, clears the
        // layout-keyed typed caches, and sweeps the shader + compute-pipeline
        // pools for every entry whose cache key carries the now-stale set
        // signature (the deleted material's opaque / edge / classify variants
        // among them). Doing the sweep there — after the typed caches are cleared
        // — is what keeps it free of dangling pool references. This is the
        // pipeline-leak fix ("aw snap" crash).
        self.dynamic_materials.remove(shader_id)
    }

    /// True if a tracked material can be (re)compiled: a canonical
    /// first-party id (PBR / Unlit / Toon / Flipbook — never dynamic), a
    /// live custom registration, or a known first-party feature-set
    /// variant. A dynamic id matching none of these is an **orphan** — its
    /// registration was removed but its scheduler group still lingers, with
    /// nothing to compile for it.
    ///
    /// Used by `ensure_bucket_pipelines` to short-circuit (mark the
    /// orphan group Ready so it can't hang the compile-status surface)
    /// instead of attempting a compile that has no source.
    pub(crate) fn is_launchable_material(&self, shader_id: MaterialShaderId) -> bool {
        !shader_id.is_dynamic()
            || self.dynamic_materials.get(shader_id).is_some()
            || self
                .dynamic_materials
                .first_party_variant_of(shader_id)
                .is_some()
    }

    /// Returns the registration record for a previously-registered id.
    pub fn dynamic_material_registration(
        &self,
        shader_id: MaterialShaderId,
    ) -> Option<&MaterialRegistration> {
        self.dynamic_materials.get(shader_id)
    }

    /// Overwrite one uniform's authored default on a registered dynamic
    /// material — see [`DynamicMaterials::set_uniform_default`].
    pub fn set_dynamic_material_uniform_default(
        &mut self,
        shader_id: MaterialShaderId,
        index: usize,
        value: awsm_renderer_materials::dynamic_layout::UniformValue,
    ) -> bool {
        self.dynamic_materials
            .set_uniform_default(shader_id, index, value)
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
    /// **Readiness flow**: `register_material` submits the scheduler group +
    /// flags the reconcile. The embedder's next [`AwsmRenderer::commit_load`]
    /// runs [`Self::ensure_scene_pipelines`], which pushes the compile promises
    /// for every pipeline the material needs — opaque, classify, per-shader
    /// edge_resolve, skybox edge_resolve, final_blend — into the scheduler's
    /// `inflight_compile` set, charged to this material's group.
    /// `commit_load`'s compile drain marks the material `Ready` when its last
    /// sub-pipeline resolves (an awaited `commit_load` blocks until then; a
    /// non-awaited one lands it over frames as [`Self::poll_pipeline_scheduler`]
    /// — still run each render-frame preamble — drains the resolved futures).
    /// Frontends that just want a progress signal subscribe to
    /// [`Self::drain_pipeline_status_events`] and render fall-back content
    /// (loading modal / placeholder mesh) until Ready arrives.
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
            shader_includes: awsm_renderer_materials::ShaderIncludes::all(),
            fragment_inputs: awsm_renderer_materials::FragmentInputs::all(),
            alpha_wgsl: None,
            wgsl_vertex: None,
        }
    }

    fn first_party_len() -> usize {
        first_party_bucket_entries().len()
    }

    /// Assembles the geometry custom-vertex shader for a representative
    /// custom-vertex material (one uniform → non-degenerate `MaterialData`; a
    /// non-trivial animated displacement body that reads `input.position`,
    /// `input.normal`, `input.tangent` + `input.globals.time`) and asserts naga
    /// reports NO errors on the assembled module. This is the first point the
    /// custom-vertex WGSL is actually rendered + validated (no GPU needed).
    #[cfg(feature = "dynamic-material-validation")]
    #[test]
    fn custom_vertex_geometry_assembles_and_naga_validates() {
        use awsm_renderer_materials::dynamic_layout::{FieldType, UniformFieldRuntime};

        let mut dm = DynamicMaterials::new();
        let mut r = reg("displacer", 1, 1);
        // A real uniform so the generated `MaterialData` isn't degenerate.
        r.layout.uniforms.push(UniformFieldRuntime {
            name: "amplitude".to_string(),
            ty: FieldType::F32,
        });
        // Non-trivial animated displacement: reads position/normal/tangent +
        // globals.time, returns the hook's `VertexDisplaceOutput(pos, n, t)`.
        r.wgsl_vertex = Some(
            "return VertexDisplaceOutput(\
                 input.position + input.normal * (0.1 * input.material.amplitude) \
                 * sin(input.globals.time + input.position.x * 4.0), \
                 input.normal, input.tangent);"
                .to_string(),
        );
        let id = dm.insert(r).unwrap();

        let errors = dm.validate_dynamic_vertex_wgsl(id);
        assert!(
            errors.is_empty(),
            "custom-vertex geometry shader failed naga validation:\n{}",
            errors.join("\n")
        );
    }

    /// MULTI-UV + MULTI-TEXTURE in the VERTEX stage: registers a custom-vertex
    /// material whose displacement body reads a SECOND UV set (`input.uv[1]`)
    /// AND samples a declared texture with it
    /// (`material_sample_albedo(input.material, input.uv[1])`), proving the
    /// hook exposes ALL of the mesh's UV sets (not just uv0) and that the
    /// generated per-texture `material_sample_<name>` helper resolves in the
    /// vertex assemble — full parity with the fragment side. A green naga
    /// result guarantees the multi-uv array + multi-texture sampling compile.
    #[cfg(feature = "dynamic-material-validation")]
    #[test]
    fn custom_vertex_geometry_multi_uv_and_texture_naga_validates() {
        use awsm_renderer_materials::dynamic_layout::{
            FieldType, TextureSlotRuntime, UniformFieldRuntime,
        };

        let mut dm = DynamicMaterials::new();
        let mut r = reg("displacer_multi_uv", 1, 1);
        r.layout.uniforms.push(UniformFieldRuntime {
            name: "amplitude".to_string(),
            ty: FieldType::F32,
        });
        // A declared texture → generates `material_sample_albedo`.
        r.layout.textures.push(TextureSlotRuntime {
            name: "albedo".to_string(),
            srgb: true,
            mipmap_kind: awsm_renderer_core::texture::mipmap::MipmapTextureKind::Albedo,
        });
        // Displace along the normal by a height sampled from `albedo` using the
        // SECOND UV set, then recompute the normal from neighbouring heights via
        // the shared `recompute_normal_from_height` helper — exercising multi-uv,
        // multi-texture, AND the normal-from-height helper in one body.
        r.wgsl_vertex = Some(
            "let h = material_sample_albedo(input.material, input.uv[1]).r; \
             let h_du = material_sample_albedo(input.material, input.uv[1] + vec2<f32>(0.01, 0.0)).r; \
             let h_dv = material_sample_albedo(input.material, input.uv[1] + vec2<f32>(0.0, 0.01)).r; \
             let n = recompute_normal_from_height(input.normal, input.tangent, h, h_du, h_dv, 0.01, input.material.amplitude); \
             return VertexDisplaceOutput(\
                 input.position + input.normal * (h * input.material.amplitude), \
                 n, input.tangent);"
                .to_string(),
        );
        let id = dm.insert(r).unwrap();

        let errors = dm.validate_dynamic_vertex_wgsl(id);
        assert!(
            errors.is_empty(),
            "multi-uv + multi-texture custom-vertex geometry shader failed naga validation:\n{}",
            errors.join("\n")
        );
    }

    /// Companion to `custom_vertex_geometry_assembles_and_naga_validates` for the
    /// SHADOW pass: registers the same animated custom-vertex material and asserts
    /// the assembled custom-vertex SHADOW module (depth-only, augmented shadow
    /// bind groups + the displacement hook) naga-validates. This is the point the
    /// shadow custom-vertex WGSL is actually rendered + validated (no GPU). A green
    /// result here guarantees the displaced shadow compiles — same body, same hook
    /// inputs as the geometry pass, so the silhouette matches.
    #[cfg(feature = "dynamic-material-validation")]
    #[test]
    fn custom_vertex_shadow_assembles_and_naga_validates() {
        use awsm_renderer_materials::dynamic_layout::{FieldType, UniformFieldRuntime};

        let mut dm = DynamicMaterials::new();
        let mut r = reg("displacer_shadow", 1, 1);
        r.layout.uniforms.push(UniformFieldRuntime {
            name: "amplitude".to_string(),
            ty: FieldType::F32,
        });
        // The SAME displacement body the geometry test uses — so the geometry and
        // shadow passes run byte-identical displacement.
        r.wgsl_vertex = Some(
            "return VertexDisplaceOutput(\
                 input.position + input.normal * (0.1 * input.material.amplitude) \
                 * sin(input.globals.time + input.position.x * 4.0), \
                 input.normal, input.tangent);"
                .to_string(),
        );
        let id = dm.insert(r).unwrap();

        let errors = dm.validate_dynamic_vertex_shadow_wgsl(id);
        assert!(
            errors.is_empty(),
            "custom-vertex shadow shader failed naga validation:\n{}",
            errors.join("\n")
        );
    }

    /// Companion to `custom_vertex_geometry_assembles_and_naga_validates` for the
    /// TRANSPARENT pass: registers an alpha-blend custom material that displaces
    /// its vertices via the same `custom_displace_vertex` hook (referencing
    /// `input.uv`, `input.material.<field>`, `input.globals.time`) AND carries a
    /// transparent fragment body, then asserts the assembled transparent
    /// custom-vertex module (the per-material `base == Custom` template + the
    /// displacement hook) naga-validates. A green result guarantees a transparent
    /// material with a vertex body compiles its hook against the transparent
    /// contract (real per-mesh uv0; `materials` + texture pool VERTEX-visible).
    #[cfg(feature = "dynamic-material-validation")]
    #[test]
    fn transparent_custom_vertex_assembles_and_naga_validates() {
        use awsm_renderer_materials::dynamic_layout::{FieldType, UniformFieldRuntime};

        let mut dm = DynamicMaterials::new();
        let mut r = reg("displacer_transparent", 1, 1);
        // Alpha-blend so this is a genuine transparent-pass material.
        r.alpha_mode = MaterialAlphaMode::Blend;
        // A real uniform so the generated `MaterialData` isn't degenerate.
        r.layout.uniforms.push(UniformFieldRuntime {
            name: "amplitude".to_string(),
            ty: FieldType::F32,
        });
        // The SAME displacement body the geometry test uses, plus a read of the
        // transparent-only `input.uv[0]` (real per-mesh uv0 on this pass) — so
        // the geometry and transparent passes run the identical hook.
        r.wgsl_vertex = Some(
            "return VertexDisplaceOutput(\
                 input.position + input.normal * (0.1 * input.material.amplitude) \
                 * sin(input.globals.time + input.position.x * 4.0 + input.uv[0].x), \
                 input.normal, input.tangent);"
                .to_string(),
        );
        // A transparent material is `base == Custom`; the fragment template
        // references `MaterialData` inside the `base == Custom` wrapper, so a
        // non-empty fragment body is required (empty → missing return).
        r.wgsl_fragment =
            "return TransparentShadingOutput(vec4<f32>(input.world_normal * 0.5 + 0.5, 0.5));"
                .to_string();
        let id = dm.insert(r).unwrap();

        let errors = dm.validate_dynamic_vertex_transparent_wgsl(id);
        assert!(
            errors.is_empty(),
            "transparent custom-vertex shader failed naga validation:\n{}",
            errors.join("\n")
        );
    }

    /// Combined masked + custom-vertex GEOMETRY: a material that is BOTH
    /// `alphaMode = MASK` (with a custom alpha-only cutout body) AND carries a
    /// `wgsl_vertex` displacement body must assemble a single module where the
    /// displaced silhouette is also alpha-tested. The shared `material_load_*` /
    /// `texture_pool_sample` helpers come from the fragment's masked_alpha; the
    /// vertex hook's `material_data_load` + the fragment alpha path resolve
    /// against that single copy — so the previously-conflicting masked + custom-
    /// vertex helper definitions no longer collide. A green naga result proves the
    /// combine is redefinition-free AND the assembled module is valid.
    #[cfg(feature = "dynamic-material-validation")]
    #[test]
    fn masked_custom_vertex_geometry_assembles_and_naga_validates() {
        use awsm_renderer_materials::dynamic_layout::{FieldType, UniformFieldRuntime};

        let mut dm = DynamicMaterials::new();
        let mut r = reg("displacer_masked", 1, 1);
        // MASK with a custom alpha-only cutout body → the combined Custom alpha
        // path (the fragment reuses the vertex hook's MaterialData struct/loader).
        r.alpha_mode = MaterialAlphaMode::Mask { cutoff: 0.5 };
        r.alpha_wgsl = Some("return input.material.amplitude;".to_string());
        r.layout.uniforms.push(UniformFieldRuntime {
            name: "amplitude".to_string(),
            ty: FieldType::F32,
        });
        // The SAME animated displacement body the geometry custom-vertex test uses.
        r.wgsl_vertex = Some(
            "return VertexDisplaceOutput(\
                 input.position + input.normal * (0.1 * input.material.amplitude) \
                 * sin(input.globals.time + input.position.x * 4.0), \
                 input.normal, input.tangent);"
                .to_string(),
        );
        let id = dm.insert(r).unwrap();

        let errors = dm.validate_dynamic_masked_vertex_wgsl(id);
        assert!(
            errors.is_empty(),
            "combined masked + custom-vertex geometry shader failed naga validation:\n{}",
            errors.join("\n")
        );
    }

    /// Combined masked + custom-vertex SHADOW: companion to
    /// `masked_custom_vertex_geometry_assembles_and_naga_validates` for the shadow
    /// pass — the displaced silhouette is ALSO cut out (depth-only alpha-test). A
    /// green naga result guarantees a Mask + custom-vertex caster compiles its
    /// combined shadow module (displaced AND hole-shaped), the shadow analog of
    /// the geometry combine.
    #[cfg(feature = "dynamic-material-validation")]
    #[test]
    fn masked_custom_vertex_shadow_assembles_and_naga_validates() {
        use awsm_renderer_materials::dynamic_layout::{FieldType, UniformFieldRuntime};

        let mut dm = DynamicMaterials::new();
        let mut r = reg("displacer_masked_shadow", 1, 1);
        r.alpha_mode = MaterialAlphaMode::Mask { cutoff: 0.5 };
        r.alpha_wgsl = Some("return input.material.amplitude;".to_string());
        r.layout.uniforms.push(UniformFieldRuntime {
            name: "amplitude".to_string(),
            ty: FieldType::F32,
        });
        r.wgsl_vertex = Some(
            "return VertexDisplaceOutput(\
                 input.position + input.normal * (0.1 * input.material.amplitude) \
                 * sin(input.globals.time + input.position.x * 4.0), \
                 input.normal, input.tangent);"
                .to_string(),
        );
        let id = dm.insert(r).unwrap();

        let errors = dm.validate_dynamic_masked_vertex_shadow_wgsl(id);
        assert!(
            errors.is_empty(),
            "combined masked + custom-vertex shadow shader failed naga validation:\n{}",
            errors.join("\n")
        );
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
    fn shader_include_flags_from_declared_set() {
        use awsm_renderer_materials::ShaderIncludes as S;
        // A textures-only custom material pulls in none of the PBR shading
        // modules — a genuinely skinnier Custom host shader.
        let f = ShaderIncludeFlags::from_includes(S::TEXTURES);
        assert!(!f.brdf);
        assert!(!f.apply_lighting);
        assert!(!f.material_color_calc);
        // Declaring the PBR material-color builder turns that gate on.
        let f = ShaderIncludeFlags::from_includes(S::MATERIAL_COLOR_CALC);
        assert!(f.material_color_calc);
        // APPLY_LIGHTING transitively resolves to BRDF (the explicit Tier-B
        // constants still map — first-party PBR declares them this way).
        let f = ShaderIncludeFlags::from_includes(S::APPLY_LIGHTING);
        assert!(f.brdf);
        assert!(f.apply_lighting);
        // Phase 3: `all()` is now Tier-A-only (generic helpers), so it lights
        // NONE of the Tier-B PBR-internal gates. A custom material declaring
        // `all()` no longer drags in the PBR shading stack.
        let f = ShaderIncludeFlags::from_includes(S::all());
        assert!(!f.brdf && !f.apply_lighting && !f.material_color_calc);
    }

    #[test]
    fn dispatch_hash_reacts_to_declared_includes() {
        // Two registries with the same single material but different declared
        // include sets must produce different dispatch hashes, so narrowing a
        // material's Pass Dependencies re-keys (re-specializes) its pipeline.
        let mut full = DynamicMaterials::new();
        let mut r1 = reg("a", 1, 1);
        r1.shader_includes = awsm_renderer_materials::ShaderIncludes::all();
        full.insert(r1).unwrap();

        let mut skinny = DynamicMaterials::new();
        let mut r2 = reg("a", 1, 1);
        r2.shader_includes = awsm_renderer_materials::ShaderIncludes::TEXTURES;
        skinny.insert(r2).unwrap();

        assert_ne!(
            full.dispatch_hash_cached(),
            skinny.dispatch_hash_cached(),
            "narrowing a custom material's declared includes must re-key its pipeline cache"
        );
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
    fn configurable_cap_admits_up_to_and_rejects_past_the_configured_ceiling() {
        // Default is the historical 32.
        let dm = DynamicMaterials::new();
        assert_eq!(dm.max_bucket_entries(), MAX_BUCKET_ENTRIES);

        // Raise the cap and confirm the registry fills to exactly the new
        // ceiling, then hard-errors on the next genuinely-new variant. The
        // runtime cap-check sites read `max_bucket_entries()`, so this test
        // drives them through the same accessor.
        let cap = 100usize;
        let mut dm = DynamicMaterials::new();
        dm.set_max_bucket_entries(cap as u32);
        assert_eq!(dm.max_bucket_entries(), cap);

        let configured = dm.max_bucket_entries();
        let mut i = 0u32;
        while dm.bucket_entries_cached().len() < cap {
            dm.resolve_first_party_variant_or_cap_err(ShadingBase::Pbr, i, configured)
                .expect("should resolve below the configured cap");
            i += 1;
        }
        assert_eq!(dm.bucket_entries_cached().len(), cap);

        let err = dm
            .resolve_first_party_variant_or_cap_err(ShadingBase::Pbr, 999_999, configured)
            .expect_err("a new variant past the configured cap must error");
        assert!(matches!(
            err,
            AwsmDynamicMaterialError::BucketCapExceeded { max, .. } if max == cap
        ));
        assert_eq!(dm.bucket_entries_cached().len(), cap);
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
        // Index 0 is the dedicated SKYBOX bucket (classify routes sky pixels to
        // bit 0); the canonical PBR bucket follows at index 1.
        assert_eq!(entries[0].shader_id, MaterialShaderId::SKYBOX);
        assert_eq!(entries[0].name, "skybox");
        assert_eq!(entries[1].shader_id, MaterialShaderId::PBR);
        assert_eq!(entries[1].base, ShadingBase::Pbr);
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
