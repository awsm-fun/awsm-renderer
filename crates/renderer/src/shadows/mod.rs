//! Shadow mapping subsystem.
//!
//! The `Shadows` struct sits on [`AwsmRenderer`](crate::AwsmRenderer)
//! and owns every GPU resource needed for shadow generation and
//! sampling: a 2D PCF/PCSS atlas, an RGBA16F EVSM atlas (allocated
//! lazily), a depth cubemap-array slot pool for point lights, the
//! descriptor uniform buffer that the material-opaque shading pass
//! reads at sample time, and the depth-only render pipeline used for
//! shadow generation.

pub mod cascade;
pub mod config;
pub mod error;
pub mod light_shadow;
pub mod render_pass;
pub mod shader;

use std::sync::LazyLock;

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
        BufferBindingLayout, BufferBindingType,
    },
    buffers::{BufferBinding, BufferDescriptor, BufferUsage},
    compare::CompareFunction,
    error::AwsmCoreError,
    pipeline::{
        depth_stencil::DepthStencilState,
        primitive::{CullMode, FrontFace, PrimitiveState, PrimitiveTopology},
    },
    renderer::AwsmRendererWebGpu,
    sampler::{FilterMode, SamplerDescriptor},
    texture::{
        Extent3d, TextureDescriptor, TextureFormat, TextureUsage, TextureViewDescriptor,
        TextureViewDimension,
    },
};
use glam::Mat4;
use slotmap::SecondaryMap;

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
        BindGroupLayouts,
    },
    bind_groups::BindGroups,
    debug::AwsmRendererLogging,
    lights::LightKey,
    pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts},
    pipelines::{
        render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey},
        Pipelines,
    },
    render_passes::geometry::{
        bind_group::GeometryBindGroups,
        pipeline::{VERTEX_BUFFER_LAYOUT, VERTEX_BUFFER_LAYOUT_INSTANCING},
    },
    render_textures::RenderTextureFormats,
    shaders::Shaders,
    AwsmRenderer,
};

pub use self::{
    cascade::Cascade,
    config::ShadowsConfig,
    error::AwsmShadowError,
    light_shadow::{
        EvsmCutoff, FarCascadeUpdateRate, LightShadowHardness, LightShadowParams, MeshShadowFlags,
    },
    shader::{cache_key::ShaderCacheKeyShadow, template::ShaderTemplateShadow},
};

/// Maximum number of shadow descriptors stored in the per-frame
/// uniform array. 32 entries × 96 B = 3 KB — well under the
/// `maxUniformBufferBindingSize` ceiling (default 64 KB).
pub const MAX_SHADOW_DESCRIPTORS: u32 = 32;

/// Size in bytes of a single packed `ShadowDescriptor` (see
/// `shared_wgsl/shadow/bind_groups.wgsl`):
/// - `view_projection: mat4x4<f32>` (64 B)
/// - `atlas_rect: vec4<f32>` (16 B)
/// - `bias_params: vec4<f32>` (16 B)
/// - `cascade_info: vec4<f32>` (16 B)
pub const SHADOW_DESCRIPTOR_BYTES: usize = 112;

/// Size in bytes of the `ShadowGlobals` uniform block.
pub const SHADOW_GLOBALS_BYTES: usize = 48;

/// Size in bytes of the per-pass shadow-view uniform.
pub const SHADOW_VIEW_BYTES: usize = 80;

/// Per-face cube shadow map resolution. Hardcoded for v1; phase 13
/// may surface this as a config knob alongside `atlas_size`.
pub const POINT_SHADOW_RESOLUTION: u32 = 512;

/// Sentinel meaning "this light has no shadow descriptor allocated"
/// in the packed `LightPacked` row 4. The shading shader uses this to
/// short-circuit shadow sampling.
pub const SHADOW_INDEX_NONE: u32 = u32::MAX;

