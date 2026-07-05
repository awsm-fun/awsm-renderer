//! Persisted **animation clip** schema — the serde projection of the editor's
//! authored animation model (Animation mode), mirroring [`crate::StoredMaterial`].
//!
//! The editor keeps a *live, reactive* model (`Mutable`-wrapped); these are the
//! plain serde types it snapshots into `animation-<slug>.toml` side files + the
//! project's `editor_animations` / `anim_mixer` sections, so clips survive
//! save/load losslessly. Editor-only — the runtime player ignores them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::assets::AssetId;
use crate::tree::NodeId;

/// How a clip's shared clock wraps at its duration boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum ClipLoop {
    Loop,
    PingPong,
    Once,
}

/// The default playback direction a clip's clock advances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum ClipDirection {
    Forward,
    Reverse,
}

/// Per-keyframe interpolation (display); lowering uses the track's [`SamplerKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Interp {
    Step,
    Linear,
    Cubic,
}

/// The renderer sampler kind a whole track lowers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum SamplerKind {
    Step,
    Linear,
    Cubic,
}

/// The typed value of one keyframe (and of its cubic in/out tangents).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
// Adjacently tagged (NOT internally tagged): the variants carry array / scalar
// payloads, which serde cannot serialize under `tag` alone (an internally-tagged
// newtype variant requires a map payload — a `[f32;N]` is a sequence and errors
// at runtime). `tag + content` round-trips any payload, in JSON *and* TOML.
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TrackValue {
    /// Translation / scale (vec3).
    Vec3([f32; 3]),
    /// Rotation (quaternion, xyzw) — quaternion-native, no Euler in the model.
    Quat([f32; 4]),
    /// A scalar (uniform / light / camera / morph weight).
    Scalar(f32),
    /// A 2-component value (e.g. a UV offset / scale on a material uniform).
    Vec2([f32; 2]),
    /// A 4-component value (e.g. a tint / rect on a material uniform).
    Vec4([f32; 4]),
}

/// One keyframe, aligned to a track's shared `times[i]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Keyframe {
    pub value: TrackValue,
    pub interp: Interp,
    pub in_tangent: TrackValue,
    pub out_tangent: TrackValue,
}

/// Which transform component a transform track drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TransformProp {
    Translation,
    Rotation,
    Scale,
}

/// Which built-in material factor a `BuiltinParam` track drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum BuiltinParamKind {
    BaseColor,
    Metallic,
    Roughness,
    Emissive,
    /// Normal-map intensity (`normal_scale`, scalar). Visible only with a normal map.
    NormalScale,
    /// Ambient-occlusion strength (`occlusion_strength`, scalar). Visible only with
    /// an occlusion map.
    OcclusionStrength,
    /// Emissive strength multiplier (`KHR_materials_emissive_strength`, scalar) —
    /// pulse a glow for bloom. Applies only when the material already has emissive
    /// strength enabled (it's a feature-gated extension; toggling it on/off
    /// recompiles, so a track animates the VALUE, not the feature). PBR only.
    EmissiveStrength,
    /// Alpha-test cutoff (`Mask` alpha mode threshold, scalar) — animate a
    /// dissolve / cutout. Applies only to a `Mask` material (no-op otherwise; the
    /// alpha MODE is a pipeline choice, not animatable). PBR only.
    AlphaCutoff,
    /// Toon: number of diffuse bands (scalar, rounded to `u32`, ≥1). Toon only.
    ToonDiffuseBands,
    /// Toon: number of specular steps (scalar, rounded to `u32`, ≥1). Toon only.
    ToonSpecularSteps,
    /// Toon: specular shininess exponent (scalar). Toon only.
    ToonShininess,
    /// Toon: rim-light strength (scalar). Toon only.
    ToonRimStrength,
    /// Toon: rim-light falloff power (scalar). Toon only.
    ToonRimPower,
    /// FlipBook: playback rate in frames/sec (scalar) — animate to speed up/slow
    /// down a sprite sheet (`0` freezes). FlipBook only.
    FlipbookFps,
    /// FlipBook: time offset in seconds (scalar) — phase/scrub the sheet per
    /// instance. FlipBook only.
    FlipbookTimeOffset,
}

