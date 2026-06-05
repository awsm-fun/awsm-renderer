//! GPU picking support for mesh selection.

use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};

use crate::{
    bind_group_layout::{
        BindGroupLayoutCacheKey, BindGroupLayoutCacheKeyEntry, BindGroupLayoutKey, BindGroupLayouts,
    },
    bind_groups::BindGroupRecreateContext,
    error::Result,
    meshes::MeshKey,
    picker::state::{PickerState, OUTPUT_BYTE_SIZE},
    pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayouts},
    pipelines::{
        compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey},
        Pipelines,
    },
    shaders::{ShaderCacheKey, Shaders},
    AwsmRenderer,
};
use askama::Template;
use awsm_renderer_core::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutResource, BindGroupResource,
        BufferBindingLayout, BufferBindingType, TextureBindingLayout,
    },
    buffers::{extract_buffer_array, BufferBinding},
    renderer::AwsmRendererWebGpu,
    texture::{TextureSampleType, TextureViewDimension},
};
use slotmap::KeyData;

mod state;

/// Result of a GPU pick request.
#[derive(Debug, Clone)]
pub enum PickResult {
    /// Picker pipelines / bind groups are still being constructed.
    /// Callers can retry on a later frame.
    Initializing,
    /// The picked pixel resolved to a registered mesh.
    Hit(MeshKey),
    /// The picked pixel did not resolve to any registered mesh.
    Miss,
    /// A previous pick request hasn't completed yet. Callers should
    /// either await the previous result or skip this frame's pick.
    InFlight,
    /// The renderer was constructed without the
    /// [`crate::features::RendererFeatures::picking`] flag enabled.
    /// No picker subsystem exists; every call to
    /// [`crate::AwsmRenderer::pick`] returns this variant. Treat it
    /// as a permanent no-op for the session — picking can't be
    /// turned on without rebuilding the renderer.
    Disabled,
}

impl PickResult {
    /// Returns the hit mesh key if this is a hit result.
    pub fn mesh_key(&self) -> Option<MeshKey> {
        match self {
            PickResult::Hit(mesh_key) => Some(*mesh_key),
            _ => None,
        }
    }
}

impl AwsmRenderer {
    /// Lazily compiles the Picker subsystem on first use.
    ///
    /// Cold-boot (Block B.4) leaves `self.picker == None` even when
    /// `features.picking == true`. The first `pick()` call funnels
    /// through here, which builds the BGLs, compiles the active
    /// MSAA's compute pipeline, allocates the picker state buffers,
    /// AND creates the bind group inline (so the immediately-following
    /// `pick()` body can dispatch without waiting for a render frame
    /// to trigger the bind-group recreate dispatcher).
    ///
    /// No-op when `features.picking == false` (callers still get
    /// `PickResult::Disabled` from `pick()` itself) or when the
    /// picker is already compiled.
    /// Drop the compiled picker so the next [`Self::pick`] rebuilds it against
    /// the current render textures + HUD state. Use when a HUD model (e.g. an
    /// editor gizmo) is populated *after* the picker was first compiled (a warm
    /// prewarm pick) — otherwise the picker's id-buffer setup predates the HUD
    /// and those meshes never become pickable. Cheap: the pipelines stay cached,
    /// so the recompile only rebuilds the bind group.
    pub fn invalidate_picker(&mut self) {
        self.picker = None;
    }

