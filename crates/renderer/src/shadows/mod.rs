//! Shadow mapping subsystem.
//!
//! The `Shadows` struct sits on [`AwsmRenderer`](crate::AwsmRenderer) and
//! owns every GPU resource needed for shadow generation and sampling:
//! a 2D PCF/PCSS atlas, an RGBA16F EVSM atlas (allocated lazily), a
//! depth cubemap-array slot pool for point lights, and the descriptor
//! storage buffer that the material-opaque and material-transparent
//! shading passes read at sample time.
//!
//! v1 covers cascaded shadow maps for directional lights, perspective
//! shadow maps for spot lights, cubemap shadow maps for point lights,
//! a hybrid PCF / EVSM far-cascade filter, an optional PCSS hardness
//! mode, screen-space contact shadows (SSCS), and temporal throttling
//! of far cascades. See `docs/plans/shadows.md` for the full design.
//!
//! Phase 0: scaffolding only. The subsystem currently holds a 1x1
//! depth atlas, a 1-slice cube array, and an empty descriptors buffer,
//! and is wired into the render graph as a no-op so that further
//! phases can fill in real generation and sampling without touching
//! the surrounding renderer plumbing again.

pub mod cascade;
pub mod config;
pub mod error;
pub mod light_shadow;
pub mod render_pass;
pub mod shader;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    compare::CompareFunction,
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
    sampler::{FilterMode, SamplerDescriptor},
    texture::{
        Extent3d, TextureDescriptor, TextureFormat, TextureUsage, TextureViewDescriptor,
        TextureViewDimension,
    },
};

use crate::{bind_groups::BindGroups, debug::AwsmRendererLogging, AwsmRenderer};

pub use self::{
    config::ShadowsConfig,
    error::AwsmShadowError,
    light_shadow::{
        EvsmCutoff, FarCascadeUpdateRate, LightShadowHardness, LightShadowParams, MeshShadowFlags,
    },
};

/// Single descriptor record (matches the WGSL `ShadowDescriptor` struct).
///
/// 64 bytes — `mat4x4<f32>` for the light-space transform + 16 bytes of
/// scalar state (atlas rect / flags). Phase 0 only allocates a 1-record
/// placeholder so the bind group has a valid storage buffer; later
/// phases populate this from per-light fits.
pub const SHADOW_DESCRIPTOR_BYTES: usize = 80;

/// Owns every GPU resource for shadow generation and sampling.
///
/// Lives on `AwsmRenderer::shadows`. Phase 0 only wires storage,
/// samplers, and a placeholder GPU layout; real generation and
/// sampling come online in later phases.
pub struct Shadows {
    /// Renderer-wide configuration. Replace via [`Shadows::set_config`].
    pub config: ShadowsConfig,
    /// Depth atlas used for PCF and PCSS sampling (directional cascades
    /// + spot lights).
    pub atlas_texture: web_sys::GpuTexture,
    /// Default view of the atlas (entire `Depth32Float` texture).
    pub atlas_view: web_sys::GpuTextureView,
    /// EVSM atlas (`RGBA16F`) — moments storage for far directional
    /// cascades. Allocated even in Phase 0 so the bind group has a
    /// stable layout; resized lazily on first EVSM use.
    pub evsm_atlas_texture: web_sys::GpuTexture,
    /// Default view of the EVSM atlas.
    pub evsm_atlas_view: web_sys::GpuTextureView,
    /// Cubemap array used for point-light shadows.
    pub cube_array_texture: web_sys::GpuTexture,
    /// Cube-array view spanning every slice.
    pub cube_array_view: web_sys::GpuTextureView,
    /// Storage buffer of per-shadow descriptors read by the shading
    /// passes. Allocated in Phase 0 but not yet bound — see
    /// `shared/material/bind_group.rs::shadow_bind_group_layout_entries`
    /// for the rationale.
    pub descriptors_buffer: web_sys::GpuBuffer,
    /// Uniform buffer of shadow globals (atlas sizes, EVSM params,
    /// SSCS flags) read by the shading passes.
    pub globals_buffer: web_sys::GpuBuffer,
    /// Comparison sampler for `textureSampleCompare` on the atlases.
    pub sampler_comparison: web_sys::GpuSampler,
    /// Linear filterable sampler for EVSM moment sampling.
    pub sampler_filterable: web_sys::GpuSampler,
    /// Frame counter used by temporal throttling (Phase 11).
    pub frame_count: u64,
    /// Whether descriptors / globals need to be re-uploaded next
    /// `write_gpu` call.
    pub dirty: bool,
}

impl Shadows {
    /// Allocates the placeholder GPU resources that satisfy the shadow
    /// bind group's layout. Real atlas / cube allocations come online
    /// when a shadow caster is actually registered.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self, AwsmShadowError> {
        let config = ShadowsConfig::default();

