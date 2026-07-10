//! SSR shader cache keys.
//!
//! The cache key is the permutation IDENTITY: every
//! field that changes the compiled WGSL text lives here, so each distinct
//! variant compiles once and only the variants actually enabled are compiled.
//! The scalar tuning knobs (`intensity`, `thickness`, …) are LIVE uniforms and
//! deliberately DO NOT appear here — tweaking them never recompiles.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Reflection lobe model. `Mirror` compiles a tight single-ray path (no
/// spread / importance-sampling / denoise code); `Glossy` compiles the
/// GGX-importance-sampled + descriptor-fetch + denoise path.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsrMode {
    Mirror,
    Glossy,
}

/// Ray-march strategy. `LinearDda` is the M1 / `gpu_culling`-off fallback;
/// `HiZ` marches the dedicated min-Z pyramid (M2 accelerator).
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsrTrace {
    LinearDda,
    HiZ,
}

impl SsrTrace {
    /// The trace strategy compiled into the PRODUCTION SSR pipeline.
    ///
    /// `LinearDda` (M1, verified pixel-perfect) is the production default. `HiZ`
    /// (M2c) is compilable, naga-validated, correctly WIRED (the boot-time
    /// pyramid-view bug is fixed — see render_passes.rs / render.rs), and its
    /// lod-0 / refinement path is pixel-perfect. What is NOT production-ready is
    /// the COARSE-MIP TRAVERSAL: a decisive A/B (capping `max_lod` to 0 in
    /// trace.wgsl removes all banding; any `max_lod > 0` reintroduces horizontal
    /// bands) localized the defect to the tile-skip advance. The ray advances a
    /// FRACTION of a tile (`dt_cell * 0.5`) instead of stepping to the actual
    /// screen-space cell boundary, so the refinement bracket `[t_prev, t]` lands
    /// at inconsistent sub-cell positions across scanlines → banding. The
    /// canonical fix is a perspective-correct screen-space DDA that crosses to
    /// exact cell boundaries (McGuire & Mara 2014; Uludağ Hi-Z SSR), NOT a
    /// fractional advance — and it is coupled to the deferred reverse-Z work,
    /// because forward-Z's far-plane precision starvation quantizes the coarse
    /// min-Z tiles and amplifies the banding (see docs/plans/reverse-z.md §"SSR
    /// Hi-Z min-Z pyramid"). Since Hi-Z is a pure ACCELERATOR — when correct it
    /// yields the SAME image as DDA — and DDA is sufficient for target scenes,
    /// DDA ships and the full Hi-Z path stays in place for that future work.
    /// Single source of truth shared by `SsrPipelines::m1_key` + `SsrBindGroups`
    /// so the layout can never drift from the shader.
    pub const PRODUCTION: SsrTrace = SsrTrace::LinearDda;

    /// Whether this variant marches the min-Z pyramid (adds the Hi-Z binding).
    pub const fn is_hiz(self) -> bool {
        matches!(self, SsrTrace::HiZ)
    }
}

/// Cache key for the SSR trace shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeySsr {
    pub mode: SsrMode,
    pub trace: SsrTrace,
    /// Temporal reproject + neighbourhood-clamp code exists only when true.
    pub temporal: bool,
    /// Half-res trace → the guided-upsample variant.
    pub half_res: bool,
    /// Under MSAA the depth + `normal_tangent` G-buffer targets are
    /// multisampled, so the binding types + `textureLoad` change. (The HDR
    /// color source stays the resolved single-sample `transparent` target.)
    pub multisampled_geometry: bool,
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeySsr> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySsr) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Ssr(key))
    }
}
