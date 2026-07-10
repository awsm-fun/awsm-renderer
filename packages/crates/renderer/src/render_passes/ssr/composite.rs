//! SSR composite pass.
//!
//! The SSR trace writes reflection-ONLY, premultiplied color (alpha = coverage;
//! misses / sky / opt-out pixels write 0) into the (usually HALF-res) `ssr`
//! target. This pass ADDITIVELY blends that over the resolved single-sample
//! `composite` HDR via a fullscreen triangle, so non-reflective pixels are left
//! untouched (they added 0). Replaces the old overwrite blit (which copied
//! `base + reflection` back and blurred non-reflective pixels at half-res).
//!
//! M4b — EDGE-AWARE (joint-bilateral) UPSAMPLE. A plain LINEAR upsample of the
//! half-res `ssr` target bleeds reflections across geometry silhouettes (the
//! bilinear footprint straddles a depth discontinuity), showing as blocky halos
//! in near-mirror reflections. Instead the fragment gathers the 4 nearest
//! half-res reflection texels and weights each by `bilinear_weight *
//! exp(-|z_tap - z_center| / sigma)` in VIEW-space linear Z — reconstructed from
//! the SAME full-res post-opaque depth the trace reads, via the same
//! `inv_proj`-based `view_pos_from_depth`. Taps on the far side of an edge are
//! attenuated, so reflection energy stays on-surface. At full-res the 4 taps
//! collapse onto one texel → the weighting degenerates to ~identity (no separate
//! code path). If every tap is rejected (weights collapse) it falls back to the
//! nearest (highest bilinear weight) tap.

use std::borrow::Cow;

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
        BindGroupLayoutResource, BindGroupResource, BufferBindingLayout, BufferBindingType,
        TextureBindingLayout,
    },
    buffers::BufferBinding,
    command::{
        render_pass::{ColorAttachment, RenderPassDescriptor},
        LoadOp, StoreOp,
    },
    pipeline::{
        fragment::{
            BlendComponent, BlendFactor, BlendOperation, BlendState, ColorTargetState,
            FragmentState,
        },
        layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
        primitive::PrimitiveState,
        vertex::VertexState,
        RenderPipelineDescriptor,
    },
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    texture::{TextureSampleType, TextureViewDimension},
};

use crate::{
    bind_groups::{AwsmBindGroupError, BindGroupRecreateContext},
    error::Result,
    render::RenderContext,
    render_passes::RenderPassInitContext,
};

