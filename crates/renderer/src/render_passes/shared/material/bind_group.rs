//! Shared material bind group helpers.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{
    BindGroupEntry, BindGroupLayoutResource, BindGroupResource, BufferBindingLayout,
    BufferBindingType, SamplerBindingLayout, SamplerBindingType, TextureBindingLayout,
};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::error::AwsmCoreError;
use awsm_renderer_core::texture::{TextureSampleType, TextureViewDimension};
use indexmap::IndexSet;

use crate::bind_group_layout::{BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry};
use crate::bind_groups::BindGroupRecreateContext;
use crate::error::Result;
use crate::{
    bind_group_layout::BindGroupLayoutKey, render_passes::RenderPassInitContext,
    textures::SamplerKey,
};

/// Bind group layout data for the texture pool.
pub struct TexturePoolDeps {
    pub bind_group_layout_key: BindGroupLayoutKey,
    pub arrays_len: u32,
    pub sampler_keys: IndexSet<SamplerKey>,
}

/// Shader stage visibility for texture pool bindings.
pub enum TexturePoolVisibility {
    Render,
    Compute,
}

impl TexturePoolVisibility {
    /// Returns true if the binding is visible to the vertex stage.
    pub fn vertex(&self) -> bool {
        matches!(self, TexturePoolVisibility::Render)
    }

    /// Returns true if the binding is visible to the fragment stage.
    pub fn fragment(&self) -> bool {
        matches!(self, TexturePoolVisibility::Render)
    }

    /// Returns true if the binding is visible to the compute stage.
    pub fn compute(&self) -> bool {
        matches!(self, TexturePoolVisibility::Compute)
    }
}

impl TexturePoolDeps {
    /// Builds texture pool bind group layout metadata from the render context.
    pub fn new(
        ctx: &mut RenderPassInitContext<'_>,
        visibility: TexturePoolVisibility,
    ) -> Result<Self> {
        // textures
        let device_limits = ctx.gpu.device.limits();
        let texture_arrays_len = ctx.textures.pool.arrays_len();

        let mut entries = Vec::new();

        if texture_arrays_len > device_limits.max_sampled_textures_per_shader_stage() as usize {
            return Err(AwsmCoreError::TexturePoolTooManyArrays {
                total_arrays: texture_arrays_len as u32,
                max_arrays: device_limits.max_sampled_textures_per_shader_stage(),
            }
            .into());
        }

        for i in 0..texture_arrays_len {
            entries.push(BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Texture(
                    TextureBindingLayout::new()
                        .with_view_dimension(TextureViewDimension::N2dArray)
                        .with_sample_type(TextureSampleType::Float),
                ),
                visibility_vertex: visibility.vertex(),
                visibility_fragment: visibility.fragment(),
                visibility_compute: visibility.compute(),
            });

            let layer_count = ctx
                .textures
                .pool
                .array_by_index(i)
                .map(|arr| arr.images.len())
                .unwrap_or_default();

            if layer_count > device_limits.max_texture_array_layers() as usize {
                return Err(AwsmCoreError::TexturePoolTooManyLayers {
                    array_index: i as u32,
                    total_layers: layer_count as u32,
                    max_layers: device_limits.max_texture_array_layers(),
                }
                .into());
            }
        }

        // samplers
        let sampler_keys = ctx.textures.pool_sampler_set.clone();

        if sampler_keys.len() > device_limits.max_samplers_per_shader_stage() as usize {
            return Err(AwsmCoreError::TexturePoolTooManySamplers {
                total_samplers: sampler_keys.len() as u32,
                max_samplers: device_limits.max_samplers_per_shader_stage(),
            }
            .into());
        }

        for _ in 0..sampler_keys.len() {
            entries.push(BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Sampler(
                    SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
                ),
                visibility_vertex: visibility.vertex(),
                visibility_fragment: visibility.fragment(),
                visibility_compute: visibility.compute(),
            });
        }

        let bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, BindGroupLayoutCacheKey { entries })?;

        Ok(Self {
            arrays_len: texture_arrays_len as u32,
            bind_group_layout_key,
            sampler_keys,
        })
    }
}

