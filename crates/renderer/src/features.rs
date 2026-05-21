//! Opt-in renderer features — plan §16.F.
//!
//! Two flags gate the always-on GPU-driven culling pipeline and the
//! projection-decals subsystem. Both default to `false` so library
//! consumers (editor / tools / 2D-with-some-3D) pay zero overhead for
//! features they don't use. Game-side and editor builds opt in
//! explicitly via [`AwsmRendererBuilder::with_features`].

/// Per-renderer feature gates picked at construction time.
///
/// Toggling a gate after `build()` requires a renderer rebuild — the
/// `Option`-shaped owning fields on `AwsmRenderer` (gated buffers /
/// textures / render passes) are populated once based on the active
/// feature set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RendererFeatures {
    /// Enable GPU-driven culling: HZB build, occlusion cull,
    /// `IndirectDrawArgs` compaction, and (once §16.7/§16.8 land) the
    /// `drawIndirect` geometry path. Required for the GPU pipeline to
    /// filter visible meshes on-device. At the 10K-mesh tier this is
    /// a 30–50% frame-time win; below ~500 meshes the always-on
    /// dispatch + per-frame CPU upload nets out to a small loss.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_features_disable_both_gates() {
        let features = RendererFeatures::default();
        assert!(
            !features.gpu_culling,
            "gpu_culling must default to false so library consumers pay no cost"
        );
        assert!(
            !features.decals,
            "decals must default to false so library consumers pay no cost"
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
