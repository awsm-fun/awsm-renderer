//! Fat-line render pipeline.
//!
//! A polyline is uploaded as `positions: &[Vec3] + colors: &[Vec4]`, packed
//! into a storage buffer of N-1 `GpuLineSegment` records (each segment carries
//! its endpoints + per-endpoint colors). The vertex shader expands each segment
//! into a screen-space triangle strip whose perpendicular offset is a fixed
//! pixel width, giving true 1-3px GPU widths without geometry-shader hacks.
//!
//! Each line owns its own storage buffer, uniform buffer (viewport + width),
//! and bind group. Per frame, [`render_lines`] re-writes the uniform buffer
//! with the current viewport size, then issues one draw call per line
//! (4 vertices × N-1 instances, `TriangleStrip` topology).
//!
//! Four pipeline variants cover the cross product
//! (`depth_compare = Less | Always`) × (`MSAA = on | off`).

use awsm_renderer_core::{
    bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource},
    buffers::{BufferBinding, BufferDescriptor, BufferUsage},
    renderer::AwsmRendererWebGpu,
};
use glam::{Vec3, Vec4};
use slotmap::{new_key_type, SlotMap};

use crate::{
    bind_group_layout::{BindGroupLayoutKey, BindGroupLayouts},
    camera::CameraBuffer,
    error::Result,
    pipeline_layouts::PipelineLayouts,
    pipelines::Pipelines,
    render::RenderContext,
    render_textures::RenderTextureFormats,
    shaders::Shaders,
    AwsmRenderer,
};

pub mod pipelines;

use pipelines::{LinePipelines, LineVariantKey};

new_key_type! {
    /// Identifier for a registered line strip.
    pub struct LineKey;
}

/// One `LineSegment` written into the per-line storage buffer (48 bytes).
#[repr(C)]
#[derive(Copy, Clone)]
struct GpuLineSegment {
    a: [f32; 4],       // .xyz = position A, .w = pad
    color_a: [f32; 4], // RGBA at A
    b: [f32; 4],       // .xyz = position B, .w = pad
    color_b: [f32; 4], // RGBA at B
}

const SEGMENT_BYTES: usize = std::mem::size_of::<GpuLineSegment>();

/// 16 bytes — `width_px`, `viewport_w`, `viewport_h`, `_pad`.
const LINE_UNIFORM_BYTES: usize = 16;

/// Per-line GPU state.
struct LineEntry {
    segment_count: u32,
    width_px: f32,
    depth_test_always: bool,
    segment_buffer: web_sys::GpuBuffer,
    segment_capacity_bytes: usize,
    uniform_buffer: web_sys::GpuBuffer,
    bind_group: web_sys::GpuBindGroup,
}

/// Renderer-side state owning the four line pipelines and every registered line strip.
pub struct LineRenderer {
    pipelines: LinePipelines,
    entries: SlotMap<LineKey, LineEntry>,
}

impl LineRenderer {
    /// Loads the four pipeline variants and creates an empty line registry.
    pub async fn load(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        pipelines: &mut Pipelines,
        shaders: &mut Shaders,
        formats: &RenderTextureFormats,
    ) -> Result<Self> {
        let pipelines = LinePipelines::load(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            pipelines,
            shaders,
            formats,
        )
        .await?;
        Ok(Self {
            pipelines,
            entries: SlotMap::with_key(),
        })
    }

    /// Returns the number of registered lines.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if there are no registered lines.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Packing topology for `positions`/`colors` into `GpuLineSegment` records.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LineTopology {
    /// `positions[i] → positions[i+1]` for each adjacent pair (connected
    /// polyline). N points produce N-1 segments.
    Strip,
    /// `positions[2*i] → positions[2*i+1]` for each pair (disjoint
    /// segments — the same model as line-list topology). N points
    /// produce N/2 segments.
    Segments,
}

impl AwsmRenderer {
    /// Registers a new line strip: `positions[i] → positions[i+1]` for each
    /// adjacent pair, with per-vertex colors interpolated A→B. `width` is in
    /// CSS pixels. `depth_test_always = true` makes the line draw through any
    /// existing depth (useful for debug overlays).
    ///
    /// Returns `None` if `positions.len() < 2` (no segments to draw).
    pub fn add_line_strip(
        &mut self,
        positions: &[Vec3],
        colors: &[Vec4],
        width: f32,
        depth_test_always: bool,
    ) -> Result<Option<LineKey>> {
        self.add_line(
            positions,
            colors,
            width,
            depth_test_always,
            LineTopology::Strip,
        )
    }

    /// Registers a disjoint-segments line draw (line-list semantics).
    /// `positions` must be even-length; consecutive pairs become independent
    /// segments. Useful for wireframe geometry where edges are not connected.
    pub fn add_line_segments(
        &mut self,
        positions: &[Vec3],
        colors: &[Vec4],
        width: f32,
        depth_test_always: bool,
    ) -> Result<Option<LineKey>> {
        self.add_line(
            positions,
            colors,
            width,
            depth_test_always,
            LineTopology::Segments,
        )
    }

