//! Mipmap generation utilities.

use std::{borrow::Cow, cell::RefCell, collections::HashMap};

use crate::{
    bind_groups::{
        BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
        BindGroupLayoutResource, BindGroupResource, StorageTextureAccess,
        StorageTextureBindingLayout, TextureBindingLayout,
    },
    command::compute_pass::ComputePassDescriptor,
    error::{AwsmCoreError, Result},
    pipeline::{
        layout::{PipelineLayoutDescriptor, PipelineLayoutKind},
        ComputePipelineDescriptor, ProgrammableStage,
    },
    renderer::AwsmRendererWebGpu,
    shaders::{ShaderModuleDescriptor, ShaderModuleExt},
    texture::{
        texture_format_to_wgsl_storage, TextureFormat, TextureFormatKey, TextureSampleType,
        TextureViewDescriptor, TextureViewDimension,
    },
};

/// Semantic texture type for mipmap filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[repr(u32)]
pub enum MipmapTextureKind {
    /// Base color texture.
    Albedo = 0,
    /// Normal map.
    Normal = 1,
    /// Metallic-roughness texture.
    MetallicRoughness = 2,
    /// Occlusion texture.
    Occlusion = 3,
    /// Emissive texture.
    Emissive = 4,
    /// Specular texture.
    Specular = 5,
    /// Specular color texture.
    SpecularColor = 6,
    /// Transmission texture.
    Transmission = 7,
    /// Volume thickness texture.
    VolumeThickness = 8,
}
/// Calculate the number of mip levels needed for a texture of the given dimensions.
///
/// Returns the full mipmap chain length, where level 0 is the full resolution and
/// the last level is 1x1.
///
/// # Example
/// ```
/// use awsm_renderer_core::texture::mipmap::calculate_mipmap_levels;
///
/// assert_eq!(calculate_mipmap_levels(512, 512), 10); // 512 → 256 → ... → 1
/// assert_eq!(calculate_mipmap_levels(256, 128), 9);  // Based on max dimension
/// ```
pub fn calculate_mipmap_levels(width: u32, height: u32) -> u32 {
    ((width.max(height) as f32).log2().floor() as u32) + 1
}

/// Get the dimensions of a mipmap level.
///
/// # Arguments
/// * `base_width` - Width at mip level 0
/// * `base_height` - Height at mip level 0
/// * `mip_level` - Mip level to query (0 = full resolution)
///
/// # Returns
/// (width, height) at the specified mip level, with a minimum of 1x1
pub fn get_mipmap_size_for_level(base_width: u32, base_height: u32, mip_level: u32) -> (u32, u32) {
    let width = (base_width >> mip_level).max(1);
    let height = (base_height >> mip_level).max(1);
    (width, height)
}

#[derive(Debug, Clone)]
struct MipmapPipeline {
    pub compute_pipeline: web_sys::GpuComputePipeline,
    pub bind_group_layout: web_sys::GpuBindGroupLayout,
}

thread_local! {
    static MIPMAP_PIPELINE: RefCell<HashMap<LookupKey, MipmapPipeline>> = RefCell::new(HashMap::new());
}

#[derive(Hash, Debug, Eq, PartialEq)]
struct LookupKey {
    pub texture_format: TextureFormatKey,
    pub is_array: bool,
}

