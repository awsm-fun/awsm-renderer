//! Augmented group-0 bind group for the **masked** (alpha-tested) shadow pass.
//!
//! Mirrors [`GeometryMaskedBindGroup`](crate::render_passes::geometry::masked_bind_group::GeometryMaskedBindGroup),
//! but the vertex-side binding at slot 0 is the per-view `shadow_view` uniform
//! (dynamic offset) instead of camera/frame_globals. The fragment-only data the
//! cutout alpha-test needs (materials pool, per-mesh material meta, the merged
//! geometry pool, texture transforms, and the texture pool) is appended onto the
//! same group 0 so the variant stays within the `maxBindGroups = 4` ceiling.
//! Groups 1-3 (transforms / meta / animation) are the geometry pass's bind
//! groups, bound at draw time — only group 0 is masked-shadow-specific.
//!
//! `shadow_view` is read from `ctx.shadows.shadow_view_buffer` at recreate time
//! (a stable buffer owned by the shadow subsystem); the render pass selects the
//! active view with the same dynamic offset the plain shadow pass uses.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
    BufferBindingLayout, BufferBindingType, SamplerBindingLayout, SamplerBindingType,
    TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};
use indexmap::IndexSet;

use crate::bind_group_layout::{
    BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
};
use crate::bind_groups::{AwsmBindGroupError, BindGroupRecreateContext};
use crate::error::Result;
use crate::render_passes::shared::material::bind_group::{TexturePoolDeps, TexturePoolVisibility};
use crate::render_passes::RenderPassInitContext;
use crate::shadows::SHADOW_VIEW_BYTES;
use crate::textures::SamplerKey;

/// Number of fixed buffer bindings on the masked-shadow group 0, before the
/// texture pool arrays/samplers. Keep in sync with
/// `shadow_masked_wgsl/bind_groups.wgsl`.
const MASKED_SHADOW_GROUP0_BUFFER_BINDINGS: u32 = 6;

/// Masked-shadow group-0 bind group (shadow_view + material data + texture pool).
pub struct ShadowMaskedBindGroup {
    pub bind_group_layout_key: BindGroupLayoutKey,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_sampler_keys: IndexSet<SamplerKey>,
    _bind_group: Option<web_sys::GpuBindGroup>,
}

