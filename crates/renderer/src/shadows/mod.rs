//! Shadow mapping subsystem.
//!
//! The `Shadows` struct sits on [`AwsmRenderer`]
//! and owns every GPU resource needed for shadow generation and
//! sampling: a 2D PCF/PCSS atlas, an RGBA16F EVSM atlas (allocated
//! lazily), a depth cubemap-array slot pool for point lights, the
//! descriptor uniform buffer that the material-opaque shading pass
//! reads at sample time, and the depth-only render pipeline used for
//! shadow generation.

pub mod cascade;
pub mod config;
pub mod error;
pub mod evsm;
pub mod light_shadow;
pub mod render_pass;
#[cfg(feature = "scene-schema")]
pub mod schema_convert;
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
        multisample::MultisampleState,
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
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey, BindGroupLayouts,
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
    evsm::EvsmPass,
    light_shadow::{
        CubeFaceUpdateRate, EvsmCutoff, FarCascadeUpdateRate, LightShadowHardness,
        LightShadowParams, MeshShadowFlags,
    },
    shader::{cache_key::ShaderCacheKeyShadow, template::ShaderTemplateShadow},
};

/// Maximum number of shadow descriptors stored in the per-frame
/// uniform array. 32 entries × 96 B = 3 KB — well under the
/// `maxUniformBufferBindingSize` ceiling (default 64 KB).
pub const MAX_SHADOW_DESCRIPTORS: u32 = 32;

/// Maximum number of shadow VIEWS per frame (one render pass each).
/// Point lights have 6 views per descriptor (cube faces); directional
/// lights have one per cascade. 96 covers a worst case of 8 point +
/// 4 directional × 4 cascades + 32 spots.
pub const MAX_SHADOW_VIEWS: u32 = 96;

/// Size in bytes of a single packed `ShadowDescriptor` (see
/// `shared_wgsl/shadow/bind_groups.wgsl`):
/// - `view_projection: mat4x4<f32>` (64 B)
/// - `atlas_rect: vec4<f32>` (16 B)
/// - `bias_params: vec4<f32>` (16 B)
/// - `cascade_info: vec4<f32>` (16 B)
pub const SHADOW_DESCRIPTOR_BYTES: usize = 112;

/// Size in bytes of the `ShadowGlobals` uniform block.
pub const SHADOW_GLOBALS_BYTES: usize = 48;

/// Logical size of a single per-view shadow uniform entry: a
/// `mat4x4` view-projection (64 B) and a `vec4` of bias parameters
/// (16 B). The actual buffer is laid out with stride
/// `SHADOW_VIEW_STRIDE` so dynamic uniform offsets stay aligned.
pub const SHADOW_VIEW_BYTES: usize = 80;

/// Stride between shadow-view buffer slots — aligned to
/// `minUniformBufferOffsetAlignment` (256 B on every adapter we
/// target) so each slot is a valid dynamic-offset target.
pub const SHADOW_VIEW_STRIDE: usize = 256;

/// Default per-face cube shadow map resolution. The runtime value is
/// `ShadowsConfig::point_shadow_resolution` (held on `Shadows` as
/// `cube_resolution`); this constant is the default the config falls
/// back to. 1024² × 6 × Depth32f × N_lights of VRAM (24 MB for 8
/// lights) — industry standard for medium-quality point shadows.
/// Drop to 512 / 256 for mobile-class browsers; bump to 2048 for
/// ultra-quality.
pub const POINT_SHADOW_RESOLUTION: u32 = 1024;

/// Minimum legal per-face cube resolution. Anything smaller than this
/// produces extreme stair-step aliasing well before saving meaningful
/// memory (a 32² face is 24 KB, vs 256 KB at 256²) — so we clamp.
pub const MIN_POINT_SHADOW_RESOLUTION: u32 = 64;

/// Clamps a user-supplied cube-face resolution to the legal range. The
/// upper bound matches `SHADOW_ATLAS_MAX_SIZE` so a single cube face
/// can't out-size the 2D atlas (`Shadows::new` already saturates VRAM
/// for the 8-light × 6-face pool when we approach that limit).
pub fn clamp_point_shadow_resolution(res: u32) -> u32 {
    res.clamp(MIN_POINT_SHADOW_RESOLUTION, SHADOW_ATLAS_MAX_SIZE)
}

/// Near plane used when generating each point-light cube face. The
/// receiver-side WGSL constant `POINT_SHADOW_NEAR` MUST match this —
/// the shadow VS writes perspective NDC.z with this near, and the
/// receiver remaps its linear distance to the same NDC.z curve for
/// the comparison. Diverging values cause silent failure (no shadow
/// or all shadow).
pub const POINT_SHADOW_NEAR: f32 = 0.05;

/// Sentinel meaning "this light has no shadow descriptor allocated"
/// in the packed `LightPacked` row 4. The shading shader uses this to
/// short-circuit shadow sampling.
pub const SHADOW_INDEX_NONE: u32 = u32::MAX;

