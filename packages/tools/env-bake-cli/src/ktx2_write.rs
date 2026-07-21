//! Minimal KTX2 container writer for cubemaps — enough to emit exactly what
//! `renderer-core/src/cubemap/ktx.rs` loads, and nothing more.
//!
//! Deliberately narrow: uncompressed (`supercompressionScheme = 0`) cubemaps,
//! `layerCount = 0`, `pixelDepth = 0`, no supercompression global data. Those
//! are the only shapes the cubemap loader accepts, so anything wider would be
//! untestable dead weight.
//!
//! Two layout rules are easy to get wrong and are the reason this module
//! exists rather than hand-rolling bytes at the call site:
//!
//! 1. **Levels are stored smallest-mip-first.** The level *index* is ordered
//!    level 0..N-1 (largest first), but the offsets it holds run backwards:
//!    the last level sits lowest in the file. Verified against
//!    `ktx create` output.
//! 2. **Each level starts on an `lcm(texel_block_bytes, 4)` boundary** — 4 for
//!    `B10G11R11_UFLOAT_PACK32`, 16 for `BC6H_UFLOAT_BLOCK`.
//!
//! The Data Format Descriptors are transcribed from `ktx create v4.4.2`
//! output rather than derived from the Khronos Data Format spec — several
//! fields (the BC6H `colorModel` value, the `sampleUpper` float encoding) are
//! easy to get subtly wrong from prose, and a mismatched DFD produces a file
//! that loads fine here but fails strict validators.

use anyhow::{bail, Result};

/// The KTX2 file identifier: «KTX 20»\r\n\x1A\n
const IDENTIFIER: [u8; 12] = [
    0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];

/// `VK_FORMAT_B10G11R11_UFLOAT_PACK32` — WebGPU `Rg11b10ufloat`.
pub const VK_FORMAT_B10G11R11_UFLOAT_PACK32: u32 = 122;
/// `VK_FORMAT_BC6H_UFLOAT_BLOCK` — WebGPU `Bc6hRgbUfloat`.
pub const VK_FORMAT_BC6H_UFLOAT_BLOCK: u32 = 143;

/// Which container format a bake targets. Both are HDR; they differ in whether
/// the data stays block-compressed in VRAM.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Container {
    /// 4 bytes/texel, uncompressed. Universally loadable — the fallback for
    /// devices without `texture-compression-bc`.
    Rg11b10,
    /// 1 byte/texel (16 bytes per 4x4 block). Stays compressed in VRAM.
    /// Requires the WebGPU `texture-compression-bc` feature.
    Bc6h,
}

impl Container {
    pub fn vk_format(self) -> u32 {
        match self {
            Self::Rg11b10 => VK_FORMAT_B10G11R11_UFLOAT_PACK32,
            Self::Bc6h => VK_FORMAT_BC6H_UFLOAT_BLOCK,
        }
    }

    /// `typeSize` per the KTX2 spec: the packed-word size for packed formats,
    /// 1 for block-compressed ones.
    pub fn type_size(self) -> u32 {
        match self {
            Self::Rg11b10 => 4,
            Self::Bc6h => 1,
        }
    }

    /// Bytes occupied by one texel block. For uncompressed formats the "block"
    /// is a single texel.
    pub fn block_bytes(self) -> u32 {
        match self {
            Self::Rg11b10 => 4,
            Self::Bc6h => 16,
        }
    }

    /// `(block_width, block_height)` in texels.
    pub fn block_dims(self) -> (u32, u32) {
        match self {
            Self::Rg11b10 => (1, 1),
            Self::Bc6h => (4, 4),
        }
    }

    /// Tight byte size of one cube face at `width`x`height`. Mirrors
    /// `renderer-core`'s `mip_level_byte_size`, which the loader validates
    /// each level against.
    pub fn face_bytes(self, width: u32, height: u32) -> usize {
        let (bw, bh) = self.block_dims();
        (width.div_ceil(bw) * self.block_bytes()) as usize * height.div_ceil(bh) as usize
    }