/// Owns every GPU resource for shadow generation and sampling.
pub struct Shadows {
    /// Renderer-wide configuration. Replace via [`Shadows::set_config`].
    pub config: ShadowsConfig,
    /// Depth atlas used for PCF and PCSS sampling.
    pub atlas_texture: web_sys::GpuTexture,
    /// Default view of the atlas.
    pub atlas_view: web_sys::GpuTextureView,
    /// Atlas resolution in texels (square). Phase 2 uses the full atlas
    /// for the one supported caster; phase 4 swaps in a packer.
    pub atlas_size: u32,
    /// EVSM atlas (`RGBA16F`) — moments storage for far directional
    /// cascades. Allocated even in phase 0 so the bind group has a
    /// stable layout.
    pub evsm_atlas_texture: web_sys::GpuTexture,
    /// Default view of the EVSM atlas.
    pub evsm_atlas_view: web_sys::GpuTextureView,
    /// Cubemap array used for point-light shadows.
    pub cube_array_texture: web_sys::GpuTexture,
    /// Cube-array view spanning every slice — used as the
    /// `texture_depth_cube_array` binding in the material-opaque
    /// shading pass.
    pub cube_array_view: web_sys::GpuTextureView,
    /// One 2D-array depth view per cube face (6 per slot). Indexed
    /// as `slot * 6 + face`. Used as the render attachment when
    /// generating each face's shadow map.
    pub cube_face_views: Vec<web_sys::GpuTextureView>,
    /// Per-slot owner. `None` means the slot is free; `Some(key)`
    /// means it currently holds the shadow for that point light.
    pub cube_slots: Vec<Option<LightKey>>,
    /// Storage buffer of per-shadow descriptors. Kept for forward
    /// compatibility with the plan's storage-buffer layout; the
    /// material-opaque bind group reads from `descriptors_uniform`
    /// instead so we stay under the storage-buffer-per-stage limit.
    pub descriptors_buffer: web_sys::GpuBuffer,
    /// Uniform buffer of per-shadow descriptors read by the shading
    /// passes. Fixed size: `MAX_SHADOW_DESCRIPTORS` entries.
    pub descriptors_uniform: web_sys::GpuBuffer,
    /// Uniform buffer of shadow globals (atlas sizes, EVSM params,
    /// SSCS flags) read by the shading passes.
    pub globals_buffer: web_sys::GpuBuffer,
    /// Per-pass uniform buffer of the current shadow view's matrix +
    /// bias parameters. Rewritten before each render pass.
    pub shadow_view_buffer: web_sys::GpuBuffer,
    /// Comparison sampler for `textureSampleCompare` on the atlases.
    pub sampler_comparison: web_sys::GpuSampler,
    /// Linear filterable sampler for EVSM moment sampling.
    pub sampler_filterable: web_sys::GpuSampler,

    /// Per-light authored shadow parameters.
    params: SecondaryMap<LightKey, LightShadowParams>,
    /// Per-light, per-frame fitted record (cascade fit, atlas rect,
    /// descriptor index). Rebuilt every `write_gpu` call.
    records: SecondaryMap<LightKey, LightShadowRecord>,
    /// Number of descriptors currently active in `descriptors_uniform`.
    active_descriptor_count: u32,

    /// Bind-group layout for slot 0 of the shadow generation pipeline
    /// — a single `ShadowView` uniform. Held for diagnostic /
    /// recreation use; the bind group itself is created eagerly in
    /// `new`.
    #[allow(dead_code)]
    shadow_view_bind_group_layout_key: BindGroupLayoutKey,
    /// Cached shadow_view bind group.
    shadow_view_bind_group: web_sys::GpuBindGroup,
    /// Shadow generation pipeline layout — `[shadow_view, transforms,
    /// meta, animation]`. Held for parity with other passes; the
    /// pipelines themselves are built once in `new`.
    #[allow(dead_code)]
    shadow_pipeline_layout_key: PipelineLayoutKey,
    /// Depth-only shadow pipeline (non-instancing).
    shadow_pipeline_no_instancing: RenderPipelineKey,
    /// Depth-only shadow pipeline (instancing).
    shadow_pipeline_instancing: RenderPipelineKey,

    /// Frame counter used by temporal throttling (phase 11).
    pub frame_count: u64,
    /// Whether descriptors / globals need to be re-uploaded.
    pub dirty: bool,
}

/// Per-light shadow state recorded each frame.
#[derive(Clone, Debug)]
pub struct LightShadowRecord {
    /// One entry per cascade / face / spot. Phase 2 always has one.
    pub views: Vec<LightShadowView>,
    /// Base index into the descriptor uniform array; the shading
    /// shader fetches `shadow_descriptors[descriptor_base]`.
    pub descriptor_base: u32,
}

/// One renderable shadow view for a light (cascade / face / spot).
#[derive(Clone, Debug)]
pub struct LightShadowView {
    /// Light-space view-projection matrix.
    pub view_projection: Mat4,
    /// Atlas rectangle in texels (x, y, w, h). Used as the viewport
    /// for 2D shadow generation; ignored for cube faces (the cube
    /// face view is rendered at the texture's native resolution).
    pub atlas_rect: [u32; 4],
    /// Cube face layer index when this view targets the cube pool —
    /// `slot * 6 + face_index`. `None` for 2D atlas views.
    pub cube_layer: Option<u32>,
}

static SHADOW_DESCRIPTOR_UNIFORM_BYTES: LazyLock<usize> =
    LazyLock::new(|| MAX_SHADOW_DESCRIPTORS as usize * SHADOW_DESCRIPTOR_BYTES);

