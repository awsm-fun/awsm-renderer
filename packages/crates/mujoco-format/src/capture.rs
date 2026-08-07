//! The **capture** format: a recorded run of a simulation, as a sequence of
//! stream frames.
//!
//! Deliberately the same shape as the live stream, dumped verbatim — a harness
//! that can feed the pose sink can record a capture by writing what it was
//! already sending. That is the whole point: this is interchange, not an asset.
//! Our editor bakes a capture into ordinary animation clips and the file is then
//! disposable, like a source `.obj` after import. There is no trajectory asset
//! type and no trajectory UI anywhere in the system (see `docs/mujoco.md`).
//!
//! ## Coordinates
//!
//! Raw MuJoCo, exactly as in the sidecar: Z-up, right-handed, metres,
//! quaternions `[w, x, y, z]`. Poses are **world** poses — the same thing the
//! sidecar's `Geom::world_pos`/`world_quat` record for the initial frame, so
//! frame 0 of a capture taken at `qpos0` reproduces the imported pose exactly.
//!
//! ## Size
//!
//! JSON, for the same reason the sidecar is JSON: a third-party pipeline must be
//! able to write one without our code. A capture is big (7 floats per geom per
//! frame) and that is fine — it exists only until the bake consumes it.

use serde::{Deserialize, Serialize};

use crate::sidecar::Source;

/// Written into every file so a reader can reject a stranger's JSON outright.
pub const MAGIC: &str = "awsm-mujoco-capture";

/// Bumped only for a *breaking* change; additive fields default instead.
pub const VERSION: u32 = 1;

/// Floats per geom in a frame: `[px, py, pz, qw, qx, qy, qz]`.
pub const FLOATS_PER_GEOM: usize = 7;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Capture {
    /// Always [`MAGIC`].
    pub format: String,
    /// Always [`VERSION`] for this schema.
    pub version: u32,

    /// Which model this run is of. Must match a scene instance's fingerprint —
    /// that match is what binds the baked clip to the right subtree, and a
    /// mismatch is a loud error rather than a silently mis-driven robot.
    pub source: Source,

    /// The model's geom count, i.e. the id space every frame is indexed by.
    /// Stated once here rather than inferred per frame so a truncated or
    /// mis-sized frame is caught instead of shifting every geom after it.
    pub geom_count: u32,

    /// Frames in capture order. Times are seconds from the start of the capture
    /// and must be non-decreasing; the bake reads them as clip keyframe times,
    /// so an irregular capture (a sim that hitched) bakes to an irregular clip
    /// rather than being silently resampled.
    #[serde(default)]
    pub frames: Vec<Frame>,
}

/// One recorded instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Frame {
    /// Seconds from the start of the capture.
    pub time: f64,
    /// `7 * geom_count` floats: `[px, py, pz, qw, qx, qy, qz]` per geom, indexed
    /// by **MuJoCo geom id**.
    ///
    /// Flat rather than a list of objects because this is exactly what the live
    /// stream carries and what a `mjData` read produces — nesting would only add
    /// a translation step on both ends. `f32` for the same reason: the pose sink
    /// takes f32, and a capture that stored more precision than the sink accepts
    /// would be storing a difference nothing can observe.
    pub geom_poses: Vec<f32>,
}

impl Capture {
    pub fn new(source: Source, geom_count: u32) -> Self {
        Self {
            format: MAGIC.to_string(),
            version: VERSION,
            source,
            geom_count,
            frames: Vec::new(),
        }
    }

    /// Check the magic, the version, and every invariant JSON cannot express.
    ///
    /// Call this before baking. A capture with a short frame would otherwise bake
    /// a clip that drives the wrong geoms from that frame onward — motion that
    /// looks like a physics bug rather than a bad file.
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != MAGIC {
            return Err(Error::WrongFormat(self.format.clone()));
        }
        if self.version != VERSION {
            return Err(Error::WrongVersion(self.version));
        }
        let expected = self.geom_count as usize * FLOATS_PER_GEOM;
        let mut previous = f64::NEG_INFINITY;
        for (i, f) in self.frames.iter().enumerate() {
            if f.geom_poses.len() != expected {
                return Err(Error::FrameSize {
                    frame: i,
                    got: f.geom_poses.len(),
                    expected,
                });
            }
            if !f.time.is_finite() {
                return Err(Error::BadTime {
                    frame: i,
                    time: f.time,
                });
            }
            if f.time < previous {
                return Err(Error::TimeWentBackwards {
                    frame: i,
                    time: f.time,
                    previous,
                });
            }
            previous = f.time;
        }
        Ok(())
    }

    /// Total duration in seconds (0 for an empty capture).
    pub fn duration(&self) -> f64 {
        match (self.frames.first(), self.frames.last()) {
            (Some(a), Some(b)) => b.time - a.time,
            _ => 0.0,
        }
    }
}