// Mipmap generation with texture-type-specific filtering
// Supports both regular textures and texture arrays
fn shader_source(format: TextureFormat, is_array: bool) -> Result<String> {
    let storage_format = texture_format_to_wgsl_storage(format)?;

    let texture_type = if is_array {
        "texture_2d_array<f32>"
    } else {
        "texture_2d<f32>"
    };

    let storage_type = if is_array {
        format!("texture_storage_2d_array<{storage_format}, write>")
    } else {
        format!("texture_storage_2d<{storage_format}, write>")
    };

    let layer_param = if is_array { ", layer: i32" } else { "" };
    let layer_arg = if is_array { ", params.layer" } else { "" };

    Ok(format!(
        r#"
            @group(0) @binding(0) var src: {texture_type};
            @group(0) @binding(1) var dst: {storage_type};

            const TEXTURE_TYPE_ALBEDO: u32 = 0u;
            const TEXTURE_TYPE_NORMAL: u32 = 1u;
            const TEXTURE_TYPE_METALLIC_ROUGHNESS: u32 = 2u;
            const TEXTURE_TYPE_OCCLUSION: u32 = 3u;
            const TEXTURE_TYPE_EMISSIVE: u32 = 4u;

            struct Params {{
                textureType: u32,       // 0=Albedo, 1=Normal, 2=MetallicRoughness, 3=Occlusion, 4=Emissive
                layer: i32,             // Only used if array texture
                dstWidth: u32,          // Destination mip level width
                dstHeight: u32          // Destination mip level height
            }};

            @group(0) @binding(2) var<uniform> params: Params;

            fn load_texel(coord: vec2<i32>{layer_param}) -> vec4<f32> {{
                return textureLoad(src, coord{layer_arg}, 0);
            }}

            fn store_texel(coord: vec2<i32>{layer_param}, color: vec4<f32>) {{
                textureStore(dst, coord{layer_arg}, color);
            }}

            // Simple box filter - average all 4 samples (used for albedo, occlusion, emissive)
            fn filter_simple(samples: array<vec4<f32>, 4>) -> vec4<f32> {{
                var sum = vec4<f32>(0.0);
                for (var i = 0; i < 4; i++) {{
                    sum += samples[i];
                }}
                return sum * 0.25;
            }}

            // Normal map filtering: average then renormalize
            fn filter_normal(samples: array<vec4<f32>, 4>) -> vec4<f32> {{
                let avg = filter_simple(samples);

                // Renormalize the normal vector (stored in RGB)
                let normal_xyz = avg.xyz * 2.0 - 1.0;  // Convert from [0,1] to [-1,1]
                let normalized = normalize(normal_xyz);
                let renormalized = normalized * 0.5 + 0.5;  // Convert back to [0,1]

                return vec4<f32>(renormalized, avg.a);
            }}

            // Metallic/Roughness: roughness in G channel needs perceptual averaging (r²)
            fn filter_metallic_roughness(samples: array<vec4<f32>, 4>) -> vec4<f32> {{
                var metallic_sum = 0.0;
                var roughness_squared_sum = 0.0;
                var b_sum = 0.0;
                var alpha_sum = 0.0;

                for (var i = 0; i < 4; i++) {{
                    metallic_sum += samples[i].r;
                    let roughness = samples[i].g;
                    roughness_squared_sum += roughness * roughness;  // Perceptual r² averaging
                    b_sum += samples[i].b;
                    alpha_sum += samples[i].a;
                }}

                return vec4<f32>(
                    metallic_sum * 0.25,
                    sqrt(roughness_squared_sum * 0.25),
                    b_sum * 0.25,
                    alpha_sum * 0.25
                );
            }}

            @compute @workgroup_size(8, 8)
            fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
                // Get destination coordinate
                let dst_coord = vec2<i32>(gid.xy);

                // Bounds check for destination
                if (dst_coord.x >= i32(params.dstWidth) || dst_coord.y >= i32(params.dstHeight)) {{
                    return;
                }}

                // Calculate source coordinates (2x2 region in previous mip level)
                let src_base = dst_coord * 2;

                // Source dimensions are 2x destination dimensions
                let src_width = i32(params.dstWidth * 2u);
                let src_height = i32(params.dstHeight * 2u);

                // Sample 4 texels with bounds clamping for edge cases
                let s00 = clamp(src_base + vec2<i32>(0, 0), vec2<i32>(0), vec2<i32>(src_width - 1, src_height - 1));
                let s10 = clamp(src_base + vec2<i32>(1, 0), vec2<i32>(0), vec2<i32>(src_width - 1, src_height - 1));
                let s01 = clamp(src_base + vec2<i32>(0, 1), vec2<i32>(0), vec2<i32>(src_width - 1, src_height - 1));
                let s11 = clamp(src_base + vec2<i32>(1, 1), vec2<i32>(0), vec2<i32>(src_width - 1, src_height - 1));

                // Load all 4 samples
                var samples: array<vec4<f32>, 4>;
                samples[0] = load_texel(s00{layer_arg});
                samples[1] = load_texel(s10{layer_arg});
                samples[2] = load_texel(s01{layer_arg});
                samples[3] = load_texel(s11{layer_arg});

                // Apply appropriate filtering based on texture type
                var result: vec4<f32>;
                switch (params.textureType) {{
                    case TEXTURE_TYPE_NORMAL: {{
                        result = filter_normal(samples);
                    }}
                    case TEXTURE_TYPE_METALLIC_ROUGHNESS: {{
                        result = filter_metallic_roughness(samples);
                    }}
                    default: {{  // TEXTURE_TYPE_ALBEDO, OCCLUSION, EMISSIVE
                        result = filter_simple(samples);
                    }}
                }}

                store_texel(dst_coord{layer_arg}, result);
            }}
            "#
    ))
}

