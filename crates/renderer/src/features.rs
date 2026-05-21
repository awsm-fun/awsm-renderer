//! Opt-in renderer features picked at construction time.
//!
//! Flags gate clusters of always-on infrastructure that not every
//! library consumer needs. Each defaults to `false`, so library
//! consumers (tools / 2D-with-some-3D / minimal viewers) pay zero
//! overhead for features they don't use. Game-side and editor builds
//! opt in explicitly via [`AwsmRendererBuilder::with_features`].

/// Per-renderer feature gates picked at construction time.
///
/// Toggling a gate after `build()` requires a renderer rebuild — the
/// `Option`-shaped owning fields on `AwsmRenderer` (gated buffers /
/// textures / render passes) are populated once based on the active
/// feature set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RendererFeatures {
    /// Enable GPU-driven culling: HZB build, occlusion cull,
    /// `IndirectDrawArgs` compaction, and the `drawIndirect` geometry
    /// path. Required for the GPU pipeline to filter visible meshes
    /// on-device. At the 10K-mesh tier this is a 30–50% frame-time
    /// win; below ~500 meshes the always-on dispatch + per-frame CPU
    /// upload nets out to a small loss. The adaptive
    /// [`RendererOptimizationPolicy`] (default `Auto`) handles that
    /// trade automatically, so keeping this `true` is the right
    /// default for editors and games once any mesh batching ramps up.
    ///
    /// [`RendererOptimizationPolicy`]: crate::optimization_policy::RendererOptimizationPolicy
    pub gpu_culling: bool,

    /// Enable projection decals. Allocates `decal_color` (~16 MB at
    /// 4K) + `decal_classify_buffers` (~17 MB at 4K) up-front and
    /// dispatches the classify + decal compute + composite passes
    /// whenever `Decals::len() > 0`. When `false`,
    /// `insert_decal()` returns [`AwsmDecalError::FeatureNotEnabled`]
    /// and none of the decal resources are allocated.
    ///
    /// [`AwsmDecalError::FeatureNotEnabled`]: crate::decals::AwsmDecalError::FeatureNotEnabled
    pub decals: bool,

    /// Enable the GPU per-mesh pixel-coverage producer that feeds the
    /// CPU [`MeshCoverage`] table via an async readback. Consumers of
    /// that table (skin-skip, cheap-material LOD) are currently
    /// parked, so the producer pays for nothing in the default case —
    /// hence opt-in. When `false`, [`MeshCoverage::is_below_threshold`]
    /// returns `false` for every mesh, which means any consumer falls
    /// back to its "above threshold / use the expensive variant" path.
    ///
    /// Flip on if you're wiring up your own consumer (or finishing
    /// the parked ones). Allocates a counts buffer (`4 B × mesh
    /// slot count`, grow-by-2) + a same-sized CPU-mappable readback
    /// buffer; per-frame cost is one compute dispatch at the
    /// visibility resolution plus one `copyBufferToBuffer` and a
    /// `mapAsync` round-trip on a future frame.
    ///
    /// [`MeshCoverage`]: crate::coverage::MeshCoverage
    /// [`MeshCoverage::is_below_threshold`]: crate::coverage::MeshCoverage::is_below_threshold
    pub coverage_lod: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_features_are_all_off() {
        let features = RendererFeatures::default();
        assert!(
            !features.gpu_culling,
            "gpu_culling must default to false so library consumers pay no cost"
        );
        assert!(
            !features.decals,
            "decals must default to false so library consumers pay no cost"
        );
        assert!(
            !features.coverage_lod,
            "coverage_lod must default to false so library consumers pay no cost"
        );
    }

    #[test]
    fn features_clone_independently() {
        let mut a = RendererFeatures::default();
        let b = a.clone();
        a.gpu_culling = true;
        assert_ne!(a, b);
        assert!(!b.gpu_culling);
    }
}
