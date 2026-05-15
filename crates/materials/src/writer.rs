//! Helpers for packing material parameters into the GPU storage buffer
//! `awsm-renderer` dispatches against.

use awsm_renderer_core::{
    keys::TextureKey,
    sampler::AddressMode,
    texture::texture_pool::{TexturePoolArray, TexturePoolEntryInfo},
};

use crate::{texture_context::TextureContext, MaterialTexture};

/// Packed value used by material writers.
pub enum Value<'a> {
    F32(f32),
    U32(u32),
    Texture {
        array: &'a TexturePoolArray<TextureKey>,
        entry_info: &'a TexturePoolEntryInfo<TextureKey>,
        uv_index: u32,
        sampler_index: u32,
        address_mode_u: u32,
        address_mode_v: u32,
        texture_transform_offset: usize,
    },
    /// Placeholder for a missing texture slot (20 zero bytes — same layout
    /// as a packed `Texture` so the shader can detect "no texture" via the
    /// `flags` bit).
    SkipTexture,
}

impl From<f32> for Value<'_> {
    fn from(value: f32) -> Self {
        Value::F32(value)
    }
}
impl From<u32> for Value<'_> {
    fn from(value: u32) -> Self {
        Value::U32(value)
    }
}
impl From<&f32> for Value<'_> {
    fn from(value: &f32) -> Self {
        Value::F32(*value)
    }
}
impl From<&u32> for Value<'_> {
    fn from(value: &u32) -> Self {
        Value::U32(*value)
    }
}

/// Encodes a WebGPU address mode for the shader's UV resolution path.
pub fn encode_address_mode(mode: Option<AddressMode>) -> u32 {
    match mode.unwrap_or(AddressMode::Repeat) {
        AddressMode::ClampToEdge => 0,
        AddressMode::Repeat => 1,
        AddressMode::MirrorRepeat => 2,
        // WebGPU exposes additional vendor-specific variants behind feature
        // flags. If we ever see one, treat it as repeat so rendering keeps
        // working instead of crashing.
        _ => 1,
    }
}

/// Writes a packed value into a byte buffer.
pub fn write(data: &mut Vec<u8>, value: Value) {
    match value {
        Value::F32(value) => {
            data.extend_from_slice(&value.to_le_bytes());
        }
        Value::U32(value) => {
            data.extend_from_slice(&value.to_le_bytes());
        }
        Value::Texture {
            array,
            entry_info,
            uv_index,
            sampler_index,
            address_mode_u,
            address_mode_v,
            texture_transform_offset,
        } => {
            let packed = pack_texture_info_raw(
                array,
                entry_info,
                uv_index,
                sampler_index,
                address_mode_u,
                address_mode_v,
                texture_transform_offset,
            );

            for word in packed {
                data.extend_from_slice(&word.to_le_bytes());
            }
        }
        Value::SkipTexture => {
            data.extend_from_slice(&[0u8; 20]);
        }
    }
}

/// Convenience: write the `MaterialTexture` if present, else `SkipTexture`.
///
/// Returns `()` so callers can fold it into a sequence of `write(...)` calls.
pub fn write_material_texture(
    data: &mut Vec<u8>,
    tex: Option<&MaterialTexture>,
    ctx: &dyn TextureContext,
) {
    match tex.and_then(|t| map_texture(t, ctx)) {
        Some(v) => write(data, v),
        None => write(data, Value::SkipTexture),
    }
}

/// Maps a `MaterialTexture` to a packed `Value::Texture` for shader use.
///
/// Returns `None` when any referenced entity is missing — the caller treats
/// that as `SkipTexture`. This mirrors the historical behavior in
/// `awsm-renderer`: a missing texture / sampler / UV-set should not abort
/// the buffer write, just fall back to the no-texture path.
pub fn map_texture<'a>(
    tex: &MaterialTexture,
    ctx: &'a dyn TextureContext,
) -> Option<Value<'a>> {
    let entry_info = ctx.texture_entry(tex.key)?;
    let array = ctx.pool_array_by_index(entry_info.array_index)?;
    let sampler_key = tex.sampler_key?;
    let sampler_index = ctx.sampler_index(sampler_key)?;
    let uv_index = tex.uv_index?;
    let (address_mode_u, address_mode_v) = ctx.sampler_address_modes(sampler_key);
    Some(Value::Texture {
        array,
        entry_info,
        uv_index,
        sampler_index,
        address_mode_u: encode_address_mode(address_mode_u),
        address_mode_v: encode_address_mode(address_mode_v),
        texture_transform_offset: tex
            .transform_key
            .and_then(|key| ctx.texture_transform_offset(key))
            .unwrap_or_else(|| ctx.texture_transform_identity_offset()),
    })
}

fn pack_texture_info_raw<ID>(
    array: &TexturePoolArray<ID>,
    entry_info: &TexturePoolEntryInfo<ID>,
    uv_index: u32,
    sampler_index: u32,
    address_mode_u: u32,
    address_mode_v: u32,
    texture_transform_offset: usize,
) -> [u32; 5] {
    // --- size: width (16 bits) + height (16 bits) ---
    let width = array.width;
    let height = array.height;

    debug_assert!(width <= 0xFFFF, "texture width too large for 16 bits");
    debug_assert!(height <= 0xFFFF, "texture height too large for 16 bits");

    let size = (height << 16) | (width & 0xFFFF);

    // --- array_and_layer: array_index (12 bits) + layer_index (20 bits) ---
    let array_index = entry_info.array_index as u32;
    let layer_index = entry_info.layer_index as u32;

    debug_assert!(array_index <= 0xFFF, "array_index too large for 12 bits");
    debug_assert!(layer_index <= 0xFFFFF, "layer_index too large for 20 bits");

    let array_and_layer = (layer_index << 12) | (array_index & 0xFFF);

    // --- uv_and_sampler: uv_set_index (8 bits) + sampler_index (24 bits) ---
    debug_assert!(uv_index <= 0xFF, "uv_index too large for 8 bits");
    debug_assert!(
        sampler_index <= 0xFFFFFF,
        "sampler_index too large for 24 bits"
    );

    let uv_and_sampler = (sampler_index << 8) | (uv_index & 0xFF);

    // --- extra: flags (8) + addr_u (8) + addr_v (8) + padding (8) ---
    // flags:
    //   bit 0: has mipmaps
    let has_mipmaps = array.mipmap;

    // start with bit 0 set to 1 to indicate texture is present
    let mut flags: u32 = 1;
    if has_mipmaps {
        flags |= 1 << 1;
    }

    debug_assert!(
        address_mode_u <= 0xFF,
        "address_mode_u too large for 8 bits"
    );
    debug_assert!(
        address_mode_v <= 0xFF,
        "address_mode_v too large for 8 bits"
    );

    let extra = (flags & 0xFF) | ((address_mode_u & 0xFF) << 8) | ((address_mode_v & 0xFF) << 16);
    // top 8 bits left as 0 (padding/reserved)

    // --- transform_offset: full 32 bits for byte offset ---
    let transform_offset = texture_transform_offset as u32;

    [
        size,
        array_and_layer,
        uv_and_sampler,
        extra,
        transform_offset,
    ]
}
