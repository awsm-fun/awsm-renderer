//! Transform hierarchy and GPU upload.

use glam::{Mat4, Quat, Vec3};
use thiserror::Error;

use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    pipeline::primitive::FrontFace,
    renderer::AwsmRendererWebGpu,
};
use slotmap::{new_key_type, SecondaryMap, SlotMap};

use crate::{
    bind_groups::{BindGroupCreate, BindGroups},
    buffer::dynamic_uniform::DynamicUniformBuffer,
    buffer::helpers::write_buffer_with_dirty_ranges,
    meshes::skins::AwsmSkinError,
    AwsmRenderer, AwsmRendererLogging,
};

impl AwsmRenderer {
    /// Updates world transforms and mesh bounds from dirty transforms.
    pub fn update_transforms(&mut self) {
        self.transforms.update_world();
        let dirty_transforms = self.transforms.take_dirty_meshes();
        let dirty_instances = self.instances.take_dirty_transforms();
        self.meshes.update_world(
            dirty_transforms,
            &dirty_instances,
            &self.transforms,
            &self.instances,
        );
    }
}

/// Transform hierarchy with GPU buffers.
pub struct Transforms {
    locals: SlotMap<TransformKey, Transform>,
    world_matrices: SecondaryMap<TransformKey, glam::Mat4>,
    children: SecondaryMap<TransformKey, Vec<TransformKey>>,
    parents: SecondaryMap<TransformKey, TransformKey>,
    // These are the transforms that are dirtied from the outside
    // e.g. may be set multiples times by the user or randomly in the hierarchy
    dirties: HashSet<TransformKey>,
    // While we calculate the dirties, we can know if meshes need to be updated
    // this is set internally
    // not every transform here is definitely a mesh, just in potential
    dirty_meshes: Vec<TransformKey>,
    gpu_dirty: bool,
    pub root_node: TransformKey,
    buffer: DynamicUniformBuffer<TransformKey>,
    normals_buffer: DynamicUniformBuffer<TransformKey>,
    pub(crate) gpu_buffer: web_sys::GpuBuffer,
    pub(crate) normals_gpu_buffer: web_sys::GpuBuffer,
}

static BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage().with_copy_dst());

impl Transforms {
    /// Initial transform slot capacity.
    pub const INITIAL_CAPACITY: usize = 32; // 32 elements is a good starting point
    /// Byte size of a transform matrix.
    pub const BYTE_SIZE: usize = 64; // 4x4 matrix of f32 is 64 bytes
    /// Byte size of a normal matrix.
    pub const NORMALS_BYTE_SIZE: usize = 36; // 3x3 matrix of f32 is 36 bytes