/// One queued EVSM moment-write + blur dispatch for the current frame.
/// `descriptor_index` lets the render-pass dispatch fetch the
/// `evsm_rect` (which was patched into the descriptor's `atlas_rect`
/// at write time), but we cache the rects here so the compute-pass
/// loop doesn't need to re-read the descriptor.
#[derive(Clone, Copy, Debug)]
pub struct EvsmDispatchEntry {
    /// Index into `descriptors_uniform` for this cascade.
    pub descriptor_index: u32,
    /// Source rect on `shadow_atlas` in texels (`x, y, w, h`).
    pub pcf_rect: [u32; 4],
    /// Destination rect on `evsm_atlas` in texels (`x, y, w, h`). Also
    /// what receivers sample (UV-converted on read).
    pub evsm_rect: [u32; 4],
    /// Params-buffer slot index for this cascade. Multiplied by
    /// `EVSM_PARAMS_STRIDE` to get the dynamic offset.
    pub params_slot: u32,
}

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
    /// cascades. Sized at `config.evsm_atlas_size`. Usage includes
    /// `STORAGE_BINDING` for the blur compute passes plus
    /// `RENDER_ATTACHMENT` for the moment-writer fragment pass.
    pub evsm_atlas_texture: web_sys::GpuTexture,
    /// Default sample-side view of the EVSM atlas. Bound at shadow
    /// group slot 4 of every receiver pipeline.
    pub evsm_atlas_view: web_sys::GpuTextureView,
    /// Active per-side dimension of the EVSM atlas in texels.
    pub evsm_atlas_size: u32,
    /// Ping-pong texture for the separable Gaussian blur. Same size as
    /// `evsm_atlas_texture`; never sampled at receiver time.
    pub evsm_blur_pingpong_texture: web_sys::GpuTexture,
    /// Default view of the ping-pong texture.
    pub evsm_blur_pingpong_view: web_sys::GpuTextureView,
    /// EVSM compute pipelines + per-cascade params buffer.
    pub evsm_pass: EvsmPass,
    /// Per-frame EVSM cascade list — `(descriptor_index, pcf_rect,
    /// evsm_rect)` for the dispatch loop. `pcf_rect` is in shadow_atlas
    /// texels; `evsm_rect` is in evsm_atlas texels.
    pub evsm_dispatch_queue: Vec<EvsmDispatchEntry>,
    /// Persistent bind group for the moment-write compute pass.
    /// Bindings: 0=shadow_atlas (depth), 1=evsm_atlas (storage write),
    /// 2=params (uniform, dynamic offset). Same group is used for
    /// every EVSM cascade; per-cascade context comes via dynamic
    /// offset.
    pub evsm_moment_write_bind_group: web_sys::GpuBindGroup,
    /// Persistent bind group for the horizontal blur half-pass.
    /// 0=evsm_atlas (read), 1=ping-pong (storage write), 2=params.
    pub evsm_blur_h_bind_group: web_sys::GpuBindGroup,
    /// Persistent bind group for the vertical blur half-pass.
    /// 0=ping-pong (read), 1=evsm_atlas (storage write), 2=params.
    pub evsm_blur_v_bind_group: web_sys::GpuBindGroup,
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
    /// Active per-face cube shadow resolution in texels (square).
    /// Mirrors `config.point_shadow_resolution` clamped via
    /// `clamp_point_shadow_resolution` (≥ `MIN_POINT_SHADOW_RESOLUTION`,
    /// ≤ `SHADOW_ATLAS_MAX_SIZE`). Power-of-two isn't enforced — WebGPU
    /// is fine with arbitrary sizes — but non-POT values waste a bit of
    /// memory on the depth-texture tail. Read in `write_gpu` as the
    /// cube viewport.
    pub cube_resolution: u32,
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
    /// O(1) cache of `cube_slots[idx] == Some(light_key)`. Each point
    /// light's slot is stable across frames (re-assigned only on
    /// cube-pool resize or first acquisition), so caching the index
    /// avoids the two linear walks the previous code did each frame.
    /// Validated against `cube_slots[idx]` on lookup — a stale entry
    /// (slot reassigned to a different light, or pool recreated) falls
    /// back to the linear search.
    cube_slot_for_light: SecondaryMap<LightKey, u32>,
    /// Per-light, per-frame fitted record (cascade fit, atlas rect,
    /// descriptor index). Rebuilt every `write_gpu` call.
    records: SecondaryMap<LightKey, LightShadowRecord>,
    /// Throttle state per view, persisted across the `records`
    /// rebuild. Indexed by light key; each entry is a `Vec` parallel
    /// to `LightShadowRecord::views`.
    throttle: SecondaryMap<LightKey, Vec<ShadowViewThrottle>>,
    /// Number of descriptors currently active in `descriptors_uniform`.
    active_descriptor_count: u32,
    /// Number of view slots used in `shadow_view_buffer` this frame.
    /// One per render pass (per cascade / spot / cube face).
    active_view_count: u32,

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
    /// Depth-only shadow pipeline used for cube-face passes
    /// (non-instancing). Identical to the 2D variant except `front_face`
    /// is `Cw` to compensate for the Y-flip applied to the cube face
    /// projection — without that, front-face culling would invert and
    /// produce peter-panning on every point-light receiver.
    shadow_pipeline_cube_no_instancing: RenderPipelineKey,
    /// Depth-only shadow pipeline used for cube-face passes (instancing).
    shadow_pipeline_cube_instancing: RenderPipelineKey,

    /// Frame counter used by temporal throttling (phase 11).
    pub frame_count: u64,
    /// Whether descriptors / globals need to be re-uploaded.
    pub dirty: bool,
    /// Set when a write_gpu pass detected atlas overflow. The next
    /// frame's write_gpu grows the atlas (and rebinds the opaque
    /// shadow bind group via `BindGroupCreate::ShadowsResourcesChange`).
    pending_atlas_grow: bool,
    /// Set by `set_config` when a resource-shape config field changed.
    /// Processed at the top of the next `write_gpu` so users get a
    /// live update from the editor without having to reload the
    /// project.
    pending_resource_recreate: PendingResourceRecreate,
    /// Scratch buffer reused across `write_gpu` calls to avoid a per-
    /// frame heap allocation for the per-mesh caster AABB list. Capacity
    /// grows monotonically to the largest scene seen so far; `clear()`
    /// preserves capacity.
    caster_aabbs_scratch: Vec<crate::bounds::Aabb>,
    /// Scratch staging buffer for the per-frame descriptor pack
    /// before upload to `descriptors_uniform`. Sized to
    /// `SHADOW_DESCRIPTOR_UNIFORM_BYTES` once at construction;
    /// `fill(0)` between frames reuses the allocation.
    descriptor_bytes_scratch: Vec<u8>,
    /// Scratch staging buffer for per-view matrices uploaded into
    /// `shadow_view_buffer`. Sized to `SHADOW_VIEW_STRIDE *
    /// MAX_SHADOW_VIEWS` once at construction.
    view_bytes_scratch: Vec<u8>,
}

/// Tracks which GPU resources need to be torn down + rebuilt because a
/// resource-shape config field changed since the last `write_gpu`. All
/// three resources are independent: a pure EVSM-atlas size bump
/// doesn't need to touch the PCF atlas or cube pool, and vice versa.
#[derive(Default, Copy, Clone)]
struct PendingResourceRecreate {
    /// `config.atlas_size` differs from `self.atlas_size`. Recreates
    /// the depth atlas texture + view and the moment-write bind group
    /// (which reads from the atlas).
    pcf_atlas: bool,
    /// `config.evsm_atlas_size` differs from `self.evsm_atlas_size`.
    /// Recreates the EVSM atlas + ping-pong textures and all three
    /// EVSM compute bind groups.
    evsm_atlas: bool,
    /// `config.max_point_shadows` or `config.point_shadow_resolution`
    /// changed. Recreates the cube-array texture, its views, and
    /// clears all slot owners so they get re-allocated next frame.
    cube_pool: bool,
}

impl PendingResourceRecreate {
    fn any(&self) -> bool {
        self.pcf_atlas || self.evsm_atlas || self.cube_pool
    }
}

/// Upper bound for `atlas_size` when dynamic resizing kicks in. Caps
/// the atlas at 8K to match the plan's "Shadow atlas size dropdown:
/// 1024 / 2048 / 4096 / 8192" ceiling.
pub const SHADOW_ATLAS_MAX_SIZE: u32 = 8192;

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
    /// Re-render cadence for this view in frames. `1` means every
    /// frame; the far directional cascade may bump this to 2/4/8 via
    /// `LightShadowParams::far_cascade_update_rate`.
    pub update_period: u64,
    /// Decision flag set by the temporal throttle (Phase 11): `true`
    /// means the render pass should re-render this view, `false`
    /// means the cached atlas tile is still valid for this frame.
    pub should_render: bool,
    /// Global slot index for this view in the per-frame shadow-view
    /// buffer. The render pass uses this as the dynamic offset
    /// multiplier when binding `shadow_view_bind_group`. Set during
    /// `write_gpu` once all views are known.
    pub shadow_view_slot: u32,
}

