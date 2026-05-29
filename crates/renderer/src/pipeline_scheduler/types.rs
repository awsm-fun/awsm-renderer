//! Type surface for the pipeline-readiness scheduler.
//!
//! Per the architecture documented in
//! [`https://github.com/dakom/awsm-renderer/pull/99`](../../../../https://github.com/dakom/awsm-renderer/pull/99):
//!
//! - [`PipelineGroupId`] / [`PipelineGroupStatus`] / [`PipelineGroupDef`] —
//!   the unified handles + state + input over both materials and passes.
//! - [`MaterialDef`] / [`MaterialDefKind`] — input data for a material
//!   group submission; for first-party variants it's just
//!   `(shader_id, alpha_mode, double_sided)`; for dynamic, also carries
//!   the WGSL fragment + layout.
//! - [`PassDef`] / [`PassKind`] — sum types over the scheduler-managed
//!   passes (EVSM, Line, ShadowGen, post-fx, MSAA variants of
//!   geometry/classify, edge-resolve helpers, etc.).
//! - [`PipelineConfigSnapshot`] — captures the renderer config at
//!   submission time so a material's compiled pipelines are tied to a
//!   specific (msaa, mipmap, ...) tuple. When `set_anti_aliasing` flips,
//!   stale-snapshot materials transition back to Pending and re-submit.
//!
//! All `Material`-flavoured ids share a `SlotMap`; `Pass`-flavoured ids
//! are keyed by `PassKind` (one-of-each-kind).

use awsm_materials::{MaterialAlphaMode, MaterialShaderId};
use awsm_renderer_core::pipeline::primitive::CullMode;
use slotmap::new_key_type;

use crate::{
    anti_alias::AntiAliasing, dynamic_materials::MaterialRegistration, error::AwsmError,
    render_passes::material_opaque::shader::template::MipmapMode,
};

new_key_type! {
    /// Renderer-internal handle to a registered material pipeline group.
    ///
    /// Returned by [`AwsmRenderer::submit_pipeline_group_batch`] (in the
    /// `Material(_)` variant of [`PipelineGroupId`]). Allocated in
    /// `Pending` state; transitions to `Ready` (or `Failed`) as the
    /// underlying compiles resolve. Session-only — no stability across
    /// process restarts. The renderer-facing handle is the `SlotMap`
    /// key; the project-facing reference is whatever external scheme
    /// the frontend uses (see the doc's three-layer naming).
    pub struct MaterialId;
}

/// Unified handle over material pipeline groups and pass pipeline
/// groups. Returned (in the same `Vec`) from
/// `submit_pipeline_group_batch`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PipelineGroupId {
    /// Material group — one per registered material (first-party or
    /// dynamic). Identified by a `SlotMap` key allocated at submission.
    Material(MaterialId),
    /// Pass group — one per supported pass kind. Identified by the
    /// pass-kind enum directly (passes are singletons within the
    /// renderer).
    Pass(PassKind),
}

/// Three-state readiness machine.
#[derive(Debug)]
pub enum PipelineGroupStatus {
    /// Submitted; compile future not yet resolved.
    Pending,
    /// Compile resolved successfully. Pipeline cache contains the
    /// keys; dispatch sites can route through this group.
    Ready,
    /// Compile failed (WGSL parse error, layout validation, driver
    /// rejection, etc.). No auto-retry — the consumer must re-submit
    /// with a corrected definition. The mesh / pass dispatch site
    /// silently skips Failed groups (one-shot warn in the
    /// render-frame preamble).
    Failed {
        /// Underlying compile error.
        error: AwsmError,
    },
}

/// Coarse-grained equality for status-stream consumers that don't need
/// to inspect the inner error. Avoids requiring `Eq` on `AwsmError`.
impl PipelineGroupStatus {
    /// Returns true when the group is `Ready`.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Returns true when the group is `Pending`.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    /// Returns true when the group is `Failed`.
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// Input shape for a single submission to the scheduler.
///
/// `submit_pipeline_group_batch` takes `Vec<PipelineGroupDef>` and
/// returns `Vec<PipelineGroupId>` of the same length.
pub enum PipelineGroupDef {
    /// A new material (first-party or dynamic).
    Material(MaterialDef),
    /// A pass-level pipeline group (singleton; resubmission overwrites
    /// the existing entry's state).
    Pass(PassDef),
}

/// Material input. The renderer's per-batch input shape — fully
/// resolved (WGSL, layout, params) and snapshot-pinned to a config
/// tuple.
pub struct MaterialDef {
    /// First-party variant (PBR/UNLIT/TOON/FLIPBOOK) OR dynamic
    /// shader_id from the registry (`>= MaterialShaderId::DYNAMIC_START`).
    pub shader_id: MaterialShaderId,