    /// Creates transform storage and GPU buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Transforms"),
                Transforms::INITIAL_CAPACITY * Transforms::BYTE_SIZE,
                *BUFFER_USAGE,
            )
            .into(),
        )?;
        let normals_gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Normal Transform Matrices"),
                Transforms::INITIAL_CAPACITY * Transforms::NORMALS_BYTE_SIZE,
                *BUFFER_USAGE,
            )
            .into(),
        )?;

        let buffer = DynamicUniformBuffer::new(
            Self::INITIAL_CAPACITY,
            Self::BYTE_SIZE,
            None,
            Some("Transforms".to_string()),
        );

        let normals_buffer = DynamicUniformBuffer::new(
            Self::INITIAL_CAPACITY,
            Self::NORMALS_BYTE_SIZE,
            None,
            Some("Normal Transform Matrices".to_string()),
        );

        let mut locals = SlotMap::with_capacity_and_key(Self::INITIAL_CAPACITY);
        let mut world_matrices = SecondaryMap::with_capacity(Self::INITIAL_CAPACITY);
        let mut children = SecondaryMap::new();

        let root_node = locals.insert(Transform::default());
        world_matrices.insert(root_node, glam::Mat4::IDENTITY);
        children.insert(root_node, Vec::new());

        Ok(Self {
            locals,
            world_matrices,
            children,
            parents: SecondaryMap::new(),
            dirties: HashSet::new(),
            dirty_meshes: Vec::with_capacity(Self::INITIAL_CAPACITY),
            gpu_dirty: true,
            root_node,
            buffer,
            normals_buffer,
            gpu_buffer,
            normals_gpu_buffer,
        })
    }

    /// Inserts a transform and returns its key.
    pub fn insert(&mut self, transform: Transform, parent: Option<TransformKey>) -> TransformKey {
        let world_matrix = transform.to_matrix();

        let key = self.locals.insert(transform);

        self.world_matrices.insert(key, world_matrix);
        self.children.insert(key, Vec::new());
        self.dirties.insert(key);

        self.buffer.update(key, &[0; Self::BYTE_SIZE]);
        self.normals_buffer
            .update(key, &[0; Self::NORMALS_BYTE_SIZE]);

        self.set_parent(key, parent);

        key
    }

    /// Duplicates a transform and returns the new key.
    pub fn duplicate(&mut self, key: TransformKey) -> Result<TransformKey> {
        let local_transform = self.get_local(key)?.clone();
        let parent_transform = self.get_parent(key).ok();
        Ok(self.insert(local_transform, parent_transform))
    }

    /// Removes a transform and its buffers.
    pub fn remove(&mut self, key: TransformKey) {
        if key == self.root_node {
            return;
        }

        // happens separately so that we can remove the node from the parent's children list
        self.unset_parent(key);

        self.locals.remove(key);
        self.world_matrices.remove(key);
        self.children.remove(key);
        self.dirties.remove(&key);
        self.buffer.remove(key);
        self.normals_buffer.remove(key);

        self.gpu_dirty = true;
    }

    // This is the only way to modify the matrices (since it must manage the dirty flags)
    // world transforms are updated by calling update()
    /// Sets the local transform for a key.
    pub fn set_local(&mut self, key: TransformKey, transform: Transform) -> Result<()> {
        if key == self.root_node {
            return Err(AwsmTransformError::CannotModifyRootNode);
        }
        match self.locals.get_mut(key) {
            Some(existing) => {
                *existing = transform;
                self.dirties.insert(key);
                Ok(())
            }
            None => Err(AwsmTransformError::LocalNotFound(key)),
        }
    }

    // if parent is None then the parent is the root node
    /// Sets the parent of a transform.
    pub fn set_parent(&mut self, child: TransformKey, parent: Option<TransformKey>) {
        if child == self.root_node {
            return;
        }

        let parent = parent.unwrap_or(self.root_node);

        if let Some(existing_parent) = self.parents.get(child) {
            if *existing_parent == parent {
                return;
            } else {
                self.unset_parent(child);
            }
        }

        // safe because all transforms have children vec when created
        self.children.get_mut(parent).unwrap().push(child);

        self.parents.insert(child, parent);
    }

    /// Returns the parent key for a transform.
    pub fn get_parent(&self, child: TransformKey) -> Result<TransformKey> {
        if child == self.root_node {
            return Err(AwsmTransformError::CannotGetParentOfRootNode);
        }

        self.parents
            .get(child)
            .copied()
            .ok_or(AwsmTransformError::CannotGetParent(child))
    }

    /// Returns the local transform for a key.
    pub fn get_local(&self, key: TransformKey) -> Result<&Transform> {
        self.locals
            .get(key)
            .ok_or(AwsmTransformError::LocalNotFound(key))
    }

    /// Returns the world matrix for a key.
    pub fn get_world(&self, key: TransformKey) -> Result<&glam::Mat4> {
        self.world_matrices
            .get(key)
            .ok_or(AwsmTransformError::WorldNotFound(key))
    }

    /// Returns the children of a transform.
    pub fn get_children(&self, key: TransformKey) -> Option<&[TransformKey]> {
        self.children.get(key).map(Vec::as_slice)
    }

    // This is the only way to update the world matrices
    // it does *not* write to the GPU, so it can be called relatively frequently for physics etc.
    pub(crate) fn update_world(&mut self) {
        self.gpu_dirty = self.gpu_dirty || !self.dirties.is_empty();

        self.update_inner_recursively(self.root_node, false);

        self.dirties.clear();
    }

    // This *does* write to the gpu, should be called only once per frame
    // just write the entire buffer in one fell swoop
    /// Writes dirty transform data to the GPU.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        if self.gpu_dirty {
            let _maybe_span_guard = if logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Transform GPU write").entered())
            } else {
                None
            };

            let mut transform_resized = false;
            if let Some(new_size) = self.buffer.take_gpu_needs_resize() {
                self.gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(Some("Transforms"), new_size, *BUFFER_USAGE).into(),
                )?;

                bind_groups.mark_create(BindGroupCreate::TransformsResize);
                transform_resized = true;
            }

            let mut normals_resized = false;
            if let Some(new_size) = self.normals_buffer.take_gpu_needs_resize() {
                self.normals_gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(
                        Some("Normal Transform Matrices"),
                        new_size,
                        *BUFFER_USAGE,
                    )
                    .into(),
                )?;

                bind_groups.mark_create(BindGroupCreate::TransformNormalsResize);
                normals_resized = true;
            }

            if transform_resized {
                self.buffer.clear_dirty_ranges();
                gpu.write_buffer(&self.gpu_buffer, None, self.buffer.raw_slice(), None, None)?;
            } else {
                let transform_ranges = self.buffer.take_dirty_ranges();
                write_buffer_with_dirty_ranges(
                    gpu,
                    &self.gpu_buffer,
                    self.buffer.raw_slice(),
                    transform_ranges,
                )?;
            }

            if normals_resized {
                self.normals_buffer.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.normals_gpu_buffer,
                    None,
                    self.normals_buffer.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let normal_ranges = self.normals_buffer.take_dirty_ranges();
                write_buffer_with_dirty_ranges(
                    gpu,
                    &self.normals_gpu_buffer,
                    self.normals_buffer.raw_slice(),
                    normal_ranges,
                )?;
            }

            self.gpu_dirty = false;
        }
        Ok(())
    }

    /// Takes and clears the list of dirty mesh transforms.
    pub fn take_dirty_meshes(&mut self) -> HashMap<TransformKey, Mat4> {
        self.dirty_meshes
            .drain(..)
            .map(|key| {
                // this for sure exists since we just drained the key
                let world_matrix = self.world_matrices.get(key).copied().unwrap();
                (key, world_matrix)
            })
            .collect()
    }

    /// Returns the GPU buffer offset for a transform.
    pub fn buffer_offset(&self, key: TransformKey) -> Result<usize> {
        self.buffer
            .offset(key)
            .ok_or(AwsmTransformError::TransformBufferSlotMissing(key))
    }

    /// Returns the GPU buffer offset for the normal matrix.
    pub fn normals_buffer_offset(&self, key: TransformKey) -> Result<usize> {
        self.normals_buffer
            .offset(key)
            .ok_or(AwsmTransformError::TransformBufferNormalsSlotMissing(key))
    }

    /// Returns a reference to world matrices.
    pub fn world_matrices_ref(&self) -> &SecondaryMap<TransformKey, glam::Mat4> {
        &self.world_matrices
    }

    // should only be used for debugging really
    /// Returns a tree of transforms for debugging.
    pub fn get_tree(&self) -> TransformTreeNode {
        fn build_node(transforms: &Transforms, key: TransformKey) -> TransformTreeNode {
            let children = transforms.children.get(key).unwrap();

            let child_nodes = children
                .iter()
                .map(|&child_key| build_node(transforms, child_key))
                .collect();

            TransformTreeNode {
                key,
                children: child_nodes,
            }
        }

        build_node(self, self.root_node)
    }

    // internal-only function
    // See: https://gameprogrammingpatterns.com/dirty-flag.html
    // the overall idea is we walk the tree and skip over nodes that are not dirty
    // whenever we encounter a dirty node, we must also mark all of its children dirty
    // finally, for each dirty node, its world transform is its parent's world transform
    // multiplied by its local transform
    // or in other words, it's the local transform, offset by its parent in world space
    //
    // we also update the CPU-side buffer as needed so it will be ready for the GPU
    fn update_inner_recursively(&mut self, key: TransformKey, dirty_tracker: bool) -> bool {
        let dirty = self.dirties.contains(&key) | dirty_tracker;

        if dirty {
            let local_matrix = self.locals[key].to_matrix();

            let world_matrix = match self.parents.get(key) {
                Some(parent) => {
                    let parent_matrix = self.world_matrices[*parent];
                    parent_matrix.mul_mat4(&local_matrix)
                }
                None => local_matrix,
            };

            self.world_matrices[key] = world_matrix;

            let values = world_matrix.to_cols_array();
            let values_u8 = unsafe {
                std::slice::from_raw_parts(values.as_ptr() as *const u8, Self::BYTE_SIZE)
            };
            self.buffer.update(key, values_u8);

            let normal_matrix = world_matrix.inverse().transpose();
            let normal_matrix = glam::Mat3::from_mat4(normal_matrix);
            let normal_values = normal_matrix.to_cols_array();
            let normal_values_u8 = unsafe {
                std::slice::from_raw_parts(
                    normal_values.as_ptr() as *const u8,
                    Self::NORMALS_BYTE_SIZE,
                )
            };

            self.normals_buffer.update(key, normal_values_u8);

            self.dirty_meshes.push(key);
        }

        // safety: can't keep a mutable reference to self while it has a borrow of the iterator
        // TODO: maybe split this function into a pure function that takes the deps?
        let children = self.children[key].clone();
        for child in children {
            self.update_inner_recursively(child, dirty);
        }

        dirty
    }

    // internal-only function - leaves the node dangling
    // after this call, the node should either be immediately removed or reparented
    fn unset_parent(&mut self, child: TransformKey) {
        if let Some(parent) = self.parents.remove(child) {
            if let Some(children) = self.children.get_mut(parent) {
                children.retain(|&c| c != child);
            }
        }
    }
}

