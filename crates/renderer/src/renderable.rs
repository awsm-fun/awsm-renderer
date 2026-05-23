//! Renderable collection and draw helpers.

use awsm_renderer_core::{command::render_pass::RenderPassEncoder, pipeline::primitive::CullMode};
use glam::Mat4;

use crate::{
    bounds::Aabb,
    error::AwsmError,
    frustum::Frustum,
    materials::MaterialKey,
    meshes::MeshKey,
    pipelines::{compute_pipeline::ComputePipelineKey, render_pipeline::RenderPipelineKey},
    render::RenderContext,
    render_passes::geometry::bind_group::GeometryBindGroups,
    scene_spatial::NodeFilter,
    AwsmRenderer,
};

/// Reusable scratch space for the per-frame renderable collection.
///
/// Held on [`AwsmRenderer`] (not on each frame's [`Renderables`]) so
/// the Vecs survive across frames and `clear_in_place` reuses the
/// existing allocation. For 10K-mesh scenes that's ~24 KB of avoided
/// allocator churn per frame. Frame-to-frame the Vec contents are
/// invalid (capacity is preserved, length is reset to 0 by `prepare`).
#[derive(Default)]
pub struct RenderablePool {
    opaque: Vec<Renderable>,
    transparent: Vec<Renderable>,
    hud: Vec<Renderable>,
}

impl RenderablePool {
    /// Drops the prior frame's content while keeping the underlying
    /// allocations. Called at the top of each frame's
    /// `collect_renderables`.
    fn clear(&mut self) {
        self.opaque.clear();
        self.transparent.clear();
        self.hud.clear();
    }
}

/// Per-frame borrowed view over the [`RenderablePool`]'s populated
/// slices. Constructed by [`AwsmRenderer::collect_renderables`] and
/// passed to the per-pass `render` functions.
#[derive(Copy, Clone)]
pub struct Renderables<'r> {
    pub opaque: &'r [Renderable],
    pub transparent: &'r [Renderable],
    pub hud: &'r [Renderable],
}

impl Renderables<'_> {
    /// Returns true if there are no renderables.
    pub fn is_empty(&self) -> bool {
        self.opaque.is_empty() && self.transparent.is_empty() && self.hud.is_empty()
    }

    /// Returns the total number of renderables.
    pub fn len(&self) -> usize {
        self.opaque.len() + self.transparent.len() + self.hud.len()
    }
}