    fn add_line(
        &mut self,
        positions: &[Vec3],
        colors: &[Vec4],
        width: f32,
        depth_test_always: bool,
        topology: LineTopology,
    ) -> Result<Option<LineKey>> {
        let segments = pack(positions, colors, topology);
        if segments.is_empty() {
            return Ok(None);
        }
        let segment_bytes = segments_byte_size(segments.len());

        let segment_buffer = create_segment_buffer(&self.gpu, segment_bytes)?;
        write_segments(&self.gpu, &segment_buffer, &segments)?;

        let uniform_buffer = create_uniform_buffer(&self.gpu)?;

        let bind_group_layout_key = self.lines.pipelines.bind_group_layout_key;
        let bind_group = create_bind_group(
            &self.gpu,
            &self.bind_group_layouts,
            bind_group_layout_key,
            &self.camera,
            &segment_buffer,
            &uniform_buffer,
        )?;

        let key = self.lines.entries.insert(LineEntry {
            segment_count: segments.len() as u32,
            width_px: width.max(0.5),
            depth_test_always,
            segment_buffer,
            segment_capacity_bytes: segment_bytes,
            uniform_buffer,
            bind_group,
        });
        Ok(Some(key))
    }

    /// Re-uploads positions + colors into an existing line strip. The segment
    /// buffer + bind group are reallocated if the new segment count exceeds
    /// the current capacity. The depth-test mode + width are preserved.
    pub fn update_line_strip(
        &mut self,
        key: LineKey,
        positions: &[Vec3],
        colors: &[Vec4],
    ) -> Result<()> {
        self.update_line(key, positions, colors, LineTopology::Strip)
    }

    /// Re-uploads positions + colors as line-list pairs (see [`add_line_segments`]).
    pub fn update_line_segments(
        &mut self,
        key: LineKey,
        positions: &[Vec3],
        colors: &[Vec4],
    ) -> Result<()> {
        self.update_line(key, positions, colors, LineTopology::Segments)
    }

    fn update_line(
        &mut self,
        key: LineKey,
        positions: &[Vec3],
        colors: &[Vec4],
        topology: LineTopology,
    ) -> Result<()> {
        if !self.lines.entries.contains_key(key) {
            return Ok(());
        }
        let bind_group_layout_key = self.lines.pipelines.bind_group_layout_key;
        let segments = pack(positions, colors, topology);
        let entry = self.lines.entries.get_mut(key).expect("checked above");
        if segments.is_empty() {
            entry.segment_count = 0;
            return Ok(());
        }
        let new_bytes = segments_byte_size(segments.len());
        if new_bytes > entry.segment_capacity_bytes {
            entry.segment_buffer = create_segment_buffer(&self.gpu, new_bytes)?;
            entry.segment_capacity_bytes = new_bytes;
            entry.bind_group = create_bind_group(
                &self.gpu,
                &self.bind_group_layouts,
                bind_group_layout_key,
                &self.camera,
                &entry.segment_buffer,
                &entry.uniform_buffer,
            )?;
        }
        write_segments(&self.gpu, &entry.segment_buffer, &segments)?;
        entry.segment_count = segments.len() as u32;
        Ok(())
    }

    /// Sets the per-line width (in CSS pixels). The change takes effect on
    /// the next frame.
    pub fn set_line_width(&mut self, key: LineKey, width: f32) {
        if let Some(entry) = self.lines.entries.get_mut(key) {
            entry.width_px = width.max(0.5);
        }
    }

    /// Sets the per-line depth-test mode. Takes effect on the next frame.
    pub fn set_line_depth_test_always(&mut self, key: LineKey, depth_test_always: bool) {
        if let Some(entry) = self.lines.entries.get_mut(key) {
            entry.depth_test_always = depth_test_always;
        }
    }

    /// Removes a registered line strip. Subsequent frames will not draw it.
    pub fn remove_line(&mut self, key: LineKey) {
        self.lines.entries.remove(key);
    }

    /// Number of registered line strips.
    pub fn line_count(&self) -> usize {
        self.lines.entries.len()
    }
}

impl LineRenderer {
    /// Executes the line render pass: re-writes each line's uniform buffer
    /// with the current viewport size + width, then draws all registered lines
    /// against the world-space transparent target. Safe to call with zero
    /// registered lines (it returns early).
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let msaa = ctx.anti_aliasing.has_msaa_checked()?;
        let viewport_w = ctx.render_texture_views.width as f32;
        let viewport_h = ctx.render_texture_views.height as f32;

        let render_pass = ctx.begin_world_transparent_pass(Some("Line Render Pass"))?;
        let mut current_variant: Option<LineVariantKey> = None;