/// Builds the composite WGSL. Under MSAA the post-opaque depth target is
/// multisampled, so the depth binding's WGSL type differs (`texture_depth_2d`
/// vs `texture_depth_multisampled_2d`) — mirroring the SSR trace's own
/// depth-binding handling. The `textureLoad(..., 0)` call is identical for both
/// (level 0 vs sample 0), so only the declaration line is swapped. Depth is read
/// via `textureLoad` at integer coords exactly like `trace.wgsl` (non-filterable).
pub(crate) fn shader_source(multisampled: bool) -> String {
    let depth_decl = if multisampled {
        "@group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;"
    } else {
        "@group(0) @binding(2) var depth_tex: texture_depth_2d;"
    };
    format!(
        r#"
// Full CameraRaw layout (matches shared_wgsl/camera.wgsl / the GPU uniform);
// only `inv_proj` is read here, but the struct must match the buffer's layout.
struct CameraRaw {{
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    inv_proj: mat4x4<f32>,
    inv_view: mat4x4<f32>,
    position: vec4<f32>,
    frustum_rays: array<vec4<f32>, 4>,
    viewport: vec4<f32>,
    dof_params: vec4<f32>,
}};

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
{depth_decl}

// Reconstruct VIEW-space position from a hardware depth sample at `uv`
// (forward-Z [0,1], NDC y flipped vs UV). Same convention as trace.wgsl's
// `view_pos_from_depth`.
fn view_pos_from_depth(uv: vec2<f32>, depth: f32, inv_proj: mat4x4<f32>) -> vec3<f32> {{
    let ndc = vec3<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth);
    let v = inv_proj * vec4<f32>(ndc, 1.0);
    return v.xyz / v.w;
}}

// Positive linear view-space depth (view looks down -Z, so +linear = -z).
fn linear_z(uv: vec2<f32>, depth: f32, inv_proj: mat4x4<f32>) -> f32 {{
    return -view_pos_from_depth(uv, depth, inv_proj).z;
}}

struct VsOut {{
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}};

@vertex
fn vert_main(@builtin(vertex_index) vi: u32) -> VsOut {{
    var out: VsOut;
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    // Screen-space UV (y-down, top-left origin) matching the compute writes.
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}}

@fragment
fn frag_main(in: VsOut) -> @location(0) vec4<f32> {{
    let inv_proj = camera_raw.inv_proj;
    let full_dims = vec2<f32>(textureDimensions(depth_tex));
    let half_dims = vec2<f32>(textureDimensions(src_tex));
    let half_max = vec2<i32>(half_dims) - vec2<i32>(1, 1);

    // Full-res destination depth → center view-Z. Sky (depth>=1) has nothing to
    // reflect; the trace wrote 0 there, so keep the additive no-op.
    let center_depth = textureLoad(depth_tex, vec2<i32>(in.uv * full_dims), 0);
    if (center_depth >= 1.0) {{
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }}
    let z_center = linear_z(in.uv, center_depth, inv_proj);

    // Locate the destination inside the half-res texel grid (texel centers at
    // integer+0.5). `f` is the bilinear fraction toward the +1 neighbour.
    let h = in.uv * half_dims - vec2<f32>(0.5, 0.5);
    let base = vec2<i32>(floor(h));
    let f = h - floor(h);

    // sigma: edge-stopping width in VIEW-space linear Z. Scale-relative — 5% of
    // the center depth (so a fixed pixel-to-pixel depth ratio behaves the same at
    // any distance), floored at 1e-2 world units to stay well-conditioned near
    // the camera. A tap 5% deeper than center gets ~e^-1 (0.37) weight, 15% ~0.05.
    let sigma = max(z_center * 0.05, 1e-2);

    var sum_color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var sum_w = 0.0;
    var best_bw = -1.0;
    var best_color = vec4<f32>(0.0, 0.0, 0.0, 0.0);

    for (var j = 0; j < 2; j = j + 1) {{
        for (var i = 0; i < 2; i = i + 1) {{
            let tap = clamp(base + vec2<i32>(i, j), vec2<i32>(0, 0), half_max);
            let refl = textureLoad(src_tex, tap, 0);
            // Bilinear weight for this corner.
            let wx = select(1.0 - f.x, f.x, i == 1);
            let wy = select(1.0 - f.y, f.y, j == 1);
            let bw = wx * wy;
            // Full-res depth under this half-res tap's center.
            let tap_uv = (vec2<f32>(tap) + vec2<f32>(0.5, 0.5)) / half_dims;
            let tap_depth = textureLoad(depth_tex, vec2<i32>(tap_uv * full_dims), 0);
            let z_tap = linear_z(tap_uv, tap_depth, inv_proj);
            let dw = exp(-abs(z_tap - z_center) / sigma);
            let w = bw * dw;
            sum_color = sum_color + refl * w;
            sum_w = sum_w + w;
            if (bw > best_bw) {{
                best_bw = bw;
                best_color = refl;
            }}
        }}
    }}

    // Fall back to the nearest tap if every depth weight collapsed.
    if (sum_w > 1e-5) {{
        return sum_color / sum_w;
    }}
    return best_color;
}}
"#
    )
}

pub struct SsrComposite {
    bind_group_layout: web_sys::GpuBindGroupLayout,
    pipeline: web_sys::GpuRenderPipeline,
    bind_group: Option<web_sys::GpuBindGroup>,
}

impl SsrComposite {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let gpu = ctx.gpu;
        // Under MSAA the post-opaque depth target is multisampled — match the SSR
        // trace's depth-binding variant so the joint-bilateral upsample reads the
        // same buffer under both AA configs.
        let multisampled = ctx.anti_aliasing.msaa_sample_count.is_some();
        let shader_module = gpu.compile_shader(
            &ShaderModuleDescriptor::new(&shader_source(multisampled), Some("SSR Composite shader"))
                .into(),
        );
        shader_module.validate_shader().await?;