    /// Level data alignment: `lcm(texel_block_bytes, 4)`.
    fn level_alignment(self) -> usize {
        match self {
            Self::Rg11b10 => 4,
            Self::Bc6h => 16,
        }
    }

    /// Data Format Descriptor, transcribed from `ktx create v4.4.2` output.
    fn dfd(self) -> Vec<u8> {
        // Common to both: BT709 primaries, LINEAR transfer, ALPHA_STRAIGHT
        // flags, sampleLower = 0, sampleUpper = 1.0f (0x3f800000).
        let words: &[u32] = match self {
            // Model BC6H (0x85), 4x4x1x1 blocks, bytesPlane0 = 16, one FLOAT
            // sample spanning all 128 bits (bitLength stored as len-1 = 127).
            Self::Bc6h => &[
                0x0000_002c, // dfdTotalSize = 44
                0x0000_0000, // vendorId = 0, descriptorType = 0
                0x0028_0002, // versionNumber = 2, descriptorBlockSize = 40
                0x0001_0185, // model=0x85(BC6H) primaries=1 transfer=1 flags=0
                0x0000_0303, // texelBlockDimension = 4,4,1,1 (stored as n-1)
                0x0000_0010, // bytesPlane0 = 16
                0x0000_0000, // bytesPlane4..7 = 0
                0x807f_0000, // sample0: bitOffset=0 bitLength=127 FLOAT ch0
                0x0000_0000, // samplePosition
                0x0000_0000, // sampleLower = 0
                0x3f80_0000, // sampleUpper = 1.0f
            ],
            // Model RGBSDA (1), 1x1 blocks, bytesPlane0 = 4, three FLOAT
            // samples packed R[0..11) G[11..22) B[22..32).
            Self::Rg11b10 => &[
                0x0000_004c, // dfdTotalSize = 76
                0x0000_0000,
                0x0048_0002, // descriptorBlockSize = 72
                0x0001_0001, // model=1(RGBSDA) primaries=1 transfer=1 flags=0
                0x0000_0000, // texelBlockDimension = 1,1,1,1
                0x0000_0004, // bytesPlane0 = 4
                0x0000_0000,
                0x800a_0000, // R: bitOffset=0  bitLength=11 FLOAT ch0
                0x0000_0000,
                0x0000_0000,
                0x3f80_0000,
                0x810a_000b, // G: bitOffset=11 bitLength=11 FLOAT ch1
                0x0000_0000,
                0x0000_0000,
                0x3f80_0000,
                0x8209_0016, // B: bitOffset=22 bitLength=10 FLOAT ch2
                0x0000_0000,
                0x0000_0000,
                0x3f80_0000,
            ],
        };
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
}

/// One mip level's payload: the six cube faces, already encoded, in
/// px, nx, py, ny, pz, nz order.
pub struct Level {
    pub width: u32,
    pub height: u32,
    /// Exactly 6 entries, each `Container::face_bytes(width, height)` long.
    pub faces: Vec<Vec<u8>>,
}

/// Serialize a cubemap to KTX2 bytes.
///
/// `levels` must be ordered largest-first (level 0 = base). Each level's
/// dimensions must halve, and every face must be tightly packed — the loader
/// rejects per-face padding outright.
pub fn write_cubemap(container: Container, levels: &[Level], writer_id: &str) -> Result<Vec<u8>> {
    if levels.is_empty() {
        bail!("cubemap must have at least one mip level");
    }

    for (i, level) in levels.iter().enumerate() {
        if level.faces.len() != 6 {
            bail!(
                "level {i} has {} faces, expected 6 (px, nx, py, ny, pz, nz)",
                level.faces.len()
            );
        }
        let expected = container.face_bytes(level.width, level.height);
        for (f, face) in level.faces.iter().enumerate() {
            if face.len() != expected {
                bail!(
                    "level {i} face {f}: {} bytes, expected {expected} for {}x{} — \
                     tightly-packed faces are required",
                    face.len(),
                    level.width,
                    level.height
                );
            }
        }
    }

    let base = &levels[0];
    let dfd = container.dfd();
    let kvd = key_value_data(writer_id);

    // --- lay out the file ------------------------------------------------
    // identifier(12) + header(36) + index(32) + levelIndex(24 * levelCount)
    let level_index_off = 12 + 36 + 32;
    let dfd_off = level_index_off + 24 * levels.len();
    let kvd_off = dfd_off + dfd.len();
    // No supercompression global data, so sgd offset/length stay 0 and the
    // level data follows the KVD (aligned).
    let mut cursor = kvd_off + kvd.len();

    // Levels are stored SMALLEST-FIRST, so walk them in reverse and record
    // each offset back into its level-index slot.
    let align = container.level_alignment();
    let mut level_offsets = vec![0u64; levels.len()];
    let mut level_lengths = vec![0u64; levels.len()];
    for (i, level) in levels.iter().enumerate().rev() {
        cursor = cursor.next_multiple_of(align);
        let len: usize = level.faces.iter().map(|f| f.len()).sum();
        level_offsets[i] = cursor as u64;
        level_lengths[i] = len as u64;
        cursor += len;
    }
    let total = cursor;

    // --- emit -------------------------------------------------------------
    let mut out = vec![0u8; total];
    out[..12].copy_from_slice(&IDENTIFIER);

    let mut w = Writer::new(&mut out, 12);
    w.u32(container.vk_format());
    w.u32(container.type_size());
    w.u32(base.width);
    w.u32(base.height);
    w.u32(0); // pixelDepth — 2D
    w.u32(0); // layerCount — not an array
    w.u32(6); // faceCount — cubemap
    w.u32(levels.len() as u32);
    w.u32(0); // supercompressionScheme — none

    w.u32(dfd_off as u32);
    w.u32(dfd.len() as u32);
    w.u32(kvd_off as u32);
    w.u32(kvd.len() as u32);
    w.u64(0); // sgdByteOffset
    w.u64(0); // sgdByteLength

    for i in 0..levels.len() {
        w.u64(level_offsets[i]);
        w.u64(level_lengths[i]);
        // Uncompressed: uncompressedByteLength == byteLength.
        w.u64(level_lengths[i]);
    }

    out[dfd_off..dfd_off + dfd.len()].copy_from_slice(&dfd);
    out[kvd_off..kvd_off + kvd.len()].copy_from_slice(&kvd);

    for (i, level) in levels.iter().enumerate() {
        let mut at = level_offsets[i] as usize;
        for face in &level.faces {
            out[at..at + face.len()].copy_from_slice(face);
            at += face.len();
        }
    }

    Ok(out)
}

/// A single `KTXwriter` entry. Each KVD entry is a u32 byte length followed by
/// a NUL-terminated key, its value, then padding to the next 4-byte boundary.
fn key_value_data(writer_id: &str) -> Vec<u8> {
    const KEY: &str = "KTXwriter";
    let mut payload = Vec::new();
    payload.extend_from_slice(KEY.as_bytes());
    payload.push(0);
    payload.extend_from_slice(writer_id.as_bytes());
    payload.push(0);

    let mut kvd = Vec::new();
    kvd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    kvd.extend_from_slice(&payload);
    while kvd.len() % 4 != 0 {
        kvd.push(0);
    }
    kvd
}

/// Tiny little-endian cursor — keeps the header emit above readable.
struct Writer<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl<'a> Writer<'a> {
    fn new(buf: &'a mut [u8], at: usize) -> Self {
        Self { buf, at }
    }
    fn u32(&mut self, v: u32) {
        self.buf[self.at..self.at + 4].copy_from_slice(&v.to_le_bytes());
        self.at += 4;
    }
    fn u64(&mut self, v: u64) {
        self.buf[self.at..self.at + 8].copy_from_slice(&v.to_le_bytes());
        self.at += 8;
    }
}
