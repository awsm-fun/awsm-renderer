//! Animation extraction — reads a source glTF's animations into NEUTRAL specs
//! (raw sampler data: times + flattened output values + interpolation), keyed by
//! glTF node index. Pure glTF reading via the crate's animation channel reader —
//! no renderer types (renderer-gltf's `extract_animations` produces renderer
//! `AnimationClip`s and so can't live here). The editor/player map node_index →
//! NodeId and lower the sampler into their own keyframes at the wiring step.

/// Which node property a channel drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimProperty {
    Translation,
    Rotation,
    Scale,
    MorphWeights,
}

/// glTF sampler interpolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Linear,
    Step,
    CubicSpline,
}

/// One channel: a sampler bound to a glTF node index + property.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimChannel {
    pub node_index: usize,
    pub property: AnimProperty,
    pub interpolation: Interpolation,
    /// Keyframe times (seconds), one per key.
    pub times: Vec<f32>,
    /// Flattened output values: 3/key (translation, scale), 4/key (rotation
    /// quaternion xyzw), or N/key (morph weights, N = morph-target count). For
    /// `CubicSpline` there are 3× as many groups (in-tangent, value, out-tangent).
    pub values: Vec<f32>,
}

/// One glTF animation → a named set of channels.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationSpec {
    pub name: Option<String>,
    pub channels: Vec<AnimChannel>,
}

/// Extract every animation in the document into neutral [`AnimationSpec`]s.
/// `buffers` is the raw glTF buffer-bytes (as from `gltf::import`).
pub fn extract_animations(doc: &gltf::Document, buffers: &[Vec<u8>]) -> Vec<AnimationSpec> {
    use gltf::animation::util::ReadOutputs;
    use gltf::animation::{Interpolation as GltfInterp, Property};

    doc.animations()
        .map(|animation| {
            let channels = animation
                .channels()
                .map(|channel| {
                    let target = channel.target();
                    let node_index = target.node().index();
                    let property = match target.property() {
                        Property::Translation => AnimProperty::Translation,
                        Property::Rotation => AnimProperty::Rotation,
                        Property::Scale => AnimProperty::Scale,
                        Property::MorphTargetWeights => AnimProperty::MorphWeights,
                    };
                    let interpolation = match channel.sampler().interpolation() {
                        GltfInterp::Linear => Interpolation::Linear,
                        GltfInterp::Step => Interpolation::Step,
                        GltfInterp::CubicSpline => Interpolation::CubicSpline,
                    };
                    let reader = channel.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
                    let times: Vec<f32> = reader
                        .read_inputs()
                        .map(|it| it.collect())
                        .unwrap_or_default();
                    let values: Vec<f32> = match reader.read_outputs() {
                        Some(ReadOutputs::Translations(it)) => it.flatten().collect(),
                        Some(ReadOutputs::Scales(it)) => it.flatten().collect(),
                        Some(ReadOutputs::Rotations(rot)) => rot.into_f32().flatten().collect(),
                        Some(ReadOutputs::MorphTargetWeights(w)) => w.into_f32().collect(),
                        None => Vec::new(),
                    };
                    AnimChannel {
                        node_index,
                        property,
                        interpolation,
                        times,
                        values,
                    }
                })
                .collect();
            AnimationSpec {
                name: animation.name().map(String::from),
                channels,
            }
        })
        .collect()
}
