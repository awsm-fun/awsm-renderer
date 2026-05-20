use awsm_renderer_core::{
    bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource},
    buffers::{BufferBinding, BufferDescriptor, BufferUsage},
    renderer::AwsmRendererWebGpu,
};
use glam::{Vec3, Vec4};

use crate::{
    bind_group_layout::{BindGroupLayoutKey, BindGroupLayouts},
    camera::CameraBuffer,
    error::Result,
};

use super::types::{GpuLineSegment, LineTopology, LINE_UNIFORM_BYTES, SEGMENT_BYTES};

pub(super) fn pack(
    positions: &[Vec3],
    colors: &[Vec4],
    topology: LineTopology,
) -> Vec<GpuLineSegment> {
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

pub(super) fn segments_byte_size(segment_count: usize) -> usize {
    (segment_count.max(1)) * SEGMENT_BYTES
}

pub(super) fn create_segment_buffer(
    gpu: &AwsmRendererWebGpu,
    byte_size: usize,
) -> Result<web_sys::GpuBuffer> {
    Ok(gpu.create_buffer(
        &BufferDescriptor::new(
            Some("LineSegments"),
            byte_size,
            BufferUsage::new().with_copy_dst().with_storage(),
        )
        .into(),
    )?)
}

pub(super) fn create_uniform_buffer(gpu: &AwsmRendererWebGpu) -> Result<web_sys::GpuBuffer> {
    Ok(gpu.create_buffer(
        &BufferDescriptor::new(
            Some("LineUniform"),
            LINE_UNIFORM_BYTES,
            BufferUsage::new().with_copy_dst().with_uniform(),
        )
        .into(),
    )?)
}

pub(super) fn write_segments(
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

pub(super) fn create_bind_group(
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