impl Shadows {
    /// Creates the shadow subsystem.
    ///
    /// Must be called after the geometry render pass has been built so
    /// the shadow pipeline can reuse the geometry pass's transform /
    /// meta / animation bind group layouts at slots 1..=3.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        pipelines: &mut Pipelines,
        shaders: &mut Shaders,
        geometry_bind_groups: &GeometryBindGroups,
        _render_texture_formats: &RenderTextureFormats,
    ) -> Result<Self, AwsmShadowError> {
        let config = ShadowsConfig::default();

        let atlas_size = config.atlas_size.max(1);
        let atlas_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Depth32float,
                Extent3d::new(atlas_size, Some(atlas_size), Some(1)),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .with_label("Shadow Atlas")
            .into(),
        )?;
        let atlas_view = atlas_texture
            .create_view()
            .map_err(AwsmCoreError::create_texture_view)?;

        let evsm_atlas_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Rgba16float,
                Extent3d::new(1, Some(1), Some(1)),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .with_label("Shadow EVSM Atlas (placeholder)")
            .into(),
        )?;
        let evsm_atlas_view = evsm_atlas_texture
            .create_view()
            .map_err(AwsmCoreError::create_texture_view)?;

        let cube_slot_count = config.max_point_shadows.max(1);
        let cube_layer_count = cube_slot_count * 6;
        let cube_array_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Depth32float,
                Extent3d::new(
                    POINT_SHADOW_RESOLUTION,
                    Some(POINT_SHADOW_RESOLUTION),
                    Some(cube_layer_count),
                ),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .with_label("Shadow Cube Pool")
            .into(),
        )?;
        let cube_array_view = create_cube_array_view(&cube_array_texture)?;
        let cube_face_views = build_cube_face_views(&cube_array_texture, cube_layer_count)?;

        let descriptors_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Descriptors (storage)"),
                SHADOW_DESCRIPTOR_BYTES,
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;

        let descriptors_uniform = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Descriptors (uniform)"),
                *SHADOW_DESCRIPTOR_UNIFORM_BYTES,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        let globals_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Globals"),
                SHADOW_GLOBALS_BYTES,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        let shadow_view_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow View (per-pass)"),
                SHADOW_VIEW_BYTES,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        let sampler_comparison = gpu.create_sampler(Some(
            &SamplerDescriptor {
                label: Some("Shadow Comparison Sampler"),
                compare: Some(CompareFunction::LessEqual),
                mag_filter: Some(FilterMode::Linear),
                min_filter: Some(FilterMode::Linear),
                ..SamplerDescriptor::default()
            }
            .into(),
        ));

        let sampler_filterable = gpu.create_sampler(Some(
            &SamplerDescriptor {
                label: Some("Shadow Filterable Sampler"),
                mag_filter: Some(FilterMode::Linear),
                min_filter: Some(FilterMode::Linear),
                ..SamplerDescriptor::default()
            }
            .into(),
        ));

        // Slot 0 of the shadow pipeline: a single uniform with the
        // current view's view-projection + bias params.
        let shadow_view_bind_group_layout_key = bind_group_layouts.get_key(
            gpu,
            BindGroupLayoutCacheKey {
                entries: vec![BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: false,
                    visibility_compute: false,
                }],
            },
        )?;

        let shadow_view_bind_group = {
            let layout = bind_group_layouts.get(shadow_view_bind_group_layout_key)?;
            let entries = vec![BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&shadow_view_buffer)),
            )];
            let descriptor = BindGroupDescriptor::new(layout, Some("Shadow View"), entries);
            gpu.create_bind_group(&descriptor.into())
        };

        // Pipeline layout: [shadow_view, transforms, meta, animation].
        // Slots 1..=3 reuse the geometry pass's layouts so the same
        // model_transforms / geometry_mesh_meta / morph + skin buffers
        // are accessible verbatim from the shadow VS.
        let shadow_pipeline_layout_cache_key = PipelineLayoutCacheKey::new(vec![
            shadow_view_bind_group_layout_key,
            geometry_bind_groups.transforms.bind_group_layout_key,
            geometry_bind_groups.meta.bind_group_layout_key,
            geometry_bind_groups.animation.bind_group_layout_key,
        ]);
        let shadow_pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            shadow_pipeline_layout_cache_key,
        )?;

        let shadow_pipeline_no_instancing = build_shadow_pipeline(
            gpu,
            shaders,
            pipelines,
            pipeline_layouts,
            shadow_pipeline_layout_key,
            false,
        )
        .await?;
        let shadow_pipeline_instancing = build_shadow_pipeline(
            gpu,
            shaders,
            pipelines,
            pipeline_layouts,
            shadow_pipeline_layout_key,
            true,
        )
        .await?;

        Ok(Self {
            config,
            atlas_texture,
            atlas_view,
            atlas_size,
            evsm_atlas_texture,
            evsm_atlas_view,
            cube_array_texture,
            cube_array_view,
            cube_face_views,
            cube_slots: vec![None; cube_slot_count as usize],
            descriptors_buffer,
            descriptors_uniform,
            globals_buffer,
            shadow_view_buffer,
            sampler_comparison,
            sampler_filterable,
            params: SecondaryMap::new(),
            records: SecondaryMap::new(),
            active_descriptor_count: 0,
            shadow_view_bind_group_layout_key,
            shadow_view_bind_group,
            shadow_pipeline_layout_key,
            shadow_pipeline_no_instancing,
            shadow_pipeline_instancing,
            frame_count: 0,
            dirty: true,
        })
    }

    /// Replaces the renderer-wide config.
    pub fn set_config(&mut self, config: ShadowsConfig) {
        self.config = config;
        self.dirty = true;
    }

    /// Returns a reference to the renderer-wide config.
    pub fn config(&self) -> &ShadowsConfig {
        &self.config
    }

    /// Number of lights currently registered as shadow casters
    /// (whether or not their `cast` flag is on).
    pub fn caster_count(&self) -> usize {
        self.params
            .values()
            .filter(|p| p.cast)
            .count()
    }

    /// `[0.0, 1.0]` — fraction of the 2D atlas occupied by active
    /// cascades + spots. Phase 2: returns 1.0 if any caster is active,
    /// 0 otherwise.
    pub fn atlas_utilization(&self) -> f32 {
        if self.caster_count() > 0 {
            1.0
        } else {
            0.0
        }
    }

    /// Fraction of cube-array slots occupied. Phase 8 wires this up.
    pub fn cube_pool_utilization(&self) -> f32 {
        0.0
    }

    /// `true` if any shadow-casting light is currently active. The
    /// render graph short-circuits the entire shadow generation pass
    /// when this is `false`.
    pub fn any_active(&self) -> bool {
        self.caster_count() > 0
    }

    /// Returns the shadow descriptor index registered for a light, or
    /// `SHADOW_INDEX_NONE` if the light has no active shadow.
    pub fn descriptor_index_for_light(&self, key: LightKey) -> u32 {
        self.records
            .get(key)
            .map(|r| r.descriptor_base)
            .unwrap_or(SHADOW_INDEX_NONE)
    }

    /// Returns the shadow pipeline key for the given instancing mode.
    pub fn shadow_pipeline_key(&self, instancing: bool) -> RenderPipelineKey {
        if instancing {
            self.shadow_pipeline_instancing
        } else {
            self.shadow_pipeline_no_instancing
        }
    }

    /// Returns the shadow_view bind group. Created eagerly in
    /// `Shadows::new` so the render pass only needs shared borrows.
    pub fn shadow_view_bind_group(&self) -> &web_sys::GpuBindGroup {
        &self.shadow_view_bind_group
    }

    /// Per-frame upload point. Refits cascades against the current
    /// camera, packs descriptors into the uniform buffer, and writes
    /// shadow globals when dirty.
    pub fn write_gpu(
        &mut self,
        _logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        _bind_groups: &mut BindGroups,
        camera: &crate::camera::CameraBuffer,
        lights: &crate::lights::Lights,
    ) -> Result<(), AwsmShadowError> {
        if self.dirty {
            // Globals layout (matches WGSL `ShadowGlobals`).
            let mut data = [0u8; SHADOW_GLOBALS_BYTES];
            let atlas = self.atlas_size as f32;
            let evsm = self.config.evsm_atlas_size as f32;
            data[0..4].copy_from_slice(&atlas.to_ne_bytes());
            data[4..8].copy_from_slice(&atlas.to_ne_bytes());
            data[8..12].copy_from_slice(&evsm.to_ne_bytes());
            data[12..16].copy_from_slice(&evsm.to_ne_bytes());
            data[16..20].copy_from_slice(&self.config.evsm_exponent.to_ne_bytes());
            data[20..24].copy_from_slice(&(self.config.evsm_blur_radius as f32).to_ne_bytes());
            data[24..28].copy_from_slice(&(self.config.sscs_step_count as f32).to_ne_bytes());
            data[28..32]
                .copy_from_slice(&(self.config.sscs_enabled as u32 as f32).to_ne_bytes());
            data[32..36]
                .copy_from_slice(&(self.config.debug_cascade_colors as u32).to_ne_bytes());
            data[36..40].copy_from_slice(&self.config.max_point_shadows.to_ne_bytes());
            gpu.write_buffer(&self.globals_buffer, None, data.as_slice(), None, None)?;
            self.dirty = false;
        }

        // Refit cascades for every casting directional light against
        // the current camera. Phase 2 supports one directional caster
        // with a single cascade covering the entire view. If the
        // camera hasn't been updated yet (very first frame, before
        // `update_camera`) we skip — the next frame picks up.
        let Some(camera_matrices) = camera.last_matrices.as_ref() else {
            self.frame_count = self.frame_count.wrapping_add(1);
            return Ok(());
        };
        let _camera_inv_view_proj = camera_matrices.inv_view_projection();

        self.records.clear();
        self.active_descriptor_count = 0;
        let mut descriptor_bytes = vec![0u8; *SHADOW_DESCRIPTOR_UNIFORM_BYTES];

        // Approximate the camera's near/far in world-space depth.
        // The actual values live on the camera but aren't exposed
        // directly here; reconstruct from the projection's column.
        // For a standard RH perspective with `Mat4::perspective_rh`
        // (which glam uses): proj[2][3] is `-2*near*far/(far-near)`
        // and proj[2][2] is `-(far+near)/(far-near)`; solving gives
        // the planes. Falls back to (0.1, 100.0) for orthographic.
        let (camera_near, camera_far) = extract_near_far(&camera_matrices.projection);

        // Cursor for the row-pack atlas allocator. Phase 13 will
        // replace this with a real packer; for now we walk left-to-
        // right and wrap to the next row when the current row fills.
        let mut atlas_x: u32 = 0;
        let mut atlas_y: u32 = 0;
        let mut row_height: u32 = 0;
        // Reset cube slot ownership for lights that no longer cast.
        // The match loop below re-claims slots for surviving casters.
        for slot in self.cube_slots.iter_mut() {
            if let Some(key) = *slot {
                if !self.params.get(key).map(|p| p.cast).unwrap_or(false) {
                    *slot = None;
                }
            }
        }
        let mut cube_overflow = false;
        let mut place = |w: u32, h: u32, atlas_size: u32| -> Option<[u32; 4]> {
            if atlas_x + w > atlas_size {
                atlas_x = 0;
                atlas_y += row_height;
                row_height = 0;
            }
            if atlas_y + h > atlas_size {
                return None;
            }
            let rect = [atlas_x, atlas_y, w, h];
            atlas_x += w;
            row_height = row_height.max(h);
            Some(rect)
        };

        for (light_key, params) in self.params.iter() {
            if !params.cast {
                continue;
            }
            let Some(light) = lights.get(light_key) else {
                continue;
            };

            match light {
                crate::lights::Light::Directional { direction, .. } => {
                    let cascade_count = params.cascade_count.max(1).min(4) as u32;
                    if self.active_descriptor_count + cascade_count > MAX_SHADOW_DESCRIPTORS {
                        tracing::warn!(
                            "shadow descriptor capacity exhausted: needed {} more, have {} slots free",
                            cascade_count,
                            MAX_SHADOW_DESCRIPTORS - self.active_descriptor_count
                        );
                        break;
                    }
                    let dir = glam::Vec3::from(*direction);
                    let cascades = cascade::fit_cascades(
                        camera_matrices.view_projection(),
                        camera_matrices.view,
                        if dir.length_squared() > 1e-8 {
                            dir.normalize()
                        } else {
                            glam::Vec3::new(0.3, -1.0, 0.3).normalize()
                        },
                        camera_near.max(0.01),
                        camera_far.min(params.max_distance).max(camera_near + 1.0),
                        cascade_count,
                        params.cascade_split_lambda.clamp(0.0, 1.0),
                        params.resolution.max(16),
                        16,
                    );

                    let descriptor_base = self.active_descriptor_count;
                    let mut views = Vec::with_capacity(cascades.len());
                    let evsm_first = match params.evsm_cutoff {
                        EvsmCutoff::Off => u32::MAX,
                        EvsmCutoff::LastCascade => cascade_count.saturating_sub(1),
                        EvsmCutoff::LastTwoCascades => cascade_count.saturating_sub(2),
                    };
                    for (cascade_index, (cascade, res, split_far)) in cascades.iter().enumerate() {
                        let Some(rect) = place(*res, *res, self.atlas_size) else {
                            tracing::warn!(
                                "shadow atlas overflow on cascade {}",
                                cascade_index
                            );
                            break;
                        };

                        let descriptor_index = self.active_descriptor_count;
                        let off = descriptor_index as usize * SHADOW_DESCRIPTOR_BYTES;
                        let is_evsm = (cascade_index as u32) >= evsm_first;
                        write_shadow_descriptor(
                            &mut descriptor_bytes[off..off + SHADOW_DESCRIPTOR_BYTES],
                            &cascade.view_projection,
                            rect,
                            self.atlas_size,
                            params.depth_bias,
                            params.normal_bias,
                            params.hardness,
                            params.pcss_penumbra_scale,
                            cascade_index as u32,
                            cascade_count,
                            *split_far,
                        );
                        if is_evsm {
                            let evsm_flag_off = off + 108;
                            descriptor_bytes[evsm_flag_off..evsm_flag_off + 4]
                                .copy_from_slice(&1.0_f32.to_ne_bytes());
                        }

                        views.push(LightShadowView {
                            view_projection: cascade.view_projection,
                            atlas_rect: rect,
                            cube_layer: None,
                        });
                        self.active_descriptor_count += 1;
                    }

                    self.records.insert(
                        light_key,
                        LightShadowRecord {
                            views,
                            descriptor_base,
                        },
                    );
                }
                crate::lights::Light::Spot {
                    position,
                    direction,
                    range,
                    outer_angle,
                    ..
                } => {
                    if self.active_descriptor_count >= MAX_SHADOW_DESCRIPTORS {
                        tracing::warn!("shadow descriptor capacity exhausted (spot)");
                        break;
                    }
                    let res = params.resolution.max(16);
                    let Some(rect) = place(res, res, self.atlas_size) else {
                        tracing::warn!("shadow atlas overflow on spot light");
                        continue;
                    };
                    let pos = glam::Vec3::from(*position);
                    let dir_v = glam::Vec3::from(*direction);
                    let dir = if dir_v.length_squared() > 1e-8 {
                        dir_v.normalize()
                    } else {
                        glam::Vec3::new(0.0, -1.0, 0.0)
                    };
                    let up = if dir.x.abs() < 0.9 {
                        glam::Vec3::X
                    } else {
                        glam::Vec3::Z
                    };
                    let view = glam::Mat4::look_at_rh(pos, pos + dir, up);
                    let fov = (*outer_angle * 2.0).clamp(0.01, std::f32::consts::PI - 0.01);
                    let near = 0.05_f32.min(*range * 0.01).max(0.005);
                    let far = (*range).max(near + 0.1);
                    let projection = glam::Mat4::perspective_rh(fov, 1.0, near, far);
                    let view_projection = projection * view;

                    let descriptor_index = self.active_descriptor_count;
                    let off = descriptor_index as usize * SHADOW_DESCRIPTOR_BYTES;
                    write_shadow_descriptor(
                        &mut descriptor_bytes[off..off + SHADOW_DESCRIPTOR_BYTES],
                        &view_projection,
                        rect,
                        self.atlas_size,
                        params.depth_bias,
                        params.normal_bias,
                        params.hardness,
                        params.pcss_penumbra_scale,
                        0,
                        1,
                        // Spot lights don't use cascade selection; setting
                        // `split_far` to +infinity-ish makes the shader's
                        // walk pick this descriptor unconditionally.
                        f32::MAX,
                    );

                    self.records.insert(
                        light_key,
                        LightShadowRecord {
                            views: vec![LightShadowView {
                                view_projection,
                                atlas_rect: rect,
                                cube_layer: None,
                            }],
                            descriptor_base: descriptor_index,
                        },
                    );
                    self.active_descriptor_count += 1;
                }
                crate::lights::Light::Point {
                    position, range, ..
                } => {
                    if self.active_descriptor_count >= MAX_SHADOW_DESCRIPTORS {
                        tracing::warn!("shadow descriptor capacity exhausted (point)");
                        break;
                    }
                    let slot = self
                        .cube_slots
                        .iter()
                        .position(|s| s.map(|k| k == light_key).unwrap_or(false))
                        .or_else(|| self.cube_slots.iter().position(|s| s.is_none()));
                    let Some(slot_index) = slot else {
                        cube_overflow = true;
                        continue;
                    };
                    self.cube_slots[slot_index] = Some(light_key);

                    let pos = glam::Vec3::from(*position);
                    let r = (*range).max(0.05);
                    let projection = glam::Mat4::perspective_rh(
                        std::f32::consts::FRAC_PI_2,
                        1.0,
                        0.05,
                        r,
                    );
                    // glTF cube-map face conventions, in the order
                    // WebGPU lays out cube layers: +X, -X, +Y, -Y, +Z, -Z.
                    let face_dirs = [
                        (glam::Vec3::X, -glam::Vec3::Y),
                        (-glam::Vec3::X, -glam::Vec3::Y),
                        (glam::Vec3::Y, glam::Vec3::Z),
                        (-glam::Vec3::Y, -glam::Vec3::Z),
                        (glam::Vec3::Z, -glam::Vec3::Y),
                        (-glam::Vec3::Z, -glam::Vec3::Y),
                    ];

                    let descriptor_base = self.active_descriptor_count;
                    let mut views: Vec<LightShadowView> = Vec::with_capacity(6);
                    for (face_idx, (dir, up)) in face_dirs.iter().enumerate() {
                        let view = glam::Mat4::look_at_rh(pos, pos + *dir, *up);
                        let vp = projection * view;
                        views.push(LightShadowView {
                            view_projection: vp,
                            atlas_rect: [0, 0, POINT_SHADOW_RESOLUTION, POINT_SHADOW_RESOLUTION],
                            cube_layer: Some(slot_index as u32 * 6 + face_idx as u32),
                        });
                    }

                    // Only one descriptor per point light. Sample-site
                    // uses world-space direction to pick the face.
                    let descriptor_index = self.active_descriptor_count;
                    let off = descriptor_index as usize * SHADOW_DESCRIPTOR_BYTES;
                    write_shadow_descriptor(
                        &mut descriptor_bytes[off..off + SHADOW_DESCRIPTOR_BYTES],
                        // view_projection unused for cube; zero is fine.
                        &glam::Mat4::ZERO,
                        // Repurpose atlas_rect for (light_pos.xyz, range)
                        // — packed at the same byte offsets so the
                        // shader can pull them straight from the same
                        // vec4 it'd otherwise use for UV math.
                        [0, 0, 0, 0],
                        self.atlas_size,
                        params.depth_bias,
                        params.normal_bias,
                        params.hardness,
                        params.pcss_penumbra_scale,
                        0,
                        1,
                        f32::MAX,
                    );
                    // Patch in the cube-specific atlas_rect (light_pos +
                    // range) and the "kind = cube + slice index" in
                    // `cascade_info.w / .y`.
                    descriptor_bytes[off + 64..off + 68]
                        .copy_from_slice(&pos.x.to_ne_bytes());
                    descriptor_bytes[off + 68..off + 72]
                        .copy_from_slice(&pos.y.to_ne_bytes());
                    descriptor_bytes[off + 72..off + 76]
                        .copy_from_slice(&pos.z.to_ne_bytes());
                    descriptor_bytes[off + 76..off + 80]
                        .copy_from_slice(&r.to_ne_bytes());
                    // cascade_info.y = slot index (as f32)
                    descriptor_bytes[off + 100..off + 104]
                        .copy_from_slice(&(slot_index as f32).to_ne_bytes());
                    // cascade_info.w = 2.0 → cube
                    descriptor_bytes[off + 108..off + 112]
                        .copy_from_slice(&2.0_f32.to_ne_bytes());

                    self.records.insert(
                        light_key,
                        LightShadowRecord {
                            views,
                            descriptor_base,
                        },
                    );
                    self.active_descriptor_count += 1;
                }
            }
        }

        if self.active_descriptor_count > 0 {
            gpu.write_buffer(
                &self.descriptors_uniform,
                None,
                descriptor_bytes.as_slice(),
                None,
                None,
            )?;
        }

        if cube_overflow {
            tracing::warn!(
                "point-light shadow cube pool exhausted (capacity {})",
                self.cube_slots.len()
            );
        }

        self.frame_count = self.frame_count.wrapping_add(1);
        Ok(())
    }

    /// Writes the supplied view-projection + bias parameters into
    /// `shadow_view_buffer`. Called per shadow view inside
    /// `record_passes`.
    pub fn write_shadow_view(
        &self,
        gpu: &AwsmRendererWebGpu,
        view_projection: &Mat4,
        depth_bias: f32,
        normal_bias: f32,
    ) -> Result<(), AwsmShadowError> {
        let mut data = [0u8; SHADOW_VIEW_BYTES];
        let cols = view_projection.to_cols_array();
        let mat_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64)
        };
        data[0..64].copy_from_slice(mat_bytes);
        data[64..68].copy_from_slice(&depth_bias.to_ne_bytes());
        data[68..72].copy_from_slice(&normal_bias.to_ne_bytes());
        gpu.write_buffer(&self.shadow_view_buffer, None, data.as_slice(), None, None)?;
        Ok(())
    }

    /// Iterates all per-frame caster records — used by the render
    /// pass loop to know which views to draw.
    pub fn records(&self) -> impl Iterator<Item = (LightKey, &LightShadowRecord)> + '_ {
        self.records.iter()
    }

    /// Returns the per-light authored shadow params, if registered.
    pub fn light_params(&self, key: LightKey) -> Option<&LightShadowParams> {
        self.params.get(key)
    }
}

