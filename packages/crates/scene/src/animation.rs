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
pub enum ClipLoop {
    Loop,
    PingPong,
    Once,
}

/// The default playback direction a clip's clock advances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipDirection {
    Forward,
    Reverse,
}

/// Per-keyframe interpolation (display); lowering uses the track's [`SamplerKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    Step,
    Linear,
    Cubic,
}

/// The renderer sampler kind a whole track lowers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
pub struct Keyframe {
    pub value: TrackValue,
    pub interp: Interp,
    pub in_tangent: TrackValue,
    pub out_tangent: TrackValue,
}

/// Which transform component a transform track drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
}
