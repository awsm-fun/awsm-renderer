mod animations;
mod blend;
mod clip;
mod clip_group;
mod data;
mod error;
mod interpolate;
mod mixer;
mod player;
mod sampler;
/// Reusable loader for editor-authored (`scene_schema`) animation data — lets a
/// game play authored clips/mixer at runtime, not just glTF. Behind the
/// `scene-schema` feature.
#[cfg(feature = "scene-schema")]
pub mod scene_loader;

pub use animations::{AnimationKey, AnimationMorphKey, Animations};
pub use blend::{blend_additive, blend_replace};
pub use clip::AnimationClip;
pub use clip_group::{
    AnimationChannel, AnimationClipGroup, AnimationClipKey, AnimationTarget, BuiltinMaterialParam,
    CameraParam, LightParam, TexSlot, TexTransformProp,
};
pub use data::{Animatable, AnimationData, TransformAnimation, VertexAnimation};
pub use error::AwsmAnimationError;
pub use mixer::{AnimationLayer, AnimationMixer, AnimationStrip, LayerMode, TargetMask};
pub use player::{AnimationLoopStyle, AnimationPlayDirection, AnimationPlayer, AnimationState};
pub use sampler::AnimationSampler;
