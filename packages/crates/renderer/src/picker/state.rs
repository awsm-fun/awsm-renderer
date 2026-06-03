use crate::error::Result;
use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    command::compute_pass::ComputePassDescriptor,
    renderer::AwsmRendererWebGpu,
};

const INPUT_BYTE_SIZE: usize = 8; // 2xi32 for x,y pixel coords
pub(super) const OUTPUT_BYTE_SIZE: usize = 12; // hi/low u32 for mesh_id + u32 for validity

pub(super) struct PickerState {
    pub in_flight: bool,
    pub gpu_input_buffer: web_sys::GpuBuffer,
    pub gpu_output_buffer: web_sys::GpuBuffer,
    pub gpu_readback_buffer: web_sys::GpuBuffer,
    pub gpu_input_raw: [u8; INPUT_BYTE_SIZE],
}

impl PickerState {
    /// Creates GPU buffers used for picking.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_input_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Picker Input"),
                INPUT_BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        let gpu_output_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Picker Output"),
                OUTPUT_BYTE_SIZE,
                BufferUsage::new().with_storage().with_copy_src(),
            )
            .into(),
        )?;

        let gpu_readback_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Picker Output"),
                OUTPUT_BYTE_SIZE,
                BufferUsage::new().with_copy_dst().with_map_read(),
            )
            .into(),
        )?;
        Ok(Self {
            in_flight: false,
            gpu_input_buffer,
            gpu_input_raw: [0; INPUT_BYTE_SIZE],
            gpu_output_buffer,
            gpu_readback_buffer,
        })
    }

    /// Begins a GPU pick pass for the given pixel coordinates.
    pub fn begin_pick(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        bind_group: &web_sys::GpuBindGroup,
        pipeline: &web_sys::GpuComputePipeline,
        x: i32,
        y: i32,
    ) -> Result<()> {
        self.in_flight = true;

        // Write input data
        self.gpu_input_raw[..4].copy_from_slice(&x.to_le_bytes());
        self.gpu_input_raw[4..8].copy_from_slice(&y.to_le_bytes());

        gpu.write_buffer(
            &self.gpu_input_buffer,
            None,
            self.gpu_input_raw.as_slice(),
            None,
            None,
        )?;

        // kick off compute pass

        let command_encoder = gpu.create_command_encoder(Some("Picker"));

        let compute_pass = command_encoder
            .begin_compute_pass(Some(&ComputePassDescriptor::new(Some("Picker")).into()));

        compute_pass.set_bind_group(0u32, bind_group, None)?;
        compute_pass.set_pipeline(pipeline);
        compute_pass.dispatch_workgroups(1, Some(1), Some(1));
        compute_pass.end();

        command_encoder.copy_buffer_to_buffer(
            &self.gpu_output_buffer,
            0,
            &self.gpu_readback_buffer,
            0,
            OUTPUT_BYTE_SIZE as u32,
        )?;

        gpu.submit_commands(&command_encoder.finish());

        Ok(())
    }
}
