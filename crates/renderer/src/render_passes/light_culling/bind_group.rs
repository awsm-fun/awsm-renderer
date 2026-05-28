//! Bind group layout + recreation for the light culling compute pass.
//!
//! Single bind group; layout matches the WGSL emitted by
//! [`super::shader::template::ShaderTemplateLightCullingBindGroups`]:
//!
//!   0 camera_raw       — uniform.
//!   1 cull_params      — uniform, per-frame tile/slice/capacity/near-far.
//!   2 lights_info      — uniform `LightsInfoPacked`.
//!   3 lights           — uniform `array<LightPacked, MAX_PUNCTUAL_LIGHTS>`.
//!   4 froxel_counts    — storage RW (atomics) per-froxel count.
//!   5 froxel_indices   — storage RW flat index list.
//!   6 overflow_counter — storage RW single atomic counter.

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType,
};
use awsm_renderer_core::buffers::BufferBinding;

use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::RenderPassInitContext;

/// Bind group layout + cached bind group for the light culling pass.
pub struct LightCullingBindGroups {
    pub bind_group_layout_key: BindGroupLayoutKey,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl LightCullingBindGroups {
    /// Creates the bind group layout. The bind group itself is built
    /// lazily via [`Self::recreate`] when
    /// `BindGroupCreate::LightCullingFroxelsResize` (or any of the
    /// upstream buffer/texture events that include LightCulling in their
    /// fan-out) fires.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let entries = vec![
            // 0 camera_raw — uniform.
            uniform_entry(),
            // 1 cull_params — uniform.
            uniform_entry(),
            // 2 lights_info — uniform.
            uniform_entry(),
            // 3 lights — uniform.
            uniform_entry(),
            // 4 froxel_counts — storage RW (atomics).
            storage_rw_entry(),
            // 5 froxel_indices — storage RW.
            storage_rw_entry(),
            // 6 overflow_counter — storage RW (atomic).
            storage_rw_entry(),
        ];
        let bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?;
        Ok(Self {
            bind_group_layout_key,
            bind_group: None,
        })
    }

    /// Returns the active light culling bind group.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Light Culling".to_string()))
    }

    /// (Re)builds the light culling bind group against the current
    /// camera + lights + light-culling storage buffers. Called from
    /// [`crate::bind_groups::BindGroups`] in response to a
    /// `LightCullingFroxelsResize` event (or any of the upstream
    /// resource-change events that include light culling in their
    /// fan-out — see [`crate::bind_groups::BindGroups::recreate`]).
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let buffers = ctx.light_culling_buffers;
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&buffers.params_buffer)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_info_buffer)),
            ),
            BindGroupEntry::new(
                3,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.lights.gpu_punctual_buffer)),
            ),
            BindGroupEntry::new(
                4,
                BindGroupResource::Buffer(BufferBinding::new(&buffers.counts_buffer)),
            ),
            BindGroupEntry::new(
                5,
                BindGroupResource::Buffer(BufferBinding::new(&buffers.indices_buffer)),
            ),
            BindGroupEntry::new(
                6,
                BindGroupResource::Buffer(BufferBinding::new(&buffers.overflow_buffer)),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.bind_group_layout_key)?,
            Some("Light Culling"),
            entries,
        );
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }
}

fn uniform_entry() -> BindGroupLayoutCacheKeyEntry {
    BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        ),
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    }
}

fn storage_rw_entry() -> BindGroupLayoutCacheKeyEntry {
    BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
        ),
        visibility_vertex: false,
        visibility_fragment: false,
        visibility_compute: true,
    }
}
