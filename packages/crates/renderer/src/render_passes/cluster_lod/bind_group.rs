//! Bind group layout + recreation for the cluster-LOD cut pass (Phase B, B.2).
//!
//! Single bind group (all compute-visible):
//!   0 pages     `storage[RO]`  array<ClusterPage>
//!   1 selected  `storage[RW]`  array<u32>
//!   2 params    uniform        ClusterCutParams
//!
//! Self-contained: the pass owns its [`ClusterLodBuffers`] and rebuilds the bind
//! group from them directly (no central `BindGroupRecreateContext`), so it stays
//! isolated behind `virtual_geometry`.

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::renderer::AwsmRendererWebGpu;

use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey, BindGroupLayouts,
};
use crate::bind_groups::AwsmBindGroupError;
use crate::error::Result;
use crate::render_passes::cluster_lod::buffers::ClusterLodBuffers;
use crate::render_passes::RenderPassInitContext;

pub struct ClusterCutBindGroups {
    pub layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl ClusterCutBindGroups {
    pub fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let compute_only = |resource| BindGroupLayoutCacheKeyEntry {
            resource,
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        };
        let entries = vec![
            // pages — storage RO
            compute_only(BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            )),
            // selected — storage RW
            compute_only(BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
            )),
            // params — uniform
            compute_only(BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            )),
        ];
        let layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?;
        Ok(Self {
            layout_key,
            bind_group: None,
        })
    }

    pub fn get_bind_group(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("ClusterCut".to_string()))
    }

    /// (Re)build the bind group against the pass's own buffers. Call after the
    /// buffers are created or grown ([`ClusterLodBuffers::ensure_capacity`]).
    pub fn recreate(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        layouts: &BindGroupLayouts,
        buffers: &ClusterLodBuffers,
    ) -> Result<()> {
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&buffers.pages_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&buffers.selected_buffer)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(&buffers.params_buffer)),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            layouts.get(self.layout_key)?,
            Some("ClusterCut"),
            entries,
        );
        self.bind_group = Some(gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}
