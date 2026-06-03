use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use crate::bind_groups::{
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindGroupLayoutResource, StorageTextureAccess,
    StorageTextureBindingLayout, TextureBindingLayout,
};
use crate::pipeline::layout::{PipelineLayoutDescriptor, PipelineLayoutKind};
use crate::pipeline::{ComputePipelineDescriptor, ProgrammableStage};
use crate::shaders::ShaderModuleExt;

use crate::error::Result;
use crate::texture::{
    texture_format_to_wgsl_storage, TextureFormat, TextureFormatKey, TextureSampleType,
};
use crate::{
    bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource},
    command::compute_pass::ComputePassDescriptor,
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
    shaders::ShaderModuleDescriptor,
    texture::{TextureViewDescriptor, TextureViewDimension},
};

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

// Tile-aware mipmap generation with texture-type-specific filtering
// Based on per-tile processing to prevent bleeding between atlas textures
fn tile_aware_mipmap_shader_source(format: TextureFormat, is_array: bool) -> Result<String> {
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
    let layer_field = if is_array { "layer: i32," } else { "" };

    Ok(format!(
        r#"
@group(0) @binding(0) var src: {texture_type};
@group(0) @binding(1) var dst: {storage_type};

struct Rect {{
    min: vec2<i32>,
    max: vec2<i32>,
}};

struct Params {{
    srcInteriorMin: vec2<i32>,
    srcInteriorMax: vec2<i32>,
    gutter: i32,
    textureType: u32,  // 0=Albedo, 1=Normal, 2=MetallicRoughness, 3=Occlusion, 4=Emissive
    {layer_field}
}};

@group(0) @binding(2) var<uniform> params: Params;

const TEXTURE_TYPE_ALBEDO: u32 = 0u;
const TEXTURE_TYPE_NORMAL: u32 = 1u;
const TEXTURE_TYPE_METALLIC_ROUGHNESS: u32 = 2u;
const TEXTURE_TYPE_OCCLUSION: u32 = 3u;
const TEXTURE_TYPE_EMISSIVE: u32 = 4u;

fn clamp_to_rect(p: vec2<i32>, r: Rect) -> vec2<i32> {{
    return clamp(p, r.min, r.max - vec2<i32>(1));
}}

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
    // Compute child interior and padded rect
    let childInteriorMin = params.srcInteriorMin / 2;
    let childInteriorMax = (params.srcInteriorMax + vec2<i32>(1, 1)) / 2;
    let g = vec2<i32>(params.gutter, params.gutter);
    let dstRectMin = childInteriorMin - g;
    let dstRectMax = childInteriorMax + g;

    let d = vec2<i32>(gid.xy) + dstRectMin;
    if (any(d < dstRectMin) || any(d >= dstRectMax)) {{
        return;
    }}

    // Clamp destination to interior for gutter filling (edge extension)
    let d_clamped = clamp(d, childInteriorMin, childInteriorMax - vec2<i32>(1));

    // Map to parent texels (2x2 region)
    let s_base = 2 * d_clamped;

    // Clamp reads to source interior - edge replication handles padding
    let srcRect = Rect(params.srcInteriorMin, params.srcInteriorMax);

    let s00 = clamp_to_rect(s_base + vec2<i32>(0, 0), srcRect);
    let s10 = clamp_to_rect(s_base + vec2<i32>(1, 0), srcRect);
    let s01 = clamp_to_rect(s_base + vec2<i32>(0, 1), srcRect);
    let s11 = clamp_to_rect(s_base + vec2<i32>(1, 1), srcRect);

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

    store_texel(d{layer_arg}, result);
}}
"#
    ))
}