impl ShadowMaskedBindGroup {
    /// Builds the masked-shadow group-0 layout against the live texture pool.
    pub fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let pool = TexturePoolDeps::new(ctx, TexturePoolVisibility::Render)?;
        let bind_group_layout_key = build_layout_key(ctx, pool.arrays_len)?;
        Ok(Self {
            bind_group_layout_key,
            texture_pool_arrays_len: pool.arrays_len,
            texture_pool_sampler_keys: pool.sampler_keys,
            _bind_group: None,
        })
    }

    /// Rebuilds the layout when the texture pool size changes (mirrors the
    /// geometry masked pass's `clone_because_texture_pool_changed`).
    pub fn clone_because_texture_pool_changed(
        &self,
        ctx: &mut RenderPassInitContext<'_>,
    ) -> Result<Self> {
        let pool = TexturePoolDeps::new(ctx, TexturePoolVisibility::Render)?;
        let bind_group_layout_key = build_layout_key(ctx, pool.arrays_len)?;
        Ok(Self {
            bind_group_layout_key,
            texture_pool_arrays_len: pool.arrays_len,
            texture_pool_sampler_keys: pool.sampler_keys,
            _bind_group: None,
        })
    }

    /// (Re)builds the group-0 bind group from the live buffers + texture pool.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let mut entries = vec![
            // 0: shadow_view (uniform, dynamic offset, vertex). Sized to one
            // view slot — the render pass selects the slot via dynamic offset.
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(
                    BufferBinding::new(&ctx.shadows.shadow_view_buffer)
                        .with_size(SHADOW_VIEW_BYTES),
                ),
            ),
            // 1: materials pool (storage, fragment)
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.materials.gpu_buffer)),
            ),
            // 2: per-mesh material meta (storage, fragment)
            BindGroupEntry::new(
                2,
                BindGroupResource::Buffer(BufferBinding::new(
                    ctx.meshes.meta.material_gpu_buffer(),
                )),
            ),
            // 3: merged geometry pool / visibility_data (storage, fragment)
            BindGroupEntry::new(
                3,
                BindGroupResource::Buffer(BufferBinding::new(
                    ctx.meshes.visibility_geometry_data_gpu_buffer(),
                )),
            ),
            // 4: texture transforms (storage, fragment)
            BindGroupEntry::new(
                4,
                BindGroupResource::Buffer(BufferBinding::new(
                    &ctx.textures.texture_transforms_gpu_buffer,
                )),
            ),
            // 5: frame_globals (uniform, fragment) — `time` for animated cutouts.
            BindGroupEntry::new(
                5,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.frame_globals.gpu_buffer)),
            ),
        ];

        // Texture pool: arrays then samplers (binding indices continue from 6).
        for view in ctx.textures.pool.texture_views() {
            entries.push(BindGroupEntry::new(
                entries.len() as u32,
                BindGroupResource::TextureView(Cow::Borrowed(view)),
            ));
        }
        for sampler_key in self.texture_pool_sampler_keys.iter() {
            let sampler = ctx.textures.get_sampler(*sampler_key)?;
            entries.push(BindGroupEntry::new(
                entries.len() as u32,
                BindGroupResource::Sampler(sampler),
            ));
        }

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.bind_group_layout_key)?,
            Some("Shadow Masked - Group 0"),
            entries,
        );
        self._bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }

    /// Returns the active masked-shadow group-0 bind group.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self._bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Shadow masked group 0".to_string()))
    }
}

/// Builds the masked-shadow group-0 layout: shadow_view (uniform, dynamic
/// offset, vertex) + 4 fragment storage buffers + frame_globals (uniform,
/// fragment), then `arrays_len` texture-array bindings, then the pool samplers.
fn build_layout_key(
    ctx: &mut RenderPassInitContext<'_>,
    arrays_len: u32,
) -> Result<BindGroupLayoutKey> {
    // Re-read the sampler set so the layout matches what `recreate` will bind.
    let pool = TexturePoolDeps::new(ctx, TexturePoolVisibility::Render)?;
    let samplers_len = pool.sampler_keys.len() as u32;

    let shadow_view_v = BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new()
                .with_binding_type(BufferBindingType::Uniform)
                .with_dynamic_offset(true),
        ),
        visibility_vertex: true,
        visibility_fragment: false,
        visibility_compute: false,
    };
    let storage_f = || BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
        ),
        visibility_vertex: false,
        visibility_fragment: true,
        visibility_compute: false,
    };
    let frame_globals_f = BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
        ),
        visibility_vertex: false,
        visibility_fragment: true,
        visibility_compute: false,
    };

    let mut entries = vec![
        shadow_view_v,   // shadow_view
        storage_f(),     // materials
        storage_f(),     // material_mesh_metas
        storage_f(),     // visibility_data (merged pool)
        storage_f(),     // texture_transforms
        frame_globals_f, // frame_globals (uniform, fragment)
    ];
    debug_assert_eq!(entries.len() as u32, MASKED_SHADOW_GROUP0_BUFFER_BINDINGS);

    for _ in 0..arrays_len {
        entries.push(BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2dArray)
                    .with_sample_type(TextureSampleType::Float),
            ),
            visibility_vertex: false,
            visibility_fragment: true,
            visibility_compute: false,
        });
    }
    for _ in 0..samplers_len {
        entries.push(BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
            ),
            visibility_vertex: false,
            visibility_fragment: true,
            visibility_compute: false,
        });
    }

    Ok(ctx
        .bind_group_layouts
        .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?)
}