    pub async fn ensure_picker_compiled(&mut self) -> Result<()> {
        if !self.features.picking || self.picker.is_some() {
            return Ok(());
        }

        // Build + compile + state alloc. Goes through `Picker::new`
        // (the un-pooled path) — there's no other subsystem to batch
        // with this far past `build()`, so the standalone
        // `ensure_keys` here is fine.
        let picker = Picker::new(
            &self.gpu,
            &mut self.bind_group_layouts,
            &mut self.pipeline_layouts,
            &mut self.shaders,
            &mut self.pipelines,
            &self.anti_aliasing,
        )
        .await?;
        self.picker = Some(picker);

        // Create the picker bind group inline so the caller's
        // `pick()` dispatch can run this frame. Build a one-shot
        // `BindGroupRecreateContext` and call `recreate_bind_group`
        // directly on the picker (rather than going through the
        // `BindGroupCreate::TextureViewRecreate` mark, which would
        // also re-create every other texture-view-dependent bind
        // group).
        // Picker compile is a one-shot ahead of the first `pick()` and
        // runs outside the render loop's `viewport_size` cache, so we
        // fetch the size here directly. Cheap (single wasm↔JS hop on a
        // user-driven path, not per-frame).
        let viewport_size = self.gpu.current_context_texture_size()?;
        let render_texture_views = self.render_textures.views(
            &self.gpu,
            self.anti_aliasing.clone(),
            viewport_size,
            self.materials.has_seen_transmission(),
            self.meshes.has_seen_hud(),
        )?;
        let ctx = crate::bind_groups::BindGroupRecreateContext {
            gpu: &self.gpu,
            render_texture_views: &render_texture_views,
            textures: &self.textures,
            materials: &self.materials,
            bind_group_layouts: &mut self.bind_group_layouts,
            meshes: &self.meshes,
            camera: &self.camera,
            frame_globals: &self.frame_globals,
            environment: &self.environment,
            lights: &self.lights,
            transforms: &self.transforms,
            instances: &self.instances,
            anti_aliasing: &self.anti_aliasing,
            shadows: &self.shadows,
            material_classify_buffers: &self.material_classify_buffers,
            light_culling_buffers: &self.light_culling_buffers,
            material_edge_buffers: self.material_edge_buffers.as_ref(),
            material_edge_layout_uniform: self.material_edge_layout_uniform.as_ref(),
            extras_pool: &self.extras_pool,
            decals: self.decals.as_ref(),
            occlusion_buffers: self.occlusion_buffers.as_ref(),
            hzb_full_view: self
                .render_passes
                .hzb
                .as_ref()
                .map(|hzb| hzb.texture.view_all.clone()),
            decal_classify_buffers: self.decal_classify_buffers.as_ref(),
            compaction_buffers: self.compaction_buffers.as_ref(),
            coverage_buffers: self.coverage_buffers.as_ref(),
            features: &self.features,
        };
        if let Some(p) = self.picker.as_mut() {
            p.recreate_bind_group(&ctx)?;
        }
        Ok(())
    }

    /// Performs a GPU pick at the given pixel coordinates.
    ///
    /// Returns [`PickResult::Disabled`] when the renderer was built
    /// without [`crate::features::RendererFeatures::picking`] — the
    /// picker subsystem doesn't exist in that case and there's no
    /// runtime cost to call this method.
    ///
    /// On first invocation (when `features.picking == true`), this
    /// lazily compiles the entire Picker subsystem via
    /// [`Self::ensure_picker_compiled`] — cold-boot now compiles 0
    /// picker pipelines, paying the cost only when the user
    /// actually clicks.
    pub async fn pick(&mut self, x: i32, y: i32) -> Result<PickResult> {
        if !self.features.picking {
            return Ok(PickResult::Disabled);
        }
        self.ensure_picker_compiled().await?;
        let Some(picker) = self.picker.as_ref() else {
            return Ok(PickResult::Disabled);
        };
        let pipeline_key_opt = if self.anti_aliasing.msaa_sample_count.is_some() {
            picker.multisampled_compute_pipeline_key
        } else {
            picker.singlesampled_compute_pipeline_key
        };

        let (bind_group, pipeline) = match (
            picker._bind_group.as_ref(),
            pipeline_key_opt.and_then(|k| self.pipelines.compute.get(k).ok()),
        ) {
            (Some(bg), Some(p)) => (bg, p),
            _ => {
                return Ok(PickResult::Initializing);
            }
        };

        // keep the lock scope before the await point
        let read_buffer = {
            let mut guard = picker.state.lock().unwrap();
            let state = &mut *guard;

            if state.in_flight {
                return Ok(PickResult::InFlight);
            }

            if let Err(err) = state.begin_pick(&self.gpu, bind_group, pipeline, x, y) {
                state.in_flight = false;
                return Err(err);
            }

            // meh, it's just a js value and now we don't need the lock anymore
            state.gpu_readback_buffer.clone()
        };

        let mut bytes = [0u8; OUTPUT_BYTE_SIZE];

        // don't error out right away, we need to set in_flight to false
        let res = extract_buffer_array(&read_buffer, &mut bytes).await;

        {
            picker.state.lock().unwrap().in_flight = false;
        }

        // now we can error out if needed
        #[allow(clippy::let_unit_value)]
        let _ = res?;

        // read validity
        if u32::from_le_bytes((&bytes[0..4]).try_into().unwrap()) == 0 {
            Ok(PickResult::Miss)
        } else {
            let hi = u32::from_le_bytes((&bytes[4..8]).try_into().unwrap()) as u64;
            let lo = u32::from_le_bytes((&bytes[8..12]).try_into().unwrap()) as u64;
            let mesh_key = (hi << 32) | lo;

            let mesh_key: MeshKey = KeyData::from_ffi(mesh_key).into();

            Ok(PickResult::Hit(mesh_key))
        }
    }
}

