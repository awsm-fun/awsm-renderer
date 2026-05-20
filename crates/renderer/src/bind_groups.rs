//! Bind group recreation coordination.

use std::collections::HashSet;

use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use strum::{EnumIter, IntoEnumIterator};
use thiserror::Error;

use crate::{
    anti_alias::AntiAliasing, bind_group_layout::BindGroupLayouts, camera::CameraBuffer,
    environment::Environment, instances::Instances, lights::Lights, materials::Materials,
    meshes::Meshes, picker::Picker, render_passes::RenderPasses,
    render_textures::RenderTextureViews, shadows::Shadows, textures::Textures,
    transforms::Transforms,
};

// There are no cache keys for bind groups, they are created on demand
// Since changes to storages, uniforms, and textures are the reason to recreate bind groups,
// and these may be shared across multiple bind groups, we use a "create list" to track which bind groups need to be recreated
//
// Specifically, typical causes of change are:
// 1. A change in raw buffer size which causes a reallocation
// 2. A change in texture view size which causes new textures to be created
//
// That conscpicuously does not include changes to material textures
// since those are looked up via the material key and do not require a bind group recreation
/// Inputs required to rebuild bind groups.
pub struct BindGroupRecreateContext<'a> {
    pub gpu: &'a AwsmRendererWebGpu,
    pub render_texture_views: &'a RenderTextureViews,
    pub textures: &'a Textures,
    pub materials: &'a Materials,
    pub bind_group_layouts: &'a BindGroupLayouts,
    pub meshes: &'a Meshes,
    pub camera: &'a CameraBuffer,
    pub environment: &'a Environment,
    pub lights: &'a Lights,
    pub transforms: &'a Transforms,
    pub instances: &'a Instances,
    pub anti_aliasing: &'a AntiAliasing,
    pub shadows: &'a Shadows,
    /// Per-mesh light-slice storage buffers (Cluster 2.1.c). Bound at
    /// group(1) bindings 2/3 of the material-opaque + material-transparent
    /// shading passes.
    pub mesh_light_slices_gpu: &'a crate::light_buckets::MeshLightSlicesGpu,
}

/// Reasons to recreate bind groups.
#[derive(Hash, Debug, Clone, PartialEq, Eq, EnumIter)]
pub enum BindGroupCreate {
    CameraInitOnly,
    LightsResize,
    LightsInfoCreate,
    BrdfLutTextures,
    IblTextures,
    EnvironmentSkyboxCreate,
    TransformsResize,
    GeometryMorphTargetWeightsResize,
    GeometryMorphTargetValuesResize,
    MaterialMorphTargetWeightsResize,
    MaterialMorphTargetValuesResize,
    SkinJointMatricesResize,
    SkinJointIndexAndWeightsResize,
    GeometryMeshMetaResize,
    MaterialMeshMetaResize,
    MeshAttributeDataResize,
    MeshAttributeIndexResize,
    MaterialResize,
    TextureViewRecreate,
    TexturePool,
    TextureTransformsResize,
    AntiAliasingChange,
    /// Per-instance attribute storage buffer was reallocated; opaque + transparent
    /// shading bind groups must re-bind the new buffer.
    InstanceAttributesResize,
    /// Shadow atlas, EVSM atlas, cube array, or descriptors buffer was recreated;
    /// the opaque + transparent shading bind groups must re-bind the new resources.
    ShadowsResourcesChange,
    /// `mesh_light_slices` / `mesh_light_indices` GPU buffers were
    /// reallocated (per-frame grow path). The lights bind groups
    /// (opaque + transparent) must re-bind the new buffer handles.
    MeshLightSlicesResize,
}

/// Tracks pending bind group recreations.
pub struct BindGroups {
    create_list: HashSet<BindGroupCreate>,
}

impl Default for BindGroups {
    fn default() -> Self {
        Self::new()
    }
}

impl BindGroups {
    /// Creates a bind group tracker with all groups marked dirty.
    pub fn new() -> Self {
        Self {
            // startup means all bind groups are "re"created
            create_list: BindGroupCreate::iter().collect::<HashSet<_>>(),
        }
    }

    /// Marks a bind group recreation reason.
    pub fn mark_create(&mut self, create: BindGroupCreate) {
        self.create_list.insert(create);
    }

