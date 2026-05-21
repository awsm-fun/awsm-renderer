//! Mesh data and rendering helpers.

use awsm_renderer_core::{
    command::render_pass::RenderPassEncoder, pipeline::primitive::IndexFormat,
};

use crate::materials::MaterialKey;
use crate::meshes::error::AwsmMeshError;
use crate::meshes::MeshKey;
use crate::render::RenderContext;
use crate::render_passes::geometry::bind_group::GeometryBindGroups;
use crate::render_passes::geometry::pipeline::GeometryRenderPipelineKeyOpts;
use crate::transforms::TransformKey;
use crate::{bounds::Aabb, pipelines::render_pipeline::RenderPipelineKey};

use crate::error::Result;

// this is most like a "primitive" in gltf, not the containing "mesh"
// because for non-gltf naming, "mesh" makes more sense
/// Camera-facing rotation override applied in the geometry vertex shader.
///
/// Mirrors `BillboardMode` in `lockstep-game-data` and the `billboard_mode`
/// field on the WGSL `GeometryMeshMeta` struct. The shader picks one of three
/// paths after `apply_vertex` builds the world transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BillboardMode {
    /// No override — the mesh uses its authored rotation.
    #[default]
    None,
    /// Yaw-only — rotates around world `+Y` to face the camera, preserving the
    /// authored pitch / roll. Right pick for upright sprites (name tags, etc.).
    YAxis,
    /// Full — overrides the rotation so the mesh's local `+Z` points at the
    /// camera with the world-up reference for the secondary basis. Right pick
    /// for particle quads and generic billboards.
    Full,
}

impl BillboardMode {
    /// Encoding written into `GeometryMeshMeta::billboard_mode` for the
    /// vertex shader. Must match the WGSL constants in `apply_vertex.wgsl`.
    pub fn as_u32(self) -> u32 {
        match self {
            BillboardMode::None => 0,
            BillboardMode::YAxis => 1,
            BillboardMode::Full => 2,
        }
    }
}

/// Mesh instance metadata and render flags.
#[derive(Debug, Clone)]
pub struct Mesh {
    pub world_aabb: Option<Aabb>, // this is the transformed AABB, used for frustum culling and depth sorting
    pub transform_key: TransformKey,
    pub material_key: MaterialKey,
    pub double_sided: bool,
    pub instanced: bool,
    pub hud: bool,
    pub hidden: bool,
    /// Base instance index into the per-instance attribute storage buffer
    /// (`u32::MAX` = no per-instance attributes; identity tint at shading
    /// time). Set by `AwsmRenderer::set_mesh_instance_attrs` after writing
    /// the attribute slice via `Instances::attribute_insert`.
    pub instance_attr_base: u32,
    /// Camera-facing rotation override applied in the geometry vertex shader.
    /// Defaults to `BillboardMode::None`; sprite + particle meshes set it to
    /// `YAxis` / `Full` at construction time.
    pub billboard_mode: BillboardMode,
    /// Whether this mesh appears in the shadow-generation pass.
    /// Defaults to `true`; sprites/particles override to `false`.
    pub cast_shadows: bool,
    /// Whether this mesh is darkened by shadow sampling during shading.
    /// Defaults to `true`.
    pub receive_shadows: bool,
    /// Skinning update cadence in frames. `1` (the default) updates every
    /// frame; `2` updates every other frame; `4` quarter-rate, etc.
    /// Distance-LOD'd characters typically run at `2` or `4` past a few
    /// metres — the visual difference at that distance is below the per-
    /// pixel threshold and the GPU animation budget drops linearly.
    ///
    /// Cluster 8.3 of the optimisation plan. Pairs with the coverage-
    /// driven skinning skip from Cluster 6.2 — coverage answers "skip
    /// this frame entirely?", `skin_update_period` answers "what's the
    /// background cadence when not skipped?".
    pub skin_update_period: u8,
    /// Cheap material variant for low-coverage shading (Cluster 6.3).
    /// When set, the renderer swaps `material_key` → this key for any
    /// frame where the mesh's last-frame coverage is below
    /// `cheap_material_pixel_threshold`. `None` (the default) opts out
    /// — the mesh always uses its full `material_key`.
    pub cheap_material_key: Option<MaterialKey>,
    /// Coverage threshold (in pixels) below which the cheap material
    /// variant takes over. Only consulted when `cheap_material_key` is
    /// `Some`. The plan suggests letting Cluster 4.1's quality tier
    /// drive this — Low → 16, Medium → 64, High → 256, Ultra → 1024.
    pub cheap_material_pixel_threshold: u32,
    /// Whether projection decals (Cluster 6.4) can land on this
    /// mesh. Default `true`. The decal compute pass reads this from
    /// each pixel's `MaterialMeshMeta` and skips the per-decal
    /// volume test for non-receiving meshes — useful for sky-domes,
    /// HUD-like geometry, or surfaces the artist wants kept clean.
    pub receive_decals: bool,
}

