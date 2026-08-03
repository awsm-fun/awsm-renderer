//! KTX2 mip-chain truncation — shrink a (cube)map by DROPPING its largest mip
//! levels, no re-encode.
//!
//! A mipmapped KTX2 already contains every smaller size of itself: capping a
//! 4096-face BC6H skybox at 1024 just means shipping the file from mip 2 down.
//! That is pure container surgery — copy the surviving level payloads and
//! rewrite the header/index — so it works for ANY block format (BC6H/UASTC/…)
//! without a decoder, and the result is bit-identical to what the GPU would
//! have sampled at that resolution anyway. Powers
//! `BundleOptions::env_max_face_size`.
//!
//! Layout (KTX2 §2, all little-endian, absolute offsets):
//! ```text
//!  0  identifier[12]
//! 12  vkFormat u32        16 typeSize u32
//! 20  pixelWidth u32      24 pixelHeight u32     28 pixelDepth u32
//! 32  layerCount u32      36 faceCount u32       40 levelCount u32
//! 44  supercompressionScheme u32
//! 48  dfdByteOffset u32   52 dfdByteLength u32
//! 56  kvdByteOffset u32   60 kvdByteLength u32
//! 64  sgdByteOffset u64   72 sgdByteLength u64
//! 80  levelIndex: levelCount × { byteOffset u64, byteLength u64,
//!                                uncompressedByteLength u64 }
//! ```
//! Level 0 is the LARGEST mip.

/// The 12-byte KTX2 identifier.
const KTX2_MAGIC: [u8; 12] = [
    0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xAB, 0x0D, 0x0A, 0x1A, 0x0A,
];

/// Result of a truncation attempt.
pub enum Shrink {
    /// Already fits (or has no mip chain to drop) — ship the original bytes.
    Unchanged,
    /// Rebuilt container with the largest levels removed.
    Truncated { bytes: Vec<u8>, new_size: u32 },
}

fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64le(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Drop the largest mip levels of `bytes` (a KTX2 file) until its base size
/// is at most `max_size` px per side. Returns `None` on anything unexpected
/// (not KTX2, truncated file, BasisLZ supercompression — whose global data
/// indexes every level and would be invalidated, 3D textures) so the caller
/// can ship the original verbatim; never fails an export.
pub fn truncate_ktx2_to_max_size(bytes: &[u8], max_size: u32) -> Option<Shrink> {
    if bytes.len() < 80 || bytes[..12] != KTX2_MAGIC {
        return None;
    }
    let max_size = max_size.max(1);
    let pixel_width = u32le(bytes, 20)?;
    let pixel_height = u32le(bytes, 24)?;
    let pixel_depth = u32le(bytes, 28)?;
    let level_count = u32le(bytes, 40)?.max(1) as usize;
    let supercompression = u32le(bytes, 44)?;
    let dfd_off = u32le(bytes, 48)? as usize;
    let dfd_len = u32le(bytes, 52)? as usize;
    let kvd_off = u32le(bytes, 56)? as usize;
    let kvd_len = u32le(bytes, 60)? as usize;
    let sgd_off = u64le(bytes, 64)? as usize;
    let sgd_len = u64le(bytes, 72)? as usize;

    if pixel_width.max(pixel_height) <= max_size {
        return Some(Shrink::Unchanged);
    }
    // BasisLZ (scheme 1): the SGD's imageDescs index every level — dropping
    // levels would corrupt it. None (0) and Zstd (2) carry no per-level SGD.
    if supercompression == 1 {
        return None;
    }
    // 3D textures halve depth per level too; refuse rather than special-case
    // (environment cubemaps are never 3D).
    if pixel_depth > 1 {
        return None;
    }

    // How many levels must go so max(w,h) fits? One halving per dropped level.
    let mut drop = 0usize;
    let (mut w, mut h) = (pixel_width, pixel_height.max(1));
    while w.max(h) > max_size && drop + 1 < level_count {
        w = (w / 2).max(1);
        h = (h / 2).max(1);
        drop += 1;
    }
    if drop == 0 {
        return Some(Shrink::Unchanged);
    }
    if w.max(h) > max_size {
        tracing::warn!(
            "ktx2 shrink: only {level_count} mip level(s) available — capping at {w}x{h} \
             instead of {max_size}"
        );
    }

    // Surviving level entries (drop the first `drop` = largest).
    struct Level {
        offset: usize,
        byte_length: u64,
        uncompressed: u64,
    }
    let mut levels = Vec::with_capacity(level_count - drop);
    for i in drop..level_count {
        let e = 80 + i * 24;
        let offset = u64le(bytes, e)? as usize;
        let byte_length = u64le(bytes, e + 8)?;
        let uncompressed = u64le(bytes, e + 16)?;
        bytes.get(offset..offset.checked_add(byte_length as usize)?)?;
        levels.push(Level {
            offset,
            byte_length,
            uncompressed,
        });
    }
    let dfd = bytes.get(dfd_off..dfd_off.checked_add(dfd_len)?)?;
    let kvd = bytes.get(kvd_off..kvd_off.checked_add(kvd_len)?)?;
    let sgd = bytes.get(sgd_off..sgd_off.checked_add(sgd_len)?)?;

    // ── rebuild ─────────────────────────────────────────────────────────
    let new_level_count = levels.len();
    let mut out = Vec::with_capacity(bytes.len());
    // Identifier + header verbatim, then patch the fields that change.
    out.extend_from_slice(&bytes[..80]);
    out[20..24].copy_from_slice(&w.to_le_bytes());
    // A 1D texture writes pixelHeight = 0; preserve that convention.
    let out_h = if pixel_height == 0 { 0 } else { h };
    out[24..28].copy_from_slice(&out_h.to_le_bytes());
    out[40..44].copy_from_slice(&(new_level_count as u32).to_le_bytes());

    // Placeholder level index; patched once payload offsets are known.
    let level_index_at = out.len();
    out.resize(out.len() + new_level_count * 24, 0);

    // DFD (its first word, dfdTotalSize, equals dfd_len — copied verbatim).
    let new_dfd_off = out.len();
    out.extend_from_slice(dfd);
    let new_kvd_off = out.len();
    out.extend_from_slice(kvd);
    // SGD is 8-aligned when present.
    let new_sgd_off = if sgd_len > 0 {
        while out.len() % 8 != 0 {
            out.push(0);
        }
        let o = out.len();
        out.extend_from_slice(sgd);
        o
    } else {
        0
    };

    // Level payloads. Offsets are explicit in the index, so emit in index
    // order with conservative 16-byte alignment (a multiple of every
    // block-compressed texel size in use: 8 or 16).
    let mut placed = Vec::with_capacity(new_level_count);
    for lvl in &levels {
        while out.len() % 16 != 0 {
            out.push(0);
        }
        let o = out.len();
        out.extend_from_slice(&bytes[lvl.offset..lvl.offset + lvl.byte_length as usize]);
        placed.push((o as u64, lvl.byte_length, lvl.uncompressed));
    }
    for (i, (off, len, unc)) in placed.iter().enumerate() {
        let e = level_index_at + i * 24;
        out[e..e + 8].copy_from_slice(&off.to_le_bytes());
        out[e + 8..e + 16].copy_from_slice(&len.to_le_bytes());
        out[e + 16..e + 24].copy_from_slice(&unc.to_le_bytes());
    }
    // Patch the file index (everything after the level index moved).
    out[48..52].copy_from_slice(&(new_dfd_off as u32).to_le_bytes());
    out[52..56].copy_from_slice(&(dfd_len as u32).to_le_bytes());
    out[56..60].copy_from_slice(&(new_kvd_off as u32).to_le_bytes());
    out[60..64].copy_from_slice(&(kvd_len as u32).to_le_bytes());
    out[64..72].copy_from_slice(&(new_sgd_off as u64).to_le_bytes());
    out[72..80].copy_from_slice(&(if sgd_len > 0 { sgd_len as u64 } else { 0 }).to_le_bytes());

    Some(Shrink::Truncated {
        bytes: out,
        new_size: w.max(h),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal synthetic KTX2: `levels` mips of a `base`² 2D texture,
    /// each level's payload a distinct filler byte, tiny DFD/KVD, no SGD.
    fn synthetic(base: u32, levels: u32) -> Vec<u8> {
        let dfd: Vec<u8> = {
            // dfdTotalSize word + 4 dummy bytes.
            let mut d = 8u32.to_le_bytes().to_vec();
            d.extend_from_slice(&[1, 2, 3, 4]);
            d
        };
        let kvd = vec![9u8; 12];
        let mut out = Vec::new();
        out.extend_from_slice(&KTX2_MAGIC);
        let header = [
            0u32, // vkFormat
            1,    // typeSize
            base, // pixelWidth
            base, // pixelHeight
            0,    // pixelDepth
            0,    // layerCount
            1,    // faceCount
            levels, 0, // levelCount, supercompression
        ];
        for v in header {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let dfd_off = 80 + levels as usize * 24;
        let kvd_off = dfd_off + dfd.len();
        let mut data_off = kvd_off + kvd.len();
        // index block
        out.extend_from_slice(&(dfd_off as u32).to_le_bytes());
        out.extend_from_slice(&(dfd.len() as u32).to_le_bytes());
        out.extend_from_slice(&(kvd_off as u32).to_le_bytes());
        out.extend_from_slice(&(kvd.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        // level index + payload layout: level i payload = (i+1) repeated
        // (base>>i) bytes, appended in index order for simplicity.
        let mut payloads = Vec::new();
        for i in 0..levels {
            let len = ((base >> i).max(1)) as usize;
            out.extend_from_slice(&(data_off as u64).to_le_bytes());
            out.extend_from_slice(&(len as u64).to_le_bytes());
            out.extend_from_slice(&(len as u64).to_le_bytes());
            payloads.push(vec![(i + 1) as u8; len]);
            data_off += len;
        }
        out.extend_from_slice(&dfd);
        out.extend_from_slice(&kvd);
        for p in payloads {
            out.extend_from_slice(&p);
        }
        out
    }

    #[test]
    fn truncates_to_cap_and_stays_parseable() {
        let src = synthetic(4096, 6); // 4096..128
        let Shrink::Truncated { bytes, new_size } =
            truncate_ktx2_to_max_size(&src, 1024).expect("parse")
        else {
            panic!("expected truncation");
        };
        assert_eq!(new_size, 1024);
        assert_eq!(u32le(&bytes, 20).unwrap(), 1024); // width
        assert_eq!(u32le(&bytes, 24).unwrap(), 1024); // height
        assert_eq!(u32le(&bytes, 40).unwrap(), 4); // levels 2..5 survive
                                                   // First surviving level's payload is the old level 2 (filler byte 3).
        let l0_off = u64le(&bytes, 80).unwrap() as usize;
        let l0_len = u64le(&bytes, 88).unwrap() as usize;
        assert_eq!(l0_len, 1024);
        assert!(bytes[l0_off..l0_off + l0_len].iter().all(|&b| b == 3));
        // DFD/KVD survived at their new offsets.
        let dfd_off = u32le(&bytes, 48).unwrap() as usize;
        assert_eq!(&bytes[dfd_off + 4..dfd_off + 8], &[1, 2, 3, 4]);
        let kvd_off = u32le(&bytes, 56).unwrap() as usize;
        assert!(bytes[kvd_off..kvd_off + 12].iter().all(|&b| b == 9));
        // Idempotent: already-fitting input is Unchanged.
        assert!(matches!(
            truncate_ktx2_to_max_size(&bytes, 1024),
            Some(Shrink::Unchanged)
        ));
    }

    #[test]
    fn refuses_basislz_and_garbage() {
        let mut src = synthetic(4096, 6);
        src[44..48].copy_from_slice(&1u32.to_le_bytes()); // BasisLZ
        assert!(truncate_ktx2_to_max_size(&src, 1024).is_none());
        assert!(truncate_ktx2_to_max_size(b"not a ktx2 file at all!!", 1024).is_none());
    }

    #[test]
    fn caps_at_smallest_available_mip() {
        let src = synthetic(4096, 2); // only 4096 + 2048
        let Shrink::Truncated { new_size, .. } =
            truncate_ktx2_to_max_size(&src, 256).expect("parse")
        else {
            panic!("expected truncation");
        };
        assert_eq!(new_size, 2048); // best it can do without re-encoding
    }
}
