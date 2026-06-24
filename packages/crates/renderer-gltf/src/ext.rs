//! Public extension trait that attaches the glTF `populate_gltf` method to
//! `AwsmRenderer`. Importing this trait into scope lets callers write
//! `renderer.populate_gltf(...)` the same way they used to before the
//! extraction.

use std::sync::Arc;

use awsm_renderer::{transforms::TransformKey, AwsmRenderer};

use crate::{
    data::GltfData,
    populate::{GltfPopulateContext, PopulateGltfOpts},
};

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

    /// Like [`populate_gltf`](Self::populate_gltf), but roots the document's
    /// scene nodes under `parent_transform` instead of at the renderer root.
    /// Used by `awsm-renderer-scene-loader` to drop a runtime-bundle's per-mesh glb
    /// (a single identity node holding geometry) beneath the scene node that
    /// carries its real TRS.
    #[allow(async_fn_in_trait)]
    async fn populate_gltf_under(
        &mut self,
        gltf_data: impl Into<Arc<GltfData>>,
        scene: Option<usize>,
        parent_transform: Option<TransformKey>,
    ) -> anyhow::Result<GltfPopulateContext>;

    /// The full-control entry point: load a glTF with explicit
    /// [`PopulateGltfOpts`] (material source, deferred texture finalize, parent
    /// transform). The two methods above are thin wrappers over this with
    /// foreign-glTF defaults. The bundle loader uses this with
    /// `GltfMaterialSource::Single` + `finalize_textures: false`.
    #[allow(async_fn_in_trait)]
    async fn populate_gltf_with(
        &mut self,
        gltf_data: impl Into<Arc<GltfData>>,
        opts: PopulateGltfOpts,
    ) -> anyhow::Result<GltfPopulateContext>;
}

impl AwsmRendererGltfExt for AwsmRenderer {
    async fn populate_gltf(
        &mut self,
        gltf_data: impl Into<Arc<GltfData>>,
        scene: Option<usize>,
    ) -> anyhow::Result<GltfPopulateContext> {
        crate::populate::populate_gltf(
            self,
            gltf_data,
            PopulateGltfOpts {
                scene,
                ..PopulateGltfOpts::foreign()
            },
        )
        .await
    }

    async fn populate_gltf_under(
        &mut self,
        gltf_data: impl Into<Arc<GltfData>>,
        scene: Option<usize>,
        parent_transform: Option<TransformKey>,
    ) -> anyhow::Result<GltfPopulateContext> {
        crate::populate::populate_gltf(
            self,
            gltf_data,
            PopulateGltfOpts {
                scene,
                parent_transform,
                ..PopulateGltfOpts::foreign()
            },
        )
        .await
    }

    async fn populate_gltf_with(
        &mut self,
        gltf_data: impl Into<Arc<GltfData>>,
        opts: PopulateGltfOpts,
    ) -> anyhow::Result<GltfPopulateContext> {
        crate::populate::populate_gltf(self, gltf_data, opts).await
    }
}