    /// Alpha mode — routes between the opaque-compute path and the
    /// transparent-fragment path.
    pub alpha_mode: MaterialAlphaMode,

    /// Double-sided culling override.
    pub double_sided: bool,

    /// Per-shader_id config snapshot. First-party variants carry their
    /// params in the `material_meta` buffer (looked up at dispatch
    /// time); the kind enum just records that this is first-party.
    /// Dynamic variants carry the registered WGSL + layout + buffer
    /// defaults so the scheduler can drive the compile without
    /// reaching back into the registry.
    pub kind: MaterialDefKind,

    /// Snapshot of renderer config at submission time. Used as the
    /// pipeline-cache key for `(shader_id, msaa, mipmap, ...)`-keyed
    /// pipelines. When `set_anti_aliasing` flips, the scheduler
    /// notices the snapshot drift, transitions Ready materials back
    /// to Pending, and re-submits with the new active snapshot.
    pub config_snapshot: PipelineConfigSnapshot,
}

/// Material flavour discriminator + per-flavour payload.
pub enum MaterialDefKind {
    /// First-party material — params live in the `material_meta`
    /// buffer, looked up by `material_offset` at dispatch. The
    /// scheduler doesn't need to embed them.
    FirstParty,
    /// Dynamic material — the registered fragment, layout, and
    /// buffer defaults. The scheduler templates this into the
    /// per-shader-id opaque kernel + (for Blend) the transparent
    /// fragment shader.
    Dynamic(Box<MaterialRegistration>),
}

/// Pass-level group definitions. One variant per pass kind; each
/// carries only the data its build path needs.
#[derive(Clone, Debug)]
pub enum PassDef {
    /// The empty opaque compute pipeline — runs on skybox-only frames
    /// and the bucket-skip path. Always eager.
    OpaqueEmpty {
        /// Renderer-config snapshot this pipeline is keyed on.
        snapshot: PipelineConfigSnapshot,
    },
    /// MSAA variant of the classify compute pass.
    ClassifyMsaa {
        /// MSAA sample count this pipeline targets.
        samples: u8,
        /// Renderer-config snapshot this pipeline is keyed on.
        snapshot: PipelineConfigSnapshot,
    },
    /// MSAA variant of the geometry render passes.
    GeometryMsaa {
        /// MSAA sample count this pipeline targets.
        samples: u8,
        /// Renderer-config snapshot this pipeline is keyed on.
        snapshot: PipelineConfigSnapshot,
    },
    /// Display blit — renders the opaque target to the swap chain.
    Display,
    /// Per-frame scene-pass clear.
    ScenePassClear,
    /// HZB seed compute (active only when `gpu_culling` feature is on).
    HzbSeed {
        /// MSAA sample count this pipeline targets.
        samples: u8,
    },
    /// EVSM compute pipelines (only after first shadow-caster).
    Evsm,
    /// Line primitives render pass.
    Line {
        /// Renderer-config snapshot this pipeline is keyed on.
        snapshot: PipelineConfigSnapshot,
    },
    /// Shadow-gen render pipelines.
    ShadowGen,
    /// Mouse-picker compute pipeline.
    Picker {
        /// Renderer-config snapshot this pipeline is keyed on.
        snapshot: PipelineConfigSnapshot,
    },
    /// Post-processing pipelines.
    Bloom {
        /// `(width, height)` of the post-fx target this pipeline is sized for.
        resolution: (u32, u32),
    },
    /// SMAA post-fx pipelines.
    Smaa {
        /// `(width, height)` of the post-fx target this pipeline is sized for.
        resolution: (u32, u32),
    },
    /// Depth-of-field post-fx pipelines.
    Dof,
    /// Priority-3 MSAA-edge-resolve helpers (shared across all
    /// materials).
    EdgeResolveSkybox {
        /// Renderer-config snapshot this pipeline is keyed on.
        snapshot: PipelineConfigSnapshot,
    },
    /// MSAA-edge-resolve helper for transparent (Blend) materials.
    EdgeResolveBlend {
        /// Renderer-config snapshot this pipeline is keyed on.
        snapshot: PipelineConfigSnapshot,
    },
}

/// Identifier for pass-level pipeline groups. Used as the `PassKind`
/// variant in [`PipelineGroupId`], and as a map key inside the
/// scheduler.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PassKind {
    /// The empty opaque compute pipeline (skybox-only / bucket-skip path).
    OpaqueEmpty,
    /// MSAA variant of the classify compute pass, parametrised by sample count.
    ClassifyMsaa {
        /// MSAA sample count.
        samples: u8,
    },
    /// MSAA variant of the geometry render passes, parametrised by sample count.
    GeometryMsaa {
        /// MSAA sample count.
        samples: u8,
    },
    /// Display-blit pipeline.
    Display,
    /// Scene-pass clear pipeline.
    ScenePassClear,
    /// HZB seed compute pipeline (parametrised by sample count).
    HzbSeed {
        /// MSAA sample count.
        samples: u8,
    },
    /// EVSM compute pipelines.
    Evsm,
    /// Line-primitives render pipeline.
    Line,
    /// Shadow-gen render pipelines.
    ShadowGen,
    /// Mouse-picker compute pipeline.
    Picker,
    /// Bloom post-fx pipelines.
    Bloom,
    /// SMAA post-fx pipelines.
    Smaa,
    /// Depth-of-field post-fx pipelines.
    Dof,
    /// MSAA-edge-resolve helper for skybox.
    EdgeResolveSkybox,
    /// MSAA-edge-resolve helper for transparent (Blend) materials.
    EdgeResolveBlend,
}