impl Mesh {
    /// Creates a mesh with the given properties.
    pub fn new(
        transform_key: TransformKey,
        material_key: MaterialKey,
        double_sided: bool,
        instanced: bool,
        hud: bool,
        hidden: bool,
    ) -> Self {
        Self {
            transform_key,
            material_key,
            double_sided,
            instanced,
            hud,
            world_aabb: None,
            hidden,
            instance_attr_base: u32::MAX,
            billboard_mode: BillboardMode::None,
            cast_shadows: true,
            receive_shadows: true,
            skin_update_period: 1,
            cheap_material_key: None,
            cheap_material_pixel_threshold: 64,
            receive_decals: true,
        }
    }

    /// Effective material to use for this frame given last-frame
    /// coverage. Returns `cheap_material_key` when it's set AND the
    /// mesh's last-frame coverage is below
    /// `cheap_material_pixel_threshold`; otherwise the authored
    /// `material_key`. Cluster 6.3 hook — call from
    /// `collect_renderables` or any other place that picks the
    /// material pipeline.
    pub fn effective_material_key(
        &self,
        mesh_key: MeshKey,
        coverage: &crate::coverage::MeshCoverage,
    ) -> MaterialKey {
        if let Some(cheap) = self.cheap_material_key {
            if coverage.is_below_threshold(mesh_key, self.cheap_material_pixel_threshold) {
                return cheap;
            }
        }
        self.material_key
    }

    /// Returns the geometry render pipeline key for this mesh.
    pub fn geometry_render_pipeline_key(&self, ctx: &RenderContext) -> Result<RenderPipelineKey> {
        ctx.render_passes
            .geometry
            .pipelines
            .get_render_pipeline_key(GeometryRenderPipelineKeyOpts {
                anti_aliasing: ctx.anti_aliasing,
                instancing: self.instanced,
                cull_mode: if self.double_sided {
                    awsm_renderer_core::pipeline::primitive::CullMode::None
                } else {
                    awsm_renderer_core::pipeline::primitive::CullMode::Back
                },
            })
    }

    /// Pushes geometry pass draw commands for this mesh.
    ///
    /// Plan §16.7/§16.8: non-instanced meshes route through the new
    /// storage-array meta binding and either `drawIndirect` (under
    /// `features.gpu_culling`) or
    /// `draw_indexed_with_first_instance` (legacy). The legacy
    /// `first_instance = mesh_meta_idx` makes the vertex shader's
    /// `geometry_mesh_metas[instance_index]` resolve to the right
    /// slot. Instanced meshes keep the legacy uniform-with-dynamic-
    /// offset binding + `draw_indexed_with_instance_count` — their
    /// `instance_index` range across the actual instances would
    /// collide with neighbouring meshes' meta slots if they used
    /// the shared storage array, so they sit out the drawIndirect
    /// path entirely.
    pub fn push_geometry_pass_commands(
        &self,
        ctx: &RenderContext,
        mesh_key: MeshKey,
        render_pass: &RenderPassEncoder,
        bind_groups: &GeometryBindGroups,
    ) -> Result<()> {
        let meta_offset = ctx.meshes.meta.geometry_buffer_offset(mesh_key)? as u32;
        // Mesh slot index = byte offset / aligned slot size. The
        // geometry meta uses `GEOMETRY_MESH_META_BYTE_ALIGNMENT`
        // (256 B) per slot.
        let mesh_meta_idx = meta_offset
            / crate::meshes::meta::geometry_meta::GEOMETRY_MESH_META_BYTE_ALIGNMENT as u32;

        if self.instanced {
            render_pass.set_bind_group(
                2,
                bind_groups.meta.get_uniform_bind_group()?,
                Some(&[meta_offset]),
            )?;
        } else {
            // Non-instanced uses the storage-array binding; no dynamic
            // offset, the shader reads
            // `geometry_mesh_metas[instance_index]`.
            render_pass.set_bind_group(2, bind_groups.meta.get_storage_bind_group()?, None)?;
        }

        render_pass.set_vertex_buffer(
            0,
            ctx.meshes.visibility_geometry_data_gpu_buffer(),
            Some(
                ctx.meshes
                    .visibility_geometry_data_buffer_offset(mesh_key)? as u64,
            ),
            None,
        );

        if self.instanced {
            let offset = ctx.instances.transform_buffer_offset(self.transform_key)?;
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
                    .visibility_geometry_index_buffer_offset(mesh_key)? as u64,
            ),
            None,
        );

