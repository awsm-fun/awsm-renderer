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
    Initializing,
    Hit(MeshKey),
    Miss,
    InFlight,
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
    /// Performs a GPU pick at the given pixel coordinates.
    pub async fn pick(&self, x: i32, y: i32) -> Result<PickResult> {
        let pipeline_key = if self.anti_aliasing.msaa_sample_count.is_some() {
            self.picker.multisampled_compute_pipeline_key
        } else {
            self.picker.singlesampled_compute_pipeline_key
        };

        let (bind_group, pipeline) = match (
            self.picker._bind_group.as_ref(),
            self.pipelines.compute.get(pipeline_key),
        ) {
            (Some(bg), Ok(p)) => (bg, p),
            _ => {
                return Ok(PickResult::Initializing);
            }
        };

        // keep the lock scope before the await point
        let read_buffer = {
            let mut guard = self.picker.state.lock().unwrap();
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
            self.picker.state.lock().unwrap().in_flight = false;
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
pub struct Picker {
    singlesampled_compute_pipeline_key: ComputePipelineKey,
    multisampled_compute_pipeline_key: ComputePipelineKey,
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

impl Picker {
    /// Creates a picker with the required bind groups and pipelines.
    pub async fn new(
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &mut BindGroupLayouts,
        pipeline_layouts: &mut PipelineLayouts,
        shaders: &mut Shaders,
        pipelines: &mut Pipelines,
    ) -> Result<Self> {
        let singlesampled_bind_group_layout_key =
            create_bind_group_layout(gpu, bind_group_layouts, false)?;
        let multisampled_bind_group_layout_key =
            create_bind_group_layout(gpu, bind_group_layouts, true)?;

        let singlesampled_pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![singlesampled_bind_group_layout_key]),
        )?;

        let multisampled_pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![multisampled_bind_group_layout_key]),
        )?;

        // Both shader variants and both pipelines batched in two
        // ensure_keys calls so Dawn parallelises the compiles
        // instead of serialising the singlesampled compile against
        // the multisampled one.
        shaders
            .ensure_keys(
                gpu,
                [
                    ShaderCacheKey::from(ShaderCacheKeyPicker {
                        multisampled_geometry: false,
                    }),
                    ShaderCacheKey::from(ShaderCacheKeyPicker {
                        multisampled_geometry: true,
                    }),
                ],
            )
            .await?;
        let singlesampled_shader = shaders
            .get_key(
                gpu,
                ShaderCacheKeyPicker {
                    multisampled_geometry: false,
                },
            )
            .await?;
        let multisampled_shader = shaders
            .get_key(
                gpu,
                ShaderCacheKeyPicker {
                    multisampled_geometry: true,
                },
            )
            .await?;
        let pipeline_keys = pipelines
            .compute
            .ensure_keys(
                gpu,
                shaders,
                pipeline_layouts,
                [
                    ComputePipelineCacheKey::new(
                        singlesampled_shader,
                        singlesampled_pipeline_layout_key,
                    ),
                    ComputePipelineCacheKey::new(
                        multisampled_shader,
                        multisampled_pipeline_layout_key,
                    ),
                ],
            )
            .await?;
        let singlesampled_compute_pipeline_key = pipeline_keys[0];
        let multisampled_compute_pipeline_key = pipeline_keys[1];

        Ok(Self {
            singlesampled_compute_pipeline_key,
            multisampled_compute_pipeline_key,
            singlesampled_bind_group_layout_key,
            multisampled_bind_group_layout_key,
            state: Arc::new(Mutex::new(PickerState::new(gpu)?)),
            _bind_group: None,
        })
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
    #[cfg(debug_assertions)]
    /// Returns an optional debug label for shader compilation.
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
