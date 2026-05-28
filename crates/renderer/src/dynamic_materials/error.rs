//! Error types for the dynamic-material registry.

use awsm_materials::MaterialShaderId;
use awsm_renderer_core::error::AwsmCoreError;
use thiserror::Error;

/// Errors returned by the dynamic-material registry.
#[derive(Error, Debug)]
pub enum AwsmDynamicMaterialError {
    /// Attempted to register a material under a name that's already in
    /// use at a different `(layout_hash, wgsl_hash)`. Re-registering a
    /// material with byte-identical `(layout, wgsl)` is idempotent and
    /// does not produce this error.
    #[error("[dynamic-material] duplicate name `{0}` already registered")]
    DuplicateName(String),

    /// Lookup / unregistration referenced a shader id that the registry
    /// has never seen, or has already removed.
    #[error("[dynamic-material] unknown shader id {0:?}")]
    UnknownShaderId(MaterialShaderId),

    /// Tried to unregister a material that still has live instances on
    /// meshes. Tear down the meshes (or reassign their materials) before
    /// unregistering.
    #[error("[dynamic-material] cannot unregister `{name}`: {instance_count} live instances")]
    InUse {
        /// Name of the material that's still referenced.
        name: String,
        /// Number of live mesh instances still pointing at the material.
        instance_count: usize,
    },

    /// The registration's layout (or the instance override) named a
    /// uniform / texture / buffer field whose name collides with a
    /// kernel-provided symbol (`material`, `texture_pool`, `extras_pool`,
    /// `frame_globals`, `camera`, `frag`, `vert`).
    #[error("[dynamic-material] reserved field name `{0}` (collides with kernel-provided symbol)")]
    ReservedName(String),

    /// The author's WGSL fragment failed to compile. The wrapped string
    /// is naga's diagnostic output (multi-line, includes file:line:col
    /// when available). `material-editor`'s error pane parses this for
    /// the line/column gutter.
    #[error("[dynamic-material] WGSL compile failed: {0}")]
    WgslCompile(String),

    /// Pass-through from the underlying WebGPU core error type — e.g.
    /// a downstream buffer create failed while the extras-pool allocator
    /// was growing.
    #[error("[dynamic-material] {0}")]
    Core(#[from] AwsmCoreError),

    /// Registration would push `bucket_entries.len()` past the
    /// `MAX_BUCKET_ENTRIES` cap. The tightest constraint is the
    /// classify pass's per-workgroup `tile_mask: atomic<u32>`, which
    /// accumulates a `BUCKET_BIT_<NAME> = (1u << index)` per visible
    /// bucket id — a bucket index `>= 32` would compile to
    /// `1u << 32u`, an implementation-defined WGSL shift that
    /// typically resolves to `0` on Dawn, silently dropping the
    /// bucket from classification. See [`crate::dynamic_materials::MAX_BUCKET_ENTRIES`]
    /// for the full encoding inventory.
    #[error("[dynamic-material] bucket-id cap exceeded: would push bucket_entries.len() to {would_be}, max is {max} (tile_mask is u32 → BUCKET_BIT fits at most 32 indices)")]
    BucketCapExceeded {
        /// What `bucket_entries.len()` would become if this
        /// registration were accepted.
        would_be: usize,
        /// The hard cap (currently 254).
        max: usize,
    },
}
