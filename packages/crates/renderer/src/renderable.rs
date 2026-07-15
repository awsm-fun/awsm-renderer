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
    /// Reused scratch for the per-frame visible mesh-key set. Was a fresh
    /// `Vec::with_capacity(mesh_count)` every frame — allocator/GC churn that grows
    /// with mesh count (16 B/mesh; ~240 KB/frame at 15K meshes) and shows up as
    /// jank spikes (GC pauses) on the frame-time p95, not the average. Keys only:
    /// the `Mesh` is re-fetched (O(1) `SlotMap` index) in the build loop, so no
    /// borrow is stored across frames.
    visible: Vec<MeshKey>,
}

impl RenderablePool {
    /// Drops the prior frame's content while keeping the underlying
    /// allocations. Called at the top of each frame's
    /// `collect_renderables`.
    fn clear(&mut self) {
        self.opaque.clear();
        self.transparent.clear();
        self.hud.clear();
        self.visible.clear();
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
        let _maybe_span_guard = if self.logging.cpu.sub_frame() {
            Some(tracing::span!(tracing::Level::INFO, "Collect renderables").entered())
        } else {
            None
        };

        // Take the pool out by std::mem::take so we can populate
        // without holding a `&mut self` borrow during the read-only
        // queries below. The pool gets put back at the end.
        let mut pool = std::mem::take(&mut self.renderable_pool);
        pool.clear();

        let frustum = self.camera.last_matrices.as_ref().map(|matrices| {
            Frustum::from_view_projection(matrices.view_projection(), matrices.reverse_z)
        });

        // Build the visible mesh-key set into the pool's reused `visible`
        // scratch (cleared above) from the BVH instead of walking every mesh.
        // The previous linear scan tested every mesh's cached `world_aabb`
        // against the frustum on every frame; the BVH path descends
        // hierarchically and surfaces only the surviving leaves. Meshes
        // without a world AABB (procedural / mid-load) aren't in the index —
        // fall back to a tail-walk of those so they still draw conservatively.
        // Stored as keys only (no `&Mesh`) so the scratch can be pooled.
        match &frustum {
            Some(f) => {
                pool.visible.extend(
                    self.scene_spatial
                        .query_frustum(f, NodeFilter::camera_default())
                        .filter(|node| self.meshes.get(node.mesh_key).is_ok())
                        .map(|node| node.mesh_key),
                );
                // Conservative fallback: any mesh without a world AABB
                // can't be tested by the BVH; keep it in the visible set.
                pool.visible.extend(
                    self.meshes
                        .iter()
                        .filter(|(_, m)| !m.hidden && m.world_aabb.is_none())
                        .map(|(k, _)| k),
                );
            }
            None => {
                pool.visible.extend(
                    self.meshes
                        .iter()
                        .filter(|(_, m)| !m.hidden)
                        .map(|(k, _)| k),
                );
            }
        }

        // Phase 2: build a `Renderable` per visible key (re-fetch the mesh —
        // O(1) `SlotMap` index). Index iteration keeps `pool.visible` (read)
        // disjoint from `pool.opaque`/etc (written) under the borrow checker.
        for idx in 0..pool.visible.len() {
            let mesh_key = pool.visible[idx];
            let Ok(mesh) = self.meshes.get(mesh_key) else {
                continue;
            };
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

            // -----------------------------------------------------------------
            // Geometry variant precedence (drawn EXACTLY ONCE):
            //   combined (masked + custom-vertex) > plain masked
            //     > plain custom-vertex > solid.
            // A material that is BOTH glTF `MASK` (`alpha_cutoff` present) AND
            // custom-vertex (`vertex_shader_info_for` is `Some`) routes to the
            // COMBINED key when its combined variant is compiled — and in that
            // case the plain masked + plain custom-vertex keys are forced `None`
            // so the mesh isn't ALSO drawn in pass 2 / pass 3 (no double-draw).
            // If the combined variant hasn't compiled yet, the combined key is
            // `None` and the mesh falls back to its plain masked / custom-vertex
            // bucket (displaced OR cut, never dropped). Non-instanced only
            // (matches the compiled shape of every variant pool).
            // -----------------------------------------------------------------
            let canonical_shader_id = self.materials.canonical_shader_id(routing_material);
            let msaa = match self.anti_aliasing.msaa_sample_count {
                Some(4) => Some(4u32),
                _ => None,
            };
            let is_masked =
                !mesh.instanced && self.materials.alpha_cutoff(routing_material).is_some();
            // Cheap per-frame existence check (no DynamicVertexShaderInfo build /
            // no WGSL-string alloc — collect_renderables runs every frame).
            let is_custom_vertex = !mesh.instanced
                && self
                    .dynamic_materials
                    .has_vertex_shader(canonical_shader_id);

            // Combined key — only when the material is BOTH masked AND
            // custom-vertex AND the combined variant is compiled.
            let geometry_masked_custom_vertex_render_pipeline_key = if is_masked && is_custom_vertex
            {
                self.render_passes
                    .geometry
                    .masked_custom_vertex_pipelines
                    .get(msaa, canonical_shader_id, cull_mode)
            } else {
                None
            };
            // When the combined key claimed this mesh, suppress the plain masked
            // + plain custom-vertex keys (so pass 4 draws it once). Otherwise the
            // plain keys apply per their own gating.
            let combined_claimed = geometry_masked_custom_vertex_render_pipeline_key.is_some();

            // Plain custom-vertex variant: strictly additive + opt-in (a material
            // with `wgsl_vertex == None` always leaves this `None`). NOT set when
            // the combined key claimed this mesh.
            let geometry_custom_vertex_render_pipeline_key =
                if !combined_claimed && is_custom_vertex {
                    self.render_passes.geometry.custom_vertex_pipelines.get(
                        msaa,
                        canonical_shader_id,
                        cull_mode,
                    )
                } else {
                    None
                };

            // Plain masked (alpha-tested) variant: only for non-instanced glTF
            // MASK meshes whose per-shader-id masked pipeline is compiled, and
            // NOT already claimed by the combined key. When a mesh is BOTH Mask
            // AND custom-vertex but the COMBINED variant hasn't compiled yet (so
            // it fell back), the plain custom-vertex key above took it — suppress
            // the masked key here too so it's drawn EXACTLY ONCE (custom-vertex
            // precedence > masked, matching the shadow path). A correct displaced
            // silhouette matters more than a hole until the combined lands.
            let geometry_masked_render_pipeline_key = if !combined_claimed
                && is_masked
                && geometry_custom_vertex_render_pipeline_key.is_none()
            {
                // Key on the CANONICAL id — the masked fragment is
                // variant-independent.
                self.render_passes.geometry.masked_pipelines.get(
                    msaa,
                    canonical_shader_id,
                    cull_mode,
                )
            } else {
                None
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
                    .inspect_err(|err| {
                        // Log the *actual* failure reason at collection
                        // time — `Renderable` only stores
                        // `Option<RenderPipelineKey>`, so the geometry
                        // pass's `None` path can only emit a generic
                        // "missing pipeline" warning. Surface the
                        // structured error here while we still have it.
                        tracing::warn!(
                            "geometry pipeline key lookup failed for mesh {mesh_key:?}: {err:?}"
                        );
                    })
                    .ok(),
                geometry_masked_render_pipeline_key,
                geometry_custom_vertex_render_pipeline_key,
                geometry_masked_custom_vertex_render_pipeline_key,
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

            // Route each mesh to the pass it actually has geometry for. A geometry's
            // reps are derived at commit from the union of materials bound to it
            // (visibility and/or transparency); an instance may carry one or both.
            // Drawing a mesh in a pass it lacks geometry for raises
            // `VisibilityGeometryBufferNotFound` (or its transparency twin) and —
            // because `render()` is atomic — blacks out the WHOLE frame, opaque
            // geometry and skybox included.
            //
            // Crucially, the material's transparency classification
            // (`is_transparency_pass`) can DRIFT from the mesh's immutable built
            // geometry: `transparency_pass_keys` is toggled on material
            // insert/update/reconcile, which can flip *after* the mesh was built. So
            // route on the geometry the mesh actually has, not on the classification
            // alone — a mesh with no visibility geometry must never enter the opaque
            // list regardless of how its material currently classifies. A mesh with
            // neither buffer yet (mid-upload) is skipped this frame rather than
            // crashing a pass.
            // Ground-truth routing: a mesh's geometry capability is cached on `Mesh`
            // (set at commit from the shared geometry resource's reps; zero-cost
            // field reads, no per-mesh buffer_info lookup in this hot path). The
            // material's transparency classification disambiguates a mesh that
            // carries BOTH buffers — the dedup case (one geometry under an opaque +
            // a transparent material) and the free opaque↔blend live-reassignment.
            let wants_transparency = self.materials.is_transparency_pass(routing_material);

            match route_renderable(
                mesh.hud,
                mesh.has_visibility_geometry,
                mesh.has_transparency_geometry,
                wants_transparency,
            ) {
                RenderableRoute::Hud => pool.hud.push(renderable),
                RenderableRoute::Opaque => pool.opaque.push(renderable),
                RenderableRoute::Transparent => pool.transparent.push(renderable),
                RenderableRoute::Skip => tracing::warn!(
                    "Skipping mesh {mesh_key:?} in collect_renderables: no visibility or \
                     transparency geometry buffer (mid-upload?) — not drawn this frame"
                ),
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
    /// Set for glTF `MASK` meshes whose masked (alpha-tested) variant has been
    /// compiled. When `Some`, the geometry pass draws this mesh with the masked
    /// pipeline + augmented group-0 (cutoff `discard`); when `None` the mesh
    /// falls back to `geometry_render_pipeline_key` (renders solid).
    pub geometry_masked_render_pipeline_key: Option<RenderPipelineKey>,
    /// Set for a mesh whose material is **custom-vertex** (its registration
    /// carries a non-empty `wgsl_vertex`) and whose custom-vertex variant has
    /// been compiled. When `Some`, the geometry pass draws this mesh with the
    /// custom-vertex pipeline + augmented group-0 (displacement hook) in pass 3;
    /// when `None` the mesh falls back to `geometry_render_pipeline_key`
    /// (renders un-displaced). Strictly additive + opt-in — a material with
    /// `wgsl_vertex == None` always leaves this `None`.
    pub geometry_custom_vertex_render_pipeline_key: Option<RenderPipelineKey>,
    /// Set for a mesh whose material is BOTH glTF `MASK` AND **custom-vertex**
    /// (its registration carries both an alpha body and a `wgsl_vertex` body) and
    /// whose COMBINED variant has been compiled. When `Some`, the geometry pass
    /// draws this mesh with the combined pipeline (displaced silhouette AND alpha
    /// cutout) in pass 4. Takes PRECEDENCE over the plain masked + plain
    /// custom-vertex keys: when this is `Some`, both of those are forced `None`
    /// (set in `collect_renderables`) so the mesh is drawn EXACTLY ONCE. When
    /// `None` (variant not compiled yet), the mesh falls back to the plain
    /// custom-vertex / masked / solid bucket.
    pub geometry_masked_custom_vertex_render_pipeline_key: Option<RenderPipelineKey>,
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

    /// Pushes geometry pass commands for this renderable. `masked` selects the
    /// alpha-tested draw path (forces the non-instanced uniform-meta CPU draw,
    /// matching the masked pipeline's compiled shape).
    pub fn push_geometry_pass_commands(
        &self,
        ctx: &RenderContext,
        render_pass: &RenderPassEncoder,
        geometry_bind_groups: &GeometryBindGroups,
        masked: bool,
    ) -> Result<()> {
        let mesh = ctx.meshes.get(self.key)?;
        mesh.push_geometry_pass_commands(ctx, self.key, render_pass, geometry_bind_groups, masked)
    }

    /// Returns the masked (alpha-tested) geometry pipeline key, if this mesh's
    /// material is glTF `MASK` and its masked variant has been compiled.
    pub fn geometry_masked_render_pipeline_key(&self) -> Option<RenderPipelineKey> {
        self.geometry_masked_render_pipeline_key
    }

    /// Returns the custom-vertex geometry pipeline key, if this mesh's material
    /// declared a `wgsl_vertex` displacement body and its custom-vertex variant
    /// has been compiled.
    pub fn geometry_custom_vertex_render_pipeline_key(&self) -> Option<RenderPipelineKey> {
        self.geometry_custom_vertex_render_pipeline_key
    }

    /// Returns the COMBINED masked + custom-vertex geometry pipeline key, if this
    /// mesh's material is BOTH glTF `MASK` AND custom-vertex and its combined
    /// variant has been compiled.
    pub fn geometry_masked_custom_vertex_render_pipeline_key(&self) -> Option<RenderPipelineKey> {
        self.geometry_masked_custom_vertex_render_pipeline_key
    }

    /// Pushes the custom-vertex geometry draw for this renderable: binds the
    /// shared zero uv0 buffer at the uv0 slot, then records the standard
    /// geometry draw. Mirrors [`Self::push_geometry_pass_commands`].
    pub fn push_geometry_custom_vertex_pass_commands(
        &self,
        ctx: &RenderContext,
        render_pass: &RenderPassEncoder,
        geometry_bind_groups: &GeometryBindGroups,
        uv0_zero_buffer: &web_sys::GpuBuffer,
    ) -> Result<()> {
        let mesh = ctx.meshes.get(self.key)?;
        mesh.push_geometry_custom_vertex_pass_commands(
            ctx,
            self.key,
            render_pass,
            geometry_bind_groups,
            uv0_zero_buffer,
        )
    }

    /// Pushes transparent material pass commands for this renderable.
    pub fn push_material_transparent_pass_commands(
        &self,
        ctx: &RenderContext,
        render_pass: &RenderPassEncoder,
        mesh_material_bind_group: &web_sys::GpuBindGroup,
    ) -> Result<()> {
        let mesh = ctx.meshes.get(self.key)?;
        mesh.push_material_transparent_pass_commands(
            ctx,
            self.key,
            render_pass,
            mesh_material_bind_group,
        )
    }
}

type Result<T> = std::result::Result<T, AwsmError>;

/// Which render pass a mesh is routed to in [`AwsmRenderer::collect_renderables`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderableRoute {
    Hud,
    Opaque,
    Transparent,
    /// No geometry buffer for any pass yet (mid-upload) — skip this frame.
    Skip,
}

/// Decide which pass a mesh can render in, based on the geometry it actually has.
///
/// A mesh drawn in a pass it lacks geometry for raises
/// `VisibilityGeometryBufferNotFound` (or its transparency twin), which — since
/// `render()` is atomic — blacks out the WHOLE frame. So the presence of each
/// geometry buffer (set from the shared geometry resource's actual reps) is the
/// ground truth for routing; the material's transparency classification
/// (`wants_transparency` = `Materials::is_transparency_pass`) only *disambiguates*.
///
/// Two cases make that disambiguation live, not theoretical:
/// - **Both reps present (the dedup case).** One geometry
///   bound to BOTH an opaque and a transparent material builds visibility AND
///   transparency reps at commit; each instance routes by its own material's
///   `wants_transparency`. This is also what makes a live opaque↔blend material
///   re-assignment (`set_mesh_material`) re-render for free when the geometry
///   already carries both kinds — no rebuild, just a different pass this frame.
/// - **Classification drift.** `wants_transparency` can flip (material edit /
///   reconcile) after the mesh's reps were frozen at commit; routing on buffer
///   presence keeps a single-rep mesh out of a pass it can't draw. A flip to a
///   kind the geometry NEVER built leaves the needed buffer absent → `Skip` (the
///   editor re-materializes such meshes from authored data, §1 ②).
///
/// A mesh with neither buffer (mid-upload / pending its first commit) is skipped
/// rather than crashing a pass.
fn route_renderable(
    hud: bool,
    has_visibility_geometry: bool,
    has_transparency_geometry: bool,
    wants_transparency: bool,
) -> RenderableRoute {
    if hud {
        RenderableRoute::Hud
    } else if has_visibility_geometry && !(wants_transparency && has_transparency_geometry) {
        // Has visibility geometry, and isn't a both-buffer mesh that the material
        // wants drawn transparent → geometry/opaque pass.
        RenderableRoute::Opaque
    } else if has_transparency_geometry {
        // Transparency geometry only (or both-buffer + wants transparency) →
        // transparency pass (reads transparency geometry; skips meshes whose
        // transparent pipeline isn't ready).
        RenderableRoute::Transparent
    } else {
        RenderableRoute::Skip
    }
}

#[cfg(test)]
mod tests {
    use super::{route_renderable, RenderableRoute};

    // (hud, has_visibility, has_transparency, wants_transparency) -> route
    #[test]
    fn routes_by_geometry_not_classification() {
        use RenderableRoute::*;

        // Normal opaque mesh.
        assert_eq!(route_renderable(false, true, false, false), Opaque);
        // Normal transparent mesh.
        assert_eq!(route_renderable(false, false, true, true), Transparent);
        // HUD always wins.
        assert_eq!(route_renderable(true, true, false, false), Hud);
        assert_eq!(route_renderable(true, false, true, true), Hud);

        // THE BUG: transparency-only mesh whose material classification drifted to
        // opaque (wants_transparency=false). Must NOT go to the opaque pass (that
        // raised VisibilityGeometryBufferNotFound and killed the frame) — it has
        // transparency geometry, so route there.
        assert_eq!(route_renderable(false, false, true, false), Transparent);

        // Symmetric drift: opaque-only mesh misclassified as transparent. Routing it
        // transparent would crash the transparency pass; draw it opaque instead.
        assert_eq!(route_renderable(false, true, false, true), Opaque);

        // No geometry yet (mid-upload): skip, don't crash a pass.
        assert_eq!(route_renderable(false, false, false, false), Skip);
        assert_eq!(route_renderable(false, false, false, true), Skip);

        // Both buffers present — the dedup case (§7): one geometry bound to both an
        // opaque and a transparent material builds both reps, and each instance
        // routes by its own material's classification. This is also the free
        // opaque↔blend live-reassignment path (set_mesh_material flips the pass
        // without a rebuild when the geometry already carries both kinds).
        assert_eq!(route_renderable(false, true, true, true), Transparent);
        assert_eq!(route_renderable(false, true, true, false), Opaque);
    }
}
