//! Unified material variant registry (the specialize-only design).
//!
//! Every render bucket — first-party AND author-registered — is a registry
//! *variant* deduped by key and addressed by an in-memory [`MaterialShaderId`]:
//!
//! - **First-party feature-set variant** `(ShadingBase, features)` — the
//!   built-in PBR/Toon/Unlit/Flipbook shader templated (Askama, compile-time)
//!   from its [`ShadingBase`] + feature mask. PBR fans out one bucket per
//!   distinct [`awsm_materials::pbr::PbrFeatures`] set (allocated via
//!   [`DynamicMaterials::resolve_first_party_variant_or_cap_err`]);
//!   Toon/Unlit/Flipbook have no compile-gateable shading paths today, so
//!   they stay single-bucket.
//! - **Custom variant** — the author's WGSL fragment + layout
//!   ([`MaterialRegistration`]), deduped by `(name, layout_hash, wgsl_hash)`.
//!
//! [`bucket_entries`] is the single source of truth the classify + opaque +
//! edge + transparent templates all walk. There is **no uber shader** — every
//! bucket compiles only the code its `(base, features)` needs; absent
//! features/textures emit nothing (DCE → lower register pressure → higher
//! occupancy). See [`ShadingBase`].
//!
//! **Cost model (honest):** steady-state cost scales with the
//! *active-in-view* bucket fanout (one classify extract arm + one opaque
//! dispatch + (MSAA) one edge-resolve per active bucket), NOT the total
//! registered count. A bucket-set change (a new feature-set variant or a
//! registration) invalidates + relaunches every bucket's layout-dependent
//! pipelines once against the final layout (the templated `ClassifyBuckets`
//! struct depends on the full list) — a one-time cold-compile, not per-frame.
//! Exceeding the [`MAX_BUCKET_ENTRIES`] cap is a hard error on BOTH the
//! batch `register_materials` path ([`DynamicMaterials::validate_batch`])
//! AND the render-loop variant reconcile
//! ([`DynamicMaterials::resolve_first_party_variant_or_cap_err`]) — there
//! is no silent fallback; a material rendered with the wrong bucket is far
//! harder to debug than a loud, actionable failure (raise [`MAX_BUCKET_WORDS`]).
//! Also owns the extras-pool storage buffer + allocator backing `BufferSlot`
//! data.

pub mod error;
pub mod extras_pool;
pub mod widths;

pub use error::AwsmDynamicMaterialError;
pub use widths::*;

mod registry;
pub use registry::*;
