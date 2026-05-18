//! Shadow generation render pass — depth-only rendering of every
//! shadow-casting renderable into the atlas / cube pool.
//!
//! Phase 2 supports a single directional caster with a single cascade
//! covering the full atlas; phases 4 / 7 / 8 generalise to multi-
//! cascade, spot, and cube shadows. The actual cascade fit happens in
//! `Shadows::write_gpu` (CPU side); this module only orchestrates the
//! per-view depth-only render passes.

use awsm_renderer_core::command::{
    render_pass::{DepthStencilAttachment, RenderPassDescriptor},
    LoadOp, StoreOp,
};
use awsm_renderer_core::pipeline::primitive::IndexFormat;

use crate::error::Result;
use crate::render::RenderContext;
use crate::shadows::Shadows;

/// Records every shadow-generation render pass for the current frame.
///
/// Called between the geometry pass and light culling. Skipped
/// entirely when [`Shadows::any_active`] returns `false`.
pub fn record(ctx: &RenderContext, shadows: &Shadows) -> Result<()> {
    for (_light_key, record) in shadows.records() {
        for view in &record.views {
            if !view.should_render {
                continue;
            }
            // Per-view shadow_view uniform write. `queue.writeBuffer`
            // is cheap enough to call per pass on the small (80B)
            // uniform; phase 4 may switch to a dynamic-offset binding
            // into the descriptor buffer if cascade counts grow.
            shadows.write_shadow_view(
                ctx.gpu,
                &view.view_projection,
                shadows
                    .light_params(_light_key)
                    .map(|p| p.depth_bias)
                    .unwrap_or(0.0),
                shadows
                    .light_params(_light_key)
                    .map(|p| p.normal_bias)
                    .unwrap_or(0.0),
            )?;

            let depth_view = match view.cube_layer {
                Some(layer) => shadows
                    .cube_face_views
                    .get(layer as usize)
                    .unwrap_or(&shadows.atlas_view),
                None => &shadows.atlas_view,
            };
            let depth_attachment = DepthStencilAttachment::new(depth_view)
                .with_depth_load_op(LoadOp::Clear)
                .with_depth_store_op(StoreOp::Store)
                .with_depth_clear_value(1.0);

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
            // sub-rect. For cube faces the attachment is already its
            // own per-face view at the cube's native resolution, so
            // the rect (POINT_RES × POINT_RES at origin) doubles up
            // — same call site either way.
            let [x, y, w, h] = view.atlas_rect;
            render_pass.set_viewport(x as f32, y as f32, w as f32, h as f32, 0.0, 1.0);

            render_pass.set_bind_group(0, shadows.shadow_view_bind_group(), None)?;
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

            // Cache the last-bound pipeline key so we don't re-bind
            // when consecutive draws share the same variant.
            let mut last_pipeline_key = None;
            for (mesh_key, mesh) in ctx.meshes.iter() {
                if !mesh.cast_shadows || mesh.hidden || mesh.hud {
                    continue;
                }
                // HUD overlay primitives also shouldn't cast shadows.

                let pipeline_key = shadows.shadow_pipeline_key(mesh.instanced);
                if last_pipeline_key != Some(pipeline_key) {
                    render_pass.set_pipeline(ctx.pipelines.render.get(pipeline_key)?);
                    last_pipeline_key = Some(pipeline_key);
                }

                let geometry_meta_offset = ctx.meshes.meta.geometry_buffer_offset(mesh_key)? as u32;
                render_pass.set_bind_group(2, meta_bind_group, Some(&[geometry_meta_offset]))?;

                render_pass.set_vertex_buffer(
                    0,
                    ctx.meshes.visibility_geometry_data_gpu_buffer(),
                    Some(ctx.meshes.visibility_geometry_data_buffer_offset(mesh_key)? as u64),
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
                    Some(ctx.meshes.visibility_geometry_index_buffer_offset(mesh_key)? as u64),
                    None,
                );

                let index_count = buffer_info.triangles.vertex_attribute_indices.count as u32;

                if mesh.instanced {
                    if let Some(instance_count) =
                        ctx.instances.transform_instance_count(mesh.transform_key)
                    {
                        render_pass.draw_indexed_with_instance_count(
                            index_count,
                            instance_count as u32,
                        );
                    }
                } else {
                    render_pass.draw_indexed(index_count);
                }
            }

            render_pass.end();
        }
    }

    Ok(())
}