        let bind_group_layout = gpu.create_bind_group_layout(
            &BindGroupLayoutDescriptor::new(Some("SSR Composite BGL"))
                .with_entries(vec![
                    // 0 — camera uniform (CameraRaw) for depth → view-Z linearize.
                    BindGroupLayoutEntry::new(
                        0,
                        BindGroupLayoutResource::Buffer(
                            BufferBindingLayout::new()
                                .with_binding_type(BufferBindingType::Uniform),
                        ),
                    )
                    .with_visibility_fragment(),
                    // 1 — SSR reflection target (sampled via textureLoad only).
                    BindGroupLayoutEntry::new(
                        1,
                        BindGroupLayoutResource::Texture(
                            TextureBindingLayout::new()
                                .with_sample_type(TextureSampleType::Float)
                                .with_view_dimension(TextureViewDimension::N2d),
                        ),
                    )
                    .with_visibility_fragment(),
                    // 2 — full-res post-opaque depth (non-filterable, textureLoad).
                    // Multisampled under MSAA, mirroring the SSR trace binding.
                    BindGroupLayoutEntry::new(
                        2,
                        BindGroupLayoutResource::Texture(
                            TextureBindingLayout::new()
                                .with_sample_type(TextureSampleType::Depth)
                                .with_view_dimension(TextureViewDimension::N2d)
                                .with_multisampled(multisampled),
                        ),
                    )
                    .with_visibility_fragment(),
                ])
                .into(),
        )?;

        let pipeline_layout = gpu.create_pipeline_layout(
            &PipelineLayoutDescriptor::new(
                Some("SSR Composite Layout"),
                vec![bind_group_layout.clone()],
            )
            .into(),
        );

        // Additive blend: `composite_new = ssr_reflection + composite_old`.
        // The `composite` target is always the resolved single-sample HDR
        // (SSR runs post-resolve), so there is no MSAA variant.
        let format = ctx.render_texture_formats.color;
        let color_target = ColorTargetState::new(format).with_blend(BlendState::new(
            BlendComponent::new()
                .with_src_factor(BlendFactor::One)
                .with_dst_factor(BlendFactor::One)
                .with_operation(BlendOperation::Add),
            BlendComponent::new()
                .with_src_factor(BlendFactor::One)
                .with_dst_factor(BlendFactor::One)
                .with_operation(BlendOperation::Add),
        ));
        let vertex = VertexState::new(&shader_module, None);
        let fragment = FragmentState::new(&shader_module, None, vec![color_target]);
        let descriptor = RenderPipelineDescriptor::new(vertex, Some("SSR Composite"))
            .with_primitive(PrimitiveState::new())
            .with_layout(PipelineLayoutKind::Custom(&pipeline_layout))
            .with_fragment(fragment);
        let pipeline = gpu.create_render_pipeline(&descriptor.into()).await?;

        Ok(Self {
            bind_group_layout,
            pipeline,
            bind_group: None,
        })
    }

    fn get_bind_group(&self) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("SSR Composite".to_string()))
    }

    /// Rebuilds the composite bind group against the live camera / `ssr` /
    /// depth views. Dispatched on `TextureViewRecreate` alongside the SSR trace
    /// bind group. Camera + depth mirror the SSR trace's bindings so the
    /// joint-bilateral upsample linearizes against the same buffers.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.ssr)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.depth)),
            ),
        ];
        let descriptor =
            BindGroupDescriptor::new(&self.bind_group_layout, Some("SSR Composite"), entries);
        self.bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));
        Ok(())
    }

    /// Records the additive composite render pass — fullscreen triangle onto
    /// `composite` with a Load op (the blend reads the existing HDR content).
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let render_pass = ctx.command_encoder.begin_render_pass(
            &RenderPassDescriptor {
                label: Some("SSR Composite Pass"),
                color_attachments: vec![ColorAttachment::new(
                    &ctx.render_texture_views.composite,
                    LoadOp::Load,
                    StoreOp::Store,
                )],
                depth_stencil_attachment: None,
                ..Default::default()
            }
            .into(),
        )?;
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, self.get_bind_group()?, None)?;
        render_pass.draw(3);
        render_pass.end();
        Ok(())
    }
}