#[allow(clippy::too_many_arguments)]
fn write_shadow_descriptor(
    dest: &mut [u8],
    view_projection: &Mat4,
    rect: [u32; 4],
    atlas_size: u32,
    depth_bias: f32,
    normal_bias: f32,
    hardness: LightShadowHardness,
    pcss_scale: f32,
    cascade_index: u32,
    cascade_count: u32,
    split_far: f32,
) {
    debug_assert!(dest.len() >= SHADOW_DESCRIPTOR_BYTES);
    let cols = view_projection.to_cols_array();
    let mat_bytes: &[u8] = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
    dest[0..64].copy_from_slice(mat_bytes);
    // atlas_rect in normalised UV space (x, y, w, h) ∈ [0, 1].
    let inv = if atlas_size == 0 {
        1.0
    } else {
        1.0 / atlas_size as f32
    };
    let x = rect[0] as f32 * inv;
    let y = rect[1] as f32 * inv;
    let w = rect[2] as f32 * inv;
    let h = rect[3] as f32 * inv;
    dest[64..68].copy_from_slice(&x.to_ne_bytes());
    dest[68..72].copy_from_slice(&y.to_ne_bytes());
    dest[72..76].copy_from_slice(&w.to_ne_bytes());
    dest[76..80].copy_from_slice(&h.to_ne_bytes());
    dest[80..84].copy_from_slice(&depth_bias.to_ne_bytes());
    dest[84..88].copy_from_slice(&normal_bias.to_ne_bytes());
    let hardness_f = match hardness {
        LightShadowHardness::Hard => 0.0_f32,
        LightShadowHardness::Soft => 1.0_f32,
        LightShadowHardness::Pcss => 2.0_f32,
    };
    dest[88..92].copy_from_slice(&hardness_f.to_ne_bytes());
    dest[92..96].copy_from_slice(&pcss_scale.to_ne_bytes());
    // cascade_info: (split_far_view_z, cascade_index, cascade_count_in_light, 0)
    dest[96..100].copy_from_slice(&split_far.to_ne_bytes());
    dest[100..104].copy_from_slice(&(cascade_index as f32).to_ne_bytes());
    dest[104..108].copy_from_slice(&(cascade_count as f32).to_ne_bytes());
    dest[108..112].copy_from_slice(&0.0_f32.to_ne_bytes());
}

