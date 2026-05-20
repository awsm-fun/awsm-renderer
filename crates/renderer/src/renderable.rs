//! Renderable collection and draw helpers.

use awsm_renderer_core::command::render_pass::RenderPassEncoder;
use glam::Mat4;

use crate::{
    bounds::Aabb,
    error::AwsmError,
    frustum::Frustum,
    meshes::{mesh::Mesh, MeshKey},
    pipelines::{compute_pipeline::ComputePipelineKey, render_pipeline::RenderPipelineKey},
    render::RenderContext,
    render_passes::geometry::bind_group::GeometryBindGroups,
    scene_spatial::NodeFilter,
    AwsmRenderer,
};

/// Renderable lists grouped by pass type.
pub struct Renderables<'a> {
    pub opaque: Vec<Renderable<'a>>,
    pub transparent: Vec<Renderable<'a>>,
    pub hud: Vec<Renderable<'a>>,
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
    /// Collects renderables for the current frame.
    pub fn collect_renderables<'a>(&'a self, ctx: &RenderContext) -> Result<Renderables<'a>> {
        let _maybe_span_guard = if self.logging.render_timings {
            Some(tracing::span!(tracing::Level::INFO, "Collect renderables").entered())
        } else {
            None
        };

        let mut opaque = Vec::new();
        let mut transparent = Vec::new();
        let mut hud = Vec::new();

        let frustum = self
            .camera
            .last_matrices
            .as_ref()
            .map(|matrices| Frustum::from_view_projection(matrices.view_projection()));

        // Build the visible mesh-key set from the BVH instead of walking
        // every mesh. The previous linear scan tested every mesh's cached
        // `world_aabb` against the frustum on every frame; the BVH path
        // descends hierarchically and surfaces only the surviving leaves.
        // Meshes without a world AABB (procedural / mid-load) aren't in
        // the index — fall back to a tail-walk of those so they still
        // draw conservatively.
        let visible: Vec<(MeshKey, &Mesh)> = match &frustum {
            Some(f) => {
                let mut out: Vec<(MeshKey, &Mesh)> = self
                    .scene_spatial
                    .query_frustum(f, NodeFilter::camera_default())
                    .filter_map(|node| {
                        self.meshes
                            .get(node.mesh_key)
                            .ok()
                            .map(|m| (node.mesh_key, m))
                    })
                    .collect();
                // Conservative fallback: any mesh without a world AABB
                // can't be tested by the BVH; keep it in the visible set.
                out.extend(
                    self.meshes
                        .iter()
                        .filter(|(_, m)| !m.hidden && m.world_aabb.is_none()),
                );
                out
            }
            None => self
                .meshes
                .iter()
                .filter(|(_, m)| !m.hidden)
                .collect::<Vec<_>>(),
        };

        for (mesh_key, mesh) in visible {
            // Cluster 6.3: classify opaque vs transparent by the
            // *effective* material this frame. A mesh with a cheap
            // opaque variant + an expensive transmissive variant will
            // route through the cheap opaque pass when distant. The
            // deeper opaque-shading material swap (which would need
            // to re-pack `MaterialMeshMeta`) is a follow-up; this
            // single hook handles the renderable-list classification.
            let effective_material =
                mesh.effective_material_key(mesh_key, &self.coverage);

            // After the shader split (Cluster 6.1 prereq), the
            // opaque compute pipeline is specialized per
            // `MaterialShaderId`. Look up the effective material's
            // shader_id and pick the matching pipeline so PBR / Unlit
            // / Toon route to their own specialized compute pass.
            let shader_id = self.materials.shader_id(effective_material);

            let renderable = Renderable::Mesh {
                key: mesh_key,
                mesh,
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
                hud.push(renderable.clone());
            } else if self.materials.is_transparency_pass(effective_material) {
                transparent.push(renderable);
            } else {
                opaque.push(renderable);
            }
        }

        if let Some(camera_matrices) = self.camera.last_matrices.as_ref() {
            let view_proj = camera_matrices.view_projection();
            opaque.sort_by(|a, b| geometry_sort_renderable(ctx, a, b, &view_proj, false));
            transparent.sort_by(|a, b| geometry_sort_renderable(ctx, a, b, &view_proj, true));
            hud.sort_by(|a, b| geometry_sort_renderable(ctx, a, b, &view_proj, true));
        }

        Ok(Renderables {
            opaque,
            transparent,
            hud,
        })
    }
}

