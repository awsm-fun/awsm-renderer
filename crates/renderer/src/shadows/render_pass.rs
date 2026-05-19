//! Shadow generation render pass — depth-only rendering of every
//! shadow-casting renderable into the 2D atlas (directional cascades,
//! spot tiles) and the cube pool (point lights).
//!
//! Cascade fit, atlas packing, and per-frame view-matrix uploads
//! happen CPU-side in `Shadows::write_gpu`; this module only walks the
//! resulting view list and records one depth-only pass per view.
//! Per-view frustum culling against the light's view-projection
//! happens here too so directional casters past a cascade or behind a
//! cube face are skipped instead of vertex-shaded.

use awsm_renderer_core::command::{
    render_pass::{DepthStencilAttachment, RenderPassDescriptor},
    LoadOp, StoreOp,
};
use awsm_renderer_core::pipeline::primitive::IndexFormat;

use crate::error::Result;
use crate::frustum::Frustum;
use crate::render::RenderContext;
use crate::shadows::Shadows;

/// Records every shadow-generation render pass for the current frame.
///
/// Called between the geometry pass and light culling. Skipped
/// entirely when [`Shadows::any_active`] returns `false`.
pub fn record(ctx: &RenderContext, shadows: &Shadows) -> Result<()> {
    // The 2D atlas is shared by every cascade / spot view. A
    // render-pass clear is attachment-wide (not viewport-scoped), so
    // if every per-cascade pass used `LoadOp::Clear`, each pass would
    // wipe the previous cascade's tile and only the last-written
    // cascade would survive. Clear it exactly once — on the first
    // 2D-atlas pass we record this frame — and `Load` on every
    // subsequent 2D pass to preserve already-written tiles. Cube-face
    // passes target their own per-face views and clear independently.
    let mut atlas_cleared = false;

    for (_light_key, record) in shadows.records() {
        for view in &record.views {
            if !view.should_render {
                continue;
            }
            // Per-view matrix is read from `shadow_view_buffer` at a
            // dynamic offset (`view.shadow_view_slot * SHADOW_VIEW_STRIDE`)
            // — Shadows::write_gpu uploaded every slot once, up front.

            let is_cube = view.cube_layer.is_some();
            let depth_view = match view.cube_layer {
                Some(layer) => shadows
                    .cube_face_views
                    .get(layer as usize)
                    .unwrap_or(&shadows.atlas_view),
                None => &shadows.atlas_view,
            };
            let load_op = if is_cube || !atlas_cleared {
                LoadOp::Clear
            } else {
                LoadOp::Load
            };
            let depth_attachment = DepthStencilAttachment::new(depth_view)
                .with_depth_load_op(load_op)
                .with_depth_store_op(StoreOp::Store)
                .with_depth_clear_value(1.0);
            if !is_cube {
                atlas_cleared = true;
            }

            let render_pass = ctx.command_encoder.begin_render_pass(
                &RenderPassDescriptor {
                    label: Some("Shadow Generation Pass"),
                    color_attachments: vec![],
                    depth_stencil_attachment: Some(depth_attachment),
                    ..Default::default()
                }
                .into(),
            )?;

            // For 2D atlas views the viewport scopes the draw to the
            // sub-rect within the shared atlas texture. For cube faces
            // the attachment is already a per-face 2D view at the
            // cube's native resolution, so `view.atlas_rect` is
            // `[0, 0, cube_resolution, cube_resolution]` and the
            // viewport call is a no-op — same call site either way.
            let [x, y, w, h] = view.atlas_rect;
            render_pass.set_viewport(x as f32, y as f32, w as f32, h as f32, 0.0, 1.0);

            let view_offset =
                crate::shadows::Shadows::shadow_view_dynamic_offset(view.shadow_view_slot);
            render_pass.set_bind_group(
                0,
                shadows.shadow_view_bind_group(),
                Some(&[view_offset]),
            )?;
            render_pass.set_bind_group(
                1,
                ctx.render_passes
                    .geometry
                    .bind_groups
                    .transforms
                    .get_bind_group()?,
                None,
            )?;
            render_pass.set_bind_group(
                3,
                ctx.render_passes
                    .geometry
                    .bind_groups
                    .animation
                    .get_bind_group()?,
                None,
            )?;

            let meta_bind_group = ctx
                .render_passes
                .geometry
                .bind_groups
                .meta
                .get_bind_group()?;

            // Per-view frustum culling. Directional cascades especially
            // see geometry the camera doesn't, so we test against the
            // light-space frustum rather than the camera's. The
            // frustum is rebuilt per view (cheap; 6 plane extractions
            // from `view_projection`).
            let shadow_frustum = Frustum::from_view_projection(view.view_projection);

            // Cache the last-bound pipeline key so we don't re-bind
            // when consecutive draws share the same variant.
            let mut last_pipeline_key = None;
            for (mesh_key, mesh) in ctx.meshes.iter() {
                if !mesh.cast_shadows || mesh.hidden || mesh.hud {
                    continue;
                }
                // HUD overlay primitives also shouldn't cast shadows.

                // Frustum cull against the light-space view. Meshes
                // without a cached world AABB are conservative kept
                // (they're typically procedural / dynamic content
                // whose bounds haven't been computed yet).
                if let Some(aabb) = &mesh.world_aabb {
                    if !shadow_frustum.intersects_aabb(aabb) {
                        continue;
                    }
                }

                let pipeline_key = shadows.shadow_pipeline_key(mesh.instanced, is_cube);
                if last_pipeline_key != Some(pipeline_key) {
                    render_pass.set_pipeline(ctx.pipelines.render.get(pipeline_key)?);
                    last_pipeline_key = Some(pipeline_key);
                }

                let geometry_meta_offset = ctx.meshes.meta.geometry_buffer_offset(mesh_key)? as u32;
                render_pass.set_bind_group(2, meta_bind_group, Some(&[geometry_meta_offset]))?;

                render_pass.set_vertex_buffer(
                    0,
                    ctx.meshes.visibility_geometry_data_gpu_buffer(),
                    Some(
                        ctx.meshes
                            .visibility_geometry_data_buffer_offset(mesh_key)?
                            as u64,
                    ),
                    None,
                );

                if mesh.instanced {
                    let offset = ctx.instances.transform_buffer_offset(mesh.transform_key)?;
                    render_pass.set_vertex_buffer(
                        1,
                        ctx.instances.gpu_transform_buffer(),
                        Some(offset as u64),
                        None,
                    );
                }

                let buffer_info = ctx.meshes.buffer_info(mesh_key)?;
                render_pass.set_index_buffer(
                    ctx.meshes.visibility_geometry_index_gpu_buffer(),
                    IndexFormat::Uint32,
                    Some(
                        ctx.meshes
                            .visibility_geometry_index_buffer_offset(mesh_key)?
                            as u64,
                    ),
                    None,
                );

                let index_count = buffer_info.triangles.vertex_attribute_indices.count as u32;

                if mesh.instanced {
                    if let Some(instance_count) =
                        ctx.instances.transform_instance_count(mesh.transform_key)
                    {
                        render_pass
                            .draw_indexed_with_instance_count(index_count, instance_count as u32);
                    }
                } else {
                    render_pass.draw_indexed(index_count);
                }
            }

            render_pass.end();
        }
    }

    // EVSM compute passes: for every cascade that landed in
    // `evsm_dispatch_queue` this frame, run moment-write → blur H →
    // blur V. The render passes above must complete first because
    // moment-write reads from `shadow_atlas`; WebGPU enforces the
    // barrier at the pass-boundary level so no explicit sync is
    // needed.
    if !shadows.evsm_dispatch_queue.is_empty() {
        dispatch_evsm(ctx, shadows)?;
    }

    Ok(())
}

