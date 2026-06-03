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

/// Typed reference to a texture asset.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
#[derive(Eq, Hash, Copy)]
pub struct TextureRef(pub AssetId);

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
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::fmt::Display for MeshRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
