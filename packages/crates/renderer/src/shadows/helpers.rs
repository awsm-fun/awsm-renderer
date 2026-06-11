//! Free helpers shared across the shadow subsystem — descriptor /
//! view packers, EVSM bind-group builders, the shadow-generation
//! pipeline builder, and a few math utilities (near/far extraction,
//! view-projection drift).
//!
//! These were extracted out of `mod.rs` purely so `mod.rs` carries
//! only module declarations + re-exports. Their callers all live in
//! sibling files inside the `shadows` module.

use std::sync::LazyLock;

use awsm_renderer_core::{
    bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource},
    buffers::BufferBinding,
    compare::CompareFunction,
    error::AwsmCoreError,
    pipeline::{
        depth_stencil::DepthStencilState,
        multisample::MultisampleState,
        primitive::{CullMode, FrontFace, PrimitiveState, PrimitiveTopology},
    },
    renderer::AwsmRendererWebGpu,
    texture::{TextureFormat, TextureViewDescriptor, TextureViewDimension},
};
use glam::Mat4;
use std::borrow::Cow;

use crate::{
    bind_group_layout::{BindGroupLayoutKey, BindGroupLayouts},
    pipeline_layouts::PipelineLayoutKey,
    pipelines::render_pipeline::RenderPipelineCacheKey,
    render_passes::geometry::pipeline::{VERTEX_BUFFER_LAYOUT, VERTEX_BUFFER_LAYOUT_INSTANCING},
    shadows::{
        consts::{
            MAX_SHADOW_DESCRIPTORS, SHADOW_DESCRIPTOR_BYTES, SHADOW_VIEW_BYTES, SHADOW_VIEW_STRIDE,
        },
        error::AwsmShadowError,
        evsm,
        light_shadow::LightShadowHardness,
    },
};

/// Total byte size of the descriptor uniform array — derived from
/// `MAX_SHADOW_DESCRIPTORS × SHADOW_DESCRIPTOR_BYTES`. Cached in a
/// `LazyLock` so the multiplication happens once at first use; both
/// the construction path (`Shadows::new`) and the per-frame upload
/// (`write_gpu`) compare against this.
pub(super) static SHADOW_DESCRIPTOR_UNIFORM_BYTES: LazyLock<usize> =
    LazyLock::new(|| MAX_SHADOW_DESCRIPTORS as usize * SHADOW_DESCRIPTOR_BYTES);

// For 2D descriptors `cascade_y_param` is world-units-per-shadow-map-
// texel (used to scale the PCF kernel for consistent world-space
// softness across cascades). For cube descriptors the caller patches
// it with the cube-pool slot index right after this returns.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_shadow_descriptor(
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

/// Writes a directional-cascade descriptor (kind = 3) whose depth
/// lives in the cascade-array texture at layer `cascade_layer`.
/// `used_res` is the cascade's effective square resolution; the layer
/// itself is `layer_resolution²`, with the cascade rendered into the
/// top-left `used_res × used_res` sub-rect. The packed `atlas_rect`
/// uses `.x` to carry the layer index (as `f32`) and `.zw` to carry
/// the sub-rect width/height in normalised UV space; `.y` stays zero
/// since the cascade always starts at the layer's origin.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_shadow_cascade_array_descriptor(
    dest: &mut [u8],
    view_projection: &Mat4,
    cascade_layer: u32,
    used_res: u32,
    layer_resolution: u32,
    depth_bias: f32,
    normal_bias: f32,
    hardness: LightShadowHardness,
    pcss_scale: f32,
    world_per_texel: f32,
    cascade_count: u32,
    split_far: f32,
) {
    debug_assert!(dest.len() >= SHADOW_DESCRIPTOR_BYTES);
    let cols = view_projection.to_cols_array();
    let mat_bytes: &[u8] = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
    dest[0..64].copy_from_slice(mat_bytes);
    let inv = if layer_resolution == 0 {
        1.0
    } else {
        1.0 / layer_resolution as f32
    };
    // `atlas_rect.x` carries the f32 layer index — the receiver
    // converts back via `u32(rect.x)`. `.y` is always zero (cascade
    // starts at layer origin). `.zw` is the valid sub-rect size in
    // normalised UV so the PCF / PCSS tile clamp keeps reads inside
    // the cascade even when the layer is larger than needed.
    let layer_f = cascade_layer as f32;
    let w = used_res as f32 * inv;
    let h = used_res as f32 * inv;
    dest[64..68].copy_from_slice(&layer_f.to_ne_bytes());
    dest[68..72].copy_from_slice(&0.0_f32.to_ne_bytes());
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
    dest[96..100].copy_from_slice(&split_far.to_ne_bytes());
    dest[100..104].copy_from_slice(&world_per_texel.to_ne_bytes());
    dest[104..108].copy_from_slice(&(cascade_count as f32).to_ne_bytes());
    // cascade_info.w = 3.0 → cascade-array PCF.
    dest[108..112].copy_from_slice(&3.0_f32.to_ne_bytes());
}

