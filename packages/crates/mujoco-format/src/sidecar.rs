//! `mujoco.json` — a flat description of a compiled model's visual content.
//!
//! Pairs with a `model.glb` carrying geometry only. Everything MuJoCo means that
//! glTF cannot say (visibility groups, geom→body binding, primitive shape params,
//! Phong material terms, the source fingerprint) lives here.
//!
//! See the crate docs for the coordinate convention: raw MuJoCo throughout.

use serde::{Deserialize, Serialize};

/// Written into every file so a reader can reject a stranger's JSON outright
/// rather than half-parsing it.
pub const MAGIC: &str = "awsm-mujoco-sidecar";

/// Bumped only for a *breaking* change. Additive fields do not bump it — readers
/// default them — which is what keeps third-party producers viable.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Sidecar {
    /// Always [`MAGIC`].
    pub format: String,
    /// Always [`VERSION`] for this schema.
    pub version: u32,

    /// Identity of the model this was exported from.
    pub source: Source,

    /// The compiled model's own name (MJCF `<mujoco model="...">`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,

    /// The companion geometry GLB, as a path relative to this file. `None` when
    /// the model is all primitives and no GLB was written.
    ///
    /// Named here rather than inferred from the sidecar's own filename so the
    /// pair is self-describing: a consumer resolves one URL against the other and
    /// never has to know our naming convention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glb: Option<String>,

    /// Body 0 is always MuJoCo's `world`, and is its own parent.
    #[serde(default)]
    pub bodies: Vec<Body>,

    /// Index in this vec IS the MuJoCo geom id — that identity is the whole basis
    /// of the pose stream's addressing, so entries are never reordered or filtered
    /// (invisible geoms are exported with their real group, not dropped).
    #[serde(default)]
    pub geoms: Vec<Geom>,

    #[serde(default)]
    pub materials: Vec<Material>,

    /// Meshes referenced by [`Geom::mesh`]; the geometry itself lives in the
    /// companion GLB.
    #[serde(default)]
    pub meshes: Vec<Mesh>,
}

impl Sidecar {
    /// A sidecar with the right magic/version and nothing in it.
    pub fn new(source: Source) -> Self {
        Self {
            format: MAGIC.to_string(),
            version: VERSION,
            source,
            model_name: None,
            glb: None,
            bodies: Vec::new(),
            geoms: Vec::new(),
            materials: Vec::new(),
            meshes: Vec::new(),
        }
    }

