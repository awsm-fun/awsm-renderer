//! `EXT_meshopt_compression` bufferView decode (and later encode) over the
//! official meshoptimizer C library via the `meshopt` FFI crate, which links
//! straight into the app wasm (no worker, no Emscripten).
//!
//! The glTF extension compresses whole bufferViews: the extension object on a
//! bufferView carries its own `buffer`/`byteOffset`/`byteLength` (the real,
//! compressed bytes) plus `count`, `byteStride`, `mode` and `filter`. Decoding
//! reconstructs the logical `byteStride × count` bytes the accessors then read.
//! The PARENT bufferView points at a `fallback: true` buffer and must never be
//! read as data.

use thiserror::Error;

/// Re-exported so downstream crates (editor bake/export) reach the full
/// meshoptimizer API (encode, optimize, simplify) through one dependency.
pub use meshopt;

/// meshoptimizer's OPTIMIZER entry points (vertex cache / overdraw / fetch —
/// the F5 pre-encode reorder) allocate scratch through `meshopt_Allocator`,
/// whose DEFAULT backend is C++ `operator new` / `operator delete`. On
/// wasm32-unknown-unknown there is no C++ runtime, so those symbols become
/// unresolved wasm imports from module "env" and the app module FAILS TO
/// INSTANTIATE ("Failed to resolve module specifier \"env\""). The
/// decode/encode surface never allocates, which is why this only surfaced
/// with the reorder pass — and only in the BROWSER: `cargo check/test` never
/// links the final wasm, so keep this in mind for any new meshopt API use.
///
/// Why NOT `meshopt_setAllocator` (the library's own hook): it swaps the
/// runtime function pointers, but the default storage is a static initializer
/// — `static Storage s = {::operator new, ::operator delete}` (allocator.cpp)
/// — that TAKES THE ADDRESS of `operator new`/`operator delete`. That
/// address-of reference is baked into the wasm regardless of any runtime
/// override, so the `_Znwm`/`_ZdlPv` imports remain unresolved and the module
/// still won't instantiate. The symbols must be DEFINED, not merely bypassed.
///
/// So we define the Itanium-mangled symbols here, backed by Rust's global
/// allocator; the C++ default then resolves straight to them (no
/// `setAllocator` call needed). A 16-byte size header carries the layout size
/// into the (unsized) scalar `operator delete`, and keeps the returned pointer
/// at C++ max_align (16).
#[cfg(target_arch = "wasm32")]
mod cxx_alloc_shim {
    use std::alloc::{alloc, dealloc, handle_alloc_error, Layout};

    const HEADER: usize = 16;
    const ALIGN: usize = 16;

    fn layout(total: usize) -> Layout {
        Layout::from_size_align(total, ALIGN).expect("operator new layout")
    }

    /// C++ `operator new(size_t)`.
    #[no_mangle]
    pub extern "C" fn _Znwm(size: usize) -> *mut u8 {
        let total = size
            .checked_add(HEADER)
            .expect("operator new size overflow");
        unsafe {
            let base = alloc(layout(total));
            if base.is_null() {
                // operator new must not return null (the C++ is compiled
                // without exceptions) — abort like OOM anywhere else.
                handle_alloc_error(layout(total));
            }
            (base as *mut usize).write(total);
            base.add(HEADER)
        }
    }

    /// C++ `operator delete(void*)`.
    #[no_mangle]
    pub extern "C" fn _ZdlPv(ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        unsafe {
            let base = ptr.sub(HEADER);
            let total = (base as *const usize).read();
            dealloc(base, layout(total));
        }
    }

    /// C++ sized `operator delete(void*, size_t)` — emitted instead of the
    /// unsized form under `-fsized-deallocation`; delegate to it.
    #[no_mangle]
    pub extern "C" fn _ZdlPvm(ptr: *mut u8, _size: usize) {
        _ZdlPv(ptr);
    }
}