        for entry in self.entries.values() {
            if entry.segment_count == 0 {
                continue;
            }
            let mut uniform_bytes = [0u8; LINE_UNIFORM_BYTES];
            uniform_bytes[0..4].copy_from_slice(&entry.width_px.to_le_bytes());
            uniform_bytes[4..8].copy_from_slice(&viewport_w.to_le_bytes());
            uniform_bytes[8..12].copy_from_slice(&viewport_h.to_le_bytes());
            ctx.gpu
                .write_buffer(&entry.uniform_buffer, None, &uniform_bytes[..], None, None)?;

            let variant = LineVariantKey {
                depth_test_always: entry.depth_test_always,
                msaa,
            };
            if current_variant != Some(variant) {
                let pipeline_key = self.pipelines.get(variant);
                render_pass.set_pipeline(ctx.pipelines.render.get(pipeline_key)?);
                current_variant = Some(variant);
            }
            render_pass.set_bind_group(0, &entry.bind_group, None)?;
            // 4 vertices per instance (triangle strip quad), N-1 instances.
            // Web GPU instanced non-indexed draw.
            render_pass.draw_with_instance_count(4, entry.segment_count);
        }
        render_pass.end();
        Ok(())
    }
}

fn pack(positions: &[Vec3], colors: &[Vec4], topology: LineTopology) -> Vec<GpuLineSegment> {
    if positions.len() < 2 {
        return Vec::new();
    }
    let last_color = colors.last().copied().unwrap_or(Vec4::ONE);
    let color_at = |i: usize| -> Vec4 { colors.get(i).copied().unwrap_or(last_color) };
    match topology {
        LineTopology::Strip => {
            let mut out = Vec::with_capacity(positions.len() - 1);
            for i in 0..positions.len() - 1 {
                out.push(GpuLineSegment {
                    a: [positions[i].x, positions[i].y, positions[i].z, 0.0],
                    color_a: color_at(i).to_array(),
                    b: [
                        positions[i + 1].x,
                        positions[i + 1].y,
                        positions[i + 1].z,
                        0.0,
                    ],
                    color_b: color_at(i + 1).to_array(),
                });
            }
            out
        }
        LineTopology::Segments => {
            let pair_count = positions.len() / 2;
            let mut out = Vec::with_capacity(pair_count);
            for i in 0..pair_count {
                let a = positions[2 * i];
                let b = positions[2 * i + 1];
                out.push(GpuLineSegment {
                    a: [a.x, a.y, a.z, 0.0],
                    color_a: color_at(2 * i).to_array(),
                    b: [b.x, b.y, b.z, 0.0],
                    color_b: color_at(2 * i + 1).to_array(),
                });
            }
            out
        }
    }
}

fn segments_byte_size(segment_count: usize) -> usize {
    (segment_count.max(1)) * SEGMENT_BYTES
}

fn create_segment_buffer(gpu: &AwsmRendererWebGpu, byte_size: usize) -> Result<web_sys::GpuBuffer> {
    Ok(gpu.create_buffer(
        &BufferDescriptor::new(
            Some("LineSegments"),
            byte_size,
            BufferUsage::new().with_copy_dst().with_storage(),
        )
        .into(),
    )?)
}

fn create_uniform_buffer(gpu: &AwsmRendererWebGpu) -> Result<web_sys::GpuBuffer> {
    Ok(gpu.create_buffer(
        &BufferDescriptor::new(
            Some("LineUniform"),
            LINE_UNIFORM_BYTES,
            BufferUsage::new().with_copy_dst().with_uniform(),
        )
        .into(),
    )?)
}

fn write_segments(
    gpu: &AwsmRendererWebGpu,
    buffer: &web_sys::GpuBuffer,
    segments: &[GpuLineSegment],
) -> Result<()> {
    let byte_data: &[u8] = unsafe {
        std::slice::from_raw_parts(
            segments.as_ptr() as *const u8,
            segments.len() * SEGMENT_BYTES,
        )
    };
    gpu.write_buffer(buffer, None, byte_data, None, None)?;
    Ok(())
}

fn create_bind_group(
    gpu: &AwsmRendererWebGpu,
    bind_group_layouts: &BindGroupLayouts,
    bind_group_layout_key: BindGroupLayoutKey,
    camera: &CameraBuffer,
    segment_buffer: &web_sys::GpuBuffer,
    uniform_buffer: &web_sys::GpuBuffer,
) -> Result<web_sys::GpuBindGroup> {
    Ok(gpu.create_bind_group(
        &BindGroupDescriptor::new(
            bind_group_layouts.get(bind_group_layout_key)?,
            Some("Line BindGroup"),
            vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::Buffer(BufferBinding::new(&camera.gpu_buffer)),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::Buffer(BufferBinding::new(segment_buffer)),
                ),
                BindGroupEntry::new(
                    2,
                    BindGroupResource::Buffer(BufferBinding::new(uniform_buffer)),
                ),
            ],
        )
        .into(),
    ))
}