/// Picker state and GPU resources.
///
/// **Lazy-pool semantics:** cold-boot only compiles the pipeline
/// matching the live `AntiAliasing` config. The other variant is
/// compiled on demand when the user calls
/// [`crate::AwsmRenderer::set_anti_aliasing`]. Both fields are `Option`;
/// `pick()` returns `PickResult::Initializing` when the requested
/// MSAA's pipeline isn't compiled yet (same behavior as before
/// when bind groups weren't ready).
pub struct Picker {
    singlesampled_compute_pipeline_key: Option<ComputePipelineKey>,
    multisampled_compute_pipeline_key: Option<ComputePipelineKey>,
    singlesampled_bind_group_layout_key: BindGroupLayoutKey,
    multisampled_bind_group_layout_key: BindGroupLayoutKey,
    _bind_group: Option<web_sys::GpuBindGroup>,

    /// `Arc<Mutex<…>>` rather than `Rc<RefCell<…>>` for renderer-wide
    /// consistency — every shared interior-mutability slot in the
    /// renderer uses `Arc`/atomics/`Mutex` so the convention stays
    /// uniform regardless of whether a given container actually gets
    /// `Sync`. (`PickerState` owns `web_sys::GpuBuffer` handles, which
    /// are `!Send`, so the `Arc<Mutex<…>>` here doesn't *grant*
    /// thread mobility today; the inner types would have to become
    /// `Send` first.) Single-threaded for now; the lock is uncontested.
    state: Arc<Mutex<PickerState>>,
}

/// Picker layouts and pre-resolved descriptors. Returned by
/// [`Picker::build_descriptors`] and consumed by
/// [`Picker::from_resolved`]. The orchestrator in
/// `AwsmRendererBuilder::build` uses these to pool Picker's 2
/// compute pipeline compiles with every other pass's pipeline
/// compile into one cross-system `ComputePipelines::ensure_keys`.
pub struct PickerDescriptors {
    pub singlesampled_bind_group_layout_key: BindGroupLayoutKey,
    pub multisampled_bind_group_layout_key: BindGroupLayoutKey,
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    pub slot: Vec<PickerPipelineSlot>,
}

/// Slot identity carried alongside each compiled pipeline so
/// `merge_resolved` knows which MSAA slot to update.
#[derive(Clone, Copy, Debug)]
pub enum PickerPipelineSlot {
    Singlesampled,
    Multisampled,
}

