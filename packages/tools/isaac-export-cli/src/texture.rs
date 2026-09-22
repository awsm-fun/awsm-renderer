//! Texture images for the sidecar: copied through when one source image IS a
//! glTF slot, packed into a new PNG when glTF wants channels USD keeps apart.
//!
//! glTF (and so the sidecar) has five fixed slots with fixed channels — base
//! colour RGB + alpha in A, roughness in G and metallic in B of ONE image,
//! occlusion in R. USD materials spread the same data over separate images
//! (OmniPBR: a roughness map, a metallic map, an opacity map), so those get
//! packed. Everything that maps one-to-one ships as the original bytes, never
//! re-encoded.
//!
//! Output files are content-addressed — `textures/<stem>-<hash>.<ext>` — so
//! two models exported into one directory share identical images and never
//! overwrite each other's different ones.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::{DynamicImage, GrayImage, ImageFormat, Luma, Rgba, RgbaImage};
use sha2::{Digest, Sha256};

/// Directory, relative to the sidecar, that texture files are written under.
pub const DIR: &str = "textures";

/// Which channel of an image a USD input reads. `Rgb` is a colour input; the
/// mono ones are single-channel inputs (a roughness or opacity map).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    Rgb,
    R,
    G,
    B,
    A,
    /// OmniPBR `mono_average`, the default for its mono inputs.
    Average,
    /// OmniPBR `mono_luminance`.
    Luminance,
    /// OmniPBR `mono_maximum`.
    Maximum,
}

impl Channel {
    /// OmniPBR's `base::mono_mode` enum as authored in USD (an int).
    pub fn from_mono_mode(mode: i64) -> Self {
        match mode {
            0 => Channel::A,
            2 => Channel::Luminance,
            3 => Channel::Maximum,
            _ => Channel::Average,
        }
    }

    /// A `UsdUVTexture` output name (`outputs:r`, `outputs:rgb`, ...).
    pub fn from_output(name: &str) -> Self {
        match name.rsplit(':').next().unwrap_or(name) {
            "r" => Channel::R,
            "g" => Channel::G,
            "b" => Channel::B,
            "a" => Channel::A,
            _ => Channel::Rgb,
        }
    }

    fn sample(self, p: &Rgba<u8>) -> u8 {
        let [r, g, b, a] = p.0;
        match self {
            Channel::R | Channel::Rgb => r,
            Channel::G => g,
            Channel::B => b,
            Channel::A => a,
            Channel::Average => ((r as u16 + g as u16 + b as u16) / 3) as u8,
            Channel::Luminance => {
                (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32).round() as u8
            }
            Channel::Maximum => r.max(g).max(b),
        }
    }
}

/// The images an export writes, keyed by their path relative to the sidecar.
#[derive(Default)]
pub struct Images {
    /// `(relative path, bytes)`, in first-use order.
    pub files: Vec<(String, Vec<u8>)>,
    by_hash: HashMap<String, String>,
    decoded: HashMap<PathBuf, Option<RgbaImage>>,
}

impl Images {
    /// Ship `file` as-is (a base colour or normal map that already is the
    /// glTF slot). Returns its sidecar-relative path.
    pub fn copy(&mut self, file: &Path) -> Result<String> {
        let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        let ext = match image::guess_format(&bytes) {
            Ok(ImageFormat::Png) => "png",
            Ok(ImageFormat::Jpeg) => "jpg",
            // Anything else (WebP, TGA, EXR, ...) is re-encoded to PNG: the
            // sidecar's images are PNG or JPEG, the two formats the editor
            // persists.
            _ => {
                let img = self
                    .decode(file)?
                    .ok_or_else(|| anyhow::anyhow!("cannot decode {}", file.display()))?;
                return self.png(stem(file), &img);
            }
        };
        Ok(self.store(stem(file), ext, bytes))
    }

    /// Encode `img` as a PNG named after `stem`.
    pub fn png(&mut self, stem: &str, img: &RgbaImage) -> Result<String> {
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .context("encoding PNG")?;
        Ok(self.store(stem, "png", bytes))
    }

