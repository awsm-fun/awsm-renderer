use glam::{Vec3, Vec4};

use crate::{error::Result, AwsmRenderer};

use super::gpu::{
    create_bind_group, create_segment_buffer, create_uniform_buffer, pack_into,
    segments_byte_size, write_segments,
};
use super::types::{LineEntry, LineKey, LineTopology};

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
        pack_into(&mut self.lines.pack_buf, positions, colors, topology);
        let segments = &self.lines.pack_buf;
        if segments.is_empty() {
            return Ok(None);
        }
        let segment_count = segments.len();
        let segment_bytes = segments_byte_size(segment_count);

        let segment_buffer = create_segment_buffer(&self.gpu, segment_bytes)?;
        write_segments(&self.gpu, &segment_buffer, segments)?;

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
            segment_count: segment_count as u32,
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
        pack_into(&mut self.lines.pack_buf, positions, colors, topology);
        let segment_count = self.lines.pack_buf.len();
        let entry = self.lines.entries.get_mut(key).expect("checked above");
        if segment_count == 0 {
            entry.segment_count = 0;
            return Ok(());
        }
        let new_bytes = segments_byte_size(segment_count);
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
        write_segments(&self.gpu, &entry.segment_buffer, &self.lines.pack_buf)?;
        entry.segment_count = segment_count as u32;
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