async fn build_shadow_pipeline(
    gpu: &AwsmRendererWebGpu,
    shaders: &mut Shaders,
    pipelines: &mut Pipelines,
    pipeline_layouts: &PipelineLayouts,
    pipeline_layout_key: PipelineLayoutKey,
    instancing: bool,
) -> Result<RenderPipelineKey, AwsmShadowError> {
    let shader_key = shaders
        .get_key(
            gpu,
            ShaderCacheKeyShadow {
                instancing_transforms: instancing,
            },
        )
        .await?;

    let mut vertex_buffer_layouts = vec![VERTEX_BUFFER_LAYOUT.clone()];
    if instancing {
        vertex_buffer_layouts.push(VERTEX_BUFFER_LAYOUT_INSTANCING.clone());
    }

    let primitive = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(FrontFace::Ccw)
        // Front-cull when generating shadows — depth-only renders look
        // best when back faces are the ones being shadowed (avoids
        // Peter Panning on caster geometry).
        .with_cull_mode(CullMode::Front);

    let depth_stencil = DepthStencilState::new(TextureFormat::Depth32float)
        .with_depth_write_enabled(true)
        .with_depth_compare(CompareFunction::LessEqual);

    let mut pipeline_cache_key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive)
        .with_depth_stencil(depth_stencil);

    for layout in vertex_buffer_layouts {
        pipeline_cache_key = pipeline_cache_key.with_push_vertex_buffer_layout(layout);
    }

    // No fragment targets → depth-only pipeline (the cache skips
    // FragmentState when targets is empty).

    pipelines
        .render
        .get_key(gpu, shaders, pipeline_layouts, pipeline_cache_key)
        .await
        .map_err(Into::into)
}