/// Which built-in material texture slot a `TextureTransform` track drives
/// (mirrors the glTF PBR texture set / `BuiltinTextureSlot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TexSlot {
    BaseColor,
    MetallicRoughness,
    Normal,
    Occlusion,
    Emissive,
}

/// Which component of a texture slot's UV transform a `TextureTransform` track
/// drives. `Offset`/`Scale` are `vec2` keyframes; `Rotation` is a scalar (radians).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TexTransformProp {
    Offset,
    Scale,
    Rotation,
}

/// Which light parameter a `Light` track drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum LightParamKind {
    Intensity,
    Color,
    Range,
    InnerAngle,
    OuterAngle,
}

/// Which camera parameter a `Camera` track drives. Lowered end-to-end: the
/// editor bridge maps this to the renderer's `CameraParam` and
/// `apply_camera_param` drives the live camera (FovY perspective-only; Near /
/// Far / Aperture / FocusDistance).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum CameraParamKind {
    FovY,
    Near,
    Far,
    Aperture,
    FocusDistance,
}

/// A serializable descriptor binding a track to a real animatable target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TrackTarget {
    Transform {
        node: NodeId,
        prop: TransformProp,
    },
    Morph {
        node: NodeId,
        index: usize,
    },
    Uniform {
        material: AssetId,
        name: String,
    },
    BuiltinParam {
        node: NodeId,
        param: BuiltinParamKind,
    },
    Light {
        node: NodeId,
        param: LightParamKind,
    },
    Camera {
        node: NodeId,
        param: CameraParamKind,
    },
    /// One component (offset/scale/rotation) of a built-in material texture
    /// slot's UV transform, on the node's assigned material. Offset/Scale are
    /// vec2 keyframes; Rotation is scalar.
    TextureTransform {
        node: NodeId,
        slot: TexSlot,
        prop: TexTransformProp,
    },
}

/// Serializable snapshot of one track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct StoredTrack {
    pub target: TrackTarget,
    pub sampler: SamplerKind,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub expanded: bool,
    #[serde(default)]
    pub times: Vec<f64>,
    #[serde(default)]
    pub keys: Vec<Keyframe>,
}

/// Serializable snapshot of one clip (the full authored model).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredAnimation {
    pub id: AssetId,
    pub name: String,
    pub duration: f64,
    pub loop_style: ClipLoop,
    pub speed: f64,
    pub direction: ClipDirection,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub tracks: Vec<StoredTrack>,
}

/// A project-root reference to a persisted clip side-file (mirrors
/// [`crate::CustomMaterialRef`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomAnimationRef {
    pub name: String,
    /// Project-relative file path (e.g. `assets/animations/walk.toml`).
    pub file: PathBuf,
}

// ───────────────────────────── NLA mixer document ───────────────────────────

/// How a mixer layer composites (clips referenced by `AssetId`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum LayerModeDoc {
    #[default]
    Replace,
    Additive {
        #[serde(default)]
        base_clip: Option<AssetId>,
    },
}

/// One clip placement on a mixer layer's timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct StripDoc {
    pub clip: AssetId,
    pub start: f64,
    pub len: f64,
    #[serde(default = "one_f64")]
    pub scale: f64,
    #[serde(default)]
    pub repeat: bool,
}

fn one_f64() -> f64 {
    1.0
}

/// One mixer layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LayerDoc {
    pub mode: LayerModeDoc,
    #[serde(default = "one_f64")]
    pub weight: f64,
    #[serde(default)]
    pub mask_nodes: Vec<NodeId>,
    #[serde(default)]
    pub include_descendants: bool,
    #[serde(default)]
    pub strips: Vec<StripDoc>,
}

