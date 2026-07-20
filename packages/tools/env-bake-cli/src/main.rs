//! `awsm-renderer-env-bake` — offline skybox/IBL cubemap packer.
//!
//! Takes cmgen's EXR cube faces and writes the three KTX2 cubemaps the
//! renderer loads (`skybox.ktx2`, `env.ktx2`, `irradiance.ktx2`), in either
//! `B10G11R11_UFLOAT_PACK32` or `BC6H_UFLOAT_BLOCK`.
//!
//! ## Why this exists rather than `ktx create`
//!
//! The Khronos `ktx` CLI cannot *encode* BC6H — it only carries pre-encoded
//! blocks via `--raw`. BC6H is the format that matters here: it is the only
//! block format in the renderer's ladder that survives HDR, and it stays
//! compressed in VRAM at 1 byte/texel against `B10G11R11`'s 4 (a 4x saving,
//! e.g. a 2048 skybox drops 134 MB -> 34 MB).
//!
//! BasisU is deliberately *not* in this path. Its universal ETC1S/UASTC
//! targets are LDR: pushing an environment map through them clips everything
//! above 1.0, which is precisely the range IBL depends on. Environment maps
//! are also a handful of baked assets rather than the hundreds of material
//! textures that make Basis's transcode-per-device indirection worth its cost,
//! so shipping BC6H directly keeps the runtime path pure Rust with no worker
//! round-trip.
//!
//! ## Pipeline
//!
//! ```text
//! myHDR.hdr --[cmgen]--> EXR faces --[this tool]--> *.ktx2 --> renderer
//! ```
//!
//! cmgen still does the heavy lifting — equirect projection, GGX prefiltering,
//! irradiance convolution. This tool only replaces the `ktx create` packing
//! step. See `docs/DEVELOPMENT.md` for the cmgen invocations.

mod encode;
mod faces;
mod ktx2_write;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use rayon::prelude::*;

use ktx2_write::{Container, Level};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    /// `BC6H_UFLOAT_BLOCK` — 1 byte/texel, stays compressed in VRAM. Needs the
    /// WebGPU `texture-compression-bc` feature.
    Bc6h,
    /// `B10G11R11_UFLOAT_PACK32` — 4 bytes/texel, uncompressed. Loads
    /// everywhere; the fallback for devices without BC support.
    Rg11b10,
}

impl From<Format> for Container {
    fn from(f: Format) -> Self {
        match f {
            Format::Bc6h => Container::Bc6h,
            Format::Rg11b10 => Container::Rg11b10,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "awsm-renderer-env-bake",
    version,
    about = "Pack cmgen EXR cube faces into the renderer's skybox/IBL KTX2 cubemaps (offline).",
    long_about = None,
)]
struct Args {
    /// cmgen `-x` output dir (contains `m0_px.exr` …). Becomes `skybox.ktx2`.
    #[arg(long, value_name = "DIR")]
    skybox_faces: Option<PathBuf>,

    /// cmgen `--ibl-ld` output dir (contains `m0_px.exr` … `m5_nz.exr`).
    /// Becomes `env.ktx2`.
    #[arg(long, value_name = "DIR")]
    specular_faces: Option<PathBuf>,

    /// cmgen `--ibl-irradiance` output dir (contains `i_px.exr` …).
    /// Becomes `irradiance.ktx2`.
    #[arg(long, value_name = "DIR")]
    irradiance_faces: Option<PathBuf>,

    /// Directory to write the `.ktx2` files into.
    #[arg(long, value_name = "DIR")]
    out: PathBuf,

    /// On-disk texel format.
    #[arg(long, value_enum, default_value_t = Format::Bc6h)]
    format: Format,

    /// Appended to each output stem, e.g. `--suffix _rg11b10` writes
    /// `skybox_rg11b10.ktx2`. Lets both formats share one directory.
    #[arg(long, default_value = "")]
    suffix: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.skybox_faces.is_none()
        && args.specular_faces.is_none()
        && args.irradiance_faces.is_none()
    {
        anyhow::bail!(
            "nothing to do — pass at least one of --skybox-faces, --specular-faces, \
             --irradiance-faces"
        );
    }

    std::fs::create_dir_all(&args.out)
        .with_context(|| format!("creating {}", args.out.display()))?;

    let container: Container = args.format.into();

    for (input, stem) in [
        (&args.skybox_faces, "skybox"),
        (&args.specular_faces, "env"),
        (&args.irradiance_faces, "irradiance"),
    ] {
        let Some(dir) = input else { continue };
        let out = args.out.join(format!("{stem}{}.ktx2", args.suffix));
        bake(dir, &out, container).with_context(|| format!("baking {}", out.display()))?;
    }

