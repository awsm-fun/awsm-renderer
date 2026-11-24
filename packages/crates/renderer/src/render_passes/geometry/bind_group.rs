//! Geometry pass bind group setup.

use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
        BufferBindingLayout, BufferBindingType,
    },
    buffers::BufferBinding,
};

use crate::error::Result;
use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey,
    },
    bind_groups::{AwsmBindGroupError, BindGroupRecreateContext},
    meshes::meta::geometry_meta::GEOMETRY_MESH_META_BYTE_ALIGNMENT,
    render_passes::RenderPassInitContext,
};

/// Bind groups used by the geometry pass.
pub struct GeometryBindGroups {
    pub camera: GeometryBindGroupCamera,
    // these could be be used for multiple meshes
    pub transforms: GeometryBindGroupTransforms,
    // These are more specific to the mesh
    pub meta: GeometryBindGroupMeta,
    pub animation: GeometryBindGroupAnimation,
}

impl GeometryBindGroups {
    /// The group-1 (transforms) and group-3 (animation) layouts for the
    /// given targets. The TAA variant's group 1 adds the shared
    /// previous-frame buffer (`buffer::history`); its layout is created on
    /// first use, so a renderer that never turns TAA on never creates it.
    pub fn layout_keys(
        &self,
        ctx: &mut RenderPassInitContext<'_>,
        targets: super::pipeline::GeometryTargets,
    ) -> Result<(BindGroupLayoutKey, BindGroupLayoutKey)> {
        if !targets.velocity() {
            return Ok((
                self.transforms.bind_group_layout_key,
                self.animation.bind_group_layout_key,
            ));
        }
        // Group 3 is the same: the previous joint matrices and morph weights
        // live in the shared previous-frame buffer bound in group 1.
        Ok((
            self.transforms.velocity_layout_key(ctx)?,
            self.animation.bind_group_layout_key,
        ))
    }

    /// Creates all geometry bind group layouts.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let camera = GeometryBindGroupCamera::new(ctx).await?;
        let transforms = GeometryBindGroupTransforms::new(ctx).await?;
        let meta = GeometryBindGroupMeta::new(ctx).await?;
        let animation = GeometryBindGroupAnimation::new(ctx).await?;

        Ok(Self {
            camera,
            transforms,
            meta,
            animation,
        })
    }
}

/// Bind group for camera data in the geometry pass.
pub struct GeometryBindGroupCamera {
    pub bind_group_layout_key: BindGroupLayoutKey,
    // this is set via `recreate` mechanism
    _bind_group: Option<web_sys::GpuBindGroup>,
    /// TAA with a HUD: the same group over the unjittered camera twin
    /// (`CameraBuffer::hud_gpu_buffer`), bound by the HUD geometry pass.
    _hud_bind_group: Option<web_sys::GpuBindGroup>,
}

impl GeometryBindGroupCamera {
    /// Creates the camera bind group layout.
    ///
    /// The "camera" group also carries `frame_globals_raw` (renderer-wide
    /// per-frame state) at binding 1 — their lifetimes are identical,
    /// so co-locating them saves a bind-group switch per pass.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_group_layout_cache_key = BindGroupLayoutCacheKey {
            entries: vec![
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
                // FrameGlobals uniform.
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
            ],
        };

        let bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, bind_group_layout_cache_key)?;

        Ok(Self {
            bind_group_layout_key,
            _bind_group: None,
            _hud_bind_group: None,
        })
    }

    /// Recreates the camera bind group.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.bind_group_layout_key)?,
            Some("Geometry Camera"),
            vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::Buffer(BufferBinding::new(&ctx.frame_globals.gpu_buffer)),
                ),
            ],
        );

        let bind_group = ctx.gpu.create_bind_group(&descriptor.into());
        self._bind_group = Some(bind_group);

        self._hud_bind_group = match ctx.camera.hud_gpu_buffer.as_ref() {
            Some(hud_camera) => {
                let descriptor = BindGroupDescriptor::new(
                    ctx.bind_group_layouts.get(self.bind_group_layout_key)?,
                    Some("Geometry Camera (HUD)"),
                    vec![
                        BindGroupEntry::new(
                            0,
                            BindGroupResource::Buffer(BufferBinding::new(hud_camera)),
                        ),
                        BindGroupEntry::new(
                            1,
                            BindGroupResource::Buffer(BufferBinding::new(
                                &ctx.frame_globals.gpu_buffer,
                            )),
                        ),
                    ],
                );
                Some(ctx.gpu.create_bind_group(&descriptor.into()))
            }
            None => None,
        };

        Ok(())
    }

    /// Returns the active camera bind group.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self._bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Geometry camera".to_string()))
    }

    /// The camera group for a pass: the unjittered HUD twin for the HUD pass
    /// when it exists (TAA with a HUD), else the main one.
    pub fn get_bind_group_for(
        &self,
        hud: bool,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        match self._hud_bind_group.as_ref() {
            Some(group) if hud => Ok(group),
            _ => self.get_bind_group(),
        }
    }
}