/// Tree node for transform hierarchy debugging.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransformTreeNode {
    pub key: TransformKey,
    pub children: Vec<TransformTreeNode>,
}

/// Transform with translation, rotation, and scale.
#[derive(Clone, Debug)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// Sets translation.
    pub fn with_translation(mut self, translation: Vec3) -> Self {
        self.translation = translation;
        self
    }
    /// Sets rotation.
    pub fn with_rotation(mut self, rotation: Quat) -> Self {
        self.rotation = rotation;
        self
    }
    /// Sets scale.
    pub fn with_scale(mut self, scale: Vec3) -> Self {
        self.scale = scale;
        self
    }

    /// Converts the transform to a matrix.
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Returns the winding order implied by the transform.
    pub fn winding_order(&self) -> FrontFace {
        /*
        Staying consistent with gltf spec: "When a mesh primitive uses any triangle-based topology (i.e., triangles, triangle strip, or triangle fan),
        the determinant of the node’s global transform defines the winding order of that primitive.
        If the determinant is a positive value, the winding order triangle faces is counterclockwise;
        in the opposite case, the winding order is clockwise.
        */
        if self.to_matrix().determinant() > 0.0 {
            FrontFace::Ccw
        } else {
            FrontFace::Cw
        }
    }
}

impl From<&Mat4> for Transform {
    fn from(matrix: &Mat4) -> Self {
        let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
        Self {
            translation,
            rotation,
            scale,
        }
    }
}