impl AwsmRenderer {
    /// Returns a borrowed view over the [`RenderablePool`]. Cheap;
    /// callers can re-call this on every pass without repopulating.
    pub fn renderables(&self) -> Renderables<'_> {
        Renderables {
            opaque: &self.renderable_pool.opaque,
            transparent: &self.renderable_pool.transparent,
            hud: &self.renderable_pool.hud,
        }
    }

    /// Populates the per-frame [`RenderablePool`] from the renderer's
    /// current state. Clears the pool in-place and refills it.
    /// Callers read the populated slices via [`Self::renderables`].
    ///
    /// Borrows `&mut self` only for the duration of population — once
    /// the function returns, `self` is no longer mutably borrowed,
    /// so the caller can construct a [`RenderContext`] over `&self`.
    pub fn collect_renderables(&mut self) -> Result<()> {
        let _maybe_span_guard = if self.logging.render_timings {
            Some(tracing::span!(tracing::Level::INFO, "Collect renderables").entered())
        } else {
            None
        };

        // Take the pool out by std::mem::take so we can populate
        // without holding a `&mut self` borrow during the read-only
        // queries below. The pool gets put back at the end.
        let mut pool = std::mem::take(&mut self.renderable_pool);
        pool.clear();

        let frustum = self
            .camera
            .last_matrices
            .as_ref()
            .map(|matrices| Frustum::from_view_projection(matrices.view_projection()));

        // Pre-size the visible scratch to the upper-bound mesh count.
        // BVH culling usually returns far fewer, but this avoids any
        // realloc-during-push on the conservative tail-walk path.
        let mesh_upper_bound = self.meshes.len();
        let mut visible: Vec<(MeshKey, &crate::meshes::mesh::Mesh)> =
            Vec::with_capacity(mesh_upper_bound);

        // Build the visible mesh-key set from the BVH instead of walking
        // every mesh. The previous linear scan tested every mesh's cached
        // `world_aabb` against the frustum on every frame; the BVH path
        // descends hierarchically and surfaces only the surviving leaves.
        // Meshes without a world AABB (procedural / mid-load) aren't in
        // the index — fall back to a tail-walk of those so they still
        // draw conservatively.
        match &frustum {
            Some(f) => {
                visible.extend(
                    self.scene_spatial
                        .query_frustum(f, NodeFilter::camera_default())
                        .filter_map(|node| {
                            self.meshes
                                .get(node.mesh_key)
                                .ok()
                                .map(|m| (node.mesh_key, m))
                        }),
                );
                // Conservative fallback: any mesh without a world AABB
                // can't be tested by the BVH; keep it in the visible set.
                visible.extend(
                    self.meshes
                        .iter()
                        .filter(|(_, m)| !m.hidden && m.world_aabb.is_none()),
                );
            }
            None => {
                visible.extend(self.meshes.iter().filter(|(_, m)| !m.hidden));
            }
        }

        for (mesh_key, mesh) in visible {
            // Route by the authored `material_key`: `MaterialMeshMeta`
            // is still packed from `mesh.material_key` (see
            // `meshes::meta`), so routing by `effective_material_key`
            // would pick a pipeline that doesn't match the data the
            // shader reads. `effective_material_key` stays available
            // on `Mesh` for a future cheap-material LOD wiring once
            // the cheap material's offset is also plumbed into meta.
            let routing_material = mesh.material_key;

            // The opaque compute pipeline is specialized per
            // `MaterialShaderId` (PBR / Unlit / Toon). Look up the
            // routing material's shader_id so the pipeline matches
            // the data the shader will read.
            let shader_id = self.materials.shader_id(routing_material);

            let cull_mode = if mesh.double_sided {
                CullMode::None
            } else {
                CullMode::Back
            };

            let renderable = Renderable {
                key: mesh_key,
                world_aabb: mesh.world_aabb.clone(),
                instanced: mesh.instanced,
                double_sided: mesh.double_sided,
                cull_mode,
                hud: mesh.hud,
                material_key: routing_material,
                geometry_render_pipeline_key: self
                    .render_passes
                    .geometry
                    .pipelines
                    .get_render_pipeline_key(
                        crate::render_passes::geometry::pipeline::GeometryRenderPipelineKeyOpts {
                            anti_aliasing: &self.anti_aliasing,
                            instancing: mesh.instanced,
                            cull_mode,
                            // Mirrors the runtime branch in
                            // `meshes/mesh.rs::push_geometry_pass_commands`.
                            meta_storage_array: !mesh.instanced
                                && self.features.indirect_first_instance_enabled(),
                        },
                    )
                    .ok(),
                material_opaque_compute_pipeline_key: self
                    .render_passes
                    .material_opaque
                    .pipelines
                    .get_compute_pipeline_key(&self.anti_aliasing, shader_id),
                material_transparent_render_pipeline_key: self
                    .render_passes
                    .material_transparent
                    .pipelines
                    .get_render_pipeline_key(mesh_key),
            };

            if mesh.hud {
                pool.hud.push(renderable);
            } else if self.materials.is_transparency_pass(routing_material) {
                pool.transparent.push(renderable);
            } else {
                pool.opaque.push(renderable);
            }
        }

        if let Some(camera_matrices) = self.camera.last_matrices.as_ref() {
            let view_proj = camera_matrices.view_projection();
            pool.opaque
                .sort_by(|a, b| geometry_sort_renderable(a, b, &view_proj, false));
            pool.transparent
                .sort_by(|a, b| geometry_sort_renderable(a, b, &view_proj, true));
            pool.hud
                .sort_by(|a, b| geometry_sort_renderable(a, b, &view_proj, true));
        }

        // Put the populated pool back; the mutable borrow ends here.
        self.renderable_pool = pool;
        Ok(())
    }
}