/// Extracts the world-space near + far planes from a projection
/// matrix. Handles glam's right-handed perspective convention; falls
/// back to `(0.1, 100.0)` for matrices we don't recognise
/// (orthographic, custom).
fn extract_near_far(projection: &Mat4) -> (f32, f32) {
    let m22 = projection.z_axis.z;
    let m23 = projection.w_axis.z;
    // Reverse the glam `Mat4::perspective_rh` formulation:
    //   m22 = far / (near - far)
    //   m23 = (near * far) / (near - far)
    // → near = m23 / m22, far = m23 / (m22 + 1)
    if m22.abs() > 1e-4 && (m22 + 1.0).abs() > 1e-4 {
        let near = m23 / m22;
        let far = m23 / (m22 + 1.0);
        if near > 0.0 && far > near {
            return (near, far);
        }
    }
    (0.1, 100.0)
}

fn create_cube_array_view(
    texture: &web_sys::GpuTexture,
) -> Result<web_sys::GpuTextureView, AwsmShadowError> {
    let descriptor: web_sys::GpuTextureViewDescriptor =
        TextureViewDescriptor::new(Some("Shadow Cube Array"))
            .with_dimension(TextureViewDimension::CubeArray)
            .into();
    texture
        .create_view_with_descriptor(&descriptor)
        .map_err(AwsmCoreError::create_texture_view)
        .map_err(Into::into)
}