/// Layout entries for the shared shadow bind group (slot 3 in the
/// material-opaque pipeline; Phase 9 will wire it into transparent).
///
/// Layout — must stay in sync with `shared_wgsl/shadow/bind_groups.wgsl`:
/// 0 texture `shadow_atlas` (depth)
/// 1 sampler `shadow_atlas_sampler` (comparison)
/// 2 texture `shadow_cube_array` (depth cube array)
/// 3 sampler `shadow_cube_sampler` (comparison)
/// 4 texture `evsm_atlas` (filterable float)
/// 5 sampler `evsm_atlas_sampler` (filtering)
/// 6 uniform `shadow_globals`
/// 7 uniform `shadow_descriptors` (fixed-size `array<ShadowDescriptor, MAX>`)
///
/// The plan's original storage-buffer descriptor binding would have
/// pushed the opaque compute stage past the adapter's
/// `maxStorageBuffersPerShaderStage = 10`; we use a uniform array of
/// `MAX_SHADOW_DESCRIPTORS = 32` slots (~3 KB) instead so the
/// descriptor lookup is one uniform read.
pub fn shadow_bind_group_layout_entries(
    compute_visibility: bool,
) -> Vec<BindGroupLayoutCacheKeyEntry> {
    let v_compute = compute_visibility;
    let v_fragment = !compute_visibility;
    vec![
        // shadow atlas (depth 2d)
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_sample_type(TextureSampleType::Depth),
            ),
            visibility_vertex: false,
            visibility_fragment: v_fragment,
            visibility_compute: v_compute,
        },
        // shadow atlas comparison sampler
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Comparison),
            ),
            visibility_vertex: false,
            visibility_fragment: v_fragment,
            visibility_compute: v_compute,
        },
        // cube array (depth cube array)
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::CubeArray)
                    .with_sample_type(TextureSampleType::Depth),
            ),
            visibility_vertex: false,
            visibility_fragment: v_fragment,
            visibility_compute: v_compute,
        },
        // cube comparison sampler
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Comparison),
            ),
            visibility_vertex: false,
            visibility_fragment: v_fragment,
            visibility_compute: v_compute,
        },
        // EVSM atlas (rgba16f) — filterable for textureSampleLevel
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_sample_type(TextureSampleType::Float),
            ),
            visibility_vertex: false,
            visibility_fragment: v_fragment,
            visibility_compute: v_compute,
        },
        // EVSM filtering sampler
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Sampler(
                SamplerBindingLayout::new().with_binding_type(SamplerBindingType::Filtering),
            ),
            visibility_vertex: false,
            visibility_fragment: v_fragment,
            visibility_compute: v_compute,
        },
        // globals uniform
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            ),
            visibility_vertex: false,
            visibility_fragment: v_fragment,
            visibility_compute: v_compute,
        },
        // descriptors uniform (fixed-size array)
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            ),
            visibility_vertex: false,
            visibility_fragment: v_fragment,
            visibility_compute: v_compute,
        },
    ]
}

/// Builds the seven bind-group entries for the shadow bind group from
/// the live `Shadows` state in the recreation context.
pub fn build_shadow_bind_group_entries<'a>(
    ctx: &'a BindGroupRecreateContext<'a>,
) -> Vec<BindGroupEntry<'a>> {
    let shadows = ctx.shadows;
    vec![
        BindGroupEntry::new(
            0,
            BindGroupResource::TextureView(Cow::Borrowed(&shadows.atlas_view)),
        ),
        BindGroupEntry::new(1, BindGroupResource::Sampler(&shadows.sampler_comparison)),
        BindGroupEntry::new(
            2,
            BindGroupResource::TextureView(Cow::Borrowed(&shadows.cube_array_view)),
        ),
        BindGroupEntry::new(3, BindGroupResource::Sampler(&shadows.sampler_comparison)),
        BindGroupEntry::new(
            4,
            BindGroupResource::TextureView(Cow::Borrowed(&shadows.evsm_atlas_view)),
        ),
        BindGroupEntry::new(5, BindGroupResource::Sampler(&shadows.sampler_filterable)),
        BindGroupEntry::new(
            6,
            BindGroupResource::Buffer(BufferBinding::new(&shadows.globals_buffer)),
        ),
        BindGroupEntry::new(
            7,
            BindGroupResource::Buffer(BufferBinding::new(&shadows.descriptors_uniform)),
        ),
    ]
}