        let atlas_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Depth32float,
                Extent3d::new(1, Some(1), Some(1)),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .with_label("Shadow Atlas (placeholder)")
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
                    .with_texture_binding()
                    .with_storage_binding(),
            )
            .with_label("Shadow EVSM Atlas (placeholder)")
            .into(),
        )?;
        let evsm_atlas_view = evsm_atlas_texture
            .create_view()
            .map_err(AwsmCoreError::create_texture_view)?;

        let cube_array_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Depth32float,
                // 6 layers per slice × 1 slice for the placeholder
                Extent3d::new(1, Some(1), Some(6)),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding(),
            )
            .with_label("Shadow Cube Pool (placeholder)")
            .into(),
        )?;
        let cube_array_view = create_cube_array_view(&cube_array_texture)?;

        let descriptors_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Descriptors"),
                SHADOW_DESCRIPTOR_BYTES,
                BufferUsage::new().with_storage().with_copy_dst(),
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

        Ok(Self {
            config,
            atlas_texture,
            atlas_view,
            evsm_atlas_texture,
            evsm_atlas_view,
            cube_array_texture,
            cube_array_view,
            descriptors_buffer,
            globals_buffer,
            sampler_comparison,
            sampler_filterable,
            frame_count: 0,
            dirty: true,
        })
    }

    /// Replaces the renderer-wide config. Atlas-size changes trigger a
    /// re-pack at the start of next frame; `max_point_shadows` changes
    /// re-create the cube array on the next `write_gpu` — call sparingly.
    pub fn set_config(&mut self, config: ShadowsConfig) {
        self.config = config;
        self.dirty = true;
    }

    /// Returns a reference to the renderer-wide config.
    pub fn config(&self) -> &ShadowsConfig {
        &self.config
    }

    /// Number of lights currently registered as shadow casters.
    /// Phase 0: always 0 (no caster bookkeeping yet).
    pub fn caster_count(&self) -> usize {
        0
    }

    /// Fraction of the 2D atlas occupied by active cascades + spots.
    /// Phase 0: always 0.
    pub fn atlas_utilization(&self) -> f32 {
        0.0
    }

    /// Fraction of cube-array slots occupied.
    /// Phase 0: always 0.
    pub fn cube_pool_utilization(&self) -> f32 {
        0.0
    }

    /// Returns `true` if any shadow-casting light is currently active.
    /// The render graph short-circuits the entire shadow-generation
    /// pass when this is `false`.
    pub fn any_active(&self) -> bool {
        false
    }

    /// Per-frame upload point. Writes descriptors / globals to the GPU
    /// when something is dirty, ticks the frame counter for temporal
    /// throttling, and marks any bind-group recreations required.
    ///
    /// Phase 0: only ticks `frame_count` and refreshes `globals` once.
    pub fn write_gpu(
        &mut self,
        _logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        _bind_groups: &mut BindGroups,
    ) -> Result<(), AwsmShadowError> {
        if self.dirty {
            // Globals layout (matches WGSL `ShadowGlobals`):
            //   vec4<f32> atlas_size_evsm_size      (atlas_size.xy, evsm_size.xy)
            //   vec4<f32> evsm_params               (exponent, blur_radius, sscs_steps, sscs_enabled)
            //   vec4<u32> flags                     (debug_cascade_colors, max_point_shadows, _, _)
            let mut data = [0u8; SHADOW_GLOBALS_BYTES];
            let atlas = self.config.atlas_size as f32;
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

        self.frame_count = self.frame_count.wrapping_add(1);
        Ok(())
    }
}

/// Size in bytes of the `ShadowGlobals` uniform block.
pub const SHADOW_GLOBALS_BYTES: usize = 48;

impl AwsmRenderer {
    /// Sets a light's shadow parameters. Pass
    /// `LightShadowParams { cast: false, .. }` to disable shadows for a
    /// specific light while keeping the light itself. Takes effect on
    /// the next `render()` call.
    ///
    /// Phase 0: returns `Ok(())` unconditionally — no-op until the
    /// caster registry comes online.
    pub fn set_light_shadow_params(
        &mut self,
        _key: crate::lights::LightKey,
        _params: LightShadowParams,
    ) -> Result<(), AwsmShadowError> {
        Ok(())
    }

    /// Returns the current shadow parameters for a light, or `None` if
    /// the light has never had shadow params set (treat as
    /// `cast = false`).
    ///
    /// Phase 0: always returns `None`.
    pub fn light_shadow_params(
        &self,
        _key: crate::lights::LightKey,
    ) -> Option<&LightShadowParams> {
        None
    }

    /// Mutates a light's shadow params in place. Convenience over the
    /// get-clone-mutate-set pattern; mirrors `Lights::update`.
    ///
    /// Phase 0: no-op.
    pub fn update_light_shadow<F: FnOnce(&mut LightShadowParams)>(
        &mut self,
        _key: crate::lights::LightKey,
        _f: F,
    ) -> Result<(), AwsmShadowError> {
        Ok(())
    }

    /// Sets a mesh's shadow flags. Takes effect on the next `render()`.
    /// Errors if the mesh key is unknown.
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

    /// Returns the current shadow flags for a mesh. Returns the
    /// per-mesh default if the mesh key is unknown.
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