/// Persistent throttle state per shadow view. Keyed by `(LightKey,
/// view_index)` on `Shadows` so the per-frame `records` rebuild
/// doesn't lose it.
#[derive(Clone, Debug)]
pub struct ShadowViewThrottle {
    /// Frame index at which the view was last rendered. `u64::MAX`
    /// means "never rendered" → force a render this frame.
    pub last_rendered_frame: u64,
    /// Last view-projection we rendered with. Compared each frame so
    /// significant camera / light movement forces an early refresh.
    pub last_view_projection: Mat4,
    /// Last atlas rect we rendered into. If the row-pack allocator
    /// moves this view to a different rect (Phase 13 will re-pack on
    /// caster-set changes), we invalidate the throttle entry so the
    /// stale rect isn't sampled at its new location.
    pub last_atlas_rect: [u32; 4],
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
        config: ShadowsConfig,
    ) -> Result<Self, AwsmShadowError> {
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

        // EVSM atlas — RGBA16F holds the four exponential moments
        // (pos_exp, pos_exp², neg_exp, neg_exp²) packed in `.rgba`.
        // Receivers do a single bilinear fetch + Chebyshev visibility
        // reconstruction instead of N comparison taps; the trade-off
        // is moment storage + a moment-write pass per EVSM cascade.
        //
        // Sized from `config.evsm_atlas_size` (default 2048², ~32 MB).
        // Usage = `RENDER_ATTACHMENT | TEXTURE_BINDING | STORAGE_BINDING`
        // so the moment-writer can render into it (fragment pipeline)
        // and the Gaussian-blur compute passes can read / write through
        // a storage view (post-blur it's bound back to the shadow group
        // for sampling).
        let evsm_atlas_size = config.evsm_atlas_size.max(1);
        let evsm_atlas_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Rgba16float,
                Extent3d::new(evsm_atlas_size, Some(evsm_atlas_size), Some(1)),
                TextureUsage::new()
                    .with_render_attachment()
                    .with_texture_binding()
                    .with_storage_binding(),
            )
            .with_label("Shadow EVSM Atlas")
            .into(),
        )?;
        // Same-size ping-pong texture for the separable Gaussian blur.
        // The horizontal blur reads from `evsm_atlas_texture`, writes
        // into this; the vertical blur reads back and writes into
        // `evsm_atlas_texture`. Storage-only — never sampled directly.
        let evsm_blur_pingpong_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Rgba16float,
                Extent3d::new(evsm_atlas_size, Some(evsm_atlas_size), Some(1)),
                TextureUsage::new()
                    .with_texture_binding()
                    .with_storage_binding(),
            )
            .with_label("Shadow EVSM Blur Ping-pong")
            .into(),
        )?;
        let evsm_blur_pingpong_view = evsm_blur_pingpong_texture
            .create_view()
            .map_err(AwsmCoreError::create_texture_view)?;
        let evsm_atlas_view = evsm_atlas_texture
            .create_view()
            .map_err(AwsmCoreError::create_texture_view)?;

        let cube_slot_count = config.max_point_shadows.max(1);
        let cube_layer_count = cube_slot_count * 6;
        let cube_resolution = clamp_point_shadow_resolution(config.point_shadow_resolution);
        let cube_array_texture = gpu.create_texture(
            &TextureDescriptor::new(
                TextureFormat::Depth32float,
                Extent3d::new(
                    cube_resolution,
                    Some(cube_resolution),
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

        // N slots × 256 B stride. Each slot stores the per-view
        // matrix + bias floats for one shadow render pass. The bind
        // group uses dynamic offsets so we can write all slots in
        // `write_gpu` (once per frame) and select the right slot
        // per render pass without re-queueing buffer writes between
        // passes — `queue.writeBuffer` flushes all writes BEFORE any
        // command buffer executes, so per-pass writes to a single
        // slot would cause every pass to see the last-written value.
        let shadow_view_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Shadow Views"),
                SHADOW_VIEW_STRIDE * MAX_SHADOW_VIEWS as usize,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        // Clamp-to-edge on all three axes prevents the cube comparison
        // sampler from wrapping at face boundaries — WebGPU has no
        // "seamless cubemap" toggle, so the address mode IS the seam
        // policy. Without this, bilinear taps at a cube face edge can
        // read from the opposite face's coordinate space and produce
        // ghost shadows at the seam.
        let sampler_comparison = gpu.create_sampler(Some(
            &SamplerDescriptor {
                label: Some("Shadow Comparison Sampler"),
                compare: Some(CompareFunction::LessEqual),
                mag_filter: Some(FilterMode::Linear),
                min_filter: Some(FilterMode::Linear),
                address_mode_u: Some(awsm_renderer_core::sampler::AddressMode::ClampToEdge),
                address_mode_v: Some(awsm_renderer_core::sampler::AddressMode::ClampToEdge),
                address_mode_w: Some(awsm_renderer_core::sampler::AddressMode::ClampToEdge),
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

        // Slot 0 of the shadow pipeline: a per-view uniform that the
        // render pass selects via dynamic offset (one slot per
        // active shadow descriptor).
        let shadow_view_bind_group_layout_key = bind_group_layouts.get_key(
            gpu,
            BindGroupLayoutCacheKey {
                entries: vec![BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::Uniform)
                            .with_dynamic_offset(true),
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
                BindGroupResource::Buffer(
                    BufferBinding::new(&shadow_view_buffer).with_size(SHADOW_VIEW_BYTES),
                ),
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
        let shadow_pipeline_layout_key =
            pipeline_layouts.get_key(gpu, bind_group_layouts, shadow_pipeline_layout_cache_key)?;

        let shadow_pipeline_no_instancing = build_shadow_pipeline(
            gpu,
            shaders,
            pipelines,
            pipeline_layouts,
            shadow_pipeline_layout_key,
            false,
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
            false,
        )
        .await?;
        let shadow_pipeline_cube_no_instancing = build_shadow_pipeline(
            gpu,
            shaders,
            pipelines,
            pipeline_layouts,
            shadow_pipeline_layout_key,
            false,
            true,
        )
        .await?;
        let shadow_pipeline_cube_instancing = build_shadow_pipeline(
            gpu,
            shaders,
            pipelines,
            pipeline_layouts,
            shadow_pipeline_layout_key,
            true,
            true,
        )
        .await?;

        let evsm_pass = EvsmPass::new(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            pipelines,
            shaders,
        )
        .await?;

        let evsm_moment_write_bind_group = build_evsm_moment_write_bind_group(
            gpu,
            bind_group_layouts,
            evsm_pass.moment_write_layout_key,
            &atlas_view,
            &evsm_atlas_view,
            &evsm_pass.params_buffer,
        )?;
        let evsm_blur_h_bind_group = build_evsm_blur_bind_group(
            gpu,
            bind_group_layouts,
            evsm_pass.blur_layout_key,
            &evsm_atlas_view,
            &evsm_blur_pingpong_view,
            &evsm_pass.params_buffer,
            "Shadow EVSM Blur H Bind Group",
        )?;
        let evsm_blur_v_bind_group = build_evsm_blur_bind_group(
            gpu,
            bind_group_layouts,
            evsm_pass.blur_layout_key,
            &evsm_blur_pingpong_view,
            &evsm_atlas_view,
            &evsm_pass.params_buffer,
            "Shadow EVSM Blur V Bind Group",
        )?;

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
            cube_resolution,
            cube_slots: vec![None; cube_slot_count as usize],
            evsm_atlas_size,
            evsm_blur_pingpong_texture,
            evsm_blur_pingpong_view,
            evsm_pass,
            evsm_dispatch_queue: Vec::new(),
            evsm_moment_write_bind_group,
            evsm_blur_h_bind_group,
            evsm_blur_v_bind_group,
            descriptors_buffer,
            descriptors_uniform,
            globals_buffer,
            shadow_view_buffer,
            sampler_comparison,
            sampler_filterable,
            params: SecondaryMap::new(),
            cube_slot_for_light: SecondaryMap::new(),
            records: SecondaryMap::new(),
            throttle: SecondaryMap::new(),
            active_descriptor_count: 0,
            active_view_count: 0,
            shadow_view_bind_group_layout_key,
            shadow_view_bind_group,
            shadow_pipeline_layout_key,
            shadow_pipeline_no_instancing,
            shadow_pipeline_instancing,
            shadow_pipeline_cube_no_instancing,
            shadow_pipeline_cube_instancing,
            frame_count: 0,
            dirty: true,
            pending_atlas_grow: false,
            pending_resource_recreate: PendingResourceRecreate::default(),
            caster_aabbs_scratch: Vec::new(),
            descriptor_bytes_scratch: vec![0u8; *SHADOW_DESCRIPTOR_UNIFORM_BYTES],
            view_bytes_scratch: vec![0u8; SHADOW_VIEW_STRIDE * MAX_SHADOW_VIEWS as usize],
        })
    }

    /// Replaces the renderer-wide config.
    ///
    /// Lightweight fields (SSCS toggle, debug flags, EVSM tuning) take
    /// effect on the next `write_gpu`. Resource-shape fields
    /// (`atlas_size`, `evsm_atlas_size`, `max_point_shadows`,
    /// `point_shadow_resolution`) trigger a tear-down + rebuild of
    /// the corresponding GPU textures + bind groups at the start of
    /// the next `write_gpu` — recreating GPU resources is not free
    /// (texture alloc + dependent-bind-group rebuild) so don't poke
    /// these at frame rate; from the editor inspector they're fine.
    pub fn set_config(&mut self, config: ShadowsConfig) {
        let new_atlas = config.atlas_size.max(1);
        let new_evsm = config.evsm_atlas_size.max(1);
        let new_cube_count = config.max_point_shadows.max(1);
        let new_cube_res = clamp_point_shadow_resolution(config.point_shadow_resolution);
        if new_atlas != self.atlas_size {
            self.pending_resource_recreate.pcf_atlas = true;
        }
        if new_evsm != self.evsm_atlas_size {
            self.pending_resource_recreate.evsm_atlas = true;
        }
        if new_cube_count != self.cube_slots.len() as u32 || new_cube_res != self.cube_resolution {
            self.pending_resource_recreate.cube_pool = true;
        }
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
        self.params.values().filter(|p| p.cast).count()
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

    /// Tear down and rebuild whichever GPU resources were marked dirty
    /// by `set_config`. Each block is independent — only the resources
    /// that actually changed get touched. After every successful path
    /// we mark `ShadowsResourcesChange` so the consumer-side opaque /
    /// transparent shadow bind groups get re-bound, and reset throttle
    /// state so previously-rendered cascades re-rasterise into the
    /// freshly-allocated texture (otherwise they would read stale or
    /// uninitialised memory and flicker).
    fn apply_pending_resource_recreate(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &BindGroupLayouts,
        bind_groups: &mut BindGroups,
    ) -> Result<(), AwsmShadowError> {
        let recreate = std::mem::take(&mut self.pending_resource_recreate);

        if recreate.pcf_atlas {
            let new_size = self.config.atlas_size.max(1);
            tracing::info!(
                "shadow PCF atlas resize (config) {} → {}",
                self.atlas_size,
                new_size
            );
            self.atlas_size = new_size;
            self.atlas_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Depth32float,
                    Extent3d::new(self.atlas_size, Some(self.atlas_size), Some(1)),
                    TextureUsage::new()
                        .with_render_attachment()
                        .with_texture_binding(),
                )
                .with_label("Shadow Atlas")
                .into(),
            )?;
            self.atlas_view = self
                .atlas_texture
                .create_view()
                .map_err(AwsmCoreError::create_texture_view)?;
            // Moment-write reads from the PCF atlas — rebind it
            // against the new view. The two blur bind groups only
            // touch the EVSM atlas / ping-pong and stay valid.
            self.evsm_moment_write_bind_group = build_evsm_moment_write_bind_group(
                gpu,
                bind_group_layouts,
                self.evsm_pass.moment_write_layout_key,
                &self.atlas_view,
                &self.evsm_atlas_view,
                &self.evsm_pass.params_buffer,
            )?;
        }

        if recreate.evsm_atlas {
            let new_size = self.config.evsm_atlas_size.max(1);
            tracing::info!(
                "shadow EVSM atlas resize (config) {} → {}",
                self.evsm_atlas_size,
                new_size
            );
            self.evsm_atlas_size = new_size;
            self.evsm_atlas_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Rgba16float,
                    Extent3d::new(new_size, Some(new_size), Some(1)),
                    TextureUsage::new()
                        .with_render_attachment()
                        .with_texture_binding()
                        .with_storage_binding(),
                )
                .with_label("Shadow EVSM Atlas")
                .into(),
            )?;
            self.evsm_atlas_view = self
                .evsm_atlas_texture
                .create_view()
                .map_err(AwsmCoreError::create_texture_view)?;
            self.evsm_blur_pingpong_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Rgba16float,
                    Extent3d::new(new_size, Some(new_size), Some(1)),
                    TextureUsage::new()
                        .with_texture_binding()
                        .with_storage_binding(),
                )
                .with_label("Shadow EVSM Blur Ping-pong")
                .into(),
            )?;
            self.evsm_blur_pingpong_view = self
                .evsm_blur_pingpong_texture
                .create_view()
                .map_err(AwsmCoreError::create_texture_view)?;
            // All three EVSM bind groups reference at least one of the
            // recreated views, so rebuild all three.
            self.evsm_moment_write_bind_group = build_evsm_moment_write_bind_group(
                gpu,
                bind_group_layouts,
                self.evsm_pass.moment_write_layout_key,
                &self.atlas_view,
                &self.evsm_atlas_view,
                &self.evsm_pass.params_buffer,
            )?;
            self.evsm_blur_h_bind_group = build_evsm_blur_bind_group(
                gpu,
                bind_group_layouts,
                self.evsm_pass.blur_layout_key,
                &self.evsm_atlas_view,
                &self.evsm_blur_pingpong_view,
                &self.evsm_pass.params_buffer,
                "Shadow EVSM Blur H Bind Group",
            )?;
            self.evsm_blur_v_bind_group = build_evsm_blur_bind_group(
                gpu,
                bind_group_layouts,
                self.evsm_pass.blur_layout_key,
                &self.evsm_blur_pingpong_view,
                &self.evsm_atlas_view,
                &self.evsm_pass.params_buffer,
                "Shadow EVSM Blur V Bind Group",
            )?;
        }

        if recreate.cube_pool {
            let new_count = self.config.max_point_shadows.max(1);
            let new_res = clamp_point_shadow_resolution(self.config.point_shadow_resolution);
            tracing::info!(
                "shadow cube pool resize (config) {} × {}² → {} × {}²",
                self.cube_slots.len(),
                self.cube_resolution,
                new_count,
                new_res,
            );
            let new_layers = new_count * 6;
            self.cube_array_texture = gpu.create_texture(
                &TextureDescriptor::new(
                    TextureFormat::Depth32float,
                    Extent3d::new(new_res, Some(new_res), Some(new_layers)),
                    TextureUsage::new()
                        .with_render_attachment()
                        .with_texture_binding(),
                )
                .with_label("Shadow Cube Pool")
                .into(),
            )?;
            self.cube_array_view = create_cube_array_view(&self.cube_array_texture)?;
            self.cube_face_views = build_cube_face_views(&self.cube_array_texture, new_layers)?;
            self.cube_resolution = new_res;
            // Slot ownership is keyed by index — when the pool size
            // changes (or any face is recreated), every previously-
            // resident shadow's contents are gone. Clear ownership so
            // the next descriptor pack re-allocates from scratch.
            self.cube_slots = vec![None; new_count as usize];
            // Slot ownership reset → drop the per-light index cache so
            // the next frame's lookup falls through to a fresh slot
            // search instead of trusting a stale slot_index.
            self.cube_slot_for_light.clear();
        }

        // Re-rasterise only the views whose backing texture actually
        // changed. The throttle is parallel-indexed with the previous
        // frame's `records.views`, so each entry's "is cube" can be
        // read by matching position. EVSM atlas resize doesn't need
        // an invalidation pass — EVSM moments are re-computed every
        // frame from the PCF atlas content during the compute pass,
        // so the PCF cascades' throttle entries already cover it.
        let invalidate_2d = recreate.pcf_atlas;
        let invalidate_cube = recreate.cube_pool;
        if invalidate_2d || invalidate_cube {
            for (key, entries) in self.throttle.iter_mut() {
                let prev_views = self.records.get(key).map(|r| r.views.as_slice());
                for (i, t) in entries.iter_mut().enumerate() {
                    let is_cube = prev_views
                        .and_then(|v| v.get(i))
                        .map(|v| v.cube_layer.is_some())
                        .unwrap_or(false);
                    let hit = if is_cube {
                        invalidate_cube
                    } else {
                        invalidate_2d
                    };
                    if hit {
                        t.last_rendered_frame = u64::MAX;
                    }
                }
            }
        }
        bind_groups.mark_create(crate::bind_groups::BindGroupCreate::ShadowsResourcesChange);
        // `set_config` already flagged `dirty` before our pending
        // recreate flags were ever consulted, so the globals upload
        // is queued — but make the dependency explicit here too:
        // the PCF/PCSS shader reads `atlas_sizes` from globals and
        // any sample after this point must see the new size, even if
        // the recreate path is invoked from a non-`set_config` source
        // in the future.
        self.dirty = true;
        Ok(())
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
    /// Cube (point) and 2D (cascade/spot) shadows use distinct pipelines:
    /// the cube pipeline has `front_face = Cw` to compensate for the
    /// Y-flip applied to the cube projection — see `write_gpu`.
    pub fn shadow_pipeline_key(&self, instancing: bool, cube_face: bool) -> RenderPipelineKey {
        match (cube_face, instancing) {
            (true, true) => self.shadow_pipeline_cube_instancing,
            (true, false) => self.shadow_pipeline_cube_no_instancing,
            (false, true) => self.shadow_pipeline_instancing,
            (false, false) => self.shadow_pipeline_no_instancing,
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
        bind_group_layouts: &BindGroupLayouts,
        bind_groups: &mut BindGroups,
        camera: &crate::camera::CameraBuffer,
        lights: &crate::lights::Lights,
        meshes: &crate::meshes::Meshes,
    ) -> Result<(), AwsmShadowError> {
        // User-driven resource recreates land first so a fresh
        // `set_config` from the editor takes effect immediately. The
        // auto-grow path below operates on whatever size landed here.
        if self.pending_resource_recreate.any() {
            self.apply_pending_resource_recreate(gpu, bind_group_layouts, bind_groups)?;
        }

        // Phase 13: dynamic atlas resize. If the previous frame's
        // packer ran out of room we grow the atlas to the next power
        // of two (capped at `SHADOW_ATLAS_MAX_SIZE`) before this
        // frame's pack. Recreates the texture + view and tells the
        // bind-group reconciler to rebind the opaque shadow group.
        if self.pending_atlas_grow {
            self.pending_atlas_grow = false;
            let new_size = (self.atlas_size.saturating_mul(2)).min(SHADOW_ATLAS_MAX_SIZE);
            if new_size > self.atlas_size {
                tracing::info!(
                    "shadow atlas overflow → growing from {} to {}",
                    self.atlas_size,
                    new_size
                );
                self.atlas_size = new_size;
                self.atlas_texture = gpu.create_texture(
                    &TextureDescriptor::new(
                        TextureFormat::Depth32float,
                        Extent3d::new(self.atlas_size, Some(self.atlas_size), Some(1)),
                        TextureUsage::new()
                            .with_render_attachment()
                            .with_texture_binding(),
                    )
                    .with_label("Shadow Atlas")
                    .into(),
                )?;
                self.atlas_view = self
                    .atlas_texture
                    .create_view()
                    .map_err(AwsmCoreError::create_texture_view)?;
                // EVSM moment-write reads from `shadow_atlas`, so its
                // bind group holds a reference to the OLD view. Rebuild
                // it now against the new view; the blur bind groups
                // only touch `evsm_atlas` + ping-pong so they survive
                // the atlas grow untouched.
                self.evsm_moment_write_bind_group = build_evsm_moment_write_bind_group(
                    gpu,
                    bind_group_layouts,
                    self.evsm_pass.moment_write_layout_key,
                    &self.atlas_view,
                    &self.evsm_atlas_view,
                    &self.evsm_pass.params_buffer,
                )?;
                bind_groups
                    .mark_create(crate::bind_groups::BindGroupCreate::ShadowsResourcesChange);
                // Force the throttle to re-render every cascade at the
                // new atlas location.
                for entries in self.throttle.values_mut() {
                    for t in entries.iter_mut() {
                        t.last_rendered_frame = u64::MAX;
                    }
                }
                // `ShadowGlobals.atlas_sizes` is read by every PCF
                // tile clamp / PCSS kernel — re-upload so the shader
                // sees the new size on the next sample.
                self.dirty = true;
            } else {
                tracing::warn!(
                    "shadow atlas at max size {}, cannot grow further",
                    SHADOW_ATLAS_MAX_SIZE
                );
            }
        }

        if self.dirty {
            // Globals layout (matches WGSL `ShadowGlobals`).
            let mut data = [0u8; SHADOW_GLOBALS_BYTES];
            let atlas = self.atlas_size as f32;
            let evsm = self.config.evsm_atlas_size as f32;
            data[0..4].copy_from_slice(&atlas.to_ne_bytes());
            data[4..8].copy_from_slice(&atlas.to_ne_bytes());
            data[8..12].copy_from_slice(&evsm.to_ne_bytes());
            data[12..16].copy_from_slice(&evsm.to_ne_bytes());
            // Clamp `evsm_exponent` to the fp16-safe range — anything
            // above ~18 saturates the half-float moments and collapses
            // the Chebyshev visibility curve into a hard binary mask.
            let evsm_exponent = self
                .config
                .evsm_exponent
                .clamp(0.5, ShadowsConfig::EVSM_EXPONENT_MAX_FP16);
            data[16..20].copy_from_slice(&evsm_exponent.to_ne_bytes());
            data[20..24].copy_from_slice(&(self.config.evsm_blur_radius as f32).to_ne_bytes());
            data[24..28].copy_from_slice(&(self.config.sscs_step_count as f32).to_ne_bytes());
            data[28..32].copy_from_slice(&(self.config.sscs_enabled as u32 as f32).to_ne_bytes());
            data[32..36].copy_from_slice(&(self.config.debug_cascade_colors as u32).to_ne_bytes());
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
        self.active_view_count = 0;

        // Early-out when no light is currently casting. Skips the
        // O(meshes) caster-AABB sweep, descriptor pack, and throttle
        // reconciliation — the entire shadow generation pass is
        // also gated by `any_active()` upstream, so leaving stale
        // descriptors here is fine. We still tick `frame_count` so
        // throttle counters stay in step when shadows resume.
        if !self.params.values().any(|p| p.cast) {
            self.frame_count = self.frame_count.wrapping_add(1);
            return Ok(());
        }

        // Reuse the scratch staging buffers across frames. Zero the
        // descriptor scratch in full (gaps between active descriptors
        // must read as zero in the uniform), and only zero the view
        // scratch up to `MAX_SHADOW_VIEWS` slots that will actually
        // be written below.
        let descriptor_bytes = &mut self.descriptor_bytes_scratch[..];
        descriptor_bytes.fill(0);
        let view_bytes = &mut self.view_bytes_scratch[..];

        // Approximate the camera's near/far in world-space depth.
        // The actual values live on the camera but aren't exposed
        // directly here; reconstruct from the projection's column.
        // For a standard RH perspective with `Mat4::perspective_rh`
        // (which glam uses): proj[2][3] is `-2*near*far/(far-near)`
        // and proj[2][2] is `-(far+near)/(far-near)`; solving gives
        // the planes. Falls back to (0.1, 100.0) for orthographic.
        let (camera_near, camera_far) = extract_near_far(&camera_matrices.projection);

        // Per-mesh shadow-caster AABBs. `fit_cascades` clips each one
        // to the cascade's world footprint per-cascade, so we hand it
        // the full list rather than a precomputed union — a single
        // pre-unioned AABB would over-include casters that lie
        // laterally outside the cascade, ballooning the cascade's Z
        // range and destroying depth precision (the canonical failure
        // mode: a 10 km × 10 km ground plane whose union AABB stretches
        // thousands of metres along the tilted light direction).
        self.caster_aabbs_scratch.clear();
        for (_, mesh) in meshes.iter() {
            if !mesh.cast_shadows || mesh.hidden || mesh.hud {
                continue;
            }
            if let Some(aabb) = mesh.world_aabb.clone() {
                self.caster_aabbs_scratch.push(aabb);
            }
        }
        let caster_world_aabbs = self.caster_aabbs_scratch.as_slice();

        // Cursor for the row-pack atlas allocator. Phase 13 will
        // replace this with a real packer; for now we walk left-to-
        // right and wrap to the next row when the current row fills.
        let mut atlas_x: u32 = 0;
        let mut atlas_y: u32 = 0;
        // EVSM atlas allocator cursors (separate from PCF). Local for
        // the duration of the cascade-placement loop; final state
        // doesn't need to persist on `self`.
        let mut evsm_x: u32 = 0;
        let mut evsm_y: u32 = 0;
        let mut evsm_row_h: u32 = 0;
        let evsm_atlas_size = self.evsm_atlas_size;
        self.evsm_dispatch_queue.clear();
        self.evsm_pass.active_cascade_count = 0;
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
                    let cascade_count = params.cascade_count.clamp(1, 4) as u32;
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
                        caster_world_aabbs,
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
                                "shadow atlas overflow on cascade {} — will grow next frame",
                                cascade_index
                            );
                            self.pending_atlas_grow = true;
                            break;
                        };

                        let descriptor_index = self.active_descriptor_count;
                        let off = descriptor_index as usize * SHADOW_DESCRIPTOR_BYTES;
                        let is_evsm = (cascade_index as u32) >= evsm_first;
                        // For EVSM cascades the *descriptor*'s atlas
                        // rect points at the EVSM atlas (the receiver
                        // samples the post-blur moments from there),
                        // but the depth pass still writes into the
                        // PCF rect on `shadow_atlas` — so
                        // `LightShadowView::atlas_rect` keeps the PCF
                        // rect for use as the render-pass viewport.
                        //
                        // If EVSM atlas allocation overflows, the
                        // cascade silently degrades to PCF: descriptor
                        // points at the PCF rect, `is_evsm` flag stays
                        // off, no compute dispatch is queued.
                        let evsm_rect_alloc = if is_evsm {
                            // Inline row-pack on the EVSM atlas, same
                            // shape as the PCF allocator. Returns None
                            // on overflow → cascade silently degrades.
                            let r = *res;
                            if evsm_x + r > evsm_atlas_size {
                                evsm_x = 0;
                                evsm_y += evsm_row_h;
                                evsm_row_h = 0;
                            }
                            if evsm_y + r > evsm_atlas_size {
                                tracing::warn!(
                                    "EVSM atlas overflow on cascade res={} — falling back to PCF",
                                    r
                                );
                                None
                            } else {
                                let rect = [evsm_x, evsm_y, r, r];
                                evsm_x += r;
                                evsm_row_h = evsm_row_h.max(r);
                                Some(rect)
                            }
                        } else {
                            None
                        };
                        let (descriptor_rect, descriptor_atlas_size, evsm_active) =
                            match evsm_rect_alloc {
                                Some(evsm_rect) => (evsm_rect, self.evsm_atlas_size, true),
                                None => (rect, self.atlas_size, false),
                            };
                        write_shadow_descriptor(
                            &mut descriptor_bytes[off..off + SHADOW_DESCRIPTOR_BYTES],
                            &cascade.view_projection,
                            descriptor_rect,
                            descriptor_atlas_size,
                            params.depth_bias,
                            params.normal_bias,
                            params.hardness,
                            params.pcss_penumbra_scale,
                            cascade.world_per_texel,
                            cascade_count,
                            *split_far,
                        );
                        if evsm_active {
                            let evsm_flag_off = off + 108;
                            descriptor_bytes[evsm_flag_off..evsm_flag_off + 4]
                                .copy_from_slice(&1.0_f32.to_ne_bytes());
                            let evsm_rect = descriptor_rect;
                            let slot = self.evsm_pass.active_cascade_count as usize;
                            if slot < evsm::MAX_EVSM_CASCADES_PER_FRAME {
                                // Match the clamp applied to
                                // `shadow_globals.evsm_sscs.x` so the
                                // writer-side moment exponent never
                                // diverges from the receiver-side
                                // reference. A mismatch would make
                                // visibility either constant-1 or
                                // constant-0 over the whole cascade.
                                let evsm_exponent = self
                                    .config
                                    .evsm_exponent
                                    .clamp(0.5, ShadowsConfig::EVSM_EXPONENT_MAX_FP16);
                                self.evsm_pass.write_params_slot(
                                    slot,
                                    [rect[0], rect[1]],
                                    [rect[2], rect[3]],
                                    [evsm_rect[0], evsm_rect[1]],
                                    [evsm_rect[2], evsm_rect[3]],
                                    evsm_exponent,
                                    self.config.evsm_blur_radius,
                                );
                                self.evsm_dispatch_queue.push(EvsmDispatchEntry {
                                    descriptor_index,
                                    pcf_rect: rect,
                                    evsm_rect,
                                    params_slot: slot as u32,
                                });
                                self.evsm_pass.active_cascade_count += 1;
                            }
                        }

                        // Throttle only the FAR cascade. Closer
                        // cascades carry per-frame contact detail and
                        // must refresh every frame.
                        let update_period =
                            if (cascade_index as u32) == cascade_count.saturating_sub(1) {
                                params.far_cascade_update_rate.period()
                            } else {
                                1
                            };
                        let view_slot = self.active_view_count;
                        write_shadow_view_slot(
                            &mut *view_bytes,
                            view_slot as usize,
                            &cascade.view_projection,
                            params.depth_bias,
                            params.normal_bias,
                        );
                        self.active_view_count += 1;
                        views.push(LightShadowView {
                            view_projection: cascade.view_projection,
                            atlas_rect: rect,
                            cube_layer: None,
                            update_period,
                            should_render: true,
                            shadow_view_slot: view_slot,
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
                        tracing::warn!(
                            "shadow atlas overflow on spot light — will grow next frame"
                        );
                        self.pending_atlas_grow = true;
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
                    // Approximate world-per-texel for the spot cone at
                    // its far plane: the perspective frustum's footprint
                    // there is `2 * far * tan(fov/2)`. Used by the PCF
                    // path to keep penumbra width consistent with
                    // directional cascades.
                    let spot_world_per_texel = 2.0 * far * (fov * 0.5).tan() / res as f32;

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
                        spot_world_per_texel,
                        1,
                        // Spot lights don't use cascade selection; setting
                        // `split_far` to +infinity-ish makes the shader's
                        // walk pick this descriptor unconditionally.
                        f32::MAX,
                    );

                    self.records.insert(
                        light_key,
                        LightShadowRecord {
                            views: vec![{
                                let view_slot = self.active_view_count;
                                write_shadow_view_slot(
                                    &mut *view_bytes,
                                    view_slot as usize,
                                    &view_projection,
                                    params.depth_bias,
                                    params.normal_bias,
                                );
                                self.active_view_count += 1;
                                LightShadowView {
                                    view_projection,
                                    atlas_rect: rect,
                                    cube_layer: None,
                                    update_period: 1,
                                    should_render: true,
                                    shadow_view_slot: view_slot,
                                }
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
                    // O(1) ownership lookup via `cube_slot_for_light`,
                    // validated against `cube_slots` (a stale entry from
                    // a previous-pool reassignment falls back to the
                    // linear free-slot search).
                    let cached = self.cube_slot_for_light.get(light_key).copied();
                    let owned = cached.and_then(|idx| {
                        let i = idx as usize;
                        if self.cube_slots.get(i).and_then(|s| *s) == Some(light_key) {
                            Some(i)
                        } else {
                            None
                        }
                    });
                    let slot = owned.or_else(|| self.cube_slots.iter().position(|s| s.is_none()));
                    let Some(slot_index) = slot else {
                        cube_overflow = true;
                        continue;
                    };
                    self.cube_slots[slot_index] = Some(light_key);
                    self.cube_slot_for_light
                        .insert(light_key, slot_index as u32);

                    let pos = glam::Vec3::from(*position);
                    let r = (*range).max(0.05);
                    // 90° per face — adjacent faces meet exactly at the
                    // cube edge and the seamless-cubemap filter handles
                    // bilinear comparison across the seam.
                    let cube_fov = std::f32::consts::FRAC_PI_2;
                    // WebGPU cube sampling (D3D convention): on the +X
                    // face, texel t=0 maps to direction +Y, etc. A
                    // plain `look_at_rh(... up=-Y) * perspective_rh` —
                    // the OpenGL-style cube convention — writes world
                    // +Y to the *bottom* of the rendered face because
                    // WebGPU's framebuffer is top-left-origin while
                    // NDC.y is bottom-up. The mismatch shows up at
                    // sample time as a V-flipped read, which on a
                    // sphere of receivers manifests as a "double" or
                    // "phantom" shadow across the seam between
                    // adjacent faces. Post-multiplying the projection
                    // by a Y-flip negates NDC.y so world +Y lands at
                    // texel t=0; the matching `front_face = Cw` in the
                    // cube shadow pipeline restores winding (and
                    // therefore front-face culling).
                    let y_flip = glam::Mat4::from_scale(glam::Vec3::new(1.0, -1.0, 1.0));
                    let projection =
                        y_flip * glam::Mat4::perspective_rh(cube_fov, 1.0, POINT_SHADOW_NEAR, r);
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
                    // Per-face throttle period. Default `EveryFrame`
                    // (period = 1) preserves the previous behaviour;
                    // higher periods are a mobile / many-light perf
                    // lever — the throttle in this same `write_gpu`
                    // call already handles per-face cadence and forces
                    // a redraw whenever the light or its descriptor
                    // moves enough to invalidate the cache.
                    let cube_update_period = params.cube_face_update_rate.period();
                    for (face_idx, (dir, up)) in face_dirs.iter().enumerate() {
                        let view = glam::Mat4::look_at_rh(pos, pos + *dir, *up);
                        let vp = projection * view;
                        let view_slot = self.active_view_count;
                        write_shadow_view_slot(
                            &mut *view_bytes,
                            view_slot as usize,
                            &vp,
                            params.depth_bias,
                            params.normal_bias,
                        );
                        self.active_view_count += 1;
                        views.push(LightShadowView {
                            view_projection: vp,
                            // For cube faces the attachment is already the
                            // per-face 2D view at the cube's native
                            // resolution, so this rect doubles as the
                            // render-pass viewport — it must match
                            // `self.cube_resolution`, not the
                            // initialization-time `POINT_SHADOW_RESOLUTION`
                            // default, or a config change would render
                            // into a sub-rect of the new texture.
                            atlas_rect: [0, 0, self.cube_resolution, self.cube_resolution],
                            cube_layer: Some(slot_index as u32 * 6 + face_idx as u32),
                            update_period: cube_update_period,
                            should_render: true,
                            shadow_view_slot: view_slot,
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
                        // Caller patches cascade_info.y with the slot
                        // index after this returns — see below.
                        0.0,
                        1,
                        f32::MAX,
                    );
                    // Patch in the cube-specific atlas_rect (light_pos +
                    // range) and the "kind = cube + slice index" in
                    // `cascade_info.w / .y`.
                    descriptor_bytes[off + 64..off + 68].copy_from_slice(&pos.x.to_ne_bytes());
                    descriptor_bytes[off + 68..off + 72].copy_from_slice(&pos.y.to_ne_bytes());
                    descriptor_bytes[off + 72..off + 76].copy_from_slice(&pos.z.to_ne_bytes());
                    descriptor_bytes[off + 76..off + 80].copy_from_slice(&r.to_ne_bytes());
                    // cascade_info.y = slot index (as f32)
                    descriptor_bytes[off + 100..off + 104]
                        .copy_from_slice(&(slot_index as f32).to_ne_bytes());
                    // cascade_info.w = 2.0 → cube
                    descriptor_bytes[off + 108..off + 112].copy_from_slice(&2.0_f32.to_ne_bytes());

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
            // Upload only the active prefix. The shader iterates
            // `descriptor_base..base+count` so trailing slots never
            // get read; the uniform buffer's tail keeps whatever it
            // held last frame (harmless — those slots are not bound
            // as descriptor indices anywhere).
            let used = self.active_descriptor_count as usize * SHADOW_DESCRIPTOR_BYTES;
            gpu.write_buffer(
                &self.descriptors_uniform,
                None,
                &descriptor_bytes[..used],
                None,
                None,
            )?;
        }
        if self.active_view_count > 0 {
            // Upload the per-view matrices once. The render pass uses
            // dynamic offsets into this buffer to select per-pass
            // matrices — a single `writeBuffer` call here is critical:
            // queue.writeBuffer flushes all queued writes BEFORE any
            // command buffer executes, so if we wrote per-pass we'd
            // see only the last matrix in every pass.
            let used = self.active_view_count as usize * SHADOW_VIEW_STRIDE;
            gpu.write_buffer(
                &self.shadow_view_buffer,
                None,
                &view_bytes[..used],
                None,
                None,
            )?;
        }

        // Reconcile throttle state with the freshly-built records.
        // Lights that vanished from the caster set drop their state;
        // views whose atlas rect moved get invalidated (the cached
        // depth is at the wrong location); the view-projection drift
        // check forces a redraw when the camera or light moved enough
        // to make the cached cascade visibly stale.
        // Drop throttle entries for lights that no longer have a
        // record this frame. `retain` is allocation-free; the
        // earlier `Vec<LightKey>` sweep + `contains()` was O(n²).
        self.throttle.retain(|k, _| self.records.contains_key(k));
        let frame = self.frame_count;
        for (light_key, record) in self.records.iter_mut() {
            if !self.throttle.contains_key(light_key) {
                self.throttle.insert(light_key, Vec::new());
            }
            let entry = &mut self.throttle[light_key];
            entry.resize(
                record.views.len(),
                ShadowViewThrottle {
                    last_rendered_frame: u64::MAX,
                    last_view_projection: Mat4::ZERO,
                    last_atlas_rect: [0; 4],
                },
            );
            for (i, view) in record.views.iter_mut().enumerate() {
                let t = &mut entry[i];
                if t.last_atlas_rect != view.atlas_rect {
                    t.last_rendered_frame = u64::MAX;
                }
                let drift = view_projection_drift(&t.last_view_projection, &view.view_projection);
                if drift > 0.001 {
                    t.last_rendered_frame = u64::MAX;
                }
                let due = t.last_rendered_frame == u64::MAX
                    || frame >= t.last_rendered_frame.saturating_add(view.update_period);
                // The 2D shadow atlas is a single shared depth texture
                // and `LoadOp::Clear` is attachment-wide, so the
                // generation pass clears the whole atlas on its first
                // pass each frame (see `render_pass::record`). If we
                // skipped any 2D view via throttling, its tile would
                // be left empty for the frame while its descriptor is
                // still sampled — that produces a flicker, so 2D views
                // are forced to render every frame until tile-local
                // clearing (or a per-view texture-array atlas) lands.
                // Cube views still throttle: each face owns its own
                // attachment view and clears independently.
                let is_cube = view.cube_layer.is_some();
                view.should_render = due || !is_cube;
                if view.should_render {
                    t.last_rendered_frame = frame;
                    t.last_view_projection = view.view_projection;
                    t.last_atlas_rect = view.atlas_rect;
                }
            }
        }

        if cube_overflow {
            tracing::warn!(
                "point-light shadow cube pool exhausted (capacity {})",
                self.cube_slots.len()
            );
        }

        // Flush EVSM per-cascade params to the GPU. One write covers
        // every active cascade; the compute-pass loop in
        // `render_pass::record` binds slot N via dynamic offset.
        self.evsm_pass.upload_params(gpu)?;

        self.frame_count = self.frame_count.wrapping_add(1);
        Ok(())
    }

    /// Dynamic-offset argument for the shadow_view bind group at
    /// `view_global_index`. The buffer is laid out with
    /// `SHADOW_VIEW_STRIDE`-byte slots so offsets are
    /// `min-uniform-buffer-offset-alignment` compatible.
    pub fn shadow_view_dynamic_offset(view_global_index: u32) -> u32 {
        view_global_index * SHADOW_VIEW_STRIDE as u32
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

// For 2D descriptors `cascade_y_param` is world-units-per-shadow-map-
// texel (used to scale the PCF kernel for consistent world-space
// softness across cascades). For cube descriptors the caller patches
// it with the cube-pool slot index right after this returns.
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
    cascade_y_param: f32,
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
    // cascade_info: (split_far_view_z, cascade_y_param, cascade_count_in_light, 0)
    //  - .y is the per-descriptor world-per-texel for 2D shadows, or
    //    the cube slot index for point lights (caller patches the
    //    cube case after this returns; same byte offsets).
    dest[96..100].copy_from_slice(&split_far.to_ne_bytes());
    dest[100..104].copy_from_slice(&cascade_y_param.to_ne_bytes());
    dest[104..108].copy_from_slice(&(cascade_count as f32).to_ne_bytes());
    dest[108..112].copy_from_slice(&0.0_f32.to_ne_bytes());
}

fn build_evsm_moment_write_bind_group(
    gpu: &AwsmRendererWebGpu,
    bind_group_layouts: &BindGroupLayouts,
    layout_key: BindGroupLayoutKey,
    shadow_atlas_view: &web_sys::GpuTextureView,
    evsm_atlas_view: &web_sys::GpuTextureView,
    params_buffer: &web_sys::GpuBuffer,
) -> Result<web_sys::GpuBindGroup, AwsmShadowError> {
    use awsm_renderer_core::bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource};
    use std::borrow::Cow;
    let entries = vec![
        BindGroupEntry::new(
            0,
            BindGroupResource::TextureView(Cow::Borrowed(shadow_atlas_view)),
        ),
        BindGroupEntry::new(
            1,
            BindGroupResource::TextureView(Cow::Borrowed(evsm_atlas_view)),
        ),
        BindGroupEntry::new(
            2,
            BindGroupResource::Buffer(
                BufferBinding::new(params_buffer).with_size(evsm::EVSM_PARAMS_STRIDE),
            ),
        ),
    ];
    let descriptor = BindGroupDescriptor::new(
        bind_group_layouts.get(layout_key)?,
        Some("Shadow EVSM Moment Write Bind Group"),
        entries,
    );
    Ok(gpu.create_bind_group(&descriptor.into()))
}

fn build_evsm_blur_bind_group(
    gpu: &AwsmRendererWebGpu,
    bind_group_layouts: &BindGroupLayouts,
    layout_key: BindGroupLayoutKey,
    src_view: &web_sys::GpuTextureView,
    dst_view: &web_sys::GpuTextureView,
    params_buffer: &web_sys::GpuBuffer,
    label: &str,
) -> Result<web_sys::GpuBindGroup, AwsmShadowError> {
    use awsm_renderer_core::bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource};
    use std::borrow::Cow;
    let entries = vec![
        BindGroupEntry::new(0, BindGroupResource::TextureView(Cow::Borrowed(src_view))),
        BindGroupEntry::new(1, BindGroupResource::TextureView(Cow::Borrowed(dst_view))),
        BindGroupEntry::new(
            2,
            BindGroupResource::Buffer(
                BufferBinding::new(params_buffer).with_size(evsm::EVSM_PARAMS_STRIDE),
            ),
        ),
    ];
    let descriptor =
        BindGroupDescriptor::new(bind_group_layouts.get(layout_key)?, Some(label), entries);
    Ok(gpu.create_bind_group(&descriptor.into()))
}

async fn build_shadow_pipeline(
    gpu: &AwsmRendererWebGpu,
    shaders: &mut Shaders,
    pipelines: &mut Pipelines,
    pipeline_layouts: &PipelineLayouts,
    pipeline_layout_key: PipelineLayoutKey,
    instancing: bool,
    cube_face: bool,
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

    // Industry-standard shadow rendering uses Front culling on caster
    // geometry: the depth-only pipeline writes the FAR (back) face's
    // depth from the light's POV. Receivers (which are the front of
    // surfaces facing the light) compare against the back-face depth
    // with a small bias and the geometry's own thickness acts as the
    // bias buffer — no Peter Panning, no acne. The slope-scale bias
    // below is the safety net for nearly-perpendicular surfaces where
    // back-face depth ≈ front-face depth.
    //
    // Cube faces apply a post-projection Y-flip (see `write_gpu`) which
    // reverses NDC winding. The cube-pipeline variant compensates with
    // `front_face = Cw` so the same "cull surfaces facing the light"
    // rule applies after the flip.
    let front_face = if cube_face {
        FrontFace::Cw
    } else {
        FrontFace::Ccw
    };
    let primitive = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(front_face)
        .with_cull_mode(CullMode::Front);

    let depth_stencil = DepthStencilState::new(TextureFormat::Depth32float)
        .with_depth_write_enabled(true)
        .with_depth_compare(CompareFunction::LessEqual)
        .with_depth_bias(1)
        .with_depth_bias_slope_scale(1.5);

    // Shadow atlas / cube faces are never multisampled — the depth
    // textures are single-sample. Pinning sample-count to 1 explicitly
    // guards against a future cache-key change (or a copy-paste from a
    // multisampled pipeline) silently enabling MSAA on the shadow
    // path, which would either error at pipeline creation or — worse,
    // if it survived — quadruple the per-pass rasterization cost.
    let multisample = MultisampleState::new().with_count(1);

    let mut pipeline_cache_key = RenderPipelineCacheKey::new(shader_key, pipeline_layout_key)
        .with_primitive(primitive)
        .with_depth_stencil(depth_stencil)
        .with_multisample(multisample);

    for layout in vertex_buffer_layouts {
        pipeline_cache_key = pipeline_cache_key.with_push_vertex_buffer_layout(layout);
    }

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
/// Writes one entry into the per-view shadow uniform buffer at slot
/// `view_slot`. Buffer is laid out at `SHADOW_VIEW_STRIDE`-byte stride
/// so dynamic offsets stay aligned; only the first
/// `SHADOW_VIEW_BYTES` of each slot carry data.
fn write_shadow_view_slot(
    dest: &mut [u8],
    view_slot: usize,
    view_projection: &Mat4,
    depth_bias: f32,
    normal_bias: f32,
) {
    let off = view_slot * SHADOW_VIEW_STRIDE;
    debug_assert!(off + SHADOW_VIEW_BYTES <= dest.len());
    let cols = view_projection.to_cols_array();
    let mat_bytes: &[u8] = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
    dest[off..off + 64].copy_from_slice(mat_bytes);
    dest[off + 64..off + 68].copy_from_slice(&depth_bias.to_ne_bytes());
    dest[off + 68..off + 72].copy_from_slice(&normal_bias.to_ne_bytes());
    dest[off + 72..off + 80].copy_from_slice(&[0u8; 8]);
}

/// Quick scalar drift metric between two view-projection matrices.
/// Sum of per-element absolute differences; used by the temporal
/// throttle to invalidate cached cascades when the camera or light
/// moves enough that the cached shadow would visibly tear.
fn view_projection_drift(prev: &Mat4, current: &Mat4) -> f32 {
    let a = prev.to_cols_array();
    let b = current.to_cols_array();
    let mut acc = 0.0_f32;
    for i in 0..16 {
        acc += (a[i] - b[i]).abs();
    }
    acc
}

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
    /// Replaces the renderer-wide shadow config. Player / runtime
    /// equivalent of the editor's "shadows" inspector — load the
    /// `ShadowsConfig` from disk (via `awsm_scene_schema` → `into()`
    /// or a custom build pipeline) and push it in at startup.
    ///
    /// Resource-shaped fields (`atlas_size`, `max_point_shadows`,
    /// `point_shadow_resolution`, `evsm_atlas_size`) are baked into
    /// `Shadows::new` at construction time, so changing them at
    /// runtime requires recreating the renderer. The other tunables
    /// (SSCS toggle, blur radius, exponent, debug overlay) take
    /// effect on the next `render()` call.
    pub fn set_shadows_config(&mut self, config: ShadowsConfig) {
        self.shadows.set_config(config);
    }

    /// Returns the current renderer-wide shadow config.
    pub fn shadows_config(&self) -> &ShadowsConfig {
        self.shadows.config()
    }

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
        // The light's `shadow_index` is baked into `LightPacked.row4.z`
        // at pack time via the `shadow_index_for` callback in
        // `Lights::write_gpu`. Changing shadow params can change that
        // index (cast=false → SHADOW_INDEX_NONE, or a freshly assigned
        // descriptor_base when shadows toggle on), so the cached pack
        // must be invalidated even though the light itself didn't move.
        self.lights.mark_punctual_dirty();
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
            // See `set_light_shadow_params` — the baked `shadow_index`
            // in the lights buffer must be reconciled.
            self.lights.mark_punctual_dirty();
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
