//! Public extension trait that attaches the glTF `populate_gltf` method to
//! `AwsmRenderer`. Importing this trait into scope lets callers write
//! `renderer.populate_gltf(...)` the same way they used to before the
//! extraction.

use std::sync::Arc;

use awsm_renderer::AwsmRenderer;

use crate::{data::GltfData, populate::GltfPopulateContext};

/// Imports the `populate_gltf` method onto `AwsmRenderer`. Bring this into
/// scope with `use awsm_renderer_gltf::AwsmRendererGltfExt;`.
pub trait AwsmRendererGltfExt {
    /// Populates renderer resources from a parsed glTF document. Returns
    /// the per-load `GltfPopulateContext` whose `key_lookups` maps
    /// per-document indices to renderer keys (mesh / material / transform /
    /// animation).
    #[allow(async_fn_in_trait)]
    async fn populate_gltf(
        &mut self,
        gltf_data: impl Into<Arc<GltfData>>,
        scene: Option<usize>,
    ) -> anyhow::Result<GltfPopulateContext>;
}

impl AwsmRendererGltfExt for AwsmRenderer {
    async fn populate_gltf(
        &mut self,
        gltf_data: impl Into<Arc<GltfData>>,
        scene: Option<usize>,
    ) -> anyhow::Result<GltfPopulateContext> {
        crate::populate::populate_gltf(self, gltf_data, scene).await
    }
}