/// `mode` of an `EXT_meshopt_compression` bufferView.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Vertex attribute data (`meshopt_decodeVertexBuffer`).
    Attributes,
    /// Triangle index data, count divisible by 3 (`meshopt_decodeIndexBuffer`).
    Triangles,
    /// Non-triangle index sequences (`meshopt_decodeIndexSequence`).
    Indices,
}

impl Mode {
    /// Parse the glTF JSON string value.
    pub fn from_gltf(s: &str) -> Option<Self> {
        match s {
            "ATTRIBUTES" => Some(Self::Attributes),
            "TRIANGLES" => Some(Self::Triangles),
            "INDICES" => Some(Self::Indices),
            _ => None,
        }
    }
}

/// `filter` of an `EXT_meshopt_compression` bufferView — applied in-place to
/// the output of an `Attributes` decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    None,
    /// Unit vectors (normals/tangents), stride 4 or 8.
    Octahedral,
    /// Unit quaternions, stride 8.
    Quaternion,
    /// Floating-point data, stride divisible by 4.
    Exponential,
}

impl Filter {
    /// Parse the glTF JSON string value (absent == `NONE`).
    pub fn from_gltf(s: &str) -> Option<Self> {
        match s {
            "NONE" => Some(Self::None),
            "OCTAHEDRAL" => Some(Self::Octahedral),
            "QUATERNION" => Some(Self::Quaternion),
            "EXPONENTIAL" => Some(Self::Exponential),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("invalid byteStride {stride} for mode {mode:?}")]
    InvalidStride { stride: usize, mode: Mode },
    #[error("count {count} invalid for mode {mode:?}")]
    InvalidCount { count: usize, mode: Mode },
    #[error("filter {filter:?} incompatible with byteStride {stride}")]
    InvalidFilterStride { filter: Filter, stride: usize },
    #[error("output size {count}×{stride} overflows")]
    SizeOverflow { count: usize, stride: usize },
    #[error("meshoptimizer rejected the compressed stream (code {code})")]
    Corrupt { code: i32 },
}

/// Upper bound on a single decoded bufferView; a malformed header must not be
/// able to make us allocate unbounded memory. 256MB comfortably covers any
/// real mesh we ship while staying far below the wasm32 address space.
pub const MAX_DECODED_BYTES: usize = 256 * 1024 * 1024;

/// Upper bound on a single COMPRESSED input stream — a container that claims
/// a larger range than this is rejected before the C library ever sees it.
pub const MAX_ENCODED_BYTES: usize = 256 * 1024 * 1024;

/// Decode one `EXT_meshopt_compression` bufferView: `data` is the extension's
/// own byte range (NOT the fallback buffer), `count`/`stride` are the
/// extension's `count`/`byteStride`. Returns the reconstructed logical
/// `stride × count` bytes.
///
/// For index modes, `stride` is the index size in bytes (2 or 4), matching how
/// the extension expresses it.
pub fn decode_buffer_view(
    data: &[u8],
    count: usize,
    stride: usize,
    mode: Mode,
    filter: Filter,
) -> Result<Vec<u8>, DecodeError> {
    if data.len() > MAX_ENCODED_BYTES {
        return Err(DecodeError::SizeOverflow {
            count: data.len(),
            stride: 1,
        });
    }
    let total = count
        .checked_mul(stride)
        .filter(|&t| t <= MAX_DECODED_BYTES)
        .ok_or(DecodeError::SizeOverflow { count, stride })?;

    match mode {
        Mode::Attributes => {
            if stride == 0 || stride > 256 || stride % 4 != 0 {
                return Err(DecodeError::InvalidStride { stride, mode });
            }
        }
        Mode::Triangles => {
            if stride != 2 && stride != 4 {
                return Err(DecodeError::InvalidStride { stride, mode });
            }
            if count % 3 != 0 {
                return Err(DecodeError::InvalidCount { count, mode });
            }
        }
        Mode::Indices => {
            if stride != 2 && stride != 4 {
                return Err(DecodeError::InvalidStride { stride, mode });
            }
        }
    }

    let mut out = vec![0u8; total];
    let code = unsafe {
        match mode {
            Mode::Attributes => meshopt::ffi::meshopt_decodeVertexBuffer(
                out.as_mut_ptr().cast(),
                count,
                stride,
                data.as_ptr(),
                data.len(),
            ),
            Mode::Triangles => meshopt::ffi::meshopt_decodeIndexBuffer(
                out.as_mut_ptr().cast(),
                count,
                stride,
                data.as_ptr(),
                data.len(),
            ),
            Mode::Indices => meshopt::ffi::meshopt_decodeIndexSequence(
                out.as_mut_ptr().cast(),
                count,
                stride,
                data.as_ptr(),
                data.len(),
            ),
        }
    };
    if code != 0 {
        return Err(DecodeError::Corrupt { code });
    }

    if mode == Mode::Attributes {
        apply_filter(&mut out, count, stride, filter)?;
    }

    Ok(out)
}

/// Apply an `EXT_meshopt_compression` filter in-place to decoded attribute
/// bytes (the output of an `Attributes`-mode decode).
fn apply_filter(
    bytes: &mut [u8],
    count: usize,
    stride: usize,
    filter: Filter,
) -> Result<(), DecodeError> {
    match filter {
        Filter::None => {}
        Filter::Octahedral => {
            if stride != 4 && stride != 8 {
                return Err(DecodeError::InvalidFilterStride { filter, stride });
            }
            unsafe {
                meshopt::ffi::meshopt_decodeFilterOct(bytes.as_mut_ptr().cast(), count, stride);
            }
        }
        Filter::Quaternion => {
            if stride != 8 {
                return Err(DecodeError::InvalidFilterStride { filter, stride });
            }
            unsafe {
                meshopt::ffi::meshopt_decodeFilterQuat(bytes.as_mut_ptr().cast(), count, stride);
            }
        }
        Filter::Exponential => {
            if stride == 0 || stride % 4 != 0 {
                return Err(DecodeError::InvalidFilterStride { filter, stride });
            }
            unsafe {
                meshopt::ffi::meshopt_decodeFilterExp(bytes.as_mut_ptr().cast(), count, stride);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_roundtrip() {
        // 4 vertices × 3 f32 (stride 12)
        #[derive(Clone, Copy, Default, PartialEq, Debug)]
        #[repr(C)]
        struct V([f32; 3]);
        let verts = [
            V([0.0, 0.0, 0.0]),
            V([1.0, 0.0, 0.5]),
            V([0.0, 1.0, -0.5]),
            V([1.0, 1.0, 1.0]),
        ];
        let encoded = meshopt::encode_vertex_buffer(&verts).unwrap();
        let decoded =
            decode_buffer_view(&encoded, verts.len(), 12, Mode::Attributes, Filter::None).unwrap();
        let out: &[V] = unsafe { std::slice::from_raw_parts(decoded.as_ptr().cast(), verts.len()) };
        assert_eq!(out, &verts);
    }

    #[test]
    fn index_roundtrip() {
        let indices: [u32; 6] = [0, 1, 2, 2, 1, 3];
        let encoded = meshopt::encode_index_buffer(&indices, 4).unwrap();
        let decoded =
            decode_buffer_view(&encoded, indices.len(), 4, Mode::Triangles, Filter::None).unwrap();
        let out: &[u32] =
            unsafe { std::slice::from_raw_parts(decoded.as_ptr().cast(), indices.len()) };
        assert_eq!(out, &indices);
    }

    #[test]
    fn corrupt_stream_fails_predictably() {
        let garbage = [0xFFu8; 16];
        assert!(matches!(
            decode_buffer_view(&garbage, 12, 12, Mode::Attributes, Filter::None),
            Err(DecodeError::Corrupt { .. })
        ));
    }

    #[test]
    fn oversized_count_rejected() {
        assert!(matches!(
            decode_buffer_view(
                &[0u8; 4],
                usize::MAX / 2,
                12,
                Mode::Attributes,
                Filter::None
            ),
            Err(DecodeError::SizeOverflow { .. })
        ));
    }
}
