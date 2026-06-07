//! Renderer-free glTF animation extraction.
//!
//! Unlike [`crate::populate`], which inserts animation players into a live
//! [`AwsmRenderer`](awsm_renderer::AwsmRenderer), this module only *parses*
//! animation data out of a [`gltf::Document`] + its raw buffers. It needs no
//! GPU and no renderer instance.
//!
//! The result is keyed by glTF node index (`channel.target().node().index()`).
//! The editor consumes this to build authored clips: it maps each
//! `node_index` to its own `NodeId` and lowers each channel's
//! [`AnimationClip`] sampler into editable keyframes (rotation is
//! quaternion-native).
//!
//! The actual Linear / Step / CubicSpline + chunk-size parsing is *not*
//! duplicated here — it delegates to the shared helpers in
//! [`crate::populate::animation`], which the populate path also uses, so
//! extraction and population stay byte-identical.

use awsm_renderer::animation::AnimationClip;

use crate::{
    error::Result,
    populate::animation::{
        gltf_animation_clip_morph_from_buffers, gltf_animation_clip_transform_from_buffers,
        TransformTarget,
    },
};

/// Which node property a channel drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractedProperty {
    Translation,
    Rotation,
    Scale,
    MorphWeights,
}

/// One channel of an extracted clip: a parsed sampler bound to a glTF node
/// index + property. The editor maps `node_index` -> its NodeId and lowers
/// `clip`'s sampler into its authored keyframes (rotation is quaternion-native).
pub struct ExtractedChannel {
    pub node_index: usize,
    pub property: ExtractedProperty,
    /// name / duration / sampler (`AnimationData`). For T/R/S this carries
    /// `AnimationData::Transform`; for `MorphWeights` it carries
    /// `AnimationData::Vertex` chunks of per-keyframe weights.
    pub clip: AnimationClip,
}

/// One glTF animation -> a named set of channels (the editor's "Clip").
pub struct ExtractedAnimation {
    pub name: Option<String>,
    pub channels: Vec<ExtractedChannel>,
}

/// Walks every animation in `doc` and returns the per-channel parsed clip
/// data, keyed (per channel) by glTF node index + property.
///
/// `buffers` is the same raw glTF buffer-bytes that the populate path reads
/// via `accessor_to_bytes` (i.e. `GltfData::buffers.raw`).
///
/// A morph-weights channel whose target node has no mesh morph targets is
/// skipped with a single `tracing::warn!` (it is malformed data, not a hard
/// error). The four `gltf::animation::Property` variants are otherwise mapped
/// exhaustively.
pub fn extract_animations(
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
) -> Result<Vec<ExtractedAnimation>> {
    let mut out = Vec::with_capacity(doc.animations().len());

    for animation in doc.animations() {
        let name = animation.name().map(String::from);
        let mut channels = Vec::new();

        for channel in animation.channels() {
            let target = channel.target();
            let node = target.node();
            let node_index = node.index();
            let gltf_sampler = channel.sampler();

            let (property, clip) = match target.property() {
                gltf::animation::Property::Translation => (
                    ExtractedProperty::Translation,
                    gltf_animation_clip_transform_from_buffers(
                        &gltf_sampler,
                        TransformTarget::Translation,
                        buffers,
                    )?,
                ),
                gltf::animation::Property::Rotation => (
                    ExtractedProperty::Rotation,
                    gltf_animation_clip_transform_from_buffers(
                        &gltf_sampler,
                        TransformTarget::Rotation,
                        buffers,
                    )?,
                ),
                gltf::animation::Property::Scale => (
                    ExtractedProperty::Scale,
                    gltf_animation_clip_transform_from_buffers(
                        &gltf_sampler,
                        TransformTarget::Scale,
                        buffers,
                    )?,
                ),
                gltf::animation::Property::MorphTargetWeights => {
                    // Per-keyframe weight count = the node mesh's morph-target
                    // count. Same source the renderer's morph-info uses
                    // (`primitive.morph_targets().len()`), just read straight
                    // off the document here since we have no renderer.
                    let targets_len = node
                        .mesh()
                        .and_then(|mesh| mesh.primitives().next())
                        .map(|primitive| primitive.morph_targets().len())
                        .unwrap_or(0);

                    if targets_len == 0 {
                        tracing::warn!(
                            node_index,
                            "skipping morph-weights animation channel: node has no \
                             mesh morph targets"
                        );
                        continue;
                    }

                    (
                        ExtractedProperty::MorphWeights,
                        gltf_animation_clip_morph_from_buffers(
                            &gltf_sampler,
                            targets_len,
                            buffers,
                        )?,
                    )
                }
            };

            channels.push(ExtractedChannel {
                node_index,
                property,
                clip,
            });
        }

        out.push(ExtractedAnimation { name, channels });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ExtractedProperty` maps every `gltf::animation::Property` variant
    /// exhaustively (the `match` itself is the totality check; if `gltf` ever
    /// adds a variant this stops compiling).
    #[test]
    fn property_mapping_is_total() {
        fn map(p: gltf::animation::Property) -> ExtractedProperty {
            match p {
                gltf::animation::Property::Translation => ExtractedProperty::Translation,
                gltf::animation::Property::Rotation => ExtractedProperty::Rotation,
                gltf::animation::Property::Scale => ExtractedProperty::Scale,
                gltf::animation::Property::MorphTargetWeights => ExtractedProperty::MorphWeights,
            }
        }

        assert_eq!(
            map(gltf::animation::Property::Translation),
            ExtractedProperty::Translation
        );
        assert_eq!(
            map(gltf::animation::Property::Rotation),
            ExtractedProperty::Rotation
        );
        assert_eq!(
            map(gltf::animation::Property::Scale),
            ExtractedProperty::Scale
        );
        assert_eq!(
            map(gltf::animation::Property::MorphTargetWeights),
            ExtractedProperty::MorphWeights
        );
    }

    /// An empty document yields no animations (and does not panic on the
    /// empty buffer slice).
    #[test]
    fn empty_doc_yields_no_animations() {
        let (doc, _buffers, _images) = gltf::import_slice(MINIMAL_GLTF).unwrap();
        let extracted = extract_animations(&doc, &[]).unwrap();
        assert!(extracted.is_empty());
    }

    // Smallest valid glTF 2.0 document: an empty asset with one empty scene.
    const MINIMAL_GLTF: &[u8] = br#"{
        "asset": { "version": "2.0" },
        "scenes": [ { "nodes": [] } ],
        "scene": 0
    }"#;
}