/// One read-only storage entry. `fragment`: also visible to the fragment
/// stage (the shape of the existing geometry-pass buffer bindings); the TAA
/// previous-frame buffer is vertex-only, so it does not count against the
/// fragment stage's storage-buffer limit.
fn storage_entry(fragment: bool) -> BindGroupLayoutCacheKeyEntry {
    BindGroupLayoutCacheKeyEntry {
        resource: BindGroupLayoutResource::Buffer(
            BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
        ),
        visibility_vertex: true,
        visibility_fragment: fragment,
        visibility_compute: false,
    }
}

/// Bind group for transform buffers in the geometry pass.
#[derive(Default)]
pub struct GeometryBindGroupTransforms {
    pub bind_group_layout_key: BindGroupLayoutKey,
    // this is set via `recreate` mechanism
    _bind_group: Option<web_sys::GpuBindGroup>,
    /// TAA variant layout: binding 1 = the shared previous-frame buffer
    /// (transforms, joint matrices, morph weights). `None` until the TAA
    /// geometry branch is first built.
    velocity_layout_key: std::cell::Cell<Option<BindGroupLayoutKey>>,
    /// TAA variant bind group (only while the previous-frame buffer exists).
    _velocity_bind_group: Option<web_sys::GpuBindGroup>,
}

impl GeometryBindGroupTransforms {
    /// Creates the transforms bind group layout.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_group_layout_cache_key = BindGroupLayoutCacheKey {
            entries: vec![
                // Transform
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::ReadOnlyStorage),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
            ],
        };

        let bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, bind_group_layout_cache_key)?;

        Ok(Self {
            bind_group_layout_key,
            _bind_group: None,
            velocity_layout_key: std::cell::Cell::new(None),
            _velocity_bind_group: None,
        })
    }

    /// The TAA variant's layout (current + previous transforms), created on
    /// first use.
    pub fn velocity_layout_key(
        &self,
        ctx: &mut RenderPassInitContext<'_>,
    ) -> Result<BindGroupLayoutKey> {
        if let Some(key) = self.velocity_layout_key.get() {
            return Ok(key);
        }
        let key = ctx.bind_group_layouts.get_key(
            ctx.gpu,
            BindGroupLayoutCacheKey {
                // Binding 0 as in the plain layout; binding 1 (previous-frame
                // state) is read by the vertex stage only.
                entries: vec![storage_entry(true), storage_entry(false)],
            },
        )?;
        self.velocity_layout_key.set(Some(key));
        Ok(key)
    }

    /// Recreates the transforms bind group (and, while TAA's previous-frame
    /// buffer exists, the TAA variant's).
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.bind_group_layout_key)?,
            Some("Geometry Transforms"),
            vec![BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&ctx.transforms.gpu_buffer)),
            )],
        );

        let bind_group = ctx.gpu.create_bind_group(&descriptor.into());
        self._bind_group = Some(bind_group);

        self._velocity_bind_group = match (
            self.velocity_layout_key.get(),
            ctx.transforms.prev_gpu_buffer(),
        ) {
            (Some(layout), Some(prev)) => {
                let descriptor = BindGroupDescriptor::new(
                    ctx.bind_group_layouts.get(layout)?,
                    Some("Geometry Transforms (TAA)"),
                    vec![
                        BindGroupEntry::new(
                            0,
                            BindGroupResource::Buffer(BufferBinding::new(
                                &ctx.transforms.gpu_buffer,
                            )),
                        ),
                        BindGroupEntry::new(1, BindGroupResource::Buffer(BufferBinding::new(prev))),
                    ],
                );
                Some(ctx.gpu.create_bind_group(&descriptor.into()))
            }
            _ => None,
        };

        Ok(())
    }

    /// Returns the active transforms bind group.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self._bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Geometry transform".to_string()))
    }

    /// The bind group for `velocity` (TAA) or plain pipelines.
    pub fn get_bind_group_for(
        &self,
        velocity: bool,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        if !velocity {
            return self.get_bind_group();
        }
        self._velocity_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Geometry transform (TAA)".to_string()))
    }
}

