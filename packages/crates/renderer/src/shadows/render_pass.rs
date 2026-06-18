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
use crate::pipeline_scheduler::warn_pipeline_not_compiled;
use crate::render::RenderContext;
use crate::scene_spatial::NodeFilter;
use crate::shadows::Shadows;

/// Records every shadow-generation render pass for the current frame.
///
/// Called between the geometry pass and light culling. Skipped
/// entirely when [`Shadows::any_active`] returns `false`. Also skipped
/// (with a one-shot `warn_pipeline_not_compiled` log) when the shadow
/// caster pipelines haven't yet been compiled — Block B.1 + B.2 defers
/// pipeline compile until the first shadow-casting light triggers
/// [`Shadows::ensure_pipelines_compiled`]; if a render frame fires
/// between "light added" and "pipelines resolved" the warn-skip keeps
/// the frame alive instead of erroring.
pub fn record(ctx: &RenderContext, shadows: &Shadows) -> Result<()> {
    // Pipelines deferred until first shadow-caster (Block B.1 + B.2).
    if !shadows.pipelines_compiled() {
        warn_pipeline_not_compiled("shadow_gen", "caster");
        return Ok(());
    }
    // The 2D `shadow_atlas` is still shared across spot-light views,
    // and `LoadOp::Clear` there is attachment-wide. So we clear it on
    // the first spot-atlas pass of the frame and `Load` on subsequent
    // spot passes to preserve already-written tiles. Cube faces and
    // cascade layers each target their own per-attachment view and
    // can always clear independently.
    let mut atlas_cleared = false;

    // Masked (alpha-tested) caster group-0 — present once a masked material's
    // variant has been compiled (texture-finalize flow). `None` → no masked
    // casters yet, so every caster takes the plain solid-shadow path.
    let masked_group0 = ctx
        .render_passes
        .shadow_masked
        .bind_group
        .get_bind_group()
        .ok();

    for (_light_key, record) in shadows.records() {
        for view in &record.views {
            if !view.should_render {
                continue;
            }
            // Per-view matrix is read from `shadow_view_buffer` at a
            // dynamic offset (`view.shadow_view_slot * SHADOW_VIEW_STRIDE`)
            // — Shadows::write_gpu uploaded every slot once, up front.

            let is_cube = view.cube_layer.is_some();
            let is_cascade = view.cascade_layer.is_some();
            let depth_view = if let Some(layer) = view.cube_layer {
                shadows
                    .cube_face_views
                    .get(layer as usize)
                    .unwrap_or(&shadows.atlas_view)
            } else if let Some(layer) = view.cascade_layer {
                shadows
                    .cascade_layer_views
                    .get(layer as usize)
                    .unwrap_or(&shadows.atlas_view)
            } else {
                &shadows.atlas_view
            };
            // Per-attachment views (cube faces, cascade layers) always
            // clear — they own their own depth surface. The 2D atlas
            // clears once per frame, then loads.
            let load_op = if is_cube || is_cascade || !atlas_cleared {
                LoadOp::Clear
            } else {
                LoadOp::Load
            };
            let depth_attachment = DepthStencilAttachment::new(depth_view)
                .with_depth_load_op(load_op)
                .with_depth_store_op(StoreOp::Store)
                .with_depth_clear_value(1.0);
            if !is_cube && !is_cascade {
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

            // Non-instanced shadow draws use the storage-array meta
            // binding (no dynamic offset; shader reads
            // `geometry_mesh_metas[instance_index]`); instanced
            // shadow draws keep the legacy uniform-with-dynamic-
            // offset binding.
            let meta_storage_bind_group = ctx
                .render_passes
                .geometry
                .bind_groups
                .meta
                .get_storage_bind_group()?;
            let meta_uniform_bind_group = ctx
                .render_passes
                .geometry
                .bind_groups
                .meta
                .get_uniform_bind_group()?;

            // Per-view frustum culling. Directional cascades especially
            // see geometry the camera doesn't, so we test against the
            // light-space frustum rather than the camera's. The
            // frustum is rebuilt per view (cheap; 6 plane extractions
            // from `view_projection`).
            let shadow_frustum = Frustum::from_view_projection(view.view_projection);

            // Surviving caster set: BVH-pruned + shadow-caster filter
            // (cast_shadows && !hidden && !hud). Meshes without a
            // world AABB aren't in the index — fall back to a tail
            // walk so procedural / mid-load content draws conservatively.
            let bvh_visible: Vec<_> = ctx
                .scene_spatial
                .query_frustum(&shadow_frustum, NodeFilter::shadow_caster())
                .map(|node| node.mesh_key)
                .collect();
            let conservative_extra: Vec<_> = ctx
                .meshes
                .iter()
                .filter(|(_, m)| m.cast_shadows && !m.hidden && !m.hud && m.world_aabb.is_none())
                .map(|(k, _)| k)
                .collect();

            // Cache the last-bound pipeline key so we don't re-bind
            // when consecutive draws share the same variant.
            let mut last_pipeline_key = None;
            // Track whether group 0 currently holds the masked (augmented)
            // bind group vs the plain shadow_view group, so we only rebind on
            // a solid↔masked transition. The plain group was bound just above.
            let mut last_group0_masked = Some(false);
            for mesh_key in bvh_visible.into_iter().chain(conservative_extra) {
                let Ok(mesh) = ctx.meshes.get(mesh_key) else {
                    continue;
                };

                // Shadow generation draws from the VISIBILITY geometry buffer
                // (set_vertex_buffer below). A transparency-pass mesh
                // (add_raw_mesh_transparent) has no visibility geometry, so if a
                // consumer enables `cast` on one it would hit
                // VisibilityGeometryBufferNotFound and — render() being atomic —
                // black out the whole frame. Skip such meshes (they can't cast a
                // visibility-buffer shadow); the routing in `collect_renderables`
                // applies the same ground-truth-geometry rule for the main passes.
                if ctx
                    .meshes
                    .buffer_info(mesh_key)
                    .map(|info| info.visibility_geometry_vertex.is_none())
                    .unwrap_or(true)
                {
                    continue;
                }

                // Masked (alpha-tested) caster → hole-shaped shadow when a
                // masked variant is compiled for this material. Gate on
                // `alpha_cutoff` present REGARDLESS of opaque/transparent
                // routing — a Mask+refractive material is transparent-routed but
                // must still cast a cutout shadow. Falls back to the solid
                // pipeline (rectangular shadow) when no masked variant exists.
                let masked_key = masked_group0.and_then(|_| {
                    if ctx.materials.alpha_cutoff(mesh.material_key).is_some() {
                        let shader_id = ctx.materials.canonical_shader_id(mesh.material_key);
                        ctx.render_passes.shadow_masked.pipelines.get(
                            shader_id,
                            mesh.instanced,
                            is_cube,
                            mesh.double_sided,
                        )
                    } else {
                        None
                    }
                });

                // Pipelines-compiled guard at the top of `record`
                // ensures the solid Option is Some here. Defensive
                // `else` skips the draw if the invariant is broken.
                let (pipeline_key, use_masked) = match masked_key {
                    Some(key) => (key, true),
                    None => match shadows.shadow_pipeline_key(
                        mesh.instanced,
                        is_cube,
                        mesh.double_sided,
                    ) {
                        Some(key) => (key, false),
                        None => continue,
                    },
                };

                // Swap group 0 between the plain shadow_view group and the
                // augmented masked group only on a transition; the per-view
                // dynamic offset is the same for both.
                if last_group0_masked != Some(use_masked) {
                    let group0 = if use_masked {
                        masked_group0.expect("masked_group0 present when use_masked")
                    } else {
                        shadows.shadow_view_bind_group()
                    };
                    render_pass.set_bind_group(0, group0, Some(&[view_offset]))?;
                    last_group0_masked = Some(use_masked);
                }
                if last_pipeline_key != Some(pipeline_key) {
                    render_pass.set_pipeline(ctx.pipelines.render.get(pipeline_key)?);
                    last_pipeline_key = Some(pipeline_key);
                }

                let geometry_meta_offset = ctx.meshes.meta.geometry_buffer_offset(mesh_key)? as u32;
                if mesh.instanced {
                    render_pass.set_bind_group(
                        2,
                        meta_uniform_bind_group,
                        Some(&[geometry_meta_offset]),
                    )?;
                } else {
                    render_pass.set_bind_group(2, meta_storage_bind_group, None)?;
                }

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
                    // `first_instance = mesh_meta_idx` so the
                    // storage-array shader lookup
                    // `geometry_mesh_metas[instance_index]` resolves
                    // to this mesh's meta slot. Shadow draws skip the
                    // GPU compaction path — shadow-caster visibility
                    // is BVH-pruned CPU-side per-view, so always
                    // emit one instance directly.
                    let mesh_meta_idx = geometry_meta_offset
                        / crate::meshes::meta::geometry_meta::GEOMETRY_MESH_META_BYTE_ALIGNMENT
                            as u32;
                    render_pass
                        .draw_indexed_with_instance_count_and_first_index_and_base_vertex_and_first_instance(
                            index_count,
                            1,
                            0,
                            0,
                            mesh_meta_idx,
                        );
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
    // EVSM pipelines deferred until the first shadow-caster (Block B.1).
    // The pipelines-compiled guard in `record` covers the common case,
    // but warn-skip here as well so an unexpected entry into this
    // function produces a controlled outcome.
    let (Some(moment_pipeline_key), Some(blur_h_pipeline_key), Some(blur_v_pipeline_key)) = (
        shadows.evsm_pass.moment_write_pipeline_key,
        shadows.evsm_pass.blur_h_pipeline_key,
        shadows.evsm_pass.blur_v_pipeline_key,
    ) else {
        warn_pipeline_not_compiled("evsm", "moment_write+blur");
        return Ok(());
    };
    let moment_pipeline = ctx.pipelines.compute.get(moment_pipeline_key)?;
    let blur_h_pipeline = ctx.pipelines.compute.get(blur_h_pipeline_key)?;
    let blur_v_pipeline = ctx.pipelines.compute.get(blur_v_pipeline_key)?;

    for entry in &shadows.evsm_dispatch_queue {
        // Skip EVSM dispatch for throttled cascades — the source
        // cascade layer wasn't rendered this frame, so its prior
        // moments in `evsm_atlas` are still valid. With the far
        // cascade on a 4-frame update period, 3 of 4 frames hit this
        // path and skip the moment-write + 2 blur passes.
        if !entry.should_render {
            continue;
        }
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
