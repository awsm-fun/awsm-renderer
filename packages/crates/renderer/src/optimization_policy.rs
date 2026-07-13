//! Adaptive runtime policy on top of the build-time `RendererFeatures`.
//!
//! `RendererFeatures` decides which buffers / textures / passes are
//! *allocated*. `RendererOptimizationPolicy` decides which of those
//! capabilities are *used this frame*. The split exists because the
//! always-on GPU-culling pipeline (HZB build + occlusion cull +
//! compaction + drawIndirect) costs more than it saves on small scenes
//! — the per-frame compute dispatches and the args-buffer plumbing
//! aren't free. Library consumers want to allocate the machinery once,
//! then let the renderer decide per-frame whether to engage it.
//!
//! The decision lives in [`compute_frame_optimizations`], a pure
//! function with hysteresis so the mode doesn't flip every frame near
//! a threshold. Call sites consult [`FrameOptimizations`] (carried on
//! `RenderContext`) rather than `RendererFeatures` for runtime
//! behaviour.

/// User-facing toggle for an adaptive optimization. The same enum
/// could gate other paths in the future, but v1 ships with one knob
/// (`RendererOptimizationPolicy::gpu_culling`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptimizationMode {
    /// Never run the optimization this session. The capability resources
    /// may still exist (allocated by `RendererFeatures`) but the
    /// per-frame work is skipped.
    Off,
    /// Engage the optimization when scene stats cross the enable
    /// threshold; disengage below the disable threshold. Hysteresis +
    /// cooldown keep the mode stable.
    #[default]
    Auto,
    /// Always run the optimization this session — useful for editor
    /// builds where the user wants to validate the GPU-driven path
    /// regardless of scene size.
    Force,
}

/// Per-renderer runtime policy. Distinct from `RendererFeatures`:
/// features gate **allocation**, policy gates **engagement**.
///
/// Defaults target the small-/mid-scene case: `Auto` mode flips on at
/// 800 opaque renderables (a rough proxy for "GPU dispatch starts to
/// pay back its overhead"), off below 500, with a 30-frame cooldown to
/// avoid flicker near the threshold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RendererOptimizationPolicy {
    pub gpu_culling: OptimizationMode,
    /// Auto-mode: enable GPU culling when the *optimizable* mesh count
    /// (non-instanced, world-AABB present) reaches this threshold.
    /// Counting just `opaque_count` would happily engage the full
    /// HZB/cull/clear path for a scene of, say, 10k instanced meshes
    /// where none of the work is recoverable via drawIndirect.
    pub gpu_culling_enable_threshold: u32,
    /// Auto-mode: disable GPU culling when the optimizable mesh count
    /// drops below this threshold. Must be < enable threshold for
    /// hysteresis to do its job.
    pub gpu_culling_disable_threshold: u32,
    /// Auto-mode: minimum frames a mode must remain active before
    /// another flip is allowed. Acts on top of the threshold band.
    pub gpu_culling_cooldown_frames: u32,
}

impl Default for RendererOptimizationPolicy {
    fn default() -> Self {
        Self {
            gpu_culling: OptimizationMode::Auto,
            gpu_culling_enable_threshold: 800,
            gpu_culling_disable_threshold: 500,
            gpu_culling_cooldown_frames: 30,
        }
    }
}

/// Per-frame derived flags. `RenderContext` carries a reference; call
/// sites consult this rather than `RendererFeatures` for runtime
/// branching.
///
/// - `gpu_occlusion`: run HZB-fed cull + compaction this frame.
/// - `indirect_geometry`: geometry pass may consume the compaction's
///   `IndirectDrawArgs` via `drawIndirect`. False unless
///   `gpu_occlusion && args_ready` — additional per-mesh gates
///   (`world_aabb.is_some()`, `!instanced`) still apply at the call
///   site in `meshes::mesh::push_geometry_pass_commands`.
/// - `hzb`: build the HZB this frame. Derived (`gpu_occlusion ||
///   decal_hzb_gate`) — the HZB feeds both cull and decal classify.
/// - `decal_hzb_gate`: decals are active and the HZB capability is
///   allocated. Doesn't currently gate any classify-pass behaviour
///   beyond ensuring the HZB has been refreshed for the classify to
///   sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FrameOptimizations {
    pub gpu_occlusion: bool,
    pub indirect_geometry: bool,
    pub hzb: bool,
    pub decal_hzb_gate: bool,
}