impl Picker {
    /// Creates a picker with the required bind groups and pipelines.
    /// Thin wrapper over [`Self::build_descriptors`] +
    /// [`Self::from_resolved`]. The pooled startup path bypasses
    /// this and calls the two halves directly so Picker's compute
    /// pipeline compiles share a global `ComputePipelines::ensure_keys`
    /// with every other pass.
    pub async fn new(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        shaders: &mut Shaders,
        pipelines: &mut Pipelines,
        anti_aliasing: &crate::anti_alias::AntiAliasing,
    ) -> Result<Self> {
        let descs = Self::build_descriptors(
            gpu,
            bind_group_layouts,
            pipeline_layouts,
            shaders,
            anti_aliasing,
        )
        .await?;
        let pipeline_keys = pipelines
            .compute
            .ensure_keys(
                gpu,
                shaders,
                pipeline_layouts,
                descs.pipeline_cache_keys.clone(),
            )
            .await?;
        Self::from_resolved(gpu, descs, pipeline_keys)
    }

    /// Shader cache keys for the *live* AA config — emits only the
    /// matching variant. Both BGLs are always created (they're
    /// cheap and needed for the lazy recompile path) but the shader
    /// compile is scoped to the active MSAA.
    pub fn shader_cache_keys(
        anti_aliasing: &crate::anti_alias::AntiAliasing,
    ) -> Vec<ShaderCacheKey> {
        let multisampled = anti_aliasing.msaa_sample_count.is_some();
        vec![ShaderCacheKey::from(ShaderCacheKeyPicker {
            multisampled_geometry: multisampled,
        })]
    }

    /// Builds layouts + pipeline cache key for the live config.
    /// Requires that [`Self::shader_cache_keys`] has been
    /// `ensure_keys`'d. Both BGLs are created (the recompile path
    /// reuses them); only the matching pipeline cache key is
    /// emitted.
    pub async fn build_descriptors(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        shaders: &mut Shaders,
        anti_aliasing: &crate::anti_alias::AntiAliasing,
    ) -> Result<PickerDescriptors> {
        let singlesampled_bind_group_layout_key =
            create_bind_group_layout(gpu, bind_group_layouts, false)?;
        let multisampled_bind_group_layout_key =
            create_bind_group_layout(gpu, bind_group_layouts, true)?;

        let multisampled = anti_aliasing.msaa_sample_count.is_some();
        let (bgl_key, slot) = if multisampled {
            (
                multisampled_bind_group_layout_key,
                PickerPipelineSlot::Multisampled,
            )
        } else {
            (
                singlesampled_bind_group_layout_key,
                PickerPipelineSlot::Singlesampled,
            )
        };

        let pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bgl_key]),
        )?;

        let shader = shaders
            .get_key(
                gpu,
                ShaderCacheKeyPicker {
                    multisampled_geometry: multisampled,
                },
            )
            .await?;

        Ok(PickerDescriptors {
            singlesampled_bind_group_layout_key,
            multisampled_bind_group_layout_key,
            pipeline_cache_keys: vec![ComputePipelineCacheKey::new(shader, pipeline_layout_key)],
            slot: vec![slot],
        })
    }

    /// Folds resolved pipeline keys back into the typed `Picker`.
    /// Sync; the caller has already run the batched
    /// `ComputePipelines::ensure_keys`. The non-active MSAA's
    /// pipeline stays `None` until [`crate::AwsmRenderer::set_anti_aliasing`]
    /// triggers its compile.
    pub fn from_resolved(
        gpu: &AwsmRendererWebGpu,
        descs: PickerDescriptors,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) -> Result<Self> {
        let mut me = Self {
            singlesampled_compute_pipeline_key: None,
            multisampled_compute_pipeline_key: None,
            singlesampled_bind_group_layout_key: descs.singlesampled_bind_group_layout_key,
            multisampled_bind_group_layout_key: descs.multisampled_bind_group_layout_key,
            state: Arc::new(Mutex::new(PickerState::new(gpu)?)),
            _bind_group: None,
        };
        me.merge_resolved(descs.slot, pipeline_keys);
        Ok(me)
    }

    /// Merge a fresh batch of resolved pipelines into `self` without
    /// dropping the previously-compiled variant. Used by
    /// [`crate::AwsmRenderer::set_anti_aliasing`].
    pub fn merge_resolved(
        &mut self,
        slot: Vec<PickerPipelineSlot>,
        pipeline_keys: Vec<ComputePipelineKey>,
    ) {
        for (s, key) in slot.into_iter().zip(pipeline_keys) {
            match s {
                PickerPipelineSlot::Singlesampled => {
                    self.singlesampled_compute_pipeline_key = Some(key);
                }
                PickerPipelineSlot::Multisampled => {
                    self.multisampled_compute_pipeline_key = Some(key);
                }
            }
        }
    }

    /// Rebuilds the bind group for the current render textures.
    pub fn recreate_bind_group(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let state = self.state.lock().unwrap();

        let mut entries = Vec::new();

        // Visibility data texture
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::TextureView(Cow::Borrowed(
                &ctx.render_texture_views.visibility_data,
            )),
        ));

        // Mesh Meta
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(ctx.meshes.meta.material_gpu_buffer())),
        ));

        // Pick input
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&state.gpu_input_buffer)),
        ));

        // Pick output
        entries.push(BindGroupEntry::new(
            entries.len() as u32,
            BindGroupResource::Buffer(BufferBinding::new(&state.gpu_output_buffer)),
        ));

        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(if ctx.anti_aliasing.msaa_sample_count.is_some() {
                    self.multisampled_bind_group_layout_key
                } else {
                    self.singlesampled_bind_group_layout_key
                })?,
            Some("Picker"),
            entries,
        );

        self._bind_group = Some(ctx.gpu.create_bind_group(&descriptor.into()));

        Ok(())
    }
}