fn geometry_sort_renderable(
    a: &Renderable,
    b: &Renderable,
    view_proj: &Mat4,
    transparent: bool,
) -> std::cmp::Ordering {
    // For the OPAQUE pass we group by render_pipeline_key first so the
    // GPU can avoid pipeline switches across consecutive draws — the
    // depth buffer handles overlap so any in-group order works. For
    // the TRANSPARENT pass that grouping is *unsafe*: alpha compositing
    // requires strict back-to-front draw order across *every* renderable,
    // not within a pipeline group, otherwise a particle in front of a
    // dome pane (or vice versa) will draw in the wrong order and one
    // will incorrectly occlude / show-through the other. Skip the
    // pipeline grouping in the transparent case and let depth alone
    // decide.
    if !transparent {
        match (
            a.geometry_render_pipeline_key,
            b.geometry_render_pipeline_key,
        ) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            (Some(_), None) => return std::cmp::Ordering::Less,
            (Some(key_a), Some(key_b)) => {
                let pipeline_ordering = key_a.cmp(&key_b);
                if pipeline_ordering != std::cmp::Ordering::Equal {
                    return pipeline_ordering;
                }
            }
        }
    }

    match (a.world_aabb.as_ref(), b.world_aabb.as_ref()) {
        (Some(a_world_aabb), Some(b_world_aabb)) => {
            let a_min_z = view_proj.transform_point3(a_world_aabb.min).z;
            let a_max_z = view_proj.transform_point3(a_world_aabb.max).z;

            let b_min_z = view_proj.transform_point3(b_world_aabb.min).z;
            let b_max_z = view_proj.transform_point3(b_world_aabb.max).z;

            let a_closest_depth = a_min_z.min(a_max_z);
            let b_closest_depth = b_min_z.min(b_max_z);

            if transparent {
                // Sort back-to-front for transparent objects.
                // (larger z is further away, and we want that to come first)
                b_closest_depth.total_cmp(&a_closest_depth)
            } else {
                // Sort front-to-back for opaque objects.
                // (smaller z is closer, and we want that to come first)
                a_closest_depth.total_cmp(&b_closest_depth)
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Single renderable entity. No lifetime — all fields are owned or
/// `Copy`. The [`RenderablePool`] on [`AwsmRenderer`] stores these
/// frame-to-frame and clears in place.
#[derive(Debug, Clone)]
pub struct Renderable {
    /// Stable mesh identity; pass-time lookups (`ctx.meshes.get(key)`)
    /// retrieve any field this struct doesn't already cache.
    pub key: MeshKey,
    /// Snapshot of the mesh's world AABB at the moment of
    /// collection. Used by depth sorting and as the cull-pass instance
    /// bounds. Cloned (24 B) so the renderable doesn't borrow from
    /// `meshes`.
    pub world_aabb: Option<Aabb>,
    pub instanced: bool,
    pub double_sided: bool,
    pub cull_mode: CullMode,
    pub hud: bool,
    pub material_key: MaterialKey,
    /// Precomputed at collection time so the per-frame sort
    /// comparator stays free of `RenderContext` access (which lets
    /// `collect_renderables` populate the pool before `ctx` is built).
    pub geometry_render_pipeline_key: Option<RenderPipelineKey>,
    pub material_opaque_compute_pipeline_key: Option<ComputePipelineKey>,
    pub material_transparent_render_pipeline_key: Option<RenderPipelineKey>,
}

impl Renderable {
    /// Returns the geometry render pipeline key.
    pub fn geometry_render_pipeline_key(&self) -> Option<RenderPipelineKey> {
        self.geometry_render_pipeline_key
    }

    /// Returns the opaque compute pipeline key, if any.
    pub fn material_opaque_compute_pipeline_key(&self) -> Option<ComputePipelineKey> {
        self.material_opaque_compute_pipeline_key
    }

    /// Returns the transparent render pipeline key, if any.
    pub fn material_transparent_render_pipeline_key(&self) -> Option<RenderPipelineKey> {
        self.material_transparent_render_pipeline_key
    }

    /// Returns the material key for this renderable.
    pub fn material_key(&self) -> MaterialKey {
        self.material_key
    }

    /// Returns the world-space AABB snapshot, if present.
    pub fn world_aabb(&self) -> Option<&Aabb> {
        self.world_aabb.as_ref()
    }

    /// Pushes geometry pass commands for this renderable.
    pub fn push_geometry_pass_commands(
        &self,
        ctx: &RenderContext,
        render_pass: &RenderPassEncoder,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<()> {
        let mesh = ctx.meshes.get(self.key)?;
        mesh.push_geometry_pass_commands(ctx, self.key, render_pass, geometry_bind_groups)
    }

    /// Pushes transparent material pass commands for this renderable.
    pub fn push_material_transparent_pass_commands(
        &self,
        ctx: &RenderContext,
        render_pass: &RenderPassEncoder,
        mesh_material_bind_group: &web_sys::GpuBindGroup,
    ) -> Result<()> {
        let mesh = ctx.meshes.get(self.key)?;
        mesh.push_material_transparent_pass_commands(ctx, self.key, render_pass, mesh_material_bind_group)
    }
}

type Result<T> = std::result::Result<T, AwsmError>;