impl PassDef {
    /// Project the def to its identifying [`PassKind`].
    pub fn kind(&self) -> PassKind {
        match self {
            Self::OpaqueEmpty { .. } => PassKind::OpaqueEmpty,
            Self::ClassifyMsaa { samples, .. } => PassKind::ClassifyMsaa { samples: *samples },
            Self::GeometryMsaa { samples, .. } => PassKind::GeometryMsaa { samples: *samples },
            Self::Display => PassKind::Display,
            Self::ScenePassClear => PassKind::ScenePassClear,
            Self::HzbSeed { samples } => PassKind::HzbSeed { samples: *samples },
            Self::Evsm => PassKind::Evsm,
            Self::Line { .. } => PassKind::Line,
            Self::ShadowGen => PassKind::ShadowGen,
            Self::Picker { .. } => PassKind::Picker,
            Self::Bloom { .. } => PassKind::Bloom,
            Self::Smaa { .. } => PassKind::Smaa,
            Self::Dof => PassKind::Dof,
            Self::EdgeResolveSkybox { .. } => PassKind::EdgeResolveSkybox,
            Self::EdgeResolveBlend { .. } => PassKind::EdgeResolveBlend,
        }
    }
}

/// Snapshot of renderer config that pipeline compiles are keyed on.
///
/// When `set_anti_aliasing` or `set_post_processing` flips, the
/// scheduler iterates Ready materials, finds those whose
/// `config_snapshot` no longer matches the active config, transitions
/// them back to Pending, and resubmits with the new active snapshot.
/// First-party material pipelines all share the snapshot's MSAA;
/// dynamic materials too. Per-pass groups carry their own MSAA in the
/// [`PassDef`] variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineConfigSnapshot {
    /// Active anti-aliasing mode (MSAA sample count or off).
    pub msaa: AntiAliasing,
    /// Active mipmap-sampling mode.
    pub mipmap: MipmapMode,
    /// Whether GPU culling (HZB) is active.
    pub gpu_culling: bool,
    /// Whether coverage-LOD selection is active.
    pub coverage_lod: bool,
    /// Currently-set debug bitmask. Most debug branches are
    /// PBR-template-only; included here for completeness.
    pub debug_bitmask: u32,
    /// Active double-sided override for the cull mode. Per-material
    /// `double_sided` flags still win at dispatch time; this is the
    /// config-level baseline for the geometry pass.
    pub default_cull_mode: CullMode,
}

impl Default for PipelineConfigSnapshot {
    fn default() -> Self {
        Self {
            msaa: AntiAliasing::default(),
            mipmap: MipmapMode::Gradient,
            gpu_culling: false,
            coverage_lod: false,
            debug_bitmask: 0,
            default_cull_mode: CullMode::Back,
        }
    }
}
