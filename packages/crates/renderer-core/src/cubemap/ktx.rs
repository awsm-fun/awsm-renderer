//! KTX2 cubemap loading helpers.

use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::renderer::latest_texture_compression;
use crate::texture::block_format::{
    aligned_bytes_per_row, block_dims, bytes_per_pixel, compression_supported, map_ktx_format,
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

    // Compressed formats are only creatable when the device enabled that
    // family's feature. Environment bakes are BC6H (the HDR block format),
    // which desktop GPUs have and mobile GPUs never do — on a phone this is
    // the difference between a scene loading and `createTexture` throwing.
    // BC6H gets a CPU decode fallback to RGBA16F (8× the bytes, identical
    // image); any other unsupported family is a hard, NAMED error instead of
    // the opaque `TypeError` the create call would throw.
    let decode_bc6h = matches!(
        format,
        TextureFormat::Bc6hRgbUfloat | TextureFormat::Bc6hRgbFloat
    ) && !latest_texture_compression().bc;
    if !decode_bc6h && !compression_supported(format, latest_texture_compression()) {
        return Err(AwsmCoreError::Cubemap(format!(
            "KTX cubemap is {format:?}, but this device does not enable that \
             compression family's feature — re-bake the asset in a format the \
             target devices support"
        )));
    }
    // What the GPU texture is created as (and what upload layouts are
    // computed from); the on-disk `format` still governs source validation.
    let upload_format = if decode_bc6h {
        TextureFormat::Rgba16float
    } else {
        format
    };

    let descriptor = TextureDescriptor::new(
        upload_format,
        Extent3d::new(header.pixel_width, Some(header.pixel_height), Some(6)),
        TextureUsage::new().with_texture_binding().with_copy_dst(),
    )
    .with_mip_level_count(header.level_count)
    .with_dimension(TextureDimension::N2d);

    let texture = gpu.create_texture(&descriptor.into())?;

    let mut non_finite_texels = 0usize;

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

        // A float texel the encoder wrote as NaN/Inf samples as NaN/Inf, and
        // one of those on screen is enough for bloom to smear a black square
        // over the frame. Sanitize before upload (copying the level only
        // when something actually needs fixing).
        let level_data: std::borrow::Cow<[u8]> = match count_non_finite_texels(format, level.data) {
            0 => std::borrow::Cow::Borrowed(level.data),
            count => {
                non_finite_texels += count;
                let mut owned = level.data.to_vec();
                sanitize_non_finite_texels(format, &mut owned);
                std::borrow::Cow::Owned(owned)
            }
        };

        // Upload layout is computed from the UPLOAD format: when decoding,
        // the tight source rows (BC6H blocks) and the uploaded rows
        // (RGBA16F pixels) have different strides.
        let upload_rows = rows_per_image(upload_format, mip_height);
        let upload_tight_bpr = if let Some((bw, _bh, bpb)) = block_dims(upload_format) {
            mip_width.div_ceil(bw) * bpb
        } else {
            mip_width * bytes_per_pixel(upload_format)
        };
        let bpr = aligned_bytes_per_row(upload_format, mip_width);
        let layout = TexelCopyBufferLayout::new()
            .with_bytes_per_row(bpr)
            .with_rows_per_image(upload_rows);
        let size = Extent3d::new(mip_width, Some(mip_height), None);

        // Convert once for reuse
        let layout_ref = &layout.into();
        let size_ref = &size.into();

        for face in 0..6 {
            let destination = TexelCopyTextureInfo::new(&texture)
                .with_mip_level(index as u32)
                .with_origin(Origin3d::new().with_z(face as u32));

            // TODO: ideally fetch per-face slices from the KTX reader
            let face_source = &level_data[face * face_bytes_tight..(face + 1) * face_bytes_tight];
            let face_data_tight: std::borrow::Cow<[u8]> = if decode_bc6h {
                std::borrow::Cow::Owned(decode_bc6h_face_rgba16f(
                    face_source,
                    mip_width,
                    mip_height,
                    matches!(format, TextureFormat::Bc6hRgbFloat),
                ))
            } else {
                std::borrow::Cow::Borrowed(face_source)
            };

            if bpr == upload_tight_bpr {
                // No padding needed, use slice directly
                gpu.write_texture(&destination.into(), &*face_data_tight, layout_ref, size_ref)?;
            } else {
                // Need padding, create staging buffer
                let mut staging = vec![0u8; (bpr * upload_rows) as usize];
                for r in 0..upload_rows as usize {
                    let src = r * upload_tight_bpr as usize
                        ..r * upload_tight_bpr as usize + upload_tight_bpr as usize;
                    let dst = r * bpr as usize..r * bpr as usize + upload_tight_bpr as usize;
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

    if non_finite_texels > 0 {
        tracing::warn!(
            "KTX cubemap ({format:?}) contains {non_finite_texels} NaN/Inf texels across its \
             mips — replaced with 0 (NaN) / the largest finite value (Inf). The asset should be \
             re-baked with awsm-renderer-env-bake (see docs/ASSET_WORKFLOWS.md)."
        );
    }

    Ok((texture, header.level_count))
}

/// Largest finite `B10G11R11_UFLOAT` 11-bit channel: exponent 30, mantissa all ones.
const UF11_MAX: u32 = (30 << 6) | 0x3f;
/// Largest finite `B10G11R11_UFLOAT` 10-bit channel: exponent 30, mantissa all ones.
const UF10_MAX: u32 = (30 << 5) | 0x1f;
/// Largest finite f16 magnitude.
const F16_MAX: u16 = 0x7bff;

/// Bytes per texel of the float formats [`sanitize_non_finite_texels`]
/// understands; `None` for every other format (nothing to sanitize).
fn float_texel_size(format: TextureFormat) -> Option<usize> {
    match format {
        TextureFormat::Rg11b10ufloat
        | TextureFormat::R16float
        | TextureFormat::Rg16float
        | TextureFormat::Rgba16float
        | TextureFormat::R32float
        | TextureFormat::Rg32float
        | TextureFormat::Rgba32float => Some(bytes_per_pixel(format) as usize),
        _ => None,
    }
}

/// One small-float channel (5-bit exponent, `mant_bits` mantissa) made
/// finite. Exponent 31 is Inf (mantissa 0) or NaN (anything else), exactly
/// as in f16.
fn finite_ufloat_channel(bits: u32, mant_bits: u32, max: u32) -> u32 {
    match (bits >> mant_bits == 31, bits & ((1 << mant_bits) - 1) == 0) {
        (false, _) => bits,
        (true, true) => max,
        (true, false) => 0,
    }
}

fn finite_f16(bits: u16) -> u16 {
    match (bits & 0x7c00 == 0x7c00, bits & 0x03ff == 0) {
        (false, _) => bits,
        (true, true) => (bits & 0x8000) | F16_MAX,
        (true, false) => 0,
    }
}

fn finite_f32(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else if value.is_infinite() {
        f32::MAX.copysign(value)
    } else {
        value
    }
}

fn is_non_finite_texel(format: TextureFormat, texel: &[u8]) -> bool {
    match format {
        TextureFormat::Rg11b10ufloat => {
            let packed = u32::from_le_bytes([texel[0], texel[1], texel[2], texel[3]]);
            (packed >> 6) & 0x1f == 31 || (packed >> 17) & 0x1f == 31 || packed >> 27 == 31
        }
        TextureFormat::R16float | TextureFormat::Rg16float | TextureFormat::Rgba16float => texel
            .chunks_exact(2)
            .any(|c| u16::from_le_bytes([c[0], c[1]]) & 0x7c00 == 0x7c00),
        TextureFormat::R32float | TextureFormat::Rg32float | TextureFormat::Rgba32float => texel
            .chunks_exact(4)
            .any(|c| !f32::from_le_bytes([c[0], c[1], c[2], c[3]]).is_finite()),
        _ => false,
    }
}

/// Rewrites `texel` (little-endian, `format`'s layout) in place: every NaN
/// component becomes 0 and every Inf the largest finite value of its sign.
fn make_texel_finite(format: TextureFormat, texel: &mut [u8]) {
    match format {
        TextureFormat::Rg11b10ufloat => {
            let packed = u32::from_le_bytes([texel[0], texel[1], texel[2], texel[3]]);
            let r = finite_ufloat_channel(packed & 0x7ff, 6, UF11_MAX);
            let g = finite_ufloat_channel((packed >> 11) & 0x7ff, 6, UF11_MAX);
            let b = finite_ufloat_channel(packed >> 22, 5, UF10_MAX);
            texel.copy_from_slice(&(r | (g << 11) | (b << 22)).to_le_bytes());
        }
        TextureFormat::R16float | TextureFormat::Rg16float | TextureFormat::Rgba16float => {
            for c in texel.chunks_exact_mut(2) {
                let fixed = finite_f16(u16::from_le_bytes([c[0], c[1]]));
                c.copy_from_slice(&fixed.to_le_bytes());
            }
        }
        TextureFormat::R32float | TextureFormat::Rg32float | TextureFormat::Rgba32float => {
            for c in texel.chunks_exact_mut(4) {
                let fixed = finite_f32(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                c.copy_from_slice(&fixed.to_le_bytes());
            }
        }
        _ => {}
    }
}

/// How many texels of a tight `format` buffer hold a NaN/Inf component.
fn count_non_finite_texels(format: TextureFormat, data: &[u8]) -> usize {
    let Some(size) = float_texel_size(format) else {
        return 0;
    };
    data.chunks_exact(size)
        .filter(|texel| is_non_finite_texel(format, texel))
        .count()
}

/// Rewrites every texel of a tight `format` buffer that holds a NaN/Inf
/// component (NaN → 0, Inf → largest finite value). A radiance map has no
/// meaning for NaN — it is what an encoder leaves behind for a negative or
/// missing input — so black is the honest replacement, while an Inf was a
/// value too bright to represent and stays as bright as the format allows.
fn sanitize_non_finite_texels(format: TextureFormat, data: &mut [u8]) {
    let Some(size) = float_texel_size(format) else {
        return;
    };
    for texel in data.chunks_exact_mut(size) {
        if is_non_finite_texel(format, texel) {
            make_texel_finite(format, texel);
        }
    }
}

/// Decode one tight BC6H cubemap face into tight little-endian RGBA16F pixels
/// (`alpha = 1.0`). The CPU half of the no-BC fallback: bit-exact with what
/// the GPU's own BC6H sampler would produce (`bcdec` is a reference decoder),
/// at 8 bytes/px instead of 1 — the price of a device that can't read the
/// blocks directly.
fn decode_bc6h_face_rgba16f(data: &[u8], width: u32, height: u32, signed: bool) -> Vec<u8> {
    const ONE_F16: u16 = 0x3C00;
    let (width, height) = (width as usize, height as usize);
    let blocks_w = width.div_ceil(4);
    let blocks_h = height.div_ceil(4);
    let mut out = vec![0u16; width * height * 4];
    // One decoded 4×4 RGB block (3 u16 halves per pixel, row pitch 4·3).
    let mut block = [0u16; 4 * 4 * 3];
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_bytes = &data[(by * blocks_w + bx) * 16..][..16];
            bcdec_rs::bc6h_half(block_bytes, &mut block, 4 * 3, signed);
            // Blit the block, clipping the partial blocks at the right and
            // bottom edges (present on the small tail mips).
            for py in 0..4usize {
                let y = by * 4 + py;
                if y >= height {
                    break;
                }
                for px in 0..4usize {
                    let x = bx * 4 + px;
                    if x >= width {
                        break;
                    }
                    let src = (py * 4 + px) * 3;
                    let dst = (y * width + x) * 4;
                    out[dst..dst + 3].copy_from_slice(&block[src..src + 3]);
                    out[dst + 3] = ONE_F16;
                }
            }
        }
    }
    out.into_iter().flat_map(u16::to_le_bytes).collect()
}

#[cfg(test)]
mod bc6h_decode_tests {
    //! Pins the PLUMBING of the no-BC fallback — block↔pixel indexing, edge
    //! clipping on partial blocks, the injected alpha, and the little-endian
    //! byte order. `bcdec` itself is a reference decoder with its own
    //! upstream verification; what can rot HERE is how its 4×4 blocks are
    //! blitted into a face.
    use super::decode_bc6h_face_rgba16f;

    const ONE_F16: u16 = 0x3C00;

    fn halfword(bytes: &[u8], texel: usize, channel: usize) -> u16 {
        let at = (texel * 4 + channel) * 2;
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    #[test]
    fn full_block_face_matches_direct_block_decode() {
        // One 4×4 face = exactly one block: our blit must reproduce bcdec's
        // own output at every texel, with alpha appended.
        let block_bytes: Vec<u8> = (0u8..16).collect();
        let mut expected = [0u16; 4 * 4 * 3];
        bcdec_rs::bc6h_half(&block_bytes, &mut expected, 4 * 3, false);

        let out = decode_bc6h_face_rgba16f(&block_bytes, 4, 4, false);
        assert_eq!(out.len(), 4 * 4 * 4 * 2);
        for texel in 0..16 {
            for channel in 0..3 {
                assert_eq!(
                    halfword(&out, texel, channel),
                    expected[texel * 3 + channel],
                    "texel {texel} channel {channel}"
                );
            }
            assert_eq!(halfword(&out, texel, 3), ONE_F16, "alpha at texel {texel}");
        }
    }

    #[test]
    fn partial_blocks_clip_to_face_dimensions() {
        // A 6×3 face spans 2×1 blocks (16 bytes each) but only 18 texels —
        // the tail-mip shape. The blit must clip rather than write past the
        // face, and every surviving texel still gets its alpha.
        let data = vec![0xA5u8; 2 * 16];
        let (w, h) = (6u32, 3u32);
        let out = decode_bc6h_face_rgba16f(&data, w, h, false);
        assert_eq!(out.len(), (w * h * 4 * 2) as usize);
        for texel in 0..(w * h) as usize {
            assert_eq!(halfword(&out, texel, 3), ONE_F16, "alpha at texel {texel}");
        }
    }

    #[test]
    fn one_by_one_mip_decodes() {
        // The last mip of any chain: one block, one surviving texel.
        let out = decode_bc6h_face_rgba16f(&[0u8; 16], 1, 1, false);
        assert_eq!(out.len(), 8);
        assert_eq!(halfword(&out, 0, 3), ONE_F16);
    }
}

#[cfg(test)]
mod non_finite_tests {
    //! Pins the cubemap NaN/Inf scrub. The regression it guards: a skybox
    //! baked with the raw `ktx create` recipe carried NaN texels (exponent 31
    //! in one channel), which the skybox pass sampled straight into the HDR
    //! target and bloom smeared into a black square.
    use super::{count_non_finite_texels, sanitize_non_finite_texels, F16_MAX};
    use crate::texture::TextureFormat;

    fn rg11b10(r: u32, g: u32, b: u32) -> [u8; 4] {
        (r | (g << 11) | (b << 22)).to_le_bytes()
    }

    #[test]
    fn rg11b10_nan_to_zero_inf_to_max_finite_rest_untouched() {
        const ONE_11: u32 = 15 << 6; // 1.0
        const ONE_10: u32 = 15 << 5;
        const NAN_11: u32 = (31 << 6) | 1;
        const INF_11: u32 = 31 << 6;
        const NAN_10: u32 = (31 << 5) | 3;
        let mut data: Vec<u8> = [
            rg11b10(ONE_11, ONE_11, ONE_10),
            rg11b10(NAN_11, ONE_11, ONE_10),
            rg11b10(ONE_11, INF_11, NAN_10),
        ]
        .concat();
        // The exact texel found in the jetpack-joust skybox: R is NaN, G/B
        // are small sane values that must survive.
        data.extend_from_slice(&0x4a8f_c7efu32.to_le_bytes());

        assert_eq!(
            count_non_finite_texels(TextureFormat::Rg11b10ufloat, &data),
            3
        );
        sanitize_non_finite_texels(TextureFormat::Rg11b10ufloat, &mut data);
        assert_eq!(
            count_non_finite_texels(TextureFormat::Rg11b10ufloat, &data),
            0
        );

        assert_eq!(&data[0..4], &rg11b10(ONE_11, ONE_11, ONE_10));
        assert_eq!(&data[4..8], &rg11b10(0, ONE_11, ONE_10));
        assert_eq!(
            &data[8..12],
            &rg11b10(ONE_11, (30 << 6) | 0x3f, 0),
            "Inf saturates to the largest finite value, NaN goes to 0"
        );
        let jetpack = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        assert_eq!(jetpack & 0x7ff, 0);
        assert_eq!(jetpack >> 11, 0x4a8f_c7ef >> 11);
    }

    #[test]
    fn f16_components_scrubbed_with_sign_kept_on_inf() {
        let halves: [u16; 4] = [0x3c00, 0x7e00, 0x7c00, 0xfc00]; // 1.0, NaN, +Inf, -Inf
        let mut data: Vec<u8> = halves.iter().flat_map(|h| h.to_le_bytes()).collect();
        assert_eq!(
            count_non_finite_texels(TextureFormat::Rgba16float, &data),
            1
        );
        sanitize_non_finite_texels(TextureFormat::Rgba16float, &mut data);
        let out: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(out, vec![0x3c00, 0, F16_MAX, 0x8000 | F16_MAX]);
    }

    #[test]
    fn f32_components_scrubbed() {
        let values = [1.5f32, f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        let mut data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(
            count_non_finite_texels(TextureFormat::Rgba32float, &data),
            1
        );
        sanitize_non_finite_texels(TextureFormat::Rgba32float, &mut data);
        let out: Vec<f32> = data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(out, vec![1.5, 0.0, f32::MAX, -f32::MAX]);
    }

    #[test]
    fn non_float_formats_are_left_alone() {
        let mut data = vec![0xffu8; 16];
        assert_eq!(count_non_finite_texels(TextureFormat::Rgba8unorm, &data), 0);
        sanitize_non_finite_texels(TextureFormat::Rgba8unorm, &mut data);
        assert_eq!(data, vec![0xffu8; 16]);
    }
}