/// Inputs to [`compute_frame_optimizations`]. Snapshotted from the
/// renderer state once per frame so the pure decision function stays
/// easy to unit-test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameOptimizationStats {
    /// Whether the `gpu_culling` capability is allocated. When false,
    /// no GPU-driven path can engage regardless of policy.
    pub features_gpu_culling: bool,
    /// Whether the `decals` capability is allocated.
    pub features_decals: bool,
    /// SSR is enabled this frame — the Hi-Z trace consumes the HZB's
    /// closest-depth channel, so an SSR frame requests the build even
    /// when mesh occlusion is disengaged (same independent-consumer
    /// pattern as decals). Requires the `gpu_culling` capability (that
    /// allocates the texture); without it SSR falls back to the linear
    /// per-pixel march variant.
    pub ssr_enabled: bool,
    /// Total opaque renderables this frame
    /// (post-`collect_renderables`). Surfaced for diagnostics / future
    /// tuning; the Auto-mode threshold reads
    /// `non_instanced_with_aabb_count` (below) since instanced and
    /// no-AABB meshes don't pay back the GPU-driven path.
    pub opaque_count: u32,
    /// Subset of `opaque_count` that would actually use drawIndirect —
    /// non-instanced, world_aabb=Some. Drives the Auto-mode enable /
    /// disable threshold comparison in
    /// [`compute_frame_optimizations`].
    pub non_instanced_with_aabb_count: u32,
    /// Decals active this frame (used by `decal_hzb_gate`).
    pub decals_count: u32,
    /// Whether the compaction args buffer holds a valid previous-frame
    /// visibility set. Without this, `indirect_geometry` falls back to
    /// `false` and the geometry pass uses its CPU branch.
    pub args_ready: bool,
}

/// Pure decision function. Given the policy, this-frame stats, and the
/// prior-frame state, produce the new `FrameOptimizations`. Caller is
/// responsible for tracking `frames_in_current_mode` and updating it
/// based on the returned flags (see `FrameOptimizations::stable_mode`).
///
/// Hysteresis lives entirely in the Auto branch:
///   - if `prev.gpu_occlusion` was on, stay on unless the optimizable
///     count (`non_instanced_with_aabb_count`) drops below
///     `gpu_culling_disable_threshold` AND cooldown elapsed
///   - if it was off, stay off unless the optimizable count reaches
///     `gpu_culling_enable_threshold` AND cooldown elapsed
///
/// The threshold reads `non_instanced_with_aabb_count` rather than
/// `opaque_count` because the HZB/cull/compaction work only pays back
/// for the optimizable subset — instanced meshes stay on the legacy
/// uniform path, and no-AABB meshes are conservatively visible (no
/// cull data to act on). A scene with 5000 instanced meshes and 50
/// non-instanced ones shouldn't engage the GPU path; the 5000 instanced
/// meshes aren't paying it back.
///
/// `Off` and `Force` ignore stats entirely.
pub fn compute_frame_optimizations(
    policy: &RendererOptimizationPolicy,
    stats: &FrameOptimizationStats,
    prev: &FrameOptimizations,
    frames_in_current_mode: u32,
) -> FrameOptimizations {
    // GPU occlusion is only possible if the capability is allocated.
    let gpu_occlusion = if !stats.features_gpu_culling {
        false
    } else {
        match policy.gpu_culling {
            OptimizationMode::Off => false,
            OptimizationMode::Force => true,
            OptimizationMode::Auto => {
                let cooldown_elapsed = frames_in_current_mode >= policy.gpu_culling_cooldown_frames;
                let optimizable = stats.non_instanced_with_aabb_count;
                if prev.gpu_occlusion {
                    // Currently on. Flip off only if both cooldown
                    // elapsed AND the optimizable count dropped below
                    // the disable threshold.
                    !(cooldown_elapsed && optimizable < policy.gpu_culling_disable_threshold)
                } else {
                    // Currently off. Flip on only if both cooldown
                    // elapsed AND the optimizable count reached the
                    // enable threshold.
                    cooldown_elapsed && optimizable >= policy.gpu_culling_enable_threshold
                }
            }
        }
    };

    // Decals can request the HZB for the classify pass even when GPU
    // mesh occlusion is disabled — they're independent consumers of
    // the same texture. Requires the gpu_culling capability because
    // that's what allocates the HZB texture itself.
    let decal_hzb_gate =
        stats.features_gpu_culling && stats.features_decals && stats.decals_count > 0;
    // SSR: same independent-consumer rule as decals (see the stats field doc).
    let ssr_hzb_gate = stats.features_gpu_culling && stats.ssr_enabled;

    let hzb = gpu_occlusion || decal_hzb_gate || ssr_hzb_gate;
    let indirect_geometry = gpu_occlusion && stats.args_ready;

    FrameOptimizations {
        gpu_occlusion,
        indirect_geometry,
        hzb,
        decal_hzb_gate,
    }
}

