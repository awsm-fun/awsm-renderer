//! A compiled model's 2D textures, as PNGs beside the sidecar, bound to its
//! materials' glTF slots.
//!
//! MuJoCo binds textures to materials by role (`mjtTextureRole`); the sidecar
//! binds them by glTF slot. Each texture's texels are written once, in MuJoCo's
//! own row order: its mesh texcoords address that order with row 0 at V = 0,
//! which is glTF's convention, so neither the image nor the UVs flip.
//!
//! Cube and skybox textures have no glTF slot and are left out, as are the
//! single-channel roughness, metallic and opacity roles (glTF packs those into
//! other maps); a material that uses them still exports, without them.

use anyhow::{Context, Result};
use awsm_renderer_mujoco_format::sidecar::{MaterialTexture, Sidecar, UvTransform};
use awsm_renderer_mujoco_sys::{mjtObj, Model};
use image::ImageEncoder as _;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// `mjNTEXROLE`: how many role slots each material has in `mat_texid`.
const ROLES: usize = 10;
const ROLE_RGB: usize = 1;
const ROLE_OCCLUSION: usize = 2;
const ROLE_NORMAL: usize = 5;
const ROLE_EMISSIVE: usize = 7;
const ROLE_RGBA: usize = 8;
const ROLE_ORM: usize = 9;
/// `mjTEXTURE_2D`.
const TEXTURE_2D: i32 = 0;

/// One image to write, at `path` relative to the sidecar.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub path: String,
    pub png: Vec<u8>,
}

/// Fill every material's texture slots from the model and return the images
/// they name, each written once however many materials share it. Textures
/// with identical texels share one image too: a model that attaches one robot
/// several times (a prefix per copy) gets one copy of each of its textures.
pub fn attach(model: &Model<'_>, doc: &mut Sidecar) -> Result<Vec<Image>> {
    let texid = model.mat_texid();
    let repeat = model.mat_texrepeat();
    let mut written: HashMap<usize, String> = HashMap::new();
    let mut by_content: HashMap<Vec<u8>, String> = HashMap::new();
    let mut images: Vec<Image> = Vec::new();
    let mut image_for = |tex: i32| -> Result<Option<MaterialTexture>> {
        if tex < 0 || model.tex_type()[tex as usize] != TEXTURE_2D {
            return Ok(None);
        }
        let tex = tex as usize;
        let path = match written.get(&tex) {
            Some(path) => path.clone(),
            None => {
                let png = encode(model, tex)?;
                let key = Sha256::digest(&png).to_vec();
                let path = match by_content.get(&key) {
                    Some(path) => path.clone(),
                    None => {
                        let path = format!("textures/{}.png", file_stem(model, tex, &written));
                        images.push(Image {
                            path: path.clone(),
                            png,
                        });
                        by_content.insert(key, path.clone());
                        path
                    }
                };
                written.insert(tex, path.clone());
                path
            }
        };
        Ok(Some(MaterialTexture::new(path)))
    };
    for (m, material) in doc.materials.iter_mut().enumerate() {
        let roles = &texid[m * ROLES..(m + 1) * ROLES];
        let transform = {
            let [u, v] = [repeat[m * 2], repeat[m * 2 + 1]];
            ((u, v) != (1.0, 1.0)).then_some(UvTransform {
                offset: [0.0, 0.0],
                rotation: 0.0,
                scale: [u, v],
            })
        };
        let with = |t: Option<MaterialTexture>| {
            t.map(|mut t| {
                t.transform = transform;
                t
            })
        };
        let base = if roles[ROLE_RGBA] >= 0 {
            roles[ROLE_RGBA]
        } else {
            roles[ROLE_RGB]
        };
        material.textures.base_color = with(image_for(base)?);
        material.textures.normal = with(image_for(roles[ROLE_NORMAL])?);
        material.textures.emissive = with(image_for(roles[ROLE_EMISSIVE])?);
        // ORM is glTF's packing exactly: occlusion in R, roughness in G,
        // metallic in B, so one image serves both slots.
        if let Some(orm) = with(image_for(roles[ROLE_ORM])?) {
            material.textures.metallic_roughness = Some(orm.clone());
            material.textures.occlusion = Some(orm);
        } else {
            material.textures.occlusion = with(image_for(roles[ROLE_OCCLUSION])?);
        }
    }
    Ok(images)
}

/// A file-safe, unique stem: the texture's name, or `texture<id>`.
fn file_stem(model: &Model<'_>, tex: usize, written: &HashMap<usize, String>) -> String {
    let name: String = model
        .name(mjtObj::mjOBJ_TEXTURE, tex)
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = if name.is_empty() {
        format!("texture{tex}")
    } else {
        name
    };
    if written
        .values()
        .any(|p| p == &format!("textures/{stem}.png"))
    {
        format!("{stem}_{tex}")
    } else {
        stem
    }
}

/// One texture's texels as a PNG.
fn encode(model: &Model<'_>, tex: usize) -> Result<Vec<u8>> {
    let (w, h) = (
        model.tex_width()[tex] as u32,
        model.tex_height()[tex] as u32,
    );
    let channels = model.tex_nchannel()[tex] as usize;
    let start = model.tex_adr()[tex] as usize;
    let len = (w as usize) * (h as usize) * channels;
    let texels = model
        .tex_data()
        .get(start..start + len)
        .with_context(|| format!("texture {tex}: texels out of range"))?
        .to_vec();
    let color = match channels {
        1 => image::ExtendedColorType::L8,
        3 => image::ExtendedColorType::Rgb8,
        4 => image::ExtendedColorType::Rgba8,
        other => anyhow::bail!("texture {tex}: {other} channels a texel"),
    };
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&texels, w, h, color)
        .with_context(|| format!("texture {tex}: encoding PNG"))?;
    Ok(png)
}