/// Information about a tile in a texture atlas for tile-aware mipmap generation.
///
/// Each tile represents a rectangular region in the atlas that should be processed
/// independently to prevent bleeding artifacts between adjacent textures.
pub struct TileInfo {
    /// Position in atlas at mip level 0 (texel coordinates)
    pub pixel_offset: [u32; 2],
    /// Size of the tile at mip level 0 (texel dimensions)
    pub size: [u32; 2],
    /// Texture type for filtering (0=Albedo, 1=Normal, 2=MetallicRoughness, 3=Occlusion, 4=Emissive)
    pub texture_type: u32,
    /// Array layer index for this tile (only used if is_array is true)
    pub layer_index: u32,
}

/// Generate mipmaps for a texture using tile-aware filtering to prevent bleeding.
///
/// This function performs per-tile mipmap generation with texture-type-specific filtering
/// and properly scaled padding at each mip level. For non-atlas textures, pass an empty
/// tiles vector.
///
/// # Arguments
/// * `gpu` - GPU context
/// * `texture` - Texture to generate mipmaps for (must have mip_levels > 1)
/// * `tiles` - Tile regions to process (empty for non-atlas textures)
/// * `gutter` - Padding size at mip level 0 (will be scaled: 8→4→2→1)
/// * `array_layers` - Number of array layers (1 for non-array textures)
/// * `is_array` - Whether this is a 2D array texture
/// * `mip_levels` - Total number of mip levels to generate (including base level)
pub async fn generate_mipmaps(
    gpu: &AwsmRendererWebGpu,
    texture: &web_sys::GpuTexture,
    tiles: Vec<TileInfo>,
    gutter: u32,
    array_layers: u32,
    is_array: bool,
    mip_levels: u32,
) -> Result<()> {
    use crate::buffers::{BufferDescriptor, BufferUsage};

    if tiles.is_empty() || mip_levels < 2 {
        return Ok(());
    }

    // Get tile-aware mipmap pipeline
    let MipmapPipeline {
        compute_pipeline: mipmap_pipeline,
        bind_group_layout: mipmap_bind_group_layout,
    } = get_tile_aware_mipmap_pipeline(gpu, texture.format(), is_array).await?;

    let command_encoder = gpu.create_command_encoder(Some("Generate Tile-Aware Mipmaps"));

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
        if is_array {
            src_view_descriptor = src_view_descriptor.with_array_layer_count(array_layers);
        }
        let src_view = texture
            .create_view_with_descriptor(&src_view_descriptor.into())
            .map_err(AwsmCoreError::create_texture_view)?;

        let mut dst_view_descriptor = TextureViewDescriptor::new(Some("Mip Dest"))
            .with_base_mip_level(mip_level)
            .with_dimension(view_dimension)
            .with_mip_level_count(1);
        if is_array {
            dst_view_descriptor = dst_view_descriptor.with_array_layer_count(array_layers);
        }
        let dst_view = texture
            .create_view_with_descriptor(&dst_view_descriptor.into())
            .map_err(AwsmCoreError::create_texture_view)?;

        // Process each tile
        for tile in &tiles {
            let src_mip_scale = 1 << (mip_level - 1);

            // Calculate tile bounds at source mip level
            let src_interior_min = [
                (tile.pixel_offset[0] / src_mip_scale) as i32,
                (tile.pixel_offset[1] / src_mip_scale) as i32,
            ];
            let src_interior_max = [
                ((tile.pixel_offset[0] + tile.size[0]) / src_mip_scale) as i32,
                ((tile.pixel_offset[1] + tile.size[1]) / src_mip_scale) as i32,
            ];

            // Padding scales with mip level: mip0=8px, mip1=4px, mip2=2px, mip3=1px
            let gutter_at_dst_mip = (gutter / (1 << mip_level)).max(1);

            // Build uniform buffer data
            let params_data: Vec<i32> = if is_array {
                vec![
                    src_interior_min[0],
                    src_interior_min[1],
                    src_interior_max[0],
                    src_interior_max[1],
                    gutter_at_dst_mip as i32,
                    tile.texture_type as i32,
                    tile.layer_index as i32,
                    0, // padding for alignment
                ]
            } else {
                vec![
                    src_interior_min[0],
                    src_interior_min[1],
                    src_interior_max[0],
                    src_interior_max[1],
                    gutter_at_dst_mip as i32,
                    tile.texture_type as i32,
                    0, // padding
                    0, // padding
                ]
            };

            let params_buffer = gpu.create_buffer(
                &BufferDescriptor::new(
                    Some("Tile Mipmap Params"),
                    params_data.len() * 4,
                    BufferUsage::new().with_uniform().with_copy_dst(),
                )
                .into(),
            )?;

            // Convert to bytes and write buffer
            let params_bytes: Vec<u8> = params_data.iter().flat_map(|&x| x.to_ne_bytes()).collect();
            gpu.write_buffer(&params_buffer, None, &*params_bytes, None, None)?;

            // Create bind group for this tile
            let buffer_binding = crate::buffers::BufferBinding::new(&params_buffer);
            let tile_bind_group = gpu.create_bind_group(
                &BindGroupDescriptor::new(
                    &mipmap_bind_group_layout,
                    Some("Tile Mipmap Bind Group"),
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
                )
                .into(),
            );

            // Calculate destination rect for dispatch
            let child_interior_min = [src_interior_min[0] / 2, src_interior_min[1] / 2];
            let child_interior_max = [(src_interior_max[0] + 1) / 2, (src_interior_max[1] + 1) / 2];

            let dst_rect_min = [
                child_interior_min[0] - gutter_at_dst_mip as i32,
                child_interior_min[1] - gutter_at_dst_mip as i32,
            ];
            let dst_rect_max = [
                child_interior_max[0] + gutter_at_dst_mip as i32,
                child_interior_max[1] + gutter_at_dst_mip as i32,
            ];

            let dispatch_width = (dst_rect_max[0] - dst_rect_min[0]).max(1);
            let dispatch_height = (dst_rect_max[1] - dst_rect_min[1]).max(1);

            let workgroup_size_x = (dispatch_width as u32).div_ceil(8);
            let workgroup_size_y = (dispatch_height as u32).div_ceil(8);

            // Dispatch compute shader for this tile
            let tile_pass = command_encoder.begin_compute_pass(Some(
                &ComputePassDescriptor::new(Some("Tile Mipmap Pass")).into(),
            ));
            tile_pass.set_pipeline(&mipmap_pipeline);
            tile_pass.set_bind_group(0, &tile_bind_group, None)?;
            tile_pass.dispatch_workgroups(workgroup_size_x, Some(workgroup_size_y), None);
            tile_pass.end();
        }
    }

    // Submit all commands
    let command_buffer = command_encoder.finish();
    gpu.submit_commands(&command_buffer);

    Ok(())
}

async fn get_tile_aware_mipmap_pipeline(
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

    let shader_source = tile_aware_mipmap_shader_source(format.clone(), is_array)?;
    let shader_module = gpu.compile_shader(
        &ShaderModuleDescriptor::new(&shader_source, Some("Tile-Aware Mipmap Shader")).into(),
    );

    shader_module.validate_shader().await?;

    let compute = ProgrammableStage::new(&shader_module, None);

    let view_dimension = if is_array {
        TextureViewDimension::N2dArray
    } else {
        TextureViewDimension::N2d
    };

    let bind_group_layout = gpu.create_bind_group_layout(
        &BindGroupLayoutDescriptor::new(Some("Tile-Aware Mipmap Bind Group Layout"))
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
                // Binding 2: Uniform buffer with tile parameters
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
            Some("Tile-Aware Mipmap Pipeline Layout"),
            vec![bind_group_layout.clone()],
        )
        .into(),
    );

    let pipeline_descriptor = ComputePipelineDescriptor::new(
        compute,
        PipelineLayoutKind::Custom(&pipeline_layout),
        Some("Tile-Aware Mipmap Pipeline"),
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
