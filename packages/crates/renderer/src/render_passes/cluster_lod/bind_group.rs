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

#[derive(Clone)]
pub struct ClusterCutBindGroups {
    pub layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
    /// Gap-B dynamic paging: when on, the layout has a 4th entry (`resident`
    /// table) and [`Self::recreate`] binds it. Read once at construction from
    /// `features.cluster_paging` and remembered (recreate has no `ctx`).
    paging: bool,
}

impl ClusterCutBindGroups {
    pub fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let paging = ctx.features.cluster_paging;
        let compute_only = |resource| BindGroupLayoutCacheKeyEntry {
            resource,
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        };
        let mut entries = vec![
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
        if paging {
            // resident (cluster_id → slot) — storage RO. Only present in the
            // paging variant so the non-paging cut layout is unchanged.
            entries.push(compute_only(BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            )));
        }
        let layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?;
        Ok(Self {
            layout_key,
            bind_group: None,
            paging,
        })
    }

    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
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
        let mut entries = vec![
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
        if self.paging {
            // The layout has a 4th entry (resident); we can only build the bind
            // group once the residency table has been uploaded. Defer until then
            // (the loader calls `upload_resident` → `recreate` right after pages).
            match buffers.resident_buffer.as_ref() {
                Some(resident) => entries.push(BindGroupEntry::new(
                    3,
                    BindGroupResource::Buffer(BufferBinding::new(resident)),
                )),
                None => return Ok(()),
            }
        }
        let descriptor =
            BindGroupDescriptor::new(layouts.get(self.layout_key)?, Some("ClusterCut"), entries);
        self.bind_group = Some(gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}

/// Bind group for the compaction pass: pages(RO), selected(RO), source_indices
/// (RO), compacted_indices(RW), draw_args(RW), params(uniform — reused cut
/// params, only `cluster_count` read). All compute-visible.
#[derive(Clone)]
pub struct ClusterCompactionBindGroups {
    pub layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl ClusterCompactionBindGroups {
    pub fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let compute_only = |resource| BindGroupLayoutCacheKeyEntry {
            resource,
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        };
        let storage_ro = || {
            BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            )
        };
        let storage_rw = || {
            BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
            )
        };
        let entries = vec![
            compute_only(storage_ro()), // pages
            compute_only(storage_ro()), // selected
            compute_only(storage_ro()), // source_indices
            compute_only(storage_rw()), // compacted_indices
            compute_only(storage_rw()), // draw_args
            compute_only(BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            )), // params
        ];
        let layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?;
        Ok(Self {
            layout_key,
            bind_group: None,
        })
    }

    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("ClusterCompaction".to_string()))
    }

    pub fn recreate(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        layouts: &BindGroupLayouts,
        buffers: &ClusterLodBuffers,
    ) -> Result<()> {
        let buf = |i, b| BindGroupEntry::new(i, BindGroupResource::Buffer(BufferBinding::new(b)));
        let entries = vec![
            buf(0, &buffers.pages_buffer),
            buf(1, &buffers.selected_buffer),
            buf(2, &buffers.source_indices_buffer),
            buf(3, &buffers.compacted_indices_buffer),
            buf(4, &buffers.draw_args_buffer),
            buf(5, &buffers.params_buffer),
        ];
        let descriptor = BindGroupDescriptor::new(
            layouts.get(self.layout_key)?,
            Some("ClusterCompaction"),
            entries,
        );
        self.bind_group = Some(gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}
