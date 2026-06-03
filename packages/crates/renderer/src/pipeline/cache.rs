//! Pipeline cache keys and descriptor builders.

use std::collections::BTreeMap;

use awsm_renderer_core::pipeline::{
    constants::{ConstantOverrideKey, ConstantOverrideValue},
    depth_stencil::DepthStencilState,
    fragment::{ColorTargetState, FragmentState},
    layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
    primitive::PrimitiveState,
    vertex::{VertexBufferLayout, VertexState},
    RenderPipelineDescriptor,
};

use crate::{
    bind_group::{material_textures::MaterialBindGroupLayoutKey, BindGroups},
    shaders::ShaderKey,
};

use super::{pipelines::PipelineLayoutKey, AwsmPipelineError};

/// Cache key for render pipeline creation.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct RenderPipelineCacheKey {
    pub shader_key: ShaderKey,
    pub layout_key: PipelineLayoutKey,
    pub primitive: PrimitiveState,
    pub depth_stencil: Option<DepthStencilState>,
    pub fragment_targets: Vec<ColorTargetState>,
    pub vertex_buffer_layouts: Vec<VertexBufferLayout>,
    pub vertex_constants: BTreeMap<ConstantOverrideKey, ConstantOverrideValue>,
}

/// Cache key for pipeline layouts.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub enum PipelineLayoutCacheKey {
    Mesh {
        has_morph_key: bool,
        has_skin_key: bool,
        material_layout_key: MaterialBindGroupLayoutKey,
    },
}

impl PipelineLayoutCacheKey {
    /// Creates a mesh pipeline layout cache key.
    pub fn new_mesh(
        material_layout_key: MaterialBindGroupLayoutKey,
        has_morph_key: bool,
        has_skin_key: bool,
    ) -> Self {
        Self::Mesh {
            has_morph_key,
            has_skin_key,
            material_layout_key,
        }
    }
}

impl RenderPipelineCacheKey {
    /// Creates a cache key with shader and layout keys.
    pub fn new(shader_key: ShaderKey, layout_key: PipelineLayoutKey) -> Self {
        Self {
            shader_key,
            layout_key,
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            fragment_targets: Vec::new(),
            vertex_buffer_layouts: Vec::new(),
            vertex_constants: BTreeMap::new(),
        }
    }

    /// Adds a vertex buffer layout to the key.
    pub fn with_push_vertex_buffer_layout(
        mut self,
        vertex_buffer_layout: VertexBufferLayout,
    ) -> Self {
        self.vertex_buffer_layouts.push(vertex_buffer_layout);
        self
    }

    /// Adds a fragment target to the key.
    pub fn with_push_fragment_target(mut self, target: ColorTargetState) -> Self {
        self.fragment_targets.push(target);
        self
    }

    /// Sets the primitive state.
    pub fn with_primitive(mut self, primitive: PrimitiveState) -> Self {
        self.primitive = primitive;
        self
    }

    /// Sets the depth/stencil state.
    pub fn with_depth_stencil(mut self, depth_stencil: DepthStencilState) -> Self {
        self.depth_stencil = Some(depth_stencil);
        self
    }

    #[allow(dead_code)]
    /// Adds a vertex constant override.
    pub fn with_vertex_constant(
        mut self,
        key: ConstantOverrideKey,
        value: ConstantOverrideValue,
    ) -> Self {
        self.vertex_constants.insert(key, value);
        self
    }
}

impl RenderPipelineCacheKey {
    /// Builds a WebGPU render pipeline descriptor.
    pub fn into_descriptor(
        self,
        shader_module: &web_sys::GpuShaderModule,
        layout: &web_sys::GpuPipelineLayout,
        label: Option<&str>,
    ) -> Result<web_sys::GpuRenderPipelineDescriptor> {
        let mut vertex = VertexState::new(shader_module, None);
        vertex.buffer_layouts = self.vertex_buffer_layouts;
        vertex.constants = self.vertex_constants;

        let fragment = FragmentState::new(shader_module, None, self.fragment_targets.clone());

        let mut descriptor = RenderPipelineDescriptor::new(vertex, label)
            .with_primitive(self.primitive)
            .with_layout(PipelineLayoutKind::Custom(layout.clone()))
            .with_fragment(fragment);

        if let Some(depth_stencil) = self.depth_stencil {
            descriptor = descriptor.with_depth_stencil(depth_stencil);
        }

        Ok(descriptor.into())
    }
}

impl PipelineLayoutCacheKey {
    /// Builds a pipeline layout descriptor from the cache key.
    pub fn into_descriptor<'a>(
        self,
        bind_groups: &BindGroups,
        label: Option<&'a str>,
    ) -> Result<PipelineLayoutDescriptor<'a>> {
        match self {
            PipelineLayoutCacheKey::Mesh {
                has_morph_key,
                has_skin_key,
                material_layout_key,
            } => {
                let mut bind_group_layouts = vec![
                    bind_groups
                        .uniform_storages
                        .gpu_universal_bind_group_layout()
                        .clone(),
                    bind_groups
                        .uniform_storages
                        .gpu_mesh_all_bind_group_layout()
                        .clone(),
                    bind_groups
                        .material_textures
                        .gpu_bind_group_layout(material_layout_key)?
                        .clone(),
                ];

                if has_morph_key || has_skin_key {
                    bind_group_layouts.push(
                        bind_groups
                            .uniform_storages
                            .gpu_mesh_shape_bind_group_layout()
                            .clone(),
                    );
                }

                Ok(PipelineLayoutDescriptor::new(label, bind_group_layouts))
            }
        }
    }
}

type Result<T> = std::result::Result<T, AwsmPipelineError>;