/// One 2D-array depth view per cube face. Indexed as
/// `slot_index * 6 + face_index` so the render-pass dispatch can grab
/// the right attachment without rebuilding the view each frame.
fn build_cube_face_views(
    texture: &web_sys::GpuTexture,
    total_layers: u32,
) -> Result<Vec<web_sys::GpuTextureView>, AwsmShadowError> {
    let mut views = Vec::with_capacity(total_layers as usize);
    for layer in 0..total_layers {
        let descriptor: web_sys::GpuTextureViewDescriptor =
            TextureViewDescriptor::new(Some("Shadow Cube Face"))
                .with_dimension(TextureViewDimension::N2d)
                .with_base_array_layer(layer)
                .with_array_layer_count(1)
                .into();
        let view = texture
            .create_view_with_descriptor(&descriptor)
            .map_err(AwsmCoreError::create_texture_view)?;
        views.push(view);
    }
    Ok(views)
}

impl AwsmRenderer {
    /// Sets a light's shadow parameters. Pass
    /// `LightShadowParams { cast: false, .. }` to disable shadows for a
    /// specific light while keeping the light itself. Takes effect on
    /// the next `render()` call.
    pub fn set_light_shadow_params(
        &mut self,
        key: LightKey,
        params: LightShadowParams,
    ) -> Result<(), AwsmShadowError> {
        self.shadows.params.insert(key, params);
        Ok(())
    }

