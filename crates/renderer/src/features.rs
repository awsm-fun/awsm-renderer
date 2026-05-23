//! Opt-in renderer features picked at construction time.
//!
//! Flags gate clusters of always-on infrastructure that not every
//! library consumer needs. Each defaults to `false`, so library
//! consumers (tools / 2D-with-some-3D / minimal viewers) pay zero
//! overhead for features they don't use. Game-side and editor builds
//! opt in explicitly via [`AwsmRendererBuilder::with_features`].

/// Tri-state toggle for renderer capabilities whose availability
/// depends on hardware / browser support.
///
/// - `Auto` (default): capability-detect at device creation; the
///   builder probes the adapter and resolves to true/false.
/// - `On`: force-enable, asserting the path is supported. Bypasses
///   detection. Use when you have out-of-band knowledge that the
///   device supports it (or to bisect adapter-detection bugs).
/// - `Off`: force-disable, opting into the portable fallback path
///   even on devices that support the optimized path. Use to test
///   the fallback path on a supported device, or to side-step a
///   device-driver bug in the optimized path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FeatureToggle {
    /// Capability-detect at device creation time.
    #[default]
    Auto,
    /// Force-enable.
    On,
    /// Force-disable.
    Off,
}

impl FeatureToggle {
    /// Resolves the toggle against a runtime capability probe.
    ///
    /// `Auto` falls through to `capability`. `On` returns `true`
    /// regardless. `Off` returns `false` regardless. The resolved
    /// boolean is what the renderer's allocation and pipeline-
    /// selection logic actually consults.
    pub fn resolve(self, capability: bool) -> bool {
        match self {
            FeatureToggle::Auto => capability,
            FeatureToggle::On => true,
            FeatureToggle::Off => false,
        }
    }
}

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

    /// Whether to use the WebGPU `indirect-first-instance` feature for
    /// the non-instanced geometry pass's drawIndirect path.
    ///
    /// When **enabled**, the compaction shader writes the per-mesh
    /// slot index into `IndirectDrawArgs.first_instance`, and the
    /// vertex shader's `geometry_mesh_metas[instance_index]` storage-
    /// array lookup resolves to that slot. One shared bind group
    /// services every non-instanced draw — no per-draw `setBindGroup`.
    ///
    /// When **disabled** (portable fallback), the non-instanced path
    /// uses the same uniform-with-dynamic-offset binding the instanced
    /// path uses: the CPU calls `setBindGroup(2, ..., &[meta_offset])`
    /// per draw, the args buffer's `first_instance` stays at 0, and
    /// the storage-array binding is omitted from the shader. The GPU
    /// culling benefit (compaction setting `instance_count` to 0/1)
    /// is preserved — only the bind-group sharing is lost.
    ///
    /// Browser support is limited (Firefox: none; Chrome desktop:
    /// Linux-Intel only as of mid-2026). The default is `Auto`, which
    /// resolves to true on adapters that expose the feature and false
    /// on those that don't. Both paths are independently optimized;
    /// neither is a "degraded" mode.
    pub indirect_first_instance: FeatureToggle,
}

impl RendererFeatures {
    /// Reads the resolved value of [`Self::indirect_first_instance`].
    ///
    /// Only meaningful after the renderer builder has resolved `Auto`
    /// against the device's capability. Before resolution, `Auto`
    /// returns `false` from this helper — which means callers outside
    /// the `build()` flow see "feature off" until the resolution step
    /// has run. Inside the renderer the builder replaces `Auto` with
    /// `On` or `Off` early in `build()`, so all downstream reads land
    /// on a deterministic boolean.
    pub fn indirect_first_instance_enabled(&self) -> bool {
        self.indirect_first_instance.resolve(false)
    }
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
        assert_eq!(
            features.indirect_first_instance,
            FeatureToggle::Auto,
            "indirect_first_instance must default to Auto — capability detection at build time"
        );
    }

    #[test]
    fn feature_toggle_resolves_correctly() {
        assert!(FeatureToggle::Auto.resolve(true), "Auto follows capability");
        assert!(
            !FeatureToggle::Auto.resolve(false),
            "Auto follows capability"
        );
        assert!(FeatureToggle::On.resolve(true), "On ignores capability");
        assert!(FeatureToggle::On.resolve(false), "On ignores capability");
        assert!(!FeatureToggle::Off.resolve(true), "Off ignores capability");
        assert!(!FeatureToggle::Off.resolve(false), "Off ignores capability");
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