    /// Checks the magic and version. Call this before trusting anything else —
    /// the failure mode otherwise is a scene that loads and silently cannot bind.
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != MAGIC {
            return Err(Error::WrongFormat(self.format.clone()));
        }
        if self.version != VERSION {
            return Err(Error::WrongVersion(self.version));
        }
        // Index-based references are the schema's only invariant that JSON itself
        // cannot express, so check them here rather than at every use site.
        for (i, g) in self.geoms.iter().enumerate() {
            if g.body >= self.bodies.len() {
                return Err(Error::BadIndex {
                    what: "geom.body",
                    index: i,
                    value: g.body,
                    len: self.bodies.len(),
                });
            }
            if let Some(m) = g.material {
                if m >= self.materials.len() {
                    return Err(Error::BadIndex {
                        what: "geom.material",
                        index: i,
                        value: m,
                        len: self.materials.len(),
                    });
                }
            }
            if let Some(m) = g.mesh {
                if m >= self.meshes.len() {
                    return Err(Error::BadIndex {
                        what: "geom.mesh",
                        index: i,
                        value: m,
                        len: self.meshes.len(),
                    });
                }
            }
        }
        for (i, b) in self.bodies.iter().enumerate() {
            if b.parent >= self.bodies.len() {
                return Err(Error::BadIndex {
                    what: "body.parent",
                    index: i,
                    value: b.parent,
                    len: self.bodies.len(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    WrongFormat(String),
    WrongVersion(u32),
    BadIndex {
        what: &'static str,
        index: usize,
        value: usize,
        len: usize,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::WrongFormat(got) => {
                write!(
                    f,
                    "not a MuJoCo sidecar: format is {got:?}, expected {MAGIC:?}"
                )
            }
            Error::WrongVersion(got) => write!(
                f,
                "MuJoCo sidecar version {got} is not supported (this build reads {VERSION})"
            ),
            Error::BadIndex {
                what,
                index,
                value,
                len,
            } => write!(
                f,
                "{what}[{index}] = {value}, out of range for {len} entries"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// How a scene instance finds its way back to the model a sim process loaded.
///
/// Never the model file itself: the sim owns that outright and we never archive or
/// transport MJCF/URDF. A harness matches its loaded models against this and fails
/// loudly on a mismatch, instead of silently driving the wrong robot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Source {
    /// File name only — never a path. Paths are machine-specific and would make
    /// the fingerprint unmatchable on the sim's side.
    pub filename: String,
    /// Lowercase hex SHA-256 of the source file's bytes.
    pub sha256: String,
    /// The MuJoCo that compiled it, e.g. `"3.11.0"`. Compilation is
    /// version-sensitive (defaults, mesh fitting), so a mismatch is worth a
    /// warning even when the hash matches.
    pub mujoco_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Body {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Index into `bodies`. Body 0 (`world`) is its own parent.
    pub parent: usize,
    /// Offset from the parent body's frame, in metres.
    pub pos: [f64; 3],
    /// Rotation from the parent body's frame, `[w, x, y, z]`.
    pub quat: [f64; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Geom {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Index into `bodies` — the node whose world pose the stream drives.
    pub body: usize,
    /// MuJoCo visibility group. 0–2 are visible by default; menagerie models put
    /// collision geometry in 3 and visual meshes in 0/2, so honoring this is what
    /// separates rendering the robot from rendering its collision capsules.
    pub group: i32,
    #[serde(rename = "type")]
    pub kind: GeomKind,
    /// Type-dependent, always three slots because MuJoCo stores it that way; see
    /// [`GeomKind`] for what each means.
    pub size: [f64; 3],
    /// Offset from the owning body's frame, in metres. The model's authored
    /// placement — NOT where the geom starts; see [`world_pos`](Self::world_pos).
    pub pos: [f64; 3],
    /// Rotation from the owning body's frame, `[w, x, y, z]`.
    pub quat: [f64; 4],
    /// Where this geom actually **is** in the model's initial configuration
    /// (`qpos0`), in world space.
    ///
    /// Deliberately the same shape and meaning as a pose-stream frame, so the
    /// initial render and the simulation's first frame agree and nothing jumps.
    /// A consumer must NOT try to derive this by composing `pos` up the body
    /// chain: that reproduces the joint-zero pose, which is not `qpos0` whenever
    /// the model has a non-zero reference configuration (most robots do).
    #[serde(default)]
    pub world_pos: [f64; 3],
    /// Initial-configuration world orientation, `[w, x, y, z]`.
    #[serde(default = "identity_quat")]
    pub world_quat: [f64; 4],
    /// Index into `meshes`, for [`GeomKind::Mesh`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<usize>,
    /// Index into `materials`. `None` means the geom's own [`rgba`](Self::rgba)
    /// is the whole appearance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<usize>,
    /// MuJoCo's `geom_rgba`, always present. Only *used* when `material` is
    /// `None`, but exported unconditionally so a reader never has to guess.
    pub rgba: [f32; 4],
}

fn identity_quat() -> [f64; 4] {
    [1.0, 0.0, 0.0, 0.0]
}

/// MuJoCo's visual geom types, as strings.
///
/// The doc comment on each variant is the meaning of [`Geom::size`], which is the
/// one thing about MuJoCo primitives that cannot be guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GeomKind {
    /// Infinite ground plane. `size` = `[x half-extent, y half-extent, grid
    /// spacing]`; a 0 half-extent means truly infinite in that axis.
    Plane,
    /// Heightfield; `size` is unused (the field's own data governs).
    Hfield,
    /// `size[0]` = radius.
    Sphere,
    /// `size` = `[radius, half-length]`, along local Z, hemispherical caps.
    Capsule,
    /// `size` = the three semi-axes.
    Ellipsoid,
    /// `size` = `[radius, half-length]`, along local Z.
    Cylinder,
    /// `size` = the three half-extents.
    Box,
    /// Triangle mesh; `size` is unused, see [`Geom::mesh`].
    Mesh,
    /// Signed-distance-field geom (plugin-provided). Rendered as its mesh if it
    /// has one; otherwise skipped.
    Sdf,
}

/// MuJoCo materials are Phong-ish; mapping them onto our PBR materials happens at
/// *import*, not here, so the sidecar stays a faithful record of the source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Material {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub rgba: [f32; 4],
    pub specular: f32,
    pub shininess: f32,
    pub reflectance: f32,
    pub emission: f32,
}

/// A mesh referenced by a geom. The vertex data itself is in the companion GLB —
/// this is only the correspondence between the two files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Mesh {
    /// The MuJoCo asset name, as authored. Not necessarily unique, and not
    /// necessarily present — which is exactly why it is not the binding key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Name of the node carrying this mesh in the companion GLB. Stated
    /// explicitly so a reader never has to reproduce our naming rule, and so a
    /// GLB that has been through another tool still binds. Absent ⇒ fall back to
    /// the GLB's root node at this same index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            filename: "go2.xml".into(),
            sha256: "abc".into(),
            mujoco_version: "3.11.0".into(),
        }
    }

    fn world() -> Body {
        Body {
            name: Some("world".into()),
            parent: 0,
            pos: [0.0; 3],
            quat: [1.0, 0.0, 0.0, 0.0],
        }
    }

    fn geom(body: usize) -> Geom {
        Geom {
            name: None,
            body,
            group: 0,
            kind: GeomKind::Sphere,
            size: [0.5, 0.0, 0.0],
            pos: [0.0; 3],
            quat: [1.0, 0.0, 0.0, 0.0],
            world_pos: [0.0; 3],
            world_quat: [1.0, 0.0, 0.0, 0.0],
            mesh: None,
            material: None,
            rgba: [1.0; 4],
        }
    }

    #[test]
    fn roundtrips_through_json() {
        let mut s = Sidecar::new(source());
        s.bodies.push(world());
        s.geoms.push(geom(0));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Sidecar>(&json).unwrap(), s);
    }

    #[test]
    fn geom_kinds_are_documented_strings() {
        // The point of the string encoding: a hand-written sidecar is readable and
        // MuJoCo's internal numbering never leaks into the format.
        let json = serde_json::to_string(&GeomKind::Capsule).unwrap();
        assert_eq!(json, "\"capsule\"");
        assert_eq!(
            serde_json::from_str::<GeomKind>("\"ellipsoid\"").unwrap(),
            GeomKind::Ellipsoid
        );
    }

    #[test]
    fn a_v1_reader_survives_a_later_producer_adding_fields() {
        // Additive changes must not bump VERSION, which only works if unknown
        // fields are ignored and absent ones default.
        let json = r#"{
            "format": "awsm-mujoco-sidecar",
            "version": 1,
            "source": {"filename":"a.xml","sha256":"x","mujoco_version":"3.11.0"},
            "bodies": [{"parent":0,"pos":[0,0,0],"quat":[1,0,0,0],"future_field":42}],
            "sites": [{"whatever": true}]
        }"#;
        let s: Sidecar = serde_json::from_str(json).unwrap();
        s.validate().unwrap();
        assert_eq!(s.bodies.len(), 1);
        assert!(s.geoms.is_empty(), "absent arrays default to empty");
    }

    #[test]
    fn validate_rejects_a_stranger() {
        let mut s = Sidecar::new(source());
        s.format = "some-other-tool".into();
        assert_eq!(
            s.validate(),
            Err(Error::WrongFormat("some-other-tool".into()))
        );
    }

    #[test]
    fn validate_catches_dangling_indices() {
        // The silent-failure mode this exists to prevent: a sidecar that parses
        // cleanly, renders something, and binds the stream to nothing.
        let mut s = Sidecar::new(source());
        s.bodies.push(world());
        s.geoms.push(geom(7));
        assert!(matches!(
            s.validate(),
            Err(Error::BadIndex {
                what: "geom.body",
                ..
            })
        ));

        let mut s = Sidecar::new(source());
        s.bodies.push(world());
        let mut g = geom(0);
        g.material = Some(0);
        s.geoms.push(g);
        assert!(matches!(
            s.validate(),
            Err(Error::BadIndex {
                what: "geom.material",
                ..
            })
        ));
    }
}
