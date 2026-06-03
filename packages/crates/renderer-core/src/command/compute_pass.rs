//! Compute pass helpers.

use crate::error::{AwsmCoreError, Result};
use std::ops::Deref;

/// Wrapper for a WebGPU compute pass encoder.
#[derive(Debug, Clone)]
pub struct ComputePassEncoder {
    inner: web_sys::GpuComputePassEncoder,
}

impl ComputePassEncoder {
    /// Wraps a compute pass encoder.
    pub fn new(inner: web_sys::GpuComputePassEncoder) -> Self {
        Self { inner }
    }

    /// Dispatches compute workgroups.
    pub fn dispatch_workgroups(
        &self,
        workgroup_count_x: u32,
        workgroup_count_y: Option<u32>,
        workgroup_count_z: Option<u32>,
    ) {
        match (workgroup_count_y, workgroup_count_z) {
            (Some(y), Some(z)) => {
                self.inner
                    .dispatch_workgroups_with_workgroup_count_y_and_workgroup_count_z(
                        workgroup_count_x,
                        y,
                        z,
                    );
            }
            (Some(y), None) => {
                self.inner
                    .dispatch_workgroups_with_workgroup_count_y(workgroup_count_x, y);
            }
            (None, Some(z)) => {
                self.inner
                    .dispatch_workgroups_with_workgroup_count_y_and_workgroup_count_z(
                        workgroup_count_x,
                        1,
                        z,
                    );
            }
            (None, None) => {
                self.inner.dispatch_workgroups(workgroup_count_x);
            }
        }
    }

    /// Sets a bind group for the pass.
    pub fn set_bind_group(
        &self,
        index: u32,
        bind_group: &web_sys::GpuBindGroup,
        dynamic_offsets: Option<&[u32]>,
    ) -> Result<()> {
        match dynamic_offsets {
            Some(offsets) => self
                .inner
                .set_bind_group_with_u32_slice_and_f64_and_dynamic_offsets_data_length(
                    index,
                    Some(bind_group),
                    offsets,
                    0 as f64,
                    offsets.len() as u32,
                )
                .map_err(AwsmCoreError::set_bind_group)?,
            None => self.inner.set_bind_group(index, Some(bind_group)),
        }

        Ok(())
    }
}

impl Deref for ComputePassEncoder {
    type Target = web_sys::GpuComputePassEncoder;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Descriptor for starting a compute pass.
#[derive(Debug, Clone, Default)]
pub struct ComputePassDescriptor<'a> {
    pub label: Option<&'a str>,
    pub timestamp_writes: Option<ComputeTimestampWrites<'a>>,
}

impl<'a> ComputePassDescriptor<'a> {
    /// Creates a compute pass descriptor.
    pub fn new(label: Option<&'a str>) -> Self {
        Self {
            label,
            timestamp_writes: None,
        }
    }

    /// Sets timestamp writes for the pass.
    pub fn with_timestamp_writes(mut self, timestamp_writes: ComputeTimestampWrites<'a>) -> Self {
        self.timestamp_writes = Some(timestamp_writes);
        self
    }
}

/// Timestamp write configuration for a compute pass.
#[derive(Debug, Clone)]
pub struct ComputeTimestampWrites<'a> {
    pub query_set: &'a web_sys::GpuQuerySet,
    pub beginning_index: Option<u32>,
    pub end_index: Option<u32>,
}

// js conversions

impl From<ComputePassDescriptor<'_>> for web_sys::GpuComputePassDescriptor {
    fn from(pass: ComputePassDescriptor) -> web_sys::GpuComputePassDescriptor {
        let pass_js = web_sys::GpuComputePassDescriptor::new();

        if let Some(label) = pass.label {
            pass_js.set_label(label);
        }

        if let Some(timestamp_writes) = pass.timestamp_writes {
            pass_js.set_timestamp_writes(&web_sys::GpuComputePassTimestampWrites::from(
                timestamp_writes,
            ));
        }

        pass_js
    }
}

impl From<ComputeTimestampWrites<'_>> for web_sys::GpuComputePassTimestampWrites {
    fn from(timestamp_writes: ComputeTimestampWrites) -> web_sys::GpuComputePassTimestampWrites {
        let timestamp_writes_js =
            web_sys::GpuComputePassTimestampWrites::new(timestamp_writes.query_set);

        if let Some(beginning_index) = timestamp_writes.beginning_index {
            timestamp_writes_js.set_beginning_of_pass_write_index(beginning_index);
        }
        if let Some(end_index) = timestamp_writes.end_index {
            timestamp_writes_js.set_end_of_pass_write_index(end_index);
        }

        timestamp_writes_js
    }
}