pub async fn generate_mipmaps(
    gpu: &AwsmRendererWebGpu,
    texture: &web_sys::GpuTexture,
    // there MUST be an entry for each layer if array texture
    texture_kinds_per_layer: &[MipmapTextureKind],
    mip_levels: u32,
) -> Result<()> {
    use crate::buffers::{BufferDescriptor, BufferUsage};

    if mip_levels < 2 {
        return Ok(());
    }

    let is_array = texture_kinds_per_layer.len() > 1;
    let array_layer_count = if is_array {
        Some(texture_kinds_per_layer.len() as u32)
    } else {
        None
    };

    // Get pipeline
    let MipmapPipeline {
        compute_pipeline: mipmap_pipeline,
        bind_group_layout: mipmap_bind_group_layout,
    } = get_pipeline(gpu, texture.format(), is_array).await?;

    let command_encoder = gpu.create_command_encoder(Some("Generate Mipmaps"));

    let view_dimension = if is_array {
        TextureViewDimension::N2dArray
    } else {
        TextureViewDimension::N2d
    };

    // Generate each mip level from the previous one
    for mip_level in 1..mip_levels {
        // Create views for source (previous mip) and destination (current mip)
        let mut src_view_descriptor = TextureViewDescriptor::new(Some("Mip Source"))
            .with_base_mip_level(mip_level - 1)
            .with_dimension(view_dimension)
            .with_mip_level_count(1);
        if let Some(array_layer_count) = array_layer_count {
            src_view_descriptor = src_view_descriptor.with_array_layer_count(array_layer_count);
        }
        let src_view = texture
            .create_view_with_descriptor(&src_view_descriptor.into())
            .map_err(AwsmCoreError::create_texture_view)?;

        let mut dst_view_descriptor = TextureViewDescriptor::new(Some("Mip Dest"))
            .with_base_mip_level(mip_level)
            .with_dimension(view_dimension)
            .with_mip_level_count(1);
        if let Some(array_layer_count) = array_layer_count {
            dst_view_descriptor = dst_view_descriptor.with_array_layer_count(array_layer_count);
        }
        let dst_view = texture
            .create_view_with_descriptor(&dst_view_descriptor.into())
            .map_err(AwsmCoreError::create_texture_view)?;

        for target_layer_index in 0..array_layer_count.unwrap_or(1) {
            let (dst_width, dst_height) =
                get_mipmap_size_for_level(texture.width(), texture.height(), mip_level);

            // Build uniform buffer data
            let params_data: Vec<u32> = vec![
                texture_kinds_per_layer[target_layer_index as usize] as u32,
                target_layer_index,
                dst_width,
                dst_height,
            ];

            let params_buffer = gpu.create_buffer(
                &BufferDescriptor::new(
                    Some("Mipmap Params"),
                    params_data.len() * 4,
                    BufferUsage::new().with_uniform().with_copy_dst(),
                )
                .into(),
            )?;

            // Convert to bytes and write buffer
            let params_bytes: Vec<u8> = params_data.iter().flat_map(|&x| x.to_le_bytes()).collect();
            gpu.write_buffer(&params_buffer, None, &*params_bytes, None, None)?;

            // Create bind group for this layer
            let buffer_binding = crate::buffers::BufferBinding::new(&params_buffer);
            let layer_bind_group = gpu.create_bind_group(&<web_sys::GpuBindGroupDescriptor>::from(
                BindGroupDescriptor::new(
                    &mipmap_bind_group_layout,
                    Some("Mipmap Bind Group"),
                    vec![
                        BindGroupEntry::new(
                            0,
                            BindGroupResource::TextureView(Cow::Borrowed(&src_view)),
                        ),
                        BindGroupEntry::new(
                            1,
                            BindGroupResource::TextureView(Cow::Borrowed(&dst_view)),
                        ),
                        BindGroupEntry::new(2, BindGroupResource::Buffer(buffer_binding)),
                    ],
                ),
            ));

            let workgroup_size_x = dst_width.div_ceil(8);
            let workgroup_size_y = dst_height.div_ceil(8);

            // Dispatch compute shader for this layer
            let compute_pass = command_encoder.begin_compute_pass(Some(
                &ComputePassDescriptor::new(Some("Mipmap Pass")).into(),
            ));
            compute_pass.set_pipeline(&mipmap_pipeline);
            compute_pass.set_bind_group(0, &layer_bind_group, None)?;
            compute_pass.dispatch_workgroups(workgroup_size_x, Some(workgroup_size_y), None);
            compute_pass.end();
        }
    }

    // Submit all commands
    let command_buffer = command_encoder.finish();
    gpu.submit_commands(&command_buffer);

    Ok(())
}

