//! Opt-in renderer features picked at construction time.
//!
//! Flags gate clusters of always-on infrastructure that not every
//! library consumer needs. Each defaults to `false`, so library
//! consumers (tools / 2D-with-some-3D / minimal viewers) pay zero
//! overhead for features they don't use. Game-side and editor builds
//! opt in explicitly via [`crate::AwsmRendererBuilder::with_features`].

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

    /// REVERSE-Z depth convention (docs/plans/003-reverse-z.md): near→1,
    /// far→0, float-depth precision near-uniform across the range — kills
    /// far-field z-fighting. Construction-time: pipelines, clears, HZB ops,
    /// frustum extraction, and shader sentinels all bake the convention via
    /// [`crate::depth_convention::DepthConvention`]. Shadows migrate in the
    /// stage-7 lockstep (they pin `DepthConvention::FORWARD` until then).
    pub reverse_z: bool,

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

    /// Enable GPU mesh-picking ([`AwsmRenderer::pick`]). When `false`
    /// (the default), `AwsmRenderer.picker` is `None`, the two
    /// picker compute pipelines never compile, the picker bind-group
    /// layouts aren't registered, and [`AwsmRenderer::pick`] returns
    /// [`PickResult::Disabled`]. Editor builds set this to `true`;
    /// game / library builds that don't need click-to-select pay
    /// zero cost.
    ///
    /// Picker has 2 compute shader variants (multisampled true/false) + 2 compute pipelines + 2 bind-group layouts. On warm-Metal that's a few task-ticks worth of work skipped at startup; on cold-Dawn it's one compile wave saved.
    ///
    /// [`AwsmRenderer::pick`]: crate::AwsmRenderer::pick
    /// [`PickResult::Disabled`]: crate::picker::PickResult::Disabled
    pub picking: bool,

    /// Enable discrete level-of-detail: load the per-mesh simplified level
    /// chain baked into the player bundle (`<id>.lod{N}.glb` + `<id>.lod.toml`)
    /// and select a level per instance by projected screen-space error. Each
    /// level is a separate `MeshKey`; the runtime reroutes an instance's draw to
    /// its selected level. When `false` (the default), no level geometry is
    /// loaded and every instance draws its base mesh — byte-identical to a build
    /// without LOD. Mirrors [`Self::gpu_culling`] as an opt-in GPU-pipeline gate.
    pub lod: bool,

    /// Enable cluster-LOD ("virtual geometry", Phase B) for static rigid meshes:
    /// load the baked cluster DAG, two-level cull + per-cluster LOD-cut selection
    /// on the GPU, and a single compacted indirect stream sharing the visibility
    /// buffer with discrete + skinned geometry. When `false` (the default), no
    /// cluster data is loaded and static meshes draw whole (discrete LOD still
    /// applies if `lod` is on) — byte-identical to a build without it. Mirrors
    /// [`Self::gpu_culling`] / [`Self::lod`] as an opt-in GPU-pipeline gate.
    pub virtual_geometry: bool,

    /// Enable cluster-LOD **streaming residency** (Phase 5): cap the cluster
    /// render mesh `M`'s uploaded geometry to a triangle budget so a
    /// multi-million-triangle asset loads without overflowing the GPU pool
    /// (today's ceiling — `M` uploads the FULL exploded cluster geometry). The
    /// loader keeps the coarse clusters plus as many fine clusters as fit the
    /// budget, clamps the resident-leaf `lod_error` to 0 so close-up stays
    /// watertight, and remaps each resident cluster's `first_index` into the
    /// compacted `M`. The per-cluster GPU cut is unchanged (it just sees fewer
    /// pages). When `false` (the default), `M` uploads every cluster — identical
    /// to the `virtual_geometry` path today; the cap only bites for assets above
    /// the budget (which currently fail to fit), so flag-off is byte-identical.
    /// Requires [`Self::virtual_geometry`]. This is the **intermediate** residency
    /// win; true per-frame paging (stream finer clusters on demand) is the
    /// follow-up — see `docs/nanite-lod.md (streaming residency).
    pub cluster_streaming: bool,

    /// Optional override for the cluster-streaming triangle budget (Phase 5). When
    /// `None` (the default), the loader uses its built-in default; `Some(n)` caps
    /// the cluster render mesh `M` to `n` triangles. Only consulted when
    /// [`Self::cluster_streaming`] is on. Exposed so a host (e.g. the editor's
    /// `?streambudget=N` URL flag) can tune the cap without a rebuild — handy for
    /// forcing the cap on a small asset to exercise the path.
    pub cluster_streaming_budget: Option<usize>,

    /// Enable cluster-LOD **dynamic per-frame paging** (Phase 5 Step 2 / Gap B):
    /// hold cluster geometry in a fixed-capacity GPU page pool of equal-size slots
    /// and a `resident: array<i32>` cluster→slot table, so a multi-million-triangle
    /// asset shows full detail near the camera within a bounded VRAM budget — the
    /// cut asks for finer pages where the camera is close, the CPU streams them in
    /// and evicts cold ones (LRU), and where a wanted page isn't yet resident the
    /// cut falls back to the nearest resident (coarser) ancestor, refining over the
    /// next frame or two. Builds on [`Self::cluster_streaming`]'s residency cap (the
    /// static intermediate); this makes residency camera-driven. When `false` (the
    /// default), no page pool / resident table / feedback buffer is built and the
    /// cluster path is exactly the `virtual_geometry` (+ optional `cluster_streaming`)
    /// path — byte-identical. Requires [`Self::virtual_geometry`].
    pub cluster_paging: bool,

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
    /// The active main-camera depth convention, derived from
    /// [`Self::reverse_z`]. Shadow paths pin
    /// [`DepthConvention::FORWARD`](crate::depth_convention::DepthConvention::FORWARD)
    /// until the stage-7 lockstep migration.
    pub fn depth(&self) -> crate::depth_convention::DepthConvention {
        crate::depth_convention::DepthConvention {
            reverse_z: self.reverse_z,
        }
    }

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
        assert!(
            !features.picking,
            "picking must default to false so library consumers pay no cost"
        );
        assert!(
            !features.lod,
            "lod must default to false so a build without LOD is byte-identical"
        );
        assert!(
            !features.virtual_geometry,
            "virtual_geometry must default to false (byte-identical without cluster LOD)"
        );
        assert!(
            !features.cluster_streaming,
            "cluster_streaming must default to false (byte-identical without streaming residency)"
        );
        assert_eq!(
            features.cluster_streaming_budget, None,
            "cluster_streaming_budget must default to None so the cap never bites unless asked"
        );
        assert!(
            !features.cluster_paging,
            "cluster_paging must default to false (byte-identical without dynamic paging)"
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