impl FrameOptimizations {
    /// Returns `true` when this frame's `gpu_occlusion` matches `prev`'s,
    /// i.e. the mode hasn't flipped. Callers use this to decide whether
    /// to bump or reset `frames_in_current_mode`.
    pub fn stable_mode(&self, prev: &FrameOptimizations) -> bool {
        self.gpu_occlusion == prev.gpu_occlusion
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(opaque: u32, decals: u32, args_ready: bool) -> FrameOptimizationStats {
        FrameOptimizationStats {
            features_gpu_culling: true,
            features_decals: true,
            ssr_enabled: false,
            opaque_count: opaque,
            non_instanced_with_aabb_count: opaque,
            decals_count: decals,
            args_ready,
        }
    }

    fn policy() -> RendererOptimizationPolicy {
        RendererOptimizationPolicy::default()
    }

    fn off_prev() -> FrameOptimizations {
        FrameOptimizations::default()
    }

    fn on_prev() -> FrameOptimizations {
        FrameOptimizations {
            gpu_occlusion: true,
            indirect_geometry: true,
            hzb: true,
            decal_hzb_gate: false,
        }
    }

    // (1) Off always disables gpu_occlusion/indirect but still allows
    // decal_hzb_gate when capability exists and decals are active.
    #[test]
    fn off_disables_gpu_occlusion_but_keeps_decal_hzb_gate() {
        let mut p = policy();
        p.gpu_culling = OptimizationMode::Off;
        let s = stats(10_000, 5, true);
        let out = compute_frame_optimizations(&p, &s, &on_prev(), 1000);
        assert!(!out.gpu_occlusion);
        assert!(!out.indirect_geometry);
        assert!(out.decal_hzb_gate);
        assert!(
            out.hzb,
            "hzb must still be built so decal classify sees fresh data"
        );
    }

    #[test]
    fn off_with_no_decals_disables_hzb_entirely() {
        let mut p = policy();
        p.gpu_culling = OptimizationMode::Off;
        let s = stats(10_000, 0, true);
        let out = compute_frame_optimizations(&p, &s, &on_prev(), 1000);
        assert!(!out.hzb);
        assert!(!out.decal_hzb_gate);
    }

    #[test]
    fn ssr_requests_hzb_like_decals_do() {
        // SSR's Hi-Z trace is an independent HZB consumer: even with mesh
        // occlusion Off and zero decals, an SSR frame must build the pyramid.
        let mut p = policy();
        p.gpu_culling = OptimizationMode::Off;
        let mut s = stats(10_000, 0, true);
        s.ssr_enabled = true;
        let out = compute_frame_optimizations(&p, &s, &on_prev(), 1000);
        assert!(!out.gpu_occlusion);
        assert!(out.hzb, "SSR must engage the HZB build");
        // Without the gpu_culling CAPABILITY (which allocates the texture)
        // the gate stays off — the trace falls back to the linear march.
        s.features_gpu_culling = false;
        let out = compute_frame_optimizations(&p, &s, &on_prev(), 1000);
        assert!(!out.hzb);
    }

    // (2) Force enables gpu_occlusion, but indirect_geometry still
    // requires args_ready.
    #[test]
    fn force_enables_occlusion_only_indirect_needs_args_ready() {
        let mut p = policy();
        p.gpu_culling = OptimizationMode::Force;
        let s = stats(10, 0, false);
        let out = compute_frame_optimizations(&p, &s, &off_prev(), 0);
        assert!(out.gpu_occlusion, "Force ignores opaque_count");
        assert!(
            !out.indirect_geometry,
            "indirect_geometry waits on args_ready"
        );
        assert!(out.hzb);
    }

    #[test]
    fn force_with_args_ready_enables_indirect() {
        let mut p = policy();
        p.gpu_culling = OptimizationMode::Force;
        let s = stats(10, 0, true);
        let out = compute_frame_optimizations(&p, &s, &on_prev(), 100);
        assert!(out.gpu_occlusion);
        assert!(out.indirect_geometry);
    }

    // (3) Auto enables/disables only across thresholds and respects
    // cooldown.
    #[test]
    fn auto_enables_only_when_threshold_and_cooldown_met() {
        let p = policy(); // enable=800, disable=500, cooldown=30
                          // Just below threshold → stay off
        let out = compute_frame_optimizations(&p, &stats(799, 0, false), &off_prev(), 60);
        assert!(!out.gpu_occlusion);
        // At threshold but cooldown not met → stay off
        let out = compute_frame_optimizations(&p, &stats(800, 0, false), &off_prev(), 5);
        assert!(!out.gpu_occlusion);
        // At threshold AND cooldown met → flip on
        let out = compute_frame_optimizations(&p, &stats(800, 0, false), &off_prev(), 60);
        assert!(out.gpu_occlusion);
    }

    #[test]
    fn auto_holds_on_inside_hysteresis_band() {
        let p = policy(); // enable=800, disable=500
                          // Currently on; opaque drops between disable (500) and enable
                          // (800) — should stay on (hysteresis band).
        let out = compute_frame_optimizations(&p, &stats(700, 0, true), &on_prev(), 60);
        assert!(out.gpu_occlusion);
        let out = compute_frame_optimizations(&p, &stats(501, 0, true), &on_prev(), 60);
        assert!(out.gpu_occlusion);
    }

    #[test]
    fn auto_disables_below_threshold_after_cooldown() {
        let p = policy();
        // Below disable, but cooldown not met → stay on
        let out = compute_frame_optimizations(&p, &stats(100, 0, true), &on_prev(), 5);
        assert!(out.gpu_occlusion);
        // Below disable, cooldown met → flip off
        let out = compute_frame_optimizations(&p, &stats(100, 0, true), &on_prev(), 60);
        assert!(!out.gpu_occlusion);
    }

    // (4) Toggling Force → Off poisons args_ready / makes
    // indirect_geometry false immediately. (The args_ready poison
    // itself is the caller's responsibility — render.rs writes
    // compaction_buffers.args_ready = false when gpu_occlusion goes
    // false. The decision function only needs to drop indirect_geometry
    // in lockstep.)
    #[test]
    fn force_to_off_drops_indirect_immediately() {
        let mut p = policy();
        p.gpu_culling = OptimizationMode::Force;
        let s = stats(10, 0, true);
        let on = compute_frame_optimizations(&p, &s, &on_prev(), 100);
        assert!(on.indirect_geometry);

        p.gpu_culling = OptimizationMode::Off;
        // Even with args_ready still true from last frame's compaction,
        // gpu_occlusion=false drops indirect_geometry without any
        // cooldown. The caller is expected to also clear args_ready so
        // a future re-enable warms up through the CPU path.
        let off = compute_frame_optimizations(&p, &s, &on, 1);
        assert!(!off.gpu_occlusion);
        assert!(!off.indirect_geometry);
    }

    // Capability gate: features.gpu_culling=false blocks all GPU paths.
    #[test]
    fn missing_capability_blocks_everything() {
        let p = policy();
        let mut s = stats(10_000, 5, true);
        s.features_gpu_culling = false;
        let out = compute_frame_optimizations(&p, &s, &on_prev(), 1000);
        assert!(!out.gpu_occlusion);
        assert!(!out.indirect_geometry);
        // decal_hzb_gate also requires the capability (because HZB
        // texture is allocated under gpu_culling).
        assert!(!out.decal_hzb_gate);
        assert!(!out.hzb);
    }

    // Auto ignores `opaque_count` when the optimizable subset stays
    // small — e.g. a scene full of instanced meshes. Without this,
    // a 10k-instance scene would engage the GPU path even though
    // none of the work is recoverable via drawIndirect.
    #[test]
    fn auto_ignores_opaque_when_optimizable_subset_is_small() {
        let p = policy(); // enable=800
        let stats = FrameOptimizationStats {
            features_gpu_culling: true,
            features_decals: false,
            ssr_enabled: false,
            opaque_count: 10_000, // huge, but all instanced/no-AABB
            non_instanced_with_aabb_count: 10,
            decals_count: 0,
            args_ready: false,
        };
        let out = compute_frame_optimizations(&p, &stats, &off_prev(), 1000);
        assert!(
            !out.gpu_occlusion,
            "Auto must look at the optimizable subset, not opaque_count"
        );
    }

    #[test]
    fn auto_engages_on_optimizable_subset_threshold() {
        let p = policy(); // enable=800
                          // Conversely, even a small opaque_count is enough if the
                          // optimizable subset hits the threshold (in practice they're
                          // typically the same; this just locks in the gating field).
        let stats = FrameOptimizationStats {
            features_gpu_culling: true,
            features_decals: false,
            ssr_enabled: false,
            opaque_count: 800,
            non_instanced_with_aabb_count: 800,
            decals_count: 0,
            args_ready: false,
        };
        let out = compute_frame_optimizations(&p, &stats, &off_prev(), 1000);
        assert!(out.gpu_occlusion);
    }

    #[test]
    fn stable_mode_helper_tracks_occlusion_flips() {
        let on = FrameOptimizations {
            gpu_occlusion: true,
            ..Default::default()
        };
        let off = FrameOptimizations::default();
        assert!(on.stable_mode(&on));
        assert!(off.stable_mode(&off));
        assert!(!on.stable_mode(&off));
    }
}