fn create_bind_group_layout(
    gpu: &AwsmRendererWebGpu,
    bind_group_layouts: &mut BindGroupLayouts,
    multisampled_geometry: bool,
) -> Result<BindGroupLayoutKey> {
    let entries = vec![
        // Binding 0: Visibility data texture
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Texture(
                TextureBindingLayout::new()
                    .with_view_dimension(TextureViewDimension::N2d)
                    .with_sample_type(TextureSampleType::Uint)
                    .with_multisampled(multisampled_geometry),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Binding 1: Mesh Meta
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::ReadOnlyStorage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Binding 2: Pick input
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Uniform),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
        // Binding 3: Pick output
        BindGroupLayoutCacheKeyEntry {
            resource: BindGroupLayoutResource::Buffer(
                BufferBindingLayout::new().with_binding_type(BufferBindingType::Storage),
            ),
            visibility_vertex: false,
            visibility_fragment: false,
            visibility_compute: true,
        },
    ];

    Ok(bind_group_layouts.get_key(gpu, BindGroupLayoutCacheKey { entries })?)
}

/// Shader cache key for the picker compute shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyPicker {
    pub multisampled_geometry: bool,
}

impl From<ShaderCacheKeyPicker> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyPicker) -> Self {
        ShaderCacheKey::Picker(key)
    }
}

/// Shader template for the picker compute shader.
#[derive(Template, Debug)]
#[template(path = "picker_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplatePicker {
    pub multisampled_geometry: bool,
}

impl ShaderTemplatePicker {
    /// Returns an optional debug label for shader compilation.
    /// Kept in release builds (see `ShaderTemplate::into_descriptor`
    /// for the cost rationale).
    pub fn debug_label(&self) -> Option<&str> {
        Some("Picker")
    }

    /// Renders the template into WGSL source.
    pub fn into_source(self) -> crate::shaders::Result<String> {
        Ok(self.render()?)
    }
}

impl From<&ShaderCacheKeyPicker> for ShaderTemplatePicker {
    fn from(key: &ShaderCacheKeyPicker) -> Self {
        ShaderTemplatePicker {
            multisampled_geometry: key.multisampled_geometry,
        }
    }
}