impl Default for LayerDoc {
    fn default() -> Self {
        Self {
            mode: LayerModeDoc::Replace,
            weight: 1.0,
            mask_nodes: Vec::new(),
            include_descendants: false,
            strips: Vec::new(),
        }
    }
}

/// The serializable NLA mixer document (controller state). Empty layers ⇒ the
/// renderer plays the active clip on its own clock.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MixerDoc {
    #[serde(default)]
    pub layers: Vec<LayerDoc>,
}

/// Generate the keyframes for a **spin** track: `turns` full revolutions about
/// (normalized) `axis` over `duration` seconds, sampled at `keys_per_turn`
/// keyframes per revolution (min 1). Returns aligned `(times, keys)` of
/// quaternion keyframes (`Interp::Linear`). Backs the `AddSpinTrack` convenience
/// command — collapses the hand-authored "keyframe N quarter-turn quats" workflow
/// into one call.
///
/// Consecutive quaternions are kept hemisphere-continuous (negated when their dot
/// with the previous key is negative) so linear interpolation takes the intended
/// short arc between samples instead of flipping the long way at the
/// double-cover boundary. The endpoints are both included, so a 1-turn spin
/// returns to an identity-equivalent rotation.
pub fn spin_keyframes(
    axis: [f32; 3],
    turns: f32,
    duration: f64,
    keys_per_turn: u32,
) -> (Vec<f64>, Vec<Keyframe>) {
    use std::f32::consts::TAU;
    let kpt = keys_per_turn.max(1);
    // ceil(|turns| * keys_per_turn) segments → +1 for the closing endpoint; never
    // fewer than 2 keys (a degenerate 0-turn still yields start+end at identity).
    let n = ((turns.abs() * kpt as f32).ceil() as usize).max(1) + 1;
    let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    let ax = if len > 1.0e-6 {
        [axis[0] / len, axis[1] / len, axis[2] / len]
    } else {
        [0.0, 1.0, 0.0] // degenerate axis → spin about +Y
    };
    let zero = TrackValue::Quat([0.0, 0.0, 0.0, 0.0]);
    let mut times = Vec::with_capacity(n);
    let mut keys = Vec::with_capacity(n);
    let mut prev: Option<[f32; 4]> = None;
    for k in 0..n {
        let frac = k as f32 / (n - 1) as f32;
        let t = duration * frac as f64;
        let half = TAU * turns * frac * 0.5;
        let s = half.sin();
        let mut q = [ax[0] * s, ax[1] * s, ax[2] * s, half.cos()];
        if let Some(p) = prev {
            let dot = p[0] * q[0] + p[1] * q[1] + p[2] * q[2] + p[3] * q[3];
            if dot < 0.0 {
                for c in q.iter_mut() {
                    *c = -*c;
                }
            }
        }
        prev = Some(q);
        times.push(t);
        keys.push(Keyframe {
            value: TrackValue::Quat(q),
            interp: Interp::Linear,
            in_tangent: zero,
            out_tangent: zero,
        });
    }
    (times, keys)
}

#[cfg(test)]
mod track_value_tests {
    use super::*;

    /// Guard: `TrackValue` must round-trip through both JSON and TOML. An
    /// internally-tagged enum over array payloads silently breaks at runtime;
    /// the adjacently-tagged form (`tag` + `content`) is what makes this pass.
    #[test]
    fn track_value_round_trips_json_and_toml() {
        for v in [
            TrackValue::Vec3([1.0, 2.0, 3.0]),
            TrackValue::Quat([0.0, 0.0, 0.0, 1.0]),
            TrackValue::Scalar(0.75),
            TrackValue::Vec2([0.25, 0.5]),
            TrackValue::Vec4([0.1, 0.2, 0.3, 0.4]),
        ] {
            let j = serde_json::to_string(&v).expect("json ser");
            let back: TrackValue = serde_json::from_str(&j).expect("json de");
            assert_eq!(v, back, "json round-trip: {j}");

            // TOML can't represent a bare value at the top level, so wrap it.
            #[derive(Serialize, Deserialize, PartialEq, Debug)]
            struct Wrap {
                v: TrackValue,
            }
            let w = Wrap { v };
            let t = toml::to_string(&w).expect("toml ser");
            let back: Wrap = toml::from_str(&t).expect("toml de");
            assert_eq!(w, back, "toml round-trip: {t}");
        }
    }

