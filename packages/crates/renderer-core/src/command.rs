//! Command encoder wrappers and pass helpers.

pub mod color;
pub mod compute_pass;
pub mod copy_texture;
pub mod render_pass;

use std::ops::Deref;

use crate::error::{AwsmCoreError, Result};
use compute_pass::ComputePassEncoder;
use render_pass::RenderPassEncoder;

/// Wrapper around a WebGPU command encoder.
#[derive(Debug, Clone)]
pub struct CommandEncoder {
    inner: web_sys::GpuCommandEncoder,
}

impl Deref for CommandEncoder {
    type Target = web_sys::GpuCommandEncoder;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl CommandEncoder {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUCommandEncoder#instance_methods
    /// Wraps a WebGPU command encoder.
    pub fn new(inner: web_sys::GpuCommandEncoder) -> Self {
        Self { inner }
    }

    /// Begins a compute pass.
    pub fn begin_compute_pass(
        &self,
        descriptor: Option<&web_sys::GpuComputePassDescriptor>,
    ) -> ComputePassEncoder {
        ComputePassEncoder::new(match descriptor {
            Some(descriptor) => self.inner.begin_compute_pass_with_descriptor(descriptor),
            None => self.inner.begin_compute_pass(),
        })
    }

    /// Begins a render pass.
    pub fn begin_render_pass(
        &self,
        descriptor: &web_sys::GpuRenderPassDescriptor,
    ) -> Result<RenderPassEncoder> {
        Ok(RenderPassEncoder::new(
            self.inner
                .begin_render_pass(descriptor)
                .map_err(AwsmCoreError::command_render_pass)?,
        ))
    }

    /// Clears a buffer range.
    pub fn clear_buffer(
        &self,
        buffer: &web_sys::GpuBuffer,
        offset: Option<u32>,
        size: Option<u32>,
    ) {
        match (offset, size) {
            (Some(offset), Some(size)) => self
                .inner
                .clear_buffer_with_u32_and_u32(buffer, offset, size),
            (Some(offset), None) => self.inner.clear_buffer_with_u32(buffer, offset),
            (None, Some(size)) => self.inner.clear_buffer_with_u32_and_u32(buffer, 0, size),
            (None, None) => self.inner.clear_buffer(buffer),
        }
    }

    /// Copies data from one buffer to another.
    pub fn copy_buffer_to_buffer(
        &self,
        source: &web_sys::GpuBuffer,
        source_offset: u32,
        destination: &web_sys::GpuBuffer,
        destination_offset: u32,
        size: u32,
    ) -> Result<()> {
        self.inner
            .copy_buffer_to_buffer_with_u32_and_u32_and_u32(
                source,
                source_offset,
                destination,
                destination_offset,
                size,
            )
            .map_err(AwsmCoreError::command_copy_buffer_to_buffer)
    }

    /// Copies a buffer region into a texture.
    pub fn copy_buffer_to_texture(
        &self,
        source: &web_sys::GpuTexelCopyBufferInfo,
        destination: &web_sys::GpuTexelCopyTextureInfo,
        copy_size: &web_sys::GpuExtent3dDict,
    ) -> Result<()> {
        self.inner
            .copy_buffer_to_texture_with_gpu_extent_3d_dict(source, destination, copy_size)
            .map_err(AwsmCoreError::command_copy_buffer_to_texture)
    }

    /// Copies a texture region into a buffer.
    pub fn copy_texture_to_buffer(
        &self,
        source: &web_sys::GpuTexelCopyTextureInfo,
        destination: &web_sys::GpuTexelCopyBufferInfo,
        copy_size: &web_sys::GpuExtent3dDict,
    ) -> Result<()> {
        self.inner
            .copy_texture_to_buffer_with_gpu_extent_3d_dict(source, destination, copy_size)
            .map_err(AwsmCoreError::command_copy_texture_to_buffer)
    }

    /// Copies a texture region into another texture.
    pub fn copy_texture_to_texture(
        &self,
        source: &web_sys::GpuTexelCopyTextureInfo,
        destination: &web_sys::GpuTexelCopyTextureInfo,
        copy_size: &web_sys::GpuExtent3dDict,
    ) -> Result<()> {
        self.inner
            .copy_texture_to_texture_with_gpu_extent_3d_dict(source, destination, copy_size)
            .map_err(AwsmCoreError::command_copy_texture_to_texture)
    }

    /// Resolves a query set into a destination buffer.
    pub fn resolve_query_set(
        &self,
        query_set: &web_sys::GpuQuerySet,
        first_query: u32,
        query_count: u32,
        destination: &web_sys::GpuBuffer,
        destination_offset: u32,
    ) {
        self.inner.resolve_query_set_with_u32(
            query_set,
            first_query,
            query_count,
            destination,
            destination_offset,
        );
    }

    /// Inserts a debug marker into the command stream.
    pub fn insert_debug_marker(&self, label: &str) {
        self.inner.insert_debug_marker(label);
    }

    /// Pushes a debug group onto the command stream.
    pub fn push_debug_group(&self, group_label: &str) {
        self.inner.push_debug_group(group_label);
    }

    /// Pops the last debug group.
    pub fn pop_debug_group(&self) {
        self.inner.pop_debug_group();
    }

    /// Finishes encoding and returns the command buffer.
    pub fn finish(&self) -> web_sys::GpuCommandBuffer {
        self.inner.finish()
    }
}

/// WebGPU load operation alias.
// https://docs.rs/web-sys/latest/web_sys/enum.GpuLoadOp.html
/// WebGPU load operation.
pub type LoadOp = web_sys::GpuLoadOp;
/// WebGPU store operation alias.
// https://docs.rs/web-sys/latest/web_sys/enum.GpuStoreOp.html
/// WebGPU store operation.
pub type StoreOp = web_sys::GpuStoreOp;
