//! Procedural primitive shapes authored via `NodeKind::Primitive`.
//!
//! The renderer materializes each shape at load time via `awsm-meshgen`'s
//! primitive generators — there's no baking, just parameters in `project.json`.

use super::assets::AssetId;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveShape {
    Plane {
        width: f32,
        depth: f32,
        segments_x: u32,
        segments_z: u32,
    },
    Box {
        dims: [f32; 3],
    },
    Sphere {
        radius: f32,
        segments_long: u32,
        segments_lat: u32,
    },
    Cylinder {
        radius: f32,
        height: f32,
        radial_segments: u32,
    },
    Cone {
        radius: f32,
        height: f32,
        radial_segments: u32,
    },
    Torus {
        radius: f32,
        thickness: f32,
        segments_major: u32,
        segments_minor: u32,
    },
}

impl PrimitiveShape {
    pub fn default_plane() -> Self {
        Self::Plane {
            width: 10.0,
            depth: 10.0,
            segments_x: 1,
            segments_z: 1,
        }
    }
    pub fn default_box() -> Self {
        Self::Box {
            dims: [1.0, 1.0, 1.0],
        }
    }
    pub fn default_sphere() -> Self {
        Self::Sphere {
            radius: 0.5,
            segments_long: 24,
            segments_lat: 16,
        }
    }
    pub fn default_cylinder() -> Self {
        Self::Cylinder {
            radius: 0.5,
            height: 1.0,
            radial_segments: 24,
        }
    }
    pub fn default_cone() -> Self {
        Self::Cone {
            radius: 0.5,
            height: 1.0,
            radial_segments: 24,
        }
    }
    pub fn default_torus() -> Self {
        Self::Torus {
            radius: 0.5,
            thickness: 0.1,
            segments_major: 24,
            segments_minor: 12,
        }
    }
}

/// Typed reference to a material asset (`AssetSource::Material`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
#[derive(Eq, Hash, Copy)]
pub struct MaterialRef(pub AssetId);

/// Typed reference to a texture asset, with the glTF per-binding metadata that
/// decides how the image is sampled: which UV set (`uv_index`, glTF `texCoord`)
/// and an optional `KHR_texture_transform`. Both are non-recompiling, so they're
/// per-mesh overridable like any other texture binding. Carries no `Eq/Hash`
/// (the transform holds `f32`s) and is no longer `serde(transparent)` — a custom
/// `Deserialize` still accepts the legacy bare-id form so old projects load.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TextureRef {
    pub asset: AssetId,
    /// Which UV set this texture samples (glTF `texCoord`). Defaults to 0.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub uv_index: u32,
    /// Optional `KHR_texture_transform` (offset / rotation / scale of the UVs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<TextureTransform>,
}

impl TextureRef {
    /// A plain reference (UV set 0, no transform) — the common case.
    pub fn new(asset: AssetId) -> Self {
        Self {
            asset,
            uv_index: 0,
            transform: None,
        }
    }
}

impl From<AssetId> for TextureRef {
    fn from(asset: AssetId) -> Self {
        Self::new(asset)
    }
}

impl<'de> serde::Deserialize<'de> for TextureRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept either the legacy bare id (`"<uuid>"`) or the full struct.
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Bare(AssetId),
            Full {
                asset: AssetId,
                #[serde(default)]
                uv_index: u32,
                #[serde(default)]
                transform: Option<TextureTransform>,
            },
        }
        Ok(match Repr::deserialize(d)? {
            Repr::Bare(asset) => TextureRef::new(asset),
            Repr::Full {
                asset,
                uv_index,
                transform,
            } => TextureRef {
                asset,
                uv_index,
                transform,
            },
        })
    }
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// glTF `KHR_texture_transform` — an affine transform applied to a texture's UVs
/// before sampling. Per-mesh uniform (no recompile).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TextureTransform {
    #[serde(default)]
    pub offset: [f32; 2],
    #[serde(default)]
    pub rotation: f32,
    #[serde(default = "default_uv_scale")]
    pub scale: [f32; 2],
}

impl Default for TextureTransform {
    fn default() -> Self {
        Self {
            offset: [0.0, 0.0],
            rotation: 0.0,
            scale: [1.0, 1.0],
        }
    }
}

fn default_uv_scale() -> [f32; 2] {
    [1.0, 1.0]
}

/// Typed reference to a procedural mesh asset.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
#[derive(Eq, Hash, Copy)]
pub struct MeshRef(pub AssetId);

impl std::fmt::Display for MaterialRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::fmt::Display for TextureRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.asset, f)
    }
}

impl std::fmt::Display for MeshRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