impl From<Mat4> for Transform {
    fn from(matrix: Mat4) -> Self {
        Self::from(&matrix)
    }
}

impl From<&Transform> for Mat4 {
    fn from(transform: &Transform) -> Self {
        Mat4::from_scale_rotation_translation(
            transform.scale,
            transform.rotation,
            transform.translation,
        )
    }
}

impl From<Transform> for Mat4 {
    fn from(transform: Transform) -> Self {
        Mat4::from(&transform)
    }
}

new_key_type! {
    /// Opaque key for transforms.
    pub struct TransformKey;
}

/// Result type for transform operations.
pub type Result<T> = std::result::Result<T, AwsmTransformError>;

/// Transform-related errors.
#[derive(Error, Debug)]
pub enum AwsmTransformError {
    #[error("[transform] local transform does not exist {0:?}")]
    LocalNotFound(TransformKey),

    #[error("[transform] world transform does not exist {0:?}")]
    WorldNotFound(TransformKey),

    #[error("[transform] cannot modify root node")]
    CannotModifyRootNode,

    #[error("[transform] buffer slot missing {0:?}")]
    TransformBufferSlotMissing(TransformKey),

    #[error("[transform] normals buffer slot missing {0:?}")]
    TransformBufferNormalsSlotMissing(TransformKey),

    #[error("[transform] cannot get parent of root node")]
    CannotGetParentOfRootNode,

    #[error("[transform] cannot get parent for {0:?}")]
    CannotGetParent(TransformKey),

    #[error("[transform] {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[transform] {0:?}")]
    Skin(#[from] AwsmSkinError),
}