    Ok(())
}

fn bake(faces_dir: &Path, out: &Path, container: Container) -> Result<()> {
    let chain = faces::load_chain(faces_dir)?;
    let base = &chain[0];
    println!(
        "{}: {} level(s), {}x{} base, {:?}",
        faces_dir.display(),
        chain.len(),
        base.width,
        base.height,
        container
    );

    // Encoding dominates the run (BC6H on a 2048 cube is tens of millions of
    // texels), and every face is independent — so flatten to one work item per
    // face and let rayon fill all cores rather than stepping level by level.
    let mut jobs: Vec<(usize, usize)> = Vec::new();
    for l in 0..chain.len() {
        for f in 0..6 {
            jobs.push((l, f));
        }
    }
    let encoded: Vec<((usize, usize), Vec<u8>)> = jobs
        .par_iter()
        .map(|&(l, f)| {
            let level = &chain[l];
            let bytes = encode::encode_face(container, &level.faces[f], level.width, level.height);
            ((l, f), bytes)
        })
        .collect();

    let mut levels: Vec<Level> = chain
        .iter()
        .map(|set| Level {
            width: set.width,
            height: set.height,
            faces: vec![Vec::new(); 6],
        })
        .collect();
    for ((l, f), bytes) in encoded {
        levels[l].faces[f] = bytes;
    }

    let writer_id = concat!("awsm-renderer-env-bake v", env!("CARGO_PKG_VERSION"));
    let bytes = ktx2_write::write_cubemap(container, &levels, writer_id)?;

    std::fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;
    println!("  -> {} ({} bytes)", out.display(), bytes.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic cubemap and read it back with `ktx2` — the same crate
    /// `renderer-core/src/cubemap/ktx.rs` parses with — asserting every
    /// constraint `create_texture` enforces before upload.
    fn round_trip(container: Container, base: u32, level_count: u32) {
        let levels: Vec<Level> = (0..level_count)
            .map(|i| {
                let size = (base >> i).max(1);
                Level {
                    width: size,
                    height: size,
                    faces: (0..6)
                        .map(|f| vec![(f as u8).wrapping_mul(17); container.face_bytes(size, size)])
                        .collect(),
                }
            })
            .collect();

        let bytes = ktx2_write::write_cubemap(container, &levels, "test").unwrap();
        let reader = ktx2::Reader::new(bytes).expect("ktx2 crate must parse our output");
        let header = reader.header();

        assert_eq!(header.face_count, 6, "must be a cubemap");
        assert_eq!(header.layer_count, 0, "must not be an array texture");
        assert_eq!(header.pixel_depth, 0, "must not be 3D");
        assert!(
            header.supercompression_scheme.is_none(),
            "the cubemap loader rejects any supercompression"
        );
        assert_eq!(header.pixel_width, base);
        assert_eq!(header.level_count, level_count);

        // The loader's per-level length check: faces * rows * tight_bpr, with
        // no per-face padding tolerated.
        for (i, level) in reader.levels().enumerate() {
            let size = (base >> i).max(1);
            assert_eq!(
                level.data.len(),
                container.face_bytes(size, size) * 6,
                "level {i} length must match 6 tightly-packed faces"
            );
        }
    }

    #[test]
    fn bc6h_cubemap_round_trips() {
        round_trip(Container::Bc6h, 64, 5); // 64..4, the BC6H block floor
    }

    #[test]
    fn rg11b10_cubemap_round_trips() {
        round_trip(Container::Rg11b10, 64, 7); // 64..1
    }

    #[test]
    fn single_level_cubemap_round_trips() {
        // The irradiance shape: one 64x64 level, sampled at mip 0 only.
        round_trip(Container::Bc6h, 64, 1);
        round_trip(Container::Rg11b10, 64, 1);
    }

    #[test]
    fn level_data_is_stored_smallest_first() {
        // Matches `ktx create` layout: level 0 is largest but sits last.
        let levels: Vec<Level> = (0..4)
            .map(|i| {
                let size = 64u32 >> i;
                Level {
                    width: size,
                    height: size,
                    faces: (0..6)
                        .map(|_| vec![0u8; Container::Bc6h.face_bytes(size, size)])
                        .collect(),
                }
            })
            .collect();
        let bytes = ktx2_write::write_cubemap(Container::Bc6h, &levels, "test").unwrap();

        // The `ktx2` crate hands back level *data* but not its file offset, so
        // read the level index straight out of the header: it starts at
        // identifier(12) + header(36) + index(32), 24 bytes per entry.
        const LEVEL_INDEX: usize = 12 + 36 + 32;
        let offsets: Vec<u64> = (0..levels.len())
            .map(|i| {
                let at = LEVEL_INDEX + i * 24;
                u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
            })
            .collect();
        for w in offsets.windows(2) {
            assert!(
                w[0] > w[1],
                "level offsets must descend (smallest mip stored first), got {offsets:?}"
            );
        }
    }

    #[test]
    fn rejects_wrong_face_count() {
        let levels = vec![Level {
            width: 4,
            height: 4,
            faces: vec![vec![0u8; 16]; 5], // 5 faces, not 6
        }];
        assert!(ktx2_write::write_cubemap(Container::Bc6h, &levels, "t").is_err());
    }

    #[test]
    fn rejects_untightly_packed_face() {
        let levels = vec![Level {
            width: 4,
            height: 4,
            faces: vec![vec![0u8; 32]; 6], // padded: 32 bytes for a 16-byte block
        }];
        assert!(ktx2_write::write_cubemap(Container::Bc6h, &levels, "t").is_err());
    }
}
