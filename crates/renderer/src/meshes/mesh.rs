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
        }
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
    pub fn push_geometry_pass_commands(
        &self,
        ctx: &RenderContext,
        mesh_key: MeshKey,
        render_pass: &RenderPassEncoder,
        bind_groups: &GeometryBindGroups,
    ) -> Result<()> {
        let meta_offset = ctx.meshes.meta.geometry_buffer_offset(mesh_key)? as u32;

        render_pass.set_bind_group(2, bind_groups.meta.get_bind_group()?, Some(&[meta_offset]))?;

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
        } else {
            render_pass.draw_indexed(index_count);
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