impl Frame {
    /// One geom's world pose as `(position, quaternion [w, x, y, z])`.
    ///
    /// Returns `None` for an out-of-range id rather than panicking: a capture is
    /// third-party data, and a reader iterating a *scene's* geoms against a
    /// possibly-shorter frame is the normal case, not a bug.
    pub fn geom(&self, geom_id: usize) -> Option<([f32; 3], [f32; 4])> {
        let base = geom_id * FLOATS_PER_GEOM;
        let s = self.geom_poses.get(base..base + FLOATS_PER_GEOM)?;
        Some(([s[0], s[1], s[2]], [s[3], s[4], s[5], s[6]]))
    }

    /// Append one geom's world pose, in id order.
    pub fn push_geom(&mut self, pos: [f32; 3], quat_wxyz: [f32; 4]) {
        self.geom_poses.extend_from_slice(&pos);
        self.geom_poses.extend_from_slice(&quat_wxyz);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    WrongFormat(String),
    WrongVersion(u32),
    FrameSize {
        frame: usize,
        got: usize,
        expected: usize,
    },
    BadTime {
        frame: usize,
        time: f64,
    },
    TimeWentBackwards {
        frame: usize,
        time: f64,
        previous: f64,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::WrongFormat(got) => write!(
                f,
                "not a MuJoCo capture: format is {got:?}, expected {MAGIC:?}"
            ),
            Error::WrongVersion(got) => write!(
                f,
                "MuJoCo capture version {got} is not supported (this build reads {VERSION})"
            ),
            Error::FrameSize {
                frame,
                got,
                expected,
            } => write!(
                f,
                "frame {frame} carries {got} floats, expected {expected} \
                 ({FLOATS_PER_GEOM} per geom x geom_count)"
            ),
            Error::BadTime { frame, time } => {
                write!(f, "frame {frame} has a non-finite time {time}")
            }
            Error::TimeWentBackwards {
                frame,
                time,
                previous,
            } => write!(
                f,
                "frame {frame} goes backwards in time ({time} after {previous})"
            ),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            filename: "go2.xml".into(),
            sha256: "c".repeat(64),
            mujoco_version: "3.11.0".into(),
        }
    }

    fn capture(frames: usize, geoms: u32) -> Capture {
        let mut c = Capture::new(source(), geoms);
        for i in 0..frames {
            let mut f = Frame {
                time: i as f64 / 60.0,
                geom_poses: Vec::new(),
            };
            for g in 0..geoms {
                f.push_geom([g as f32, 0.0, i as f32], [1.0, 0.0, 0.0, 0.0]);
            }
            c.frames.push(f);
        }
        c
    }

    #[test]
    fn round_trips_through_json() {
        let c = capture(3, 4);
        c.validate().unwrap();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Capture>(&json).unwrap(), c);
    }

    #[test]
    fn geom_accessor_matches_what_was_pushed() {
        let c = capture(2, 3);
        // Frame 1, geom 2: pushed as position [2, 0, 1], identity quaternion.
        let (pos, quat) = c.frames[1].geom(2).unwrap();
        assert_eq!(pos, [2.0, 0.0, 1.0]);
        assert_eq!(quat, [1.0, 0.0, 0.0, 0.0]);
        // Past the end is None, not a panic — a scene may hold more geoms than a
        // third-party frame carries.
        assert!(c.frames[1].geom(3).is_none());
    }

    #[test]
    fn duration_spans_first_to_last() {
        assert!((capture(61, 1).duration() - 1.0).abs() < 1e-9);
        assert_eq!(Capture::new(source(), 1).duration(), 0.0);
    }

    /// The failure this format's validation exists for: a frame that is short by
    /// one geom bakes a clip that drives every geom after it from the wrong
    /// slot — motion that reads as a physics bug rather than a bad file.
    #[test]
    fn validate_catches_a_mis_sized_frame() {
        let mut c = capture(2, 3);
        c.frames[1].geom_poses.truncate(13);
        assert_eq!(
            c.validate(),
            Err(Error::FrameSize {
                frame: 1,
                got: 13,
                expected: 21
            })
        );
    }

    #[test]
    fn validate_catches_time_going_backwards() {
        let mut c = capture(3, 1);
        c.frames[2].time = 0.0;
        assert!(matches!(c.validate(), Err(Error::TimeWentBackwards { .. })));
    }

    #[test]
    fn validate_rejects_a_stranger() {
        let mut c = capture(1, 1);
        c.format = "some-other-tool".into();
        assert_eq!(
            c.validate(),
            Err(Error::WrongFormat("some-other-tool".into()))
        );
    }

    #[test]
    fn a_v1_reader_survives_a_later_producer_adding_fields() {
        // Additive channels (tendon waypoints, contacts, flex vertices) must not
        // bump VERSION, which only works if unknown fields are ignored.
        let json = r#"{
            "format": "awsm-mujoco-capture",
            "version": 1,
            "source": {"filename":"a.xml","sha256":"x","mujoco_version":"3.11.0"},
            "geom_count": 1,
            "frames": [{"time": 0.0, "geom_poses": [0,0,0,1,0,0,0], "contacts": []}],
            "sim_options": {"timestep": 0.002}
        }"#;
        let c: Capture = serde_json::from_str(json).unwrap();
        c.validate().unwrap();
        assert_eq!(c.frames.len(), 1);
    }
}