fn dispatch_evsm(ctx: &RenderContext, shadows: &Shadows) -> Result<()> {
    use awsm_renderer_core::command::compute_pass::{ComputePassDescriptor, ComputePassEncoder};
    let moment_pipeline = ctx
        .pipelines
        .compute
        .get(shadows.evsm_pass.moment_write_pipeline_key)?;
    let blur_h_pipeline = ctx
        .pipelines
        .compute
        .get(shadows.evsm_pass.blur_h_pipeline_key)?;
    let blur_v_pipeline = ctx
        .pipelines
        .compute
        .get(shadows.evsm_pass.blur_v_pipeline_key)?;

    for entry in &shadows.evsm_dispatch_queue {
        let dst_w = entry.evsm_rect[2];
        let dst_h = entry.evsm_rect[3];
        if dst_w == 0 || dst_h == 0 {
            continue;
        }
        let offset = crate::shadows::EvsmPass::params_dynamic_offset(entry.params_slot);

        // ── Moment write ────────────────────────────────────────────
        let pass: ComputePassEncoder = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Shadow EVSM Moment Write")).into(),
        ));
        pass.set_pipeline(moment_pipeline);
        pass.set_bind_group(0, &shadows.evsm_moment_write_bind_group, Some(&[offset]))?;
        pass.dispatch_workgroups(dst_w.div_ceil(8), Some(dst_h.div_ceil(8)), None);
        pass.end();

        // ── Blur H (evsm → ping-pong) ──────────────────────────────
        let pass: ComputePassEncoder = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Shadow EVSM Blur H")).into(),
        ));
        pass.set_pipeline(blur_h_pipeline);
        pass.set_bind_group(0, &shadows.evsm_blur_h_bind_group, Some(&[offset]))?;
        pass.dispatch_workgroups(dst_w.div_ceil(64), Some(dst_h), None);
        pass.end();

        // ── Blur V (ping-pong → evsm) ──────────────────────────────
        let pass: ComputePassEncoder = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Shadow EVSM Blur V")).into(),
        ));
        pass.set_pipeline(blur_v_pipeline);
        pass.set_bind_group(0, &shadows.evsm_blur_v_bind_group, Some(&[offset]))?;
        pass.dispatch_workgroups(dst_w, Some(dst_h.div_ceil(64)), None);
        pass.end();
    }

    Ok(())
}