async fn get_pipeline(
    gpu: &AwsmRendererWebGpu,
    format: TextureFormat,
    is_array: bool,
) -> Result<MipmapPipeline> {
    let key = LookupKey {
        texture_format: format.into(),
        is_array,
    };

    if let Some(pipeline) =
        MIPMAP_PIPELINE.with(|pipeline_cell| pipeline_cell.borrow().get(&key).cloned())
    {
        return Ok(pipeline);
    }

    let shader_source = shader_source(format, is_array)?;
    let shader_module = gpu
        .compile_shader(&ShaderModuleDescriptor::new(&shader_source, Some("Mipmap Shader")).into());

    shader_module.validate_shader().await?;

    let compute = ProgrammableStage::new(&shader_module, None);

    let view_dimension = if is_array {
        TextureViewDimension::N2dArray
    } else {
        TextureViewDimension::N2d
    };

    let bind_group_layout = gpu.create_bind_group_layout(
        &BindGroupLayoutDescriptor::new(Some("Mipmap Bind Group Layout"))
            .with_entries(vec![
                // Binding 0: Source texture (previous mip level)
                BindGroupLayoutEntry::new(
                    0,
                    BindGroupLayoutResource::Texture(
                        TextureBindingLayout::new()
                            .with_sample_type(TextureSampleType::Float)
                            .with_view_dimension(view_dimension)
                            .with_multisampled(false),
                    ),
                )
                .with_visibility_compute(),
                // Binding 1: Destination texture (current mip level - storage)
                BindGroupLayoutEntry::new(
                    1,
                    BindGroupLayoutResource::StorageTexture(
                        StorageTextureBindingLayout::new(format)
                            .with_view_dimension(view_dimension)
                            .with_access(StorageTextureAccess::WriteOnly),
                    ),
                )
                .with_visibility_compute(),
                // Binding 2: Uniform buffer with parameters
                BindGroupLayoutEntry::new(
                    2,
                    BindGroupLayoutResource::Buffer(crate::bind_groups::BufferBindingLayout::new()),
                )
                .with_visibility_compute(),
            ])
            .into(),
    )?;

    let pipeline_layout = gpu.create_pipeline_layout(
        &PipelineLayoutDescriptor::new(
            Some("Mipmap Pipeline Layout"),
            vec![bind_group_layout.clone()],
        )
        .into(),
    );

    let pipeline_descriptor = ComputePipelineDescriptor::new(
        compute,
        PipelineLayoutKind::Custom(&pipeline_layout),
        Some("Mipmap Pipeline"),
    );

    let pipeline = gpu
        .create_compute_pipeline(&pipeline_descriptor.into())
        .await?;

    MIPMAP_PIPELINE.with(|pipeline_cell| {
        let pipeline = MipmapPipeline {
            compute_pipeline: pipeline,
            bind_group_layout,
        };
        pipeline_cell.borrow_mut().insert(key, pipeline.clone());
        Ok(pipeline)
    })
}