pub(super) fn build_evsm_moment_write_bind_group(
    gpu: &AwsmRendererWebGpu,
    bind_group_layouts: &BindGroupLayouts,
    layout_key: BindGroupLayoutKey,
    cascade_array_view: &web_sys::GpuTextureView,
    evsm_atlas_view: &web_sys::GpuTextureView,
    params_buffer: &web_sys::GpuBuffer,
) -> Result<web_sys::GpuBindGroup, AwsmShadowError> {
    let entries = vec![
        BindGroupEntry::new(
            0,
            BindGroupResource::TextureView(Cow::Borrowed(cascade_array_view)),
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

pub(super) fn build_evsm_blur_bind_group(
    gpu: &AwsmRendererWebGpu,
    bind_group_layouts: &BindGroupLayouts,
    layout_key: BindGroupLayoutKey,
    src_view: &web_sys::GpuTextureView,
    dst_view: &web_sys::GpuTextureView,
    params_buffer: &web_sys::GpuBuffer,
    label: &str,
) -> Result<web_sys::GpuBindGroup, AwsmShadowError> {
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

/// Builds a `RenderPipelineCacheKey` for one shadow-caster pipeline
/// variant. Pure-sync — caller is responsible for ensuring
/// `shader_key` is already in the `Shaders` cache before passing it
/// in. Lifted out of the async per-pipeline builder so the four
/// shadow variants can be issued through one batched
/// `RenderPipelines::ensure_keys` call.
pub(crate) fn shadow_pipeline_cache_key(
    shader_key: crate::shaders::ShaderKey,
    pipeline_layout_key: PipelineLayoutKey,
    instancing: bool,
    cube_face: bool,
    double_sided: bool,
) -> RenderPipelineCacheKey {
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
    // Double-sided casters (thin / open geometry like a cutout panel or a
    // single-quad leaf) have no back face to use as the depth-bias buffer, so
    // Front culling would drop them entirely — a plane facing the light writes
    // nothing and casts no shadow. Render both faces (`CullMode::None`); the
    // slope-scale depth bias above is the acne safety net these surfaces rely on
    // instead of geometric thickness. `front_face` is irrelevant when nothing is
    // culled, so the cube Cw/Ccw split below is harmless in this branch.
    let cull_mode = if double_sided {
        CullMode::None
    } else {
        CullMode::Front
    };
    let primitive = PrimitiveState::new()
        .with_topology(PrimitiveTopology::TriangleList)
        .with_front_face(front_face)
        .with_cull_mode(cull_mode);

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
    pipeline_cache_key
}

/// Writes one entry into the per-view shadow uniform buffer at slot
/// `view_slot`. Buffer is laid out at `SHADOW_VIEW_STRIDE`-byte stride
/// so dynamic offsets stay aligned; only the first
/// `SHADOW_VIEW_BYTES` of each slot carry data.
pub(super) fn write_shadow_view_slot(
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
pub(super) fn view_projection_drift(prev: &Mat4, current: &Mat4) -> f32 {
    let a = prev.to_cols_array();
    let b = current.to_cols_array();
    let mut acc = 0.0_f32;
    for i in 0..16 {
        acc += (a[i] - b[i]).abs();
    }
    acc
}

/// Extracts the world-space near + far planes from a projection
/// matrix. Handles glam's right-handed perspective convention; falls
/// back to `(0.1, 100.0)` for matrices we don't recognise
/// (orthographic, custom).
pub(super) fn extract_near_far(projection: &Mat4) -> (f32, f32) {
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

/// 2D-array sampling view of the cascade depth texture. Receivers
/// sample with `textureSampleCompareLevel(tex, samp, uv, layer, ref)`.
pub(super) fn create_cascade_array_view(
    texture: &web_sys::GpuTexture,
) -> Result<web_sys::GpuTextureView, AwsmShadowError> {
    let descriptor: web_sys::GpuTextureViewDescriptor =
        TextureViewDescriptor::new(Some("Shadow Cascade Array"))
            .with_dimension(TextureViewDimension::N2dArray)
            .into();
    texture
        .create_view_with_descriptor(&descriptor)
        .map_err(AwsmCoreError::create_texture_view)
        .map_err(Into::into)
}

/// One 2D depth view per cascade layer, used as the render attachment
/// during shadow generation. Built once at cascade-array allocation
/// time so the per-frame pass loop can grab the right attachment
/// without re-creating the view.
pub(super) fn build_cascade_layer_views(
    texture: &web_sys::GpuTexture,
    layer_count: u32,
) -> Result<Vec<web_sys::GpuTextureView>, AwsmShadowError> {
    let mut views = Vec::with_capacity(layer_count as usize);
    for layer in 0..layer_count {
        let descriptor: web_sys::GpuTextureViewDescriptor =
            TextureViewDescriptor::new(Some("Shadow Cascade Layer"))
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

pub(super) fn create_cube_array_view(
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

/// Alternative 2D-array view of the cube pool. The cube-array view
/// gives `textureSampleCompare(cubedir, layer, ref)` for the standard
/// per-direction depth compare, but PCSS needs to *read* raw depth
/// values at specific cube-face texels for the blocker search — and
/// `texture_depth_cube_array` exposes no `textureLoad`. The same
/// underlying texture, viewed as `texture_depth_2d_array`, supports
/// the per-texel load: face index `slot * 6 + face` becomes the
/// array layer.
pub(super) fn create_cube_2d_array_view(
    texture: &web_sys::GpuTexture,
) -> Result<web_sys::GpuTextureView, AwsmShadowError> {
    let descriptor: web_sys::GpuTextureViewDescriptor =
        TextureViewDescriptor::new(Some("Shadow Cube 2D-Array"))
            .with_dimension(TextureViewDimension::N2dArray)
            .into();
    texture
        .create_view_with_descriptor(&descriptor)
        .map_err(AwsmCoreError::create_texture_view)
        .map_err(Into::into)
}

/// One 2D-array depth view per cube face. Indexed as
/// `slot_index * 6 + face_index` so the render-pass dispatch can grab
/// the right attachment without rebuilding the view each frame.
pub(super) fn build_cube_face_views(
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
