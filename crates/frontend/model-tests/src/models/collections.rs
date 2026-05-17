use std::collections::HashMap;
use std::hash::Hash;

use crate::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GltfSetId {
    Standard,
    Animation,
    Basics,
    Extensions,
}

impl GltfSetId {
    pub fn list() -> Vec<GltfSetId> {
        vec![
            GltfSetId::Standard,
            GltfSetId::Animation,
            GltfSetId::Basics,
            GltfSetId::Extensions,
        ]
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Animation => "Animation",
            Self::Basics => "Basics",
            Self::Extensions => "Extensions",
        }
    }
}
pub static GLTF_SETS: LazyLock<HashMap<GltfSetId, Vec<GltfId>>> = LazyLock::new(|| {
    let mut h = HashMap::new();

    h.insert(
        GltfSetId::Standard,
        vec![GltfId::DamagedHelmet, GltfId::AlphaBlendMode],
    );

    h.insert(
        GltfSetId::Animation,
        vec![
            GltfId::BrainStem,
            GltfId::Fox,
            GltfId::SimpleSkin,
            GltfId::SimpleMorph,
            GltfId::AnimatedTriangle,
            GltfId::AnimatedMorphCube,
            GltfId::InterpolationTest,
            GltfId::RiggedSimple,
            GltfId::RiggedFigure,
            GltfId::RecursiveSkeletons,
            GltfId::MorphStressTest,
        ],
    );

    h.insert(
        GltfSetId::Basics,
        vec![
            GltfId::CompareBaseColor,
            GltfId::TextureCoordinate,
            GltfId::TextureLinearInterpolation,
            GltfId::TextureSettings,
            GltfId::VertexColor,
            GltfId::BoomBoxAxes,
            GltfId::TriangleWithoutIndices,
            GltfId::SimpleSparseAccessor,
            GltfId::SimpleMeshes,
            GltfId::SimpleTexture,
            GltfId::SimpleMaterial,
            GltfId::MorphPrimitives,
            GltfId::MultiUv,
            GltfId::NegativeScale,
            GltfId::Orientation,
            GltfId::NormalTangent,
            GltfId::NormalTangentMirror,
            GltfId::Triangle,
            GltfId::BoxTextured,
            GltfId::MetalRoughSpheresTextureless,
            GltfId::MetalRoughSpheres,
            GltfId::Box,
            GltfId::BoxInterleaved,
            GltfId::BoxTexturedNpoT,
            GltfId::BoxWithSpaces,
            GltfId::BoxVertexColors,
            GltfId::Cube,
            GltfId::EnvironmentTest,
            GltfId::EnvironmentIblTest,
            GltfId::CompareAlphaCoverage,
            GltfId::CompareMetallic,
            GltfId::CompareNormal,
            GltfId::CompareRoughness,
        ],
    );

    h.insert(
        GltfSetId::Extensions,
        vec![
            GltfId::ClearCoatCarPaint,
            GltfId::ClearCoatWicker,
            GltfId::ClearCoatTest,
            GltfId::SheenChair,
            GltfId::SheenCloth,
            GltfId::SheenTestGrid,
            GltfId::SheenWoodLeatherSofa,
            GltfId::TransmissionRoughness,
            GltfId::IorTestGrid,
            GltfId::SimpleInstancing,
            GltfId::EmissiveStrength,
            GltfId::TextureTransformMultiTest,
            GltfId::TextureTransformTest,
            GltfId::CompareAnisotropy,
            GltfId::CompareAmbientOcclusion,
            GltfId::CompareClearcoat,
            GltfId::CompareDispersion,
            GltfId::CompareEmissiveStrength,
            GltfId::CompareIor,
            GltfId::CompareIridescence,
            GltfId::CompareSheen,
            GltfId::CompareSpecular,
            GltfId::CompareTransmission,
            GltfId::CompareVolume,
            GltfId::UnlitTest,
            GltfId::SpecularTest,
        ],
    );

    // make sure no ids are in multiple sets
    let mut all_ids = std::collections::HashSet::new();
    for ids in h.values() {
        for id in ids {
            if !all_ids.insert(id) {
                panic!("[{:?}] is in multiple sets!", id);
            }
        }
    }

    for collection in h.values_mut() {
        collection.sort();
    }

    h
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GltfId {
    #[default]
    Fox,
    DamagedHelmet,
    ClearCoatTest,
    ClearCoatCarPaint,
    ClearCoatWicker,
    SheenChair,
    SheenCloth,
    SheenTestGrid,
    SheenWoodLeatherSofa,
    TransmissionRoughness,
    BrainStem,
    AlphaBlendMode,
    BoomBoxAxes,
    MetalRoughSpheres,
    MetalRoughSpheresTextureless,
    MorphPrimitives,
    MorphStressTest,
    MultiUv,
    NegativeScale,
    NormalTangent,
    NormalTangentMirror,
    Orientation,
    RecursiveSkeletons,
    TextureCoordinate,
    TextureLinearInterpolation,
    TextureSettings,
    VertexColor,
    TriangleWithoutIndices,
    Triangle,
    SimpleSparseAccessor,
    SimpleMeshes,
    SimpleMorph,
    AnimatedTriangle,
    AnimatedMorphCube,
    SimpleSkin,
    SimpleInstancing,
    SimpleTexture,
    SimpleMaterial,
    InterpolationTest,
    Box,
    BoxInterleaved,
    BoxTextured,
    BoxTexturedNpoT,
    BoxWithSpaces,
    BoxVertexColors,
    Cube,
    CompareAlphaCoverage,
    CompareAmbientOcclusion,
    CompareAnisotropy,
    CompareBaseColor,
    CompareClearcoat,
    CompareDispersion,
    CompareEmissiveStrength,
    CompareIor,
    CompareIridescence,
    CompareMetallic,
    CompareNormal,
    CompareRoughness,
    CompareSheen,
    CompareSpecular,
    CompareTransmission,
    CompareVolume,
    RiggedFigure,
    RiggedSimple,
    EnvironmentTest,
    EnvironmentIblTest,
    EmissiveStrength,
    TextureTransformTest,
    TextureTransformMultiTest,
    UnlitTest,
    SpecularTest,
    AwsmTransformGizmo,
    IorTestGrid,
}

impl PartialOrd for GltfId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GltfId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self == other {
            return std::cmp::Ordering::Equal;
        }

        // prioritize some special cases
        match (self, other) {
            (GltfId::Fox, _) => return std::cmp::Ordering::Less,
            (_, GltfId::Fox) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        match (self, other) {
            (GltfId::MorphStressTest, _) => return std::cmp::Ordering::Less,
            (_, GltfId::MorphStressTest) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        match (self, other) {
            (GltfId::DamagedHelmet, _) => return std::cmp::Ordering::Less,
            (_, GltfId::DamagedHelmet) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        match (self, other) {
            (GltfId::AlphaBlendMode, _) => return std::cmp::Ordering::Less,
            (_, GltfId::AlphaBlendMode) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        self.label().cmp(other.label())
    }
}

impl TryFrom<&str> for GltfId {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let list: Vec<&GltfId> = GLTF_SETS.iter().flat_map(|x| x.1).collect();

        for id in list {
            let id_str = id.to_string();
            if id_str == s {
                return Ok(*id);
            }
        }

        Err(format!("{} is not a valid GltfId", s))
    }
}

impl std::fmt::Display for GltfId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl GltfId {
    pub fn url(&self) -> String {
        let base_url = match self {
            Self::AwsmTransformGizmo => &CONFIG.media_base_url_additional_assets,
            _ => &CONFIG.media_base_url_gltf_samples,
        };

        format!("{}/{}", base_url, self.filepath())
    }

    pub fn filepath(&self) -> &'static str {
        match self {
            Self::ClearCoatCarPaint => "ClearCoatCarPaint/glTF/ClearCoatCarPaint.gltf",
            Self::ClearCoatWicker => "ClearCoatWicker/glTF/ClearCoatWicker.gltf",
            Self::SheenChair => "SheenChair/glTF/SheenChair.gltf",
            Self::SheenCloth => "SheenCloth/glTF/SheenCloth.gltf",
            Self::SheenTestGrid => "SheenTestGrid/glTF/SheenTestGrid.gltf",
            Self::SheenWoodLeatherSofa => "SheenWoodLeatherSofa/glTF/SheenWoodLeatherSofa.gltf",
            Self::ClearCoatTest => "ClearCoatTest/glTF/ClearCoatTest.gltf",
            Self::TransmissionRoughness => {
                "TransmissionRoughnessTest/glTF/TransmissionRoughnessTest.gltf"
            }
            Self::IorTestGrid => "IORTestGrid/glTF/IORTestGrid.gltf",
            Self::UnlitTest => "UnlitTest/glTF/UnlitTest.gltf",
            Self::SpecularTest => "SpecularTest/glTF/SpecularTest.gltf",
            Self::BrainStem => "BrainStem/glTF/BrainStem.gltf",
            Self::Fox => "Fox/glTF/Fox.gltf",
            Self::AlphaBlendMode => "AlphaBlendModeTest/glTF/AlphaBlendModeTest.gltf",
            Self::BoomBoxAxes => "BoomBoxWithAxes/glTF/BoomBoxWithAxes.gltf",
            Self::MetalRoughSpheres => "MetalRoughSpheres/glTF/MetalRoughSpheres.gltf",
            Self::MetalRoughSpheresTextureless => {
                "MetalRoughSpheresNoTextures/glTF/MetalRoughSpheresNoTextures.gltf"
            }
            Self::MorphPrimitives => "MorphPrimitivesTest/glTF/MorphPrimitivesTest.gltf",
            Self::MorphStressTest => "MorphStressTest/glTF/MorphStressTest.gltf",
            Self::MultiUv => "MultiUVTest/glTF/MultiUVTest.gltf",
            Self::NegativeScale => "NegativeScaleTest/glTF/NegativeScaleTest.gltf",
            Self::NormalTangent => "NormalTangentTest/glTF/NormalTangentTest.gltf",
            Self::NormalTangentMirror => {
                "NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf"
            }
            Self::Orientation => "OrientationTest/glTF/OrientationTest.gltf",
            Self::RecursiveSkeletons => "RecursiveSkeletons/glTF/RecursiveSkeletons.gltf",
            Self::TextureCoordinate => "TextureCoordinateTest/glTF/TextureCoordinateTest.gltf",
            Self::TextureLinearInterpolation => {
                "TextureLinearInterpolationTest/glTF/TextureLinearInterpolationTest.gltf"
            }
            Self::TextureSettings => "TextureSettingsTest/glTF/TextureSettingsTest.gltf",
            Self::TextureTransformTest => "TextureTransformTest/glTF/TextureTransformTest.gltf",
            Self::TextureTransformMultiTest => {
                "TextureTransformMultiTest/glTF/TextureTransformMultiTest.gltf"
            }
            Self::VertexColor => "VertexColorTest/glTF/VertexColorTest.gltf",
            Self::TriangleWithoutIndices => {
                "TriangleWithoutIndices/glTF/TriangleWithoutIndices.gltf"
            }
            Self::Triangle => "Triangle/glTF/Triangle.gltf",
            Self::SimpleSparseAccessor => "SimpleSparseAccessor/glTF/SimpleSparseAccessor.gltf",
            Self::SimpleMeshes => "SimpleMeshes/glTF/SimpleMeshes.gltf",
            Self::SimpleMorph => "SimpleMorph/glTF/SimpleMorph.gltf",
            Self::SimpleInstancing => "SimpleInstancing/glTF/SimpleInstancing.gltf",
            Self::SimpleTexture => "SimpleTexture/glTF/SimpleTexture.gltf",
            Self::SimpleMaterial => "SimpleMaterial/glTF/SimpleMaterial.gltf",
            Self::AnimatedTriangle => "AnimatedTriangle/glTF/AnimatedTriangle.gltf",
            Self::AnimatedMorphCube => "AnimatedMorphCube/glTF/AnimatedMorphCube.gltf",
            Self::SimpleSkin => "SimpleSkin/glTF/SimpleSkin.gltf",
            Self::InterpolationTest => "InterpolationTest/glTF/InterpolationTest.gltf",
            Self::Box => "Box/glTF/Box.gltf",
            Self::BoxInterleaved => "BoxInterleaved/glTF/BoxInterleaved.gltf",
            Self::BoxTextured => "BoxTextured/glTF/BoxTextured.gltf",
            Self::BoxTexturedNpoT => "BoxTexturedNonPowerOfTwo/glTF/BoxTexturedNonPowerOfTwo.gltf",
            Self::BoxWithSpaces => "Box With Spaces/glTF/Box With Spaces.gltf",
            Self::BoxVertexColors => "BoxVertexColors/glTF/BoxVertexColors.gltf",
            Self::Cube => "Cube/glTF/Cube.gltf",
            Self::CompareAlphaCoverage => "CompareAlphaCoverage/glTF/CompareAlphaCoverage.gltf",
            Self::CompareAmbientOcclusion => {
                "CompareAmbientOcclusion/glTF/CompareAmbientOcclusion.gltf"
            }
            Self::CompareAnisotropy => "CompareAnisotropy/glTF/CompareAnisotropy.gltf",
            Self::CompareBaseColor => "CompareBaseColor/glTF/CompareBaseColor.gltf",
            Self::CompareClearcoat => "CompareClearcoat/glTF/CompareClearcoat.gltf",
            Self::CompareDispersion => "CompareDispersion/glTF/CompareDispersion.gltf",
            Self::CompareEmissiveStrength => {
                "CompareEmissiveStrength/glTF/CompareEmissiveStrength.gltf"
            }
            Self::CompareIor => "CompareIor/glTF/CompareIor.gltf",
            Self::CompareIridescence => "CompareIridescence/glTF/CompareIridescence.gltf",
            Self::CompareMetallic => "CompareMetallic/glTF/CompareMetallic.gltf",
            Self::CompareNormal => "CompareNormal/glTF/CompareNormal.gltf",
            Self::CompareRoughness => "CompareRoughness/glTF/CompareRoughness.gltf",
            Self::CompareSheen => "CompareSheen/glTF/CompareSheen.gltf",
            Self::CompareSpecular => "CompareSpecular/glTF/CompareSpecular.gltf",
            Self::CompareTransmission => "CompareTransmission/glTF/CompareTransmission.gltf",
            Self::CompareVolume => "CompareVolume/glTF/CompareVolume.gltf",
            Self::RiggedFigure => "RiggedFigure/glTF/RiggedFigure.gltf",
            Self::RiggedSimple => "RiggedSimple/glTF/RiggedSimple.gltf",
            Self::DamagedHelmet => "DamagedHelmet/glTF/DamagedHelmet.gltf",
            Self::EnvironmentTest => "EnvironmentTest/glTF/EnvironmentTest.gltf",
            Self::EnvironmentIblTest => "EnvironmentTest/glTF-IBL/EnvironmentTest.gltf",
            Self::EmissiveStrength => "EmissiveStrengthTest/glTF/EmissiveStrengthTest.gltf",
            Self::AwsmTransformGizmo => "gizmo/gizmo.glb",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::ClearCoatCarPaint => "Clear Coat Car Paint",
            Self::ClearCoatWicker => "Clear Coat Wicker",
            Self::SheenChair => "Sheen Chair",
            Self::SheenCloth => "Sheen Cloth",
            Self::SheenTestGrid => "Sheen Test Grid",
            Self::SheenWoodLeatherSofa => "Sheen Wood Leather Sofa",
            Self::ClearCoatTest => "ClearCoatTest",
            Self::TransmissionRoughness => "TransmissionRoughnessTest",
            Self::IorTestGrid => "Ior test grid",
            Self::SpecularTest => "Specular test",
            Self::UnlitTest => "Unlit test",
            Self::BrainStem => "Brain stem",
            Self::Fox => "Fox",
            Self::AlphaBlendMode => "Alpha blend mode",
            Self::BoomBoxAxes => "Boom box w/ axes",
            Self::MetalRoughSpheres => "Metal rough spheres",
            Self::MetalRoughSpheresTextureless => "Metal rough spheres w/o textures",
            Self::MorphPrimitives => "Morph primitives",
            Self::MorphStressTest => "Morph stress test",
            Self::MultiUv => "Multi uvs",
            Self::NegativeScale => "Negative scale",
            Self::NormalTangent => "Normal tangent auto",
            Self::NormalTangentMirror => "Normal tangent supplied",
            Self::Orientation => "Orientation",
            Self::RecursiveSkeletons => "Recursive skeletons",
            Self::TextureCoordinate => "Texture coordinates",
            Self::TextureLinearInterpolation => "Linear texture interpolation",
            Self::TextureSettings => "Texture settings",
            Self::TextureTransformTest => "Texture transform test",
            Self::TextureTransformMultiTest => "Texture transform multi test",
            Self::VertexColor => "Vertex colors",
            Self::TriangleWithoutIndices => "Triangle without indices",
            Self::Triangle => "Triangle",
            Self::SimpleSparseAccessor => "Simple Sparse Accessor",
            Self::SimpleMeshes => "Simple Meshes",
            Self::SimpleMorph => "Simple Morph",
            Self::SimpleInstancing => "Simple Instancing",
            Self::SimpleTexture => "Simple Texture",
            Self::SimpleMaterial => "Simple Material",
            Self::AnimatedTriangle => "Animated Triangle",
            Self::AnimatedMorphCube => "Animated Morph Cube",
            Self::SimpleSkin => "Simple Skin",
            Self::InterpolationTest => "Interpolation Test",
            Self::Box => "Box",
            Self::BoxInterleaved => "BoxInterleaved",
            Self::BoxTextured => "BoxTextured",
            Self::BoxTexturedNpoT => "BoxTextured non-power-of-2",
            Self::BoxWithSpaces => "Box with spaces",
            Self::BoxVertexColors => "Box vertex colors",
            Self::Cube => "Cube",
            Self::CompareAlphaCoverage => "Alpha coverage compare",
            Self::CompareAmbientOcclusion => "Ambient occlusion compare",
            Self::CompareAnisotropy => "Anisotropy compare",
            Self::CompareBaseColor => "Base color compare",
            Self::CompareClearcoat => "Clearcoat compare",
            Self::CompareDispersion => "Dispersion compare",
            Self::CompareEmissiveStrength => "Emissive strength compare",
            Self::CompareIor => "IOR compare",
            Self::CompareIridescence => "Iridescence compare",
            Self::CompareMetallic => "Metallic compare",
            Self::CompareNormal => "Normal compare",
            Self::CompareRoughness => "Roughness compare",
            Self::CompareSheen => "Sheen compare",
            Self::CompareSpecular => "Specular compare",
            Self::CompareTransmission => "Transmission compare",
            Self::CompareVolume => "Volume compare",
            Self::RiggedFigure => "Rigged figure",
            Self::RiggedSimple => "Rigged simple",
            Self::DamagedHelmet => "Damaged helmet",
            Self::EnvironmentTest => "Environment test",
            Self::EnvironmentIblTest => "Environment ibl test",
            Self::EmissiveStrength => "Emissive strength",
            Self::AwsmTransformGizmo => "Awsm Transform Gizmo",
        }
    }
}
