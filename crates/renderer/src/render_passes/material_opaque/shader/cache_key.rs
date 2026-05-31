//! Shader cache keys for the opaque material pass.

use awsm_materials::MaterialShaderId;

use crate::{
    dynamic_materials::{BucketEntry, ShadingBase},
    render_passes::shader_cache_key::ShaderCacheKeyRenderPass,
    shaders::ShaderCacheKey,
};

/// Cache key for opaque material shaders.
///
/// The opaque pass keys per `(MsaaConfig, mipmaps, shader_id)`. Each
/// variant lives in its own compute pipeline so the runtime `if
/// (shader_id == PBR) …` branch becomes a static `{% match shader_id %}`
/// template choice.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialOpaque {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub msaa_sample_count: Option<u32>,
    pub mipmaps: bool,
    pub shader_id: MaterialShaderId,
    /// Which built-in shading family this bucket's template body comes
    /// from. Decoupled from `shader_id` so a per-feature-set PBR variant
    /// (id in the dynamic range) still selects the PBR body. See
    /// [`ShadingBase`].
    pub base: ShadingBase,
    /// True for exactly one bucket — the canonical PBR bucket
    /// (`MaterialShaderId::PBR`, index 0) — which writes the skybox on
    /// skybox/uncovered pixels. classify routes skybox pixels to bit 0,
    /// so only that bucket's dispatch covers skybox-only tiles. Every
    /// other bucket (incl. specialized PBR variants, all `base == Pbr`)
    /// leaves `false` and returns without writing on skybox pixels, so a
    /// mixed tile's skybox pixels aren't double-written / raced.
    pub owns_skybox: bool,
    /// Opaque PBR feature mask ([`awsm_materials::pbr::PbrFeatures::bits`])
    /// the specialized PBR shader is compiled for (Phase B.2). Two PBR
    /// pipelines with different feature masks are distinct entries, so a
    /// scene that uses no clearcoat compiles a clearcoat-free shader.
    /// Only meaningful when `shader_id == PBR`; carried as
    /// `PbrFeatures::all().bits()` (the uber config) for every other
    /// shader_id, where it's inert.
    pub pbr_features: u32,
    /// Stable hash over the currently-registered dynamic-material set
    /// (sorted by shader_id, then `(name, layout_hash, wgsl_hash)` per
    /// entry).
    ///
    /// **Returns `0` when no dynamic materials are registered**, which
    /// is the stable empty-state sentinel — the cache key's hash is
    /// bit-identical to the pre-dynamic-material build, so first-party
    /// pipelines compile to the same WGSL they did before this feature
    /// shipped. Registering / unregistering a dynamic material changes
    /// `dispatch_hash`, invalidates affected pipelines on next render,
    /// and triggers a recompile.
    ///
    /// See `awsm_renderer::dynamic_materials::DynamicMaterials::dispatch_hash`
    /// for the hashing details.
    pub dispatch_hash: u64,
    /// `Some` when `shader_id.is_dynamic()`: carries the registered
    /// material's WGSL fragment + the auto-generated `MaterialData`
    /// struct declaration so the opaque-compute template can emit the
    /// wrapped `custom_shade_<id>` function + matching dispatch arm.
    /// `None` for first-party ids — those are still handled by the
    /// hand-rolled `{% if shader_id == ... %}` arms in compute.wgsl.
    pub dynamic_shader: Option<DynamicShaderInfo>,
    /// Full registry bucket list — needed to template the read-only
    /// `ClassifyBuckets` struct in `bind_groups.wgsl` AND the
    /// per-shader-id `bucket_offset` lookup in `compute.wgsl`. The
    /// byte layout of `ClassifyBuckets` here MUST match the
    /// classify-pass-side struct (which is also templated from the
    /// same `bucket_entries`) so the read view aligns with the
    /// write view byte-for-byte.
    pub bucket_entries: Vec<BucketEntry>,
}

/// Per-dynamic-material info embedded in the opaque cache key so the
/// template emission can wrap the author's WGSL into a
/// `fn custom_shade_<id>(...)` and dispatch to it from the kernel.
///
/// Hashed by `(shader_id, layout_hash, wgsl_hash)` — the field names
/// and bodies are recomputed from the layout / WGSL at
/// template-render time, so two distinct registrations with
/// byte-identical hashes produce the same compiled WGSL.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct DynamicShaderInfo {
    /// The auto-generated `struct MaterialData` declaration (output
    /// of `dynamic_layout::generate_wgsl_struct`).
    pub struct_decl: String,
    /// The auto-generated `fn material_data_load(byte_offset: u32) ->
    /// MaterialData` accessor (output of
    /// `dynamic_layout::generate_wgsl_loader`). Reads the per-instance
    /// uniform / texture-index / buffer-offset values back out of the
    /// `materials: array<u32>` storage buffer at exactly the byte
    /// offsets `pack_uniform_values` wrote.
    pub loader_decl: String,
    /// The author's WGSL fragment, verbatim. Wrapped at template-
    /// render time into `fn custom_shade_dynamic(input: OpaqueShadingInput)
    /// -> OpaqueShadingOutput { <fragment> }`. The wrapper populates
    /// `input.material` by calling `material_data_load(input.material_offset)`
    /// before invoking the author's body.
    pub wgsl_fragment: String,
}

impl From<ShaderCacheKeyMaterialOpaque> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialOpaque) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialOpaque(key))
    }
}

/// Cache key for the opaque pass when no geometry is rendered.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialOpaqueEmpty {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub msaa_sample_count: Option<u32>,
}

impl From<ShaderCacheKeyMaterialOpaqueEmpty> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialOpaqueEmpty) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialOpaqueEmpty(key))
    }
}