    /// Recreates bind groups affected by pending changes.
    pub fn recreate(
        &mut self,
        ctx: BindGroupRecreateContext<'_>,
        render_passes: &mut RenderPasses,
        picker: &mut Picker,
    ) -> crate::error::Result<()> {
        if self.create_list.is_empty() {
            return Ok(());
        }

        #[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
        enum FunctionToCall {
            GeometryCamera,
            GeometryTransformMaterials,
            GeometryMeta,
            GeometryAnimation,
            OpaqueMain,
            OpaqueLights,
            OpaqueTextures,
            OpaqueShadows,
            TransparentMain,
            TransparentMeshMaterial,
            TransparentLights,
            TransparentTextures,
            TransparentShadows,
            LightCulling,
            Effects,
            Display,
            Picker,
        }

        let mut functions_to_call = HashSet::new();

        for create in self.create_list.drain() {
            match create {
                BindGroupCreate::CameraInitOnly => {
                    functions_to_call.insert(FunctionToCall::GeometryCamera);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::LightsInfoCreate => {
                    functions_to_call.insert(FunctionToCall::OpaqueLights);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                }
                BindGroupCreate::LightsResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueLights);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                }
                BindGroupCreate::TransformsResize => {
                    functions_to_call.insert(FunctionToCall::GeometryTransformMaterials);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                    functions_to_call.insert(FunctionToCall::TransparentMeshMaterial);
                }
                BindGroupCreate::MaterialResize => {
                    functions_to_call.insert(FunctionToCall::GeometryTransformMaterials);
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMeshMaterial);
                }
                BindGroupCreate::GeometryMeshMetaResize => {
                    functions_to_call.insert(FunctionToCall::GeometryMeta);
                    functions_to_call.insert(FunctionToCall::TransparentMeshMaterial);
                }
                BindGroupCreate::GeometryMorphTargetWeightsResize
                | BindGroupCreate::GeometryMorphTargetValuesResize
                | BindGroupCreate::SkinJointMatricesResize
                | BindGroupCreate::SkinJointIndexAndWeightsResize => {
                    functions_to_call.insert(FunctionToCall::GeometryAnimation);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::TextureViewRecreate => {
                    functions_to_call.insert(FunctionToCall::LightCulling);
                    functions_to_call.insert(FunctionToCall::Display);
                    functions_to_call.insert(FunctionToCall::Effects);
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                    functions_to_call.insert(FunctionToCall::Picker);
                }
                BindGroupCreate::TexturePool => {
                    functions_to_call.insert(FunctionToCall::OpaqueTextures);
                    functions_to_call.insert(FunctionToCall::TransparentTextures);
                }
                BindGroupCreate::TextureTransformsResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueTextures);
                    functions_to_call.insert(FunctionToCall::TransparentTextures);
                }
                BindGroupCreate::BrdfLutTextures => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                }
                BindGroupCreate::IblTextures => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                }
                BindGroupCreate::EnvironmentSkyboxCreate => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                }
                BindGroupCreate::MaterialMorphTargetWeightsResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::MaterialMorphTargetValuesResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::MaterialMeshMetaResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                    functions_to_call.insert(FunctionToCall::Picker);
                }
                BindGroupCreate::MeshAttributeDataResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::MeshAttributeIndexResize => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::AntiAliasingChange => {
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::InstanceAttributesResize => {
                    // Per-instance attribute storage buffer is bound on the
                    // opaque + transparent main bind groups for shading lookup.
                    functions_to_call.insert(FunctionToCall::OpaqueMain);
                    functions_to_call.insert(FunctionToCall::TransparentMain);
                }
                BindGroupCreate::ShadowsResourcesChange => {
                    functions_to_call.insert(FunctionToCall::OpaqueShadows);
                    functions_to_call.insert(FunctionToCall::TransparentShadows);
                }
                BindGroupCreate::MeshLightSlicesResize => {
                    // Buffers are bound on the lights bind group of
                    // both shading passes.
                    functions_to_call.insert(FunctionToCall::OpaqueLights);
                    functions_to_call.insert(FunctionToCall::TransparentLights);
                }
            }
        }

        for f in functions_to_call {
            match f {
                FunctionToCall::GeometryCamera => {
                    render_passes.geometry.bind_groups.camera.recreate(&ctx)?;
                }
                FunctionToCall::GeometryTransformMaterials => {
                    render_passes
                        .geometry
                        .bind_groups
                        .transforms
                        .recreate(&ctx)?;
                }
                FunctionToCall::GeometryMeta => {
                    render_passes.geometry.bind_groups.meta.recreate(&ctx)?;
                }
                FunctionToCall::GeometryAnimation => {
                    render_passes
                        .geometry
                        .bind_groups
                        .animation
                        .recreate(&ctx)?;
                }
                FunctionToCall::OpaqueMain => {
                    render_passes
                        .material_opaque
                        .bind_groups
                        .recreate_main(&ctx)?;
                }
                FunctionToCall::OpaqueLights => {
                    render_passes
                        .material_opaque
                        .bind_groups
                        .recreate_lights(&ctx)?;
                }
                FunctionToCall::OpaqueTextures => {
                    render_passes
                        .material_opaque
                        .bind_groups
                        .recreate_texture_pool(&ctx)?;
                }
                FunctionToCall::OpaqueShadows => {
                    render_passes
                        .material_opaque
                        .bind_groups
                        .recreate_shadows(&ctx)?;
                }
                FunctionToCall::TransparentShadows => {
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_shadows(&ctx)?;
                }
                FunctionToCall::TransparentMain => {
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_main(&ctx)?;
                }
                FunctionToCall::TransparentMeshMaterial => {
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_mesh_material(&ctx)?;
                }
                FunctionToCall::TransparentLights => {
                    // 16.B folded `lights` into `main` on the transparent
                    // pipeline; the upstream signal still fires, but it
                    // now routes through `recreate_main` so the merged
                    // group picks up the new IBL / light buffer views.
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_main(&ctx)?;
                }
                FunctionToCall::TransparentTextures => {
                    render_passes
                        .material_transparent
                        .bind_groups
                        .recreate_texture_pool(&ctx)?;
                }
                FunctionToCall::LightCulling => {
                    render_passes.light_culling.bind_groups.recreate(&ctx)?;
                }
                FunctionToCall::Effects => {
                    render_passes.effects.bind_groups.recreate(&ctx)?;
                }
                FunctionToCall::Display => {
                    render_passes.display.bind_groups.recreate(&ctx)?;
                }
                FunctionToCall::Picker => {
                    picker.recreate_bind_group(&ctx)?;
                }
            }
        }

        Ok(())
    }
}

/// Bind group errors.
#[derive(Error, Debug)]
pub enum AwsmBindGroupError {
    #[error("[bind group] bind group not found for {0}")]
    NotFound(String),
}
