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
    /// Plan B (stage 2b): whether the shared prep pass is enabled. The
    /// derived template bool `prep_read = prep_enabled &&
    /// msaa_sample_count.is_none()` gates whether `texture_uv()` /
    /// `vertex_color()` read the prep-materialized array textures
    /// (`prep_uv` / `prep_vcolor`) instead of recomputing from the
    /// geometry pool — so a prep-on vs prep-off pipeline are distinct
    /// cache entries. Only the no-MSAA primary reads prep (the MSAA edge
    /// kernel `cs_edge` needs per-sample data prep doesn't hold), so this
    /// is inert under MSAA but still carried accurately on the key.
    pub prep_enabled: bool,
    /// Plan B (stage 4): `K` — the clamped per-pixel shadow-caster cap
    /// (`PrepPassConfig::clamped_k`). Threaded onto the opaque key so the
    /// `shadow_from_buffer` read path's slot bounds-check (`slot >= K`) and the
    /// packed-layer index (`slot / 4`) match the prep buffer's K exactly. Inert
    /// when `prep_read` is false (the `{% if shadow_from_buffer %}` block never
    /// renders), but carried on the key so a K change still re-keys the
    /// pipeline. Default 4.
    pub max_shadow_casters: u32,
    pub shader_id: MaterialShaderId,
    /// Which built-in shading family this bucket's template body comes
    /// from. Decoupled from `shader_id` so a per-feature-set PBR variant
    /// (id in the dynamic range) still selects the PBR body. See
    /// [`ShadingBase`].
    pub base: ShadingBase,
    /// True for exactly one bucket — the dedicated SKYBOX bucket
    /// (`MaterialShaderId::SKYBOX`, index 0) — whose pipeline is the
    /// `skybox_primary` writer. classify routes every uncovered pixel to bit 0,
    /// so only that bucket's dispatch covers skybox-only tiles. Every material
    /// bucket leaves `false` (its kernel shades geometry only), so a mixed
    /// tile's skybox pixels aren't double-written / raced.
    pub owns_skybox: bool,
    /// Opaque PBR feature mask ([`awsm_materials::pbr::PbrFeatures::bits`])
    /// the specialized PBR shader is compiled for. Two PBR
    /// pipelines with different feature masks are distinct entries, so a
    /// scene that uses no clearcoat compiles a clearcoat-free shader.
    /// Only meaningful for PBR-family buckets; the empty set for non-PBR
    /// ids (inert — their body doesn't read it) and for the SKYBOX bucket
    /// (the minimal skybox-only shader). Never the full "uber" set —
    /// specialize-only compiles no all-features shader.
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
    /// Unified-edge (U1, `docs/plans/unified-edge-shading.md`). When `true`,
    /// the opaque module additionally emits the merged `cs_shade` entry point
    /// (interior sample-0 → opaque_tex + edge per-sample → accumulator) and
    /// the `edge_id_tex` group(3) binding it reads. `cs_opaque`/`cs_edge` are
    /// UNCHANGED (both paths coexist; the build-time toggle selects which is
    /// dispatched). `false` (default) ⇒ WGSL byte-identical to pre-U1.
    /// Threaded build-time from `AwsmRendererBuilder::with_unified_edge`.
    pub unified_edge: bool,
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
    /// Author-declared shared-module set (already transitively resolved via
    /// [`awsm_materials::ShaderIncludes::resolve`]) for this dynamic
    /// material. The Custom-base shading host gates its optional modules
    /// (BRDF / apply_lighting / material_color_calc) on this instead of the
    /// blanket `ShaderIncludes::all()`, so a material that declares less
    /// compiles a leaner shader. Defaults to the resolved `all()` set when
    /// the author hasn't narrowed it.
    pub shader_includes: awsm_materials::ShaderIncludes,
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
