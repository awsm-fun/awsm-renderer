//! GPU instance compaction (§16.7 Phase 2 + §16.8 infrastructure).
//!
//! Reads `visible_this_frame[]` from the occlusion-cull pass and
//! atomically bumps the matching per-mesh `IndirectDrawArgs.instance_count`.
//! The args buffer is laid out as
//! [`web_sys::GpuBuffer`]-of-`DrawIndexedIndirectArgs` (5 × u32 per slot),
//! one slot per `MeshKey` index. CPU initializes the static fields
//! (`index_count`, `first_index`, `base_vertex`, `first_instance`) at
//! mesh insert time; the compaction zeros + repopulates `instance_count`
//! per frame.
//!
//! The geometry pass's `drawIndirect` path consumes this buffer
//! when `features.gpu_culling` is on: each non-instanced mesh's
//! slot carries `(index_count, instance_count, first_index=0,
//! base_vertex=0, first_instance=slot)`, with `instance_count`
//! GPU-written by this compaction shader. The geometry vertex
//! shader reads `geometry_mesh_metas[instance_index]` from a
//! storage-array binding indexed by `@builtin(instance_index)`;
//! `first_instance = slot` lands the lookup on the right meta.
//! Instanced meshes stay on the legacy
//! `draw_indexed_with_instance_count` path (their `instance_index`
//! ranges would collide between meshes in the shared lookup).

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    command::compute_pass::ComputePassDescriptor,
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
    },
    bind_groups::{AwsmBindGroupError, BindGroupRecreateContext},
    error::Result,
    pipeline_layouts::PipelineLayoutCacheKey,
    pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey},
    render::RenderContext,
    render_passes::{
        occlusion::shader::cache_key::ShaderCacheKeyOcclusionCompaction, RenderPassInitContext,
    },
};

/// Stride per draw-indirect entry: `(index_count, instance_count,
/// first_index, base_vertex, first_instance)` = 5 × u32 = 20 B.
/// Padded to 32 B for nice alignment.
pub const INDIRECT_DRAW_ARGS_STRIDE: usize = 32;

/// Starting capacity in mesh slots. Grows 2× when needed.
const INITIAL_CAPACITY: u32 = 1024;

pub struct CompactionBuffers {
    /// `drawIndirect`-shaped buffer. `INDIRECT` + `STORAGE` + `COPY_DST`
    /// usage so the compaction shader writes it and the future geometry
    /// pass reads it as indirect args.
    pub args_buffer: web_sys::GpuBuffer,
    pub capacity: u32,
    /// `true` once a frame has completed a CPU args prep + GPU
    /// compaction pass — meaning the buffer holds a valid
    /// previous-frame visibility set. The geometry pass under
    /// `features.gpu_culling` falls back to the legacy CPU
    /// `draw_indexed_with_first_instance` path until this flips,
    /// so frame 0 (and any frame after a `ensure_capacity` resize,
    /// which zeroes the args buffer) doesn't drawIndirect against
    /// a zero `instance_count`.
    ///
    /// `Cell` so render.rs can set it after the compaction dispatch
    /// while `RenderContext` still holds an immutable borrow on the
    /// buffers (the alternative would be to drop the ctx early or
    /// add a separate bookkeeping field on `AwsmRenderer`).
    pub args_ready: std::cell::Cell<bool>,
}

impl CompactionBuffers {
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        Self::with_capacity(gpu, INITIAL_CAPACITY)
    }

    fn with_capacity(gpu: &AwsmRendererWebGpu, capacity: u32) -> Result<Self> {
        let capacity = capacity.max(1);
        let size_bytes = capacity as usize * INDIRECT_DRAW_ARGS_STRIDE;
        let args_buffer = gpu
            .create_buffer(
                &BufferDescriptor::new(
                    Some("CompactionIndirectArgs"),
                    size_bytes,
                    BufferUsage::new()
                        .with_storage()
                        .with_indirect()
                        .with_copy_dst(),
                )
                .into(),
            )
            .map_err(AwsmCoreError::from)?;
        Ok(Self {
            args_buffer,
            capacity,
            args_ready: std::cell::Cell::new(false),
        })
    }

    /// Grows the args buffer when the mesh slot count exceeds capacity.
    /// Returns `true` when reallocated.
    pub fn ensure_capacity(&mut self, gpu: &AwsmRendererWebGpu, needed: u32) -> Result<bool> {
        if needed <= self.capacity {
            return Ok(false);
        }
        let new_capacity = needed.saturating_mul(2).max(needed);
        *self = Self::with_capacity(gpu, new_capacity)?;
        Ok(true)
    }
}

pub struct CompactionBindGroups {
    pub layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl CompactionBindGroups {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        use awsm_renderer_core::bind_groups::{
            BindGroupLayoutResource, BufferBindingLayout, BufferBindingType,
        };
        let entries = vec![
            // occlusion_instances (RO)
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // visible_this_frame (RO)
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // indirect_args (RW, atomics)
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
            // params (active_count uniform — shared with cull pass)
            BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                ),
                visibility_vertex: false,
                visibility_fragment: false,
                visibility_compute: true,
            },
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
            .ok_or_else(|| AwsmBindGroupError::NotFound("Occlusion Compaction".to_string()))
    }

    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        use awsm_renderer_core::bind_groups::{
            BindGroupDescriptor, BindGroupEntry, BindGroupResource,
        };
        use awsm_renderer_core::buffers::BufferBinding;
        // Only invoked when `features.gpu_culling` is on (plan §16.F).
        let occlusion_buffers = ctx
            .occlusion_buffers
            .expect("Occlusion buffers missing despite gpu_culling feature on");
        let compaction_buffers = ctx
            .compaction_buffers
            .expect("Compaction buffers missing despite gpu_culling feature on");
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&occlusion_buffers.instances_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&occlusion_buffers.visible_buffer)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(&compaction_buffers.args_buffer)),
            ),
            BindGroupEntry::new(
                3,
                BindGroupResource::Buffer(BufferBinding::new(&occlusion_buffers.params_buffer)),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.layout_key)?,
            Some("Occlusion Compaction"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}

pub struct CompactionPipeline {
    pub key: ComputePipelineKey,
}

impl CompactionPipeline {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &CompactionBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyOcclusionCompaction)
            .await?;
        let key = ctx
            .pipelines
            .compute
            .get_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key),
            )
            .await?;
        Ok(Self { key })
    }
}

pub struct CompactionRenderPass {
    pub bind_groups: CompactionBindGroups,
    pub pipeline: CompactionPipeline,
}

impl CompactionRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = CompactionBindGroups::new(ctx).await?;
        let pipeline = CompactionPipeline::new(ctx, &bind_groups).await?;
        Ok(Self {
            bind_groups,
            pipeline,
        })
    }

    /// Dispatches the compaction shader over `instance_count`
    /// per-instance threads (workgroup_size 64). The shader reads
    /// `visible_this_frame[i]` and atomicAdds 1 to the matching
    /// per-mesh `IndirectDrawArgs.instance_count` slot when visible.
    pub fn render(&self, ctx: &RenderContext, instance_count: u32) -> Result<()> {
        if instance_count == 0 {
            return Ok(());
        }
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Occlusion Compaction")).into(),
        ));
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipeline.key)?);
        compute_pass.set_bind_group(0, self.bind_groups.get_bind_group()?, None)?;
        let workgroups = instance_count.div_ceil(64);
        compute_pass.dispatch_workgroups(workgroups, Some(1), Some(1));
        compute_pass.end();
        Ok(())
    }
}