/// Bind group for mesh metadata in the geometry pass.
///
/// The layout forks on the instancing flag. The
/// non-instanced shader variant binds a storage-array of
/// `GeometryMeshMeta` and indexes it by `@builtin(instance_index)`
/// (enabling `drawIndirect` under `features.gpu_culling`), while
/// the instanced variant keeps the legacy uniform-with-dynamic-
/// offset binding (its instance_index ranges would otherwise
/// collide with neighbouring meshes' meta slots).
/// Group 2 of a geometry-family draw: the mesh meta as a storage array
/// (non-instanced under `indirect-first-instance`) or as a uniform with a
/// dynamic offset. The geometry pass's [`GeometryBindGroupMeta`]; the TAA
/// velocity overlay's group 2 holds the same meta binding plus its own.
pub trait GeometryMetaGroups {
    fn storage_meta_group(&self)
        -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError>;
    fn uniform_meta_group(&self)
        -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError>;
}

impl GeometryMetaGroups for GeometryBindGroupMeta {
    fn storage_meta_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.get_storage_bind_group()
    }
    fn uniform_meta_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self.get_uniform_bind_group()
    }
}

#[derive(Default)]
pub struct GeometryBindGroupMeta {
    /// Storage-array layout used by the non-instanced pipeline
    /// variant. `@group(2) @binding(0)` is a `read` storage buffer
    /// covering the whole geometry-meta buffer; the shader indexes
    /// it by `instance_index`.
    pub storage_layout_key: BindGroupLayoutKey,
    /// Legacy uniform-with-dynamic-offset layout used by the
    /// instanced pipeline variant. `@group(2) @binding(0)` is a
    /// `uniform` of one `GeometryMeshMeta` with the dynamic offset
    /// pointing at the active mesh's slot.
    pub uniform_layout_key: BindGroupLayoutKey,
    _storage_bind_group: Option<web_sys::GpuBindGroup>,
    _uniform_bind_group: Option<web_sys::GpuBindGroup>,
}