    fn store(&mut self, stem: &str, ext: &str, bytes: Vec<u8>) -> String {
        let hash = format!("{:x}", Sha256::digest(&bytes));
        if let Some(path) = self.by_hash.get(&hash) {
            return path.clone();
        }
        let path = format!("{DIR}/{}-{}.{ext}", sanitize(stem), &hash[..8]);
        self.by_hash.insert(hash, path.clone());
        self.files.push((path.clone(), bytes));
        path
    }

    /// Decode `file` to RGBA8, cached. `None` when it cannot be read.
    pub fn decode(&mut self, file: &Path) -> Result<Option<RgbaImage>> {
        if let Some(img) = self.decoded.get(file) {
            return Ok(img.clone());
        }
        let img = image::open(file).ok().map(|i| i.to_rgba8());
        self.decoded.insert(file.to_path_buf(), img.clone());
        Ok(img)
    }

    /// One channel of `file` as a greyscale image.
    pub fn channel(&mut self, file: &Path, channel: Channel) -> Result<Option<GrayImage>> {
        let Some(img) = self.decode(file)? else {
            return Ok(None);
        };
        Ok(Some(GrayImage::from_fn(
            img.width(),
            img.height(),
            |x, y| Luma([channel.sample(img.get_pixel(x, y))]),
        )))
    }
}

/// A single-channel source for packing: an image (already reduced to one
/// channel) or a constant, both in 0..=1.
pub enum Mono {
    Image(GrayImage),
    Constant(f32),
}

/// A mono input with OmniPBR's "influence": `lerp(constant, texture, influence)`.
pub fn blend(constant: f32, texture: Option<GrayImage>, influence: f32) -> Mono {
    match texture {
        Some(img) if influence > 0.0 => {
            if influence >= 1.0 {
                return Mono::Image(img);
            }
            let c = constant.clamp(0.0, 1.0) * 255.0;
            Mono::Image(GrayImage::from_fn(img.width(), img.height(), |x, y| {
                let t = img.get_pixel(x, y).0[0] as f32;
                Luma([(c + (t - c) * influence).round().clamp(0.0, 255.0) as u8])
            }))
        }
        _ => Mono::Constant(constant),
    }
}

/// Pack four channels into one RGBA image at the largest input size, resizing
/// smaller images to it. `rgb`, when given, fills R, G and B from a colour
/// image and the three mono slots are ignored for those channels.
pub fn pack(rgb: Option<&RgbaImage>, channels: [&Mono; 4]) -> RgbaImage {
    let mut w = 1;
    let mut h = 1;
    let mut grow = |iw: u32, ih: u32| {
        w = w.max(iw);
        h = h.max(ih);
    };
    if let Some(img) = rgb {
        grow(img.width(), img.height());
    }
    for c in channels {
        if let Mono::Image(img) = c {
            grow(img.width(), img.height());
        }
    }
    let rgb = rgb.map(|img| fit_rgba(img, w, h));
    let mono: Vec<Option<GrayImage>> = channels
        .iter()
        .map(|c| match c {
            Mono::Image(img) => Some(fit_gray(img, w, h)),
            Mono::Constant(_) => None,
        })
        .collect();
    RgbaImage::from_fn(w, h, |x, y| {
        let mut px = [0u8; 4];
        for (i, out) in px.iter_mut().enumerate() {
            *out = match (&rgb, i < 3) {
                (Some(img), true) => img.get_pixel(x, y).0[i],
                _ => match (&mono[i], channels[i]) {
                    (Some(img), _) => img.get_pixel(x, y).0[0],
                    (None, Mono::Constant(v)) => (v.clamp(0.0, 1.0) * 255.0).round() as u8,
                    (None, Mono::Image(_)) => unreachable!("mono images are resized above"),
                },
            };
        }
        Rgba(px)
    })
}

fn fit_rgba(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    if img.dimensions() == (w, h) {
        img.clone()
    } else {
        image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle)
    }
}

fn fit_gray(img: &GrayImage, w: u32, h: u32) -> GrayImage {
    if img.dimensions() == (w, h) {
        img.clone()
    } else {
        image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle)
    }
}

fn stem(file: &Path) -> &str {
    file.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("texture")
}

/// File-name-safe: the stem comes from asset names authored anywhere.
fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "texture".into()
    } else {
        out
    }
}
