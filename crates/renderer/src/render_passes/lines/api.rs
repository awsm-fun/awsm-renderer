use glam::{Vec3, Vec4};

use crate::{error::Result, AwsmRenderer};

use super::gpu::{
    create_bind_group, create_segment_buffer, create_uniform_buffer, pack_into, segments_byte_size,
    write_segments,
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
        let segments_uploader = std::sync::Mutex::new(
            crate::buffer::mapped_uploader::MappedUploader::new("Line Segments"),
        );
        write_segments(
            &self.gpu,
            &mut segments_uploader.lock().unwrap(),
            &segment_buffer,
            segment_bytes,
            segments,
        )?;

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
            segments_uploader,
            uniform_uploader: std::sync::Mutex::new(
                crate::buffer::mapped_uploader::MappedUploader::new("Line Uniform"),
            ),
        });
        // Block B.3: cold-boot path leaves `lines.pipelines.variants`
        // unpopulated; on the first `add_line_*` we flip the request
        // flag so the renderer's `wait_for_pipelines_ready` (or an
        // explicit `ensure_line_pipelines_compiled`) drives the
        // compile. Dispatch warn-skips until then.
        if self.lines.pipelines.variants.is_none() {
            self.lines.pipelines_compile_requested = true;
        }
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
        write_segments(
            &self.gpu,
            &mut entry.segments_uploader.lock().unwrap(),
            &entry.segment_buffer,
            entry.segment_capacity_bytes,
            &self.lines.pack_buf,
        )?;
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

    /// Block B.3: lazily compiles the 4 line pipeline variants on the
    /// transition from "no line primitives" to "first line primitive
    /// inserted". Idempotent — subsequent calls are no-ops once
    /// `pipelines.variants` is populated.
    ///
    /// Cold-boot leaves `LineRenderer::pipelines.variants = None`; the
    /// first `add_line_strip` / `add_line_segments` sets
    /// `pipelines_compile_requested = true`. This method (driven by
    /// `wait_for_pipelines_ready` and by the MSAA-toggle path in
    /// `set_anti_aliasing`) checks both flags and compiles when
    /// either is set. Until compile completes, the line dispatch
    /// warn-skips via `pipeline_scheduler::warn_pipeline_not_compiled`.
    pub async fn ensure_line_pipelines_compiled(&mut self) -> Result<()> {
        if self.lines.pipelines.variants.is_some() && !self.lines.pipelines_compile_requested {
            return Ok(());
        }
        // Nothing to do if no entries AND no explicit request — keep
        // cold-boot lazy until a real consumer shows up.
        if self.lines.entries.is_empty() && !self.lines.pipelines_compile_requested {
            return Ok(());
        }
        self.lines
            .ensure_pipelines_compiled(
                &self.gpu,
                &mut self.bind_group_layouts,
                &mut self.pipeline_layouts,
                &mut self.pipelines,
                &mut self.shaders,
                &self.render_textures.formats,
            )
            .await
    }
}