    /// Returns the current shadow parameters for a light, or `None` if
    /// the light has never had shadow params set.
    pub fn light_shadow_params(&self, key: LightKey) -> Option<&LightShadowParams> {
        self.shadows.params.get(key)
    }

    /// Mutates a light's shadow params in place. Convenience over the
    /// get-clone-mutate-set pattern.
    pub fn update_light_shadow<F: FnOnce(&mut LightShadowParams)>(
        &mut self,
        key: LightKey,
        f: F,
    ) -> Result<(), AwsmShadowError> {
        if let Some(params) = self.shadows.params.get_mut(key) {
            f(params);
            Ok(())
        } else {
            Err(AwsmShadowError::UnknownLight)
        }
    }

    /// Sets a mesh's shadow flags. Takes effect on the next `render()`.
    pub fn set_mesh_shadow_flags(
        &mut self,
        key: crate::meshes::MeshKey,
        flags: MeshShadowFlags,
    ) -> Result<(), AwsmShadowError> {
        let mesh = self
            .meshes
            .get_mut(key)
            .map_err(|_| AwsmShadowError::UnknownMesh)?;
        mesh.cast_shadows = flags.cast;
        mesh.receive_shadows = flags.receive;
        Ok(())
    }

    /// Returns the current shadow flags for a mesh.
    pub fn mesh_shadow_flags(&self, key: crate::meshes::MeshKey) -> MeshShadowFlags {
        match self.meshes.get(key) {
            Ok(mesh) => MeshShadowFlags {
                cast: mesh.cast_shadows,
                receive: mesh.receive_shadows,
            },
            Err(_) => MeshShadowFlags::default(),
        }
    }
}