        let index_count = buffer_info.triangles.vertex_attribute_indices.count as u32;

        if self.instanced {
            let instance_count = ctx
                .instances
                .transform_instance_count(self.transform_key)
                .ok_or(AwsmMeshError::InstancingMissingTransforms(mesh_key))?;
            render_pass.draw_indexed_with_instance_count(index_count, instance_count as u32);
        } else if ctx.features.gpu_culling {
            // §16.7/§16.8 drawIndirect path. The compaction shader
            // populated `IndirectDrawArgs[mesh_meta_idx].instance_count`
            // for this frame from `visible_this_frame[]`; the static
            // fields (`index_count`, `first_instance = mesh_meta_idx`)
            // were pre-uploaded by the per-frame
            // `populate_indirect_args` pass in `render.rs`.
            let args_buffer = ctx
                .compaction_buffers
                .expect("compaction buffers missing despite gpu_culling feature on")
                .args_buffer
                .clone();
            let args_offset = (mesh_meta_idx as u64)
                * crate::render_passes::occlusion::compaction::INDIRECT_DRAW_ARGS_STRIDE as u64;
            render_pass.draw_indexed_indirect_with_f64(&args_buffer, args_offset as f64);
        } else {
            // Legacy CPU-recorded path. `first_instance` carries the
            // slot index so the vertex shader's storage-array lookup
            // still resolves to this mesh's meta.
            render_pass
                .draw_indexed_with_instance_count_and_first_index_and_base_vertex_and_first_instance(
                    index_count,
                    1,
                    0,
                    0,
                    mesh_meta_idx,
                );
        }

        Ok(())
    }

    /// Pushes transparent material pass commands for this mesh.
    pub fn push_material_transparent_pass_commands(
        &self,
        ctx: &RenderContext,
        mesh_key: MeshKey,
        render_pass: &RenderPassEncoder,
        mesh_material_bind_group: &web_sys::GpuBindGroup,
    ) -> Result<()> {
        let geometry_meta_offset = ctx.meshes.meta.geometry_buffer_offset(mesh_key)? as u32;
        let material_meta_offset = ctx.meshes.meta.material_buffer_offset(mesh_key)? as u32;
        let buffer_info = ctx.meshes.buffer_info(mesh_key)?;

        render_pass.set_bind_group(
            3,
            mesh_material_bind_group,
            Some(&[geometry_meta_offset, material_meta_offset]),
        )?;

        // Geometry stuff Slot 0 (locations 0-4)
        render_pass.set_vertex_buffer(
            0,
            ctx.meshes.transparency_geometry_data_gpu_buffer(),
            Some(
                ctx.meshes
                    .transparency_geometry_data_buffer_offset(mesh_key)? as u64,
            ),
            None,
        );

        // Instancing Slot 1 (locations 5-8)
        let attribute_slot = if self.instanced {
            let offset = ctx.instances.transform_buffer_offset(self.transform_key)?;
            render_pass.set_vertex_buffer(
                1,
                ctx.instances.gpu_transform_buffer(),
                Some(offset as u64),
                None,
            );

            2
        } else {
            1
        };

        // Attributes
        // If instanced: slot 2 (locations 9+)
        // If not instanced: slot 1 (locations 5+)
        render_pass.set_vertex_buffer(
            attribute_slot,
            ctx.meshes.custom_attribute_data_gpu_buffer(),
            Some(ctx.meshes.custom_attribute_data_buffer_offset(mesh_key)? as u64),
            None,
        );

        render_pass.set_index_buffer(
            ctx.meshes.transparency_geometry_index_gpu_buffer(),
            IndexFormat::Uint32,
            Some(
                ctx.meshes
                    .transparency_geometry_index_buffer_offset(mesh_key)? as u64,
            ),
            None,
        );

        let index_count = buffer_info.triangles.vertex_attribute_indices.count as u32;

        if self.instanced {
            let instance_count = ctx
                .instances
                .transform_instance_count(self.transform_key)
                .ok_or(AwsmMeshError::InstancingMissingTransforms(mesh_key))?;
            render_pass.draw_indexed_with_instance_count(index_count, instance_count as u32);
        } else {
            render_pass.draw_indexed(index_count);
        }

        Ok(())
    }
}