impl GeometryBindGroupMeta {
    /// Creates the metadata bind group layouts (storage + uniform).
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let storage_layout_cache_key = BindGroupLayoutCacheKey {
            entries: vec![BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::ReadOnlyStorage),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            }],
        };
        let uniform_layout_cache_key = BindGroupLayoutCacheKey {
            entries: vec![BindGroupLayoutCacheKeyEntry {
                resource: BindGroupLayoutResource::Buffer(
                    BufferBindingLayout::new()
                        .with_binding_type(BufferBindingType::Uniform)
                        .with_dynamic_offset(true),
                ),
                visibility_vertex: true,
                visibility_fragment: true,
                visibility_compute: false,
            }],
        };

        let storage_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, storage_layout_cache_key)?;
        let uniform_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, uniform_layout_cache_key)?;

        Ok(Self {
            storage_layout_key,
            uniform_layout_key,
            _storage_bind_group: None,
            _uniform_bind_group: None,
        })
    }

    /// Recreates both metadata bind groups (storage-array + legacy
    /// uniform-with-dynamic-offset) against the live geometry-meta
    /// buffer.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        // Storage view — whole buffer, no offset, no size override.
        let storage_descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.storage_layout_key)?,
            Some("Geometry meta (storage array)"),
            vec![BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(
                    ctx.meshes.meta.geometry_gpu_buffer(),
                )),
            )],
        );
        self._storage_bind_group = Some(ctx.gpu.create_bind_group(&storage_descriptor.into()));

        // Legacy uniform view — single-slot binding, dynamic offset
        // is set per-draw by `push_geometry_pass_commands`.
        let uniform_descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.uniform_layout_key)?,
            Some("Geometry meta (uniform + dyn offset)"),
            vec![BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(
                    BufferBinding::new(ctx.meshes.meta.geometry_gpu_buffer())
                        .with_size(GEOMETRY_MESH_META_BYTE_ALIGNMENT),
                ),
            )],
        );
        self._uniform_bind_group = Some(ctx.gpu.create_bind_group(&uniform_descriptor.into()));

        Ok(())
    }

    /// Returns the storage-array bind group (non-instanced path).
    pub fn get_storage_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self._storage_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Geometry meta (storage)".to_string()))
    }

    /// Returns the uniform-with-dynamic-offset bind group (instanced
    /// path).
    pub fn get_uniform_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self._uniform_bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Geometry meta (uniform)".to_string()))
    }
}

/// Bind group for morph and skin buffers in the geometry pass.
#[derive(Default)]
pub struct GeometryBindGroupAnimation {
    pub bind_group_layout_key: BindGroupLayoutKey,
    // this is set via `recreate` mechanism
    _bind_group: Option<web_sys::GpuBindGroup>,
}

impl GeometryBindGroupAnimation {
    /// Creates the animation bind group layout.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_group_layout_cache_key = BindGroupLayoutCacheKey {
            entries: vec![
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::ReadOnlyStorage),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::ReadOnlyStorage),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::ReadOnlyStorage),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
                BindGroupLayoutCacheKeyEntry {
                    resource: BindGroupLayoutResource::Buffer(
                        BufferBindingLayout::new()
                            .with_binding_type(BufferBindingType::ReadOnlyStorage),
                    ),
                    visibility_vertex: true,
                    visibility_fragment: true,
                    visibility_compute: false,
                },
            ],
        };

        let bind_group_layout_key = ctx
            .bind_group_layouts
            .get_key(ctx.gpu, bind_group_layout_cache_key)?;

        Ok(Self {
            bind_group_layout_key,
            _bind_group: None,
        })
    }

    /// Recreates the animation bind group.
    pub fn recreate(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(self.bind_group_layout_key)?,
            Some("Geometry animation"),
            vec![
                BindGroupEntry::new(
                    0,
                    BindGroupResource::Buffer(BufferBinding::new(
                        &ctx.meshes.morphs.geometry.gpu_buffer_weights,
                    )),
                ),
                BindGroupEntry::new(
                    1,
                    BindGroupResource::Buffer(BufferBinding::new(
                        &ctx.meshes.morphs.geometry.gpu_buffer_values,
                    )),
                ),
                BindGroupEntry::new(
                    2,
                    BindGroupResource::Buffer(BufferBinding::new(
                        &ctx.meshes.skins.matrices_gpu_buffer,
                    )),
                ),
                BindGroupEntry::new(
                    3,
                    BindGroupResource::Buffer(BufferBinding::new(
                        &ctx.meshes.skins.joint_index_weights_gpu_buffer,
                    )),
                ),
            ],
        );

        let bind_group = ctx.gpu.create_bind_group(&descriptor.into());
        self._bind_group = Some(bind_group);

        Ok(())
    }

    /// Returns the active animation bind group.
    pub fn get_bind_group(
        &self,
    ) -> std::result::Result<&web_sys::GpuBindGroup, AwsmBindGroupError> {
        self._bind_group
            .as_ref()
            .ok_or_else(|| AwsmBindGroupError::NotFound("Geometry skin".to_string()))
    }
}
