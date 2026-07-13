//! KTX2 cubemap loading helpers.

use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::texture::block_format::{
    aligned_bytes_per_row, block_dims, bytes_per_pixel, is_block_compressed, map_ktx_format,
    rows_per_image,
};
use crate::texture::TextureFormat;
use crate::{
    command::copy_texture::{Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo},
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::{Extent3d, TextureDescriptor, TextureDimension, TextureUsage},
};

/// Loads a KTX2 file from a URL.
pub async fn load_url(url: &str) -> anyhow::Result<ktx2::Reader<Vec<u8>>> {
    let resp: web_sys::Response = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| AwsmCoreError::Fetch(e.to_string()))?
        .into();

    let js_value = JsFuture::from(resp.array_buffer().map_err(AwsmCoreError::fetch)?)
        .await
        .map_err(AwsmCoreError::fetch)?;

    let array_buffer: ArrayBuffer = js_value.unchecked_into();

    let bytes = Uint8Array::new(&array_buffer).to_vec();

    Ok(ktx2::Reader::new(bytes).map_err(|e| AwsmCoreError::Ktx(e.to_string()))?)
}

/// Parses a KTX2 file from already-fetched bytes (no network). The bytes-in
/// counterpart of [`load_url`] — used when the `.ktx2` comes from a player-bundle
/// asset map (or any in-memory source) rather than a URL.
pub fn load_bytes(bytes: Vec<u8>) -> anyhow::Result<ktx2::Reader<Vec<u8>>> {
    Ok(ktx2::Reader::new(bytes).map_err(|e| AwsmCoreError::Ktx(e.to_string()))?)
}

/// Creates a cubemap texture from a KTX2 reader.
pub async fn create_texture(
    reader: &ktx2::Reader<Vec<u8>>,
    gpu: &AwsmRendererWebGpu,
) -> Result<(web_sys::GpuTexture, u32)> {
    let header = reader.header();

    if header.face_count != 6 {
        return Err(AwsmCoreError::Cubemap(
            "KTX file does not contain a cubemap".to_string(),
        ));
    }

    if header.layer_count != 0 {
        return Err(AwsmCoreError::Cubemap(
            "KTX file contains array textures, which are not supported for cubemaps".to_string(),
        ));
    }

    if header.pixel_depth > 1 {
        return Err(AwsmCoreError::Cubemap(
            "KTX file contains 3D textures, which are not supported for cubemaps".to_string(),
        ));
    }

    if header.supercompression_scheme.is_some() {
        return Err(AwsmCoreError::Cubemap(
            "KTX file uses supercompression, which is not supported".to_string(),
        ));
    }

    let ktx_format = match header.format {
        Some(f) => f,
        None => {
            return Err(AwsmCoreError::Cubemap(
                "KTX file does not specify a format".to_string(),
            ));
        }
    };

    let format = match map_ktx_format(ktx_format) {
        Some(format) => {
            // // Check for KTX metadata that might indicate exposure/scaling
            // for (key, value) in reader.key_value_data() {
            //     tracing::info!("metadata key: {key}");
            // }

            format
        }
        None => {
            return Err(AwsmCoreError::Cubemap(format!(
                "KTX file has unsupported format: {:?}",
                header.format
            )));
        }
    };

    // Warn about potential depth format compatibility issues
    if matches!(
        format,
        TextureFormat::Depth24plus | TextureFormat::Depth24plusStencil8
    ) {
        tracing::warn!("Using Depth24plus format - some backends implement this as 32-bit float internally. If texture upload fails, consider converting the asset to Depth32float format.");
    }

    // Validate device features for compressed formats
    if is_block_compressed(format) {
        // Note: In a full implementation, you would check gpu device features here
        // For now, we assume the features are available
        tracing::warn!(
            "Using compressed texture format {:?} - ensure device supports required features",
            format
        );
    }

    let descriptor = TextureDescriptor::new(
        format,
        Extent3d::new(header.pixel_width, Some(header.pixel_height), Some(6)),
        TextureUsage::new().with_texture_binding().with_copy_dst(),
    )
    .with_mip_level_count(header.level_count)
    .with_dimension(TextureDimension::N2d);

    let texture = gpu.create_texture(&descriptor.into())?;

    for (index, level) in reader.levels().enumerate() {
        // Calculate mip level dimensions with bounds checking
        let mip_width = if index < 32 {
            std::cmp::max(1u32, header.pixel_width >> index)
        } else {
            1u32
        };
        let mip_height = if index < 32 {
            std::cmp::max(1u32, header.pixel_height >> index)
        } else {
            1u32
        };

        // Validate level size matches expected tight size
        let rows = rows_per_image(format, mip_height);
        let tight_bpr = if let Some((bw, _bh, bpb)) = block_dims(format) {
            mip_width.div_ceil(bw) * bpb
        } else {
            mip_width * bytes_per_pixel(format)
        };
        let face_bytes_tight = tight_bpr as usize * rows as usize;
        let expected_level_len = face_bytes_tight * 6;

        if level.data.len() != expected_level_len {
            return Err(AwsmCoreError::Cubemap(format!(
                "Level {} byte length {} doesn't match expected face*rows*tight_bpr {} (possible KTX per-face padding not supported)",
                index, level.data.len(), expected_level_len
            )));
        }

        // Calculate values once per mip level
        let bpr = aligned_bytes_per_row(format, mip_width);
        let layout = TexelCopyBufferLayout::new()
            .with_bytes_per_row(bpr)
            .with_rows_per_image(rows);
        let size = Extent3d::new(mip_width, Some(mip_height), None);

        // Convert once for reuse
        let layout_ref = &layout.into();
        let size_ref = &size.into();

        for face in 0..6 {
            let destination = TexelCopyTextureInfo::new(&texture)
                .with_mip_level(index as u32)
                .with_origin(Origin3d::new().with_z(face as u32));

            // TODO: ideally fetch per-face slices from the KTX reader
            let face_data_tight =
                &level.data[face * face_bytes_tight..(face + 1) * face_bytes_tight];

            if bpr == tight_bpr {
                // No padding needed, use slice directly
                gpu.write_texture(&destination.into(), face_data_tight, layout_ref, size_ref)?;
            } else {
                // Need padding, create staging buffer
                let mut staging = vec![0u8; (bpr * rows) as usize];
                for r in 0..rows as usize {
                    let src = r * tight_bpr as usize..r * tight_bpr as usize + tight_bpr as usize;
                    let dst = r * bpr as usize..r * bpr as usize + tight_bpr as usize;
                    staging[dst].copy_from_slice(&face_data_tight[src]);
                }
                gpu.write_texture(
                    &destination.into(),
                    staging.as_slice(),
                    layout_ref,
                    size_ref,
                )?;
            }
        }
    }

    Ok((texture, header.level_count))
}
