//! Discovering and loading cmgen's EXR cube-face output.
//!
//! cmgen emits two layouts, both of which land in a directory named after the
//! input file's basename:
//!
//! - **Mipped** (`-x` skybox, `--ibl-ld` specular): `m{N}_{face}.exr` for each
//!   level. `-x` additionally writes unprefixed `{face}.exr` duplicates of
//!   level 0, which we ignore in favour of the `m0_` set.
//! - **Single level** (`--ibl-irradiance`): `i_{face}.exr`.
//!
//! Face order is always px, nx, py, ny, pz, nz — the order KTX2 stores cube
//! faces in, and the order the loader slices each level by.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Cube face order, as stored in KTX2.
pub const FACES: [&str; 6] = ["px", "nx", "py", "ny", "pz", "nz"];

/// One mip level's six faces, as linear RGB f32.
pub struct FaceSet {
    pub width: u32,
    pub height: u32,
    /// Six faces in [`FACES`] order, each `width * height * 3` floats.
    pub faces: Vec<Vec<f32>>,
}

/// Load a cmgen output directory as an ordered mip chain (largest first).
///
/// Detects the layout by probing for `m0_px.exr` (mipped) then `i_px.exr`
/// (single level). Mip levels are taken while `m{N}_px.exr` exists, so the
/// chain ends wherever cmgen stopped — for BC6H that is a feature: cmgen's
/// chains bottom out at 16x16, comfortably above the 4x4 block floor.
pub fn load_chain(dir: &Path) -> Result<Vec<FaceSet>> {
    if dir.join("m0_px.exr").is_file() {
        let mut chain = Vec::new();
        for level in 0.. {
            if !dir.join(format!("m{level}_px.exr")).is_file() {
                break;
            }
            chain.push(
                load_face_set(dir, &|face| format!("m{level}_{face}.exr"))
                    .with_context(|| format!("loading mip level {level} from {}", dir.display()))?,
            );
        }
        validate_chain(&chain, dir)?;
        Ok(chain)
    } else if dir.join("i_px.exr").is_file() {
        let set = load_face_set(dir, &|face| format!("i_{face}.exr"))
            .with_context(|| format!("loading irradiance faces from {}", dir.display()))?;
        Ok(vec![set])
    } else {
        bail!(
            "no cmgen face set in {} — expected m0_px.exr (mipped) or i_px.exr (irradiance)",
            dir.display()
        )
    }
}

/// Each level must be square and exactly half the previous one — the loader
/// derives mip dimensions by shifting, so anything else silently misaligns.
fn validate_chain(chain: &[FaceSet], dir: &Path) -> Result<()> {
    if chain.is_empty() {
        bail!("no mip levels found in {}", dir.display());
    }
    for (i, level) in chain.iter().enumerate() {
        let expected_w = (chain[0].width >> i).max(1);
        let expected_h = (chain[0].height >> i).max(1);
        if level.width != expected_w || level.height != expected_h {
            bail!(
                "{}: mip {i} is {}x{}, expected {expected_w}x{expected_h} — \
                 the loader derives mip dimensions by halving",
                dir.display(),
                level.width,
                level.height
            );
        }
    }
    Ok(())
}

fn load_face_set(dir: &Path, name: &dyn Fn(&str) -> String) -> Result<FaceSet> {
    let mut faces = Vec::with_capacity(6);
    let mut dims: Option<(u32, u32)> = None;

    for face in FACES {
        let path = dir.join(name(face));
        let (w, h, pixels) =
            read_exr_rgb(&path).with_context(|| format!("reading {}", path.display()))?;
        match dims {
            None => dims = Some((w, h)),
            Some((dw, dh)) if (dw, dh) != (w, h) => bail!(
                "{}: face {face} is {w}x{h} but a sibling face is {dw}x{dh} — \
                 all six faces must match",
                dir.display()
            ),
            _ => {}
        }
        faces.push(pixels);
    }

    let (width, height) = dims.expect("FACES is non-empty");
    Ok(FaceSet {
        width,
        height,
        faces,
    })
}

/// Scanline buffer for the EXR reader — carries the row stride so the setter
/// callback can index into a flat RGB array.
struct RgbBuf {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

/// Read an EXR as linear RGB f32, row-major, tightly packed.
///
/// Alpha is discarded: cmgen writes opaque faces and neither target format
/// carries alpha.
fn read_exr_rgb(path: &PathBuf) -> Result<(u32, u32, Vec<f32>)> {
    use exr::prelude::*;

    let image = read_first_rgba_layer_from_file(
        path,
        |resolution, _: &RgbaChannels| RgbBuf {
            width: resolution.width(),
            height: resolution.height(),
            data: vec![0.0; resolution.width() * resolution.height() * 3],
        },
        |buf: &mut RgbBuf, position, (r, g, b, _a): (f32, f32, f32, f32)| {
            let i = (position.y() * buf.width + position.x()) * 3;
            buf.data[i] = r;
            buf.data[i + 1] = g;
            buf.data[i + 2] = b;
        },
    )?;

    let buf = image.layer_data.channel_data.pixels;
    Ok((buf.width as u32, buf.height as u32, buf.data))
}