fn geometry_sort_renderable(
    ctx: &RenderContext,
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
            a.geometry_render_pipeline_key(ctx),
            b.geometry_render_pipeline_key(ctx),
        ) {
            (Err(_), Err(_)) => return std::cmp::Ordering::Equal,
            (Err(_), Ok(_)) => return std::cmp::Ordering::Greater,
            (Ok(_), Err(_)) => return std::cmp::Ordering::Less,
            (Ok(key_a), Ok(key_b)) => {
                let pipeline_ordering = key_a.cmp(&key_b);
                if pipeline_ordering != std::cmp::Ordering::Equal {
                    return pipeline_ordering;
                }
            }
        }
    }

    match (a.world_aabb(), b.world_aabb()) {
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

/// Single renderable entity.
#[derive(Debug, Clone)]
pub enum Renderable<'a> {
    Mesh {
        key: MeshKey,
        mesh: &'a Mesh,
        material_opaque_compute_pipeline_key: Option<ComputePipelineKey>,
        material_transparent_render_pipeline_key: Option<RenderPipelineKey>,
    },
}

impl Renderable<'_> {
    /// Returns the geometry render pipeline key.
    pub fn geometry_render_pipeline_key(&self, ctx: &RenderContext) -> Result<RenderPipelineKey> {
        match self {
            Self::Mesh { mesh, .. } => mesh.geometry_render_pipeline_key(ctx),
        }
    }

    /// Returns the opaque compute pipeline key, if any.
    pub fn material_opaque_compute_pipeline_key(&self) -> Option<ComputePipelineKey> {
        match self {
            Self::Mesh {
                material_opaque_compute_pipeline_key,
                ..
            } => *material_opaque_compute_pipeline_key,
        }
    }

    /// Returns the transparent render pipeline key, if any.
    pub fn material_transparent_render_pipeline_key(
        &self,
        _ctx: &RenderContext,
    ) -> Option<RenderPipelineKey> {
        match self {
            Self::Mesh {
                material_transparent_render_pipeline_key,
                ..
            } => *material_transparent_render_pipeline_key,
        }
    }

    /// Returns the material key for this renderable.
    pub fn material_key(&self) -> crate::materials::MaterialKey {
        match self {
            Self::Mesh { mesh, .. } => mesh.material_key,
        }
    }

    /// Returns the world-space AABB, if present.
    pub fn world_aabb(&self) -> Option<&'_ Aabb> {
        match self {
            Self::Mesh { mesh, .. } => mesh.world_aabb.as_ref(),
        }
    }

    /// Pushes geometry pass commands for this renderable.
    pub fn push_geometry_pass_commands(
        &self,
        ctx: &RenderContext,
        render_pass: &RenderPassEncoder,
        geometry_bind_groups: &GeometryBindGroups,
    ) -> Result<()> {
        match self {
            Self::Mesh { mesh, key, .. } => {
                mesh.push_geometry_pass_commands(ctx, *key, render_pass, geometry_bind_groups)
            }
        }
    }

    /// Pushes transparent material pass commands for this renderable.
    pub fn push_material_transparent_pass_commands(
        &self,
        ctx: &RenderContext,
        render_pass: &RenderPassEncoder,
        mesh_material_bind_group: &web_sys::GpuBindGroup,
    ) -> Result<()> {
        match self {
            Self::Mesh { mesh, key, .. } => mesh.push_material_transparent_pass_commands(
                ctx,
                *key,
                render_pass,
                mesh_material_bind_group,
            ),
        }
    }
}

type Result<T> = std::result::Result<T, AwsmError>;