    /// A populated `Keyframe` (which embeds `TrackValue` for value + tangents)
    /// must also round-trip — this is what persistence + commands rely on.
    #[test]
    fn keyframe_round_trips() {
        let k = Keyframe {
            value: TrackValue::Quat([0.1, 0.2, 0.3, 0.92]),
            interp: Interp::Cubic,
            in_tangent: TrackValue::Quat([0.0, 0.0, 0.0, 0.0]),
            out_tangent: TrackValue::Quat([0.0, 0.0, 0.0, 0.0]),
        };
        let j = serde_json::to_string(&k).expect("json ser");
        let back: Keyframe = serde_json::from_str(&j).expect("json de");
        assert_eq!(k, back);
    }

    fn is_identity_rotation(q: [f32; 4]) -> bool {
        // identity OR its double-cover negative: xyz≈0, |w|≈1.
        q[0].abs() < 1.0e-4
            && q[1].abs() < 1.0e-4
            && q[2].abs() < 1.0e-4
            && (q[3].abs() - 1.0).abs() < 1.0e-4
    }

    #[test]
    fn spin_keyframes_one_full_turn() {
        let (times, keys) = spin_keyframes([0.0, 1.0, 0.0], 1.0, 2.0, 4);
        // 4 segments per turn + closing endpoint = 5 keys.
        assert_eq!(times.len(), 5);
        assert_eq!(keys.len(), 5);
        // times span [0, duration], monotonic.
        assert!((times[0]).abs() < 1.0e-9);
        assert!((times[4] - 2.0).abs() < 1.0e-6);
        assert!(times.windows(2).all(|w| w[1] > w[0]));
        // first key = identity; last key = full revolution ⇒ identity rotation.
        let q0 = match keys[0].value {
            TrackValue::Quat(q) => q,
            _ => panic!("expected quat"),
        };
        assert!(is_identity_rotation(q0));
        let q4 = match keys[4].value {
            TrackValue::Quat(q) => q,
            _ => panic!("expected quat"),
        };
        assert!(
            is_identity_rotation(q4),
            "1 turn should return to identity: {q4:?}"
        );
        // 180° key ≈ rotation by π about Y ⇒ |y|≈1, w≈0.
        let q2 = match keys[2].value {
            TrackValue::Quat(q) => q,
            _ => panic!("expected quat"),
        };
        assert!((q2[1].abs() - 1.0).abs() < 1.0e-4, "180deg key: {q2:?}");
        // all keys + sampler are Linear.
        assert!(keys.iter().all(|k| k.interp == Interp::Linear));
    }

    #[test]
    fn spin_keyframes_degenerate_axis_and_quarter_turn() {
        // zero axis falls back to +Y; quarter turn → 2 keys (start + 90°).
        let (times, keys) = spin_keyframes([0.0, 0.0, 0.0], 0.25, 1.0, 4);
        assert_eq!(keys.len(), 2);
        assert_eq!(times.len(), 2);
        let q1 = match keys[1].value {
            TrackValue::Quat(q) => q,
            _ => panic!(),
        };
        // 90° about Y: y = sin(45°) ≈ 0.7071, w = cos(45°) ≈ 0.7071.
        assert!((q1[1] - 0.70710677).abs() < 1.0e-4, "{q1:?}");
        assert!((q1[3] - 0.70710677).abs() < 1.0e-4, "{q1:?}");
    }
}
