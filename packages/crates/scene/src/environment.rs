use super::assets::AssetId;

/// Image-based-lighting + skybox for a scene. Three **independent** slots, each a
/// self-contained [`EnvSlot`]:
/// - `skybox`     — the background cubemap the camera sees.
/// - `specular`   — the prefiltered (roughness-mipped) env map that drives
///   specular reflections. ("Prefiltered env" and "specular" are the same thing.)
/// - `irradiance` — the diffuse-convolved env map that drives ambient lighting.
///
/// Slots are fully decoupled: a scene can keep the built-in default sky for the
/// skybox and irradiance while overriding *only* the specular with a KTX file,
/// or any other mix. Each slot serializes inline into the scene document.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct EnvironmentConfig {
    #[serde(default)]
    pub skybox: EnvSlot,
    #[serde(default)]
    pub specular: EnvSlot,
    #[serde(default)]
    pub irradiance: EnvSlot,
    #[serde(default)]
    pub probe: ReflectionProbe,
    /// Per-slot resolution STREAMING ladders (§ progressive environments).
    /// Each slot's base (`skybox` / `specular` / `irradiance`) stays the
    /// thing a load BLOCKS on — author it small. `stream` then lists
    /// higher-resolution KTX2 cubemaps for that slot in ASCENDING quality
    /// order; players fetch them after load and swap each in as it decodes,
    /// so the background/reflections sharpen progressively while play has
    /// already begun. Empty ladders (the default) mean no streaming — every
    /// pre-feature document parses to that.
    #[serde(default)]
    pub stream: EnvStream,
    /// Per-slot rigid rotation of the environment cubemaps. PER SLOT, and
    /// deliberately so: pointing the background one way while the reflections
    /// or the ambient come from another is a real authoring move (aim a bake's
    /// interesting quadrant at the camera for reflections while keeping the
    /// visible backdrop where it was, key a room from one side without
    /// swinging the walls, …). Slots stay as decoupled here as they are
    /// everywhere else in this struct.
    #[serde(default)]
    pub rotation: EnvRotation,
}

/// Per-slot streaming ladders, mirroring the slot fields on
/// [`EnvironmentConfig`] one-for-one (the same shape discipline as
/// [`EnvRotation`]). Entries are KTX2 cubemap assets in ASCENDING quality
/// order; the slot's base asset is NOT repeated here. Only file-based
/// levels exist — procedural slots have nothing to stream.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct EnvStream {
    /// Higher-res background cubemaps, streamed after load.
    #[serde(default)]
    pub skybox: Vec<AssetId>,
    /// Higher-res prefiltered (specular) maps — reflections sharpen in.
    #[serde(default)]
    pub specular: Vec<AssetId>,
    /// Higher-res irradiance maps. Rarely worth a ladder (irradiance is
    /// tiny), present for slot uniformity.
    #[serde(default)]
    pub irradiance: Vec<AssetId>,
}

impl EnvStream {
    /// Whether no slot has a ladder.
    pub fn is_empty(&self) -> bool {
        self.skybox.is_empty() && self.specular.is_empty() && self.irradiance.is_empty()
    }

    /// Every asset id in every ladder, in slot order then ladder order.
    pub fn asset_ids(&self) -> impl Iterator<Item = &AssetId> {
        self.skybox
            .iter()
            .chain(self.specular.iter())
            .chain(self.irradiance.iter())
    }
}

/// Euler-degree rotations for the three environment slots, mirroring the slot
/// fields on [`EnvironmentConfig`] one-for-one.
///
/// This is an AUTHORING transform on the environment, not on the scene: it
/// turns a cubemap under a fixed world, letting a bake whose interesting
/// quadrant faces the wrong way be aimed at the camera without re-baking.
///
/// Angles are DEGREES applied X then Y then Z (intrinsic), which makes
/// `[0, 180, 0]` the "spin the room around" knob an author reaches for most.
/// All-zero is identity, and costs one mat3 multiply per env fetch — the
/// matrices upload already-inverted, so no shader ever inverts per pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct EnvRotation {
    /// Turns the visible background only.
    #[serde(default)]
    pub skybox: [f32; 3],
    /// Turns the prefiltered (roughness-mipped) map that drives REFLECTIONS —
    /// both the material IBL specular term and the SSR miss fallback.
    #[serde(default)]
    pub specular: [f32; 3],
    /// Turns the diffuse-convolved map that drives AMBIENT light.
    #[serde(default)]
    pub irradiance: [f32; 3],
}

impl EnvRotation {
    /// Whether every slot is unrotated.
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// The same rotation on all three slots — the "turn the whole room"
    /// shorthand, for when the slots are NOT meant to disagree.
    pub fn uniform(euler_degrees: [f32; 3]) -> Self {
        Self {
            skybox: euler_degrees,
            specular: euler_degrees,
            irradiance: euler_degrees,
        }
    }
}

impl EnvironmentConfig {
    /// Every KTX2 cubemap asset id this environment references (across all three
    /// slots, when file-based). These are exactly the ids whose BYTES must
    /// accompany the config — the editor's Save/export write them to
    /// [`crate::project_dir::env_ktx_path`] and the player's `apply_environment`
    /// reads them back from the same path. Procedural variants (built-in default
    /// / sky-gradient) reference no assets. Duplicates are preserved so the count
    /// reflects the referencing slots, but callers that dedup (bundle/save) are
    /// free to collect into a set.
    pub fn ktx_asset_ids(&self) -> Vec<AssetId> {
        [&self.skybox, &self.specular, &self.irradiance]
            .into_iter()
            .filter_map(|slot| match slot {
                EnvSlot::Ktx { asset_id } => Some(*asset_id),
                _ => None,
            })
            .chain(self.stream.asset_ids().copied())
            .collect()
    }
}

/// Box-projected reflection probe: anchors the specular-env fallback to the
/// scene's actual bounds (parallax correction). When enabled, every specular
/// env-map lookup (IBL specular + the SSR miss fallback) intersects the
/// reflection ray with this axis-aligned box and samples the cubemap toward
/// the INTERSECTION point instead of along the raw direction — so fallback
/// reflections track the surface's position inside the room/arena rather than
/// behaving like an infinitely-distant sky. One global probe per scene (MVP);
/// disabled = classic direction-only sampling, bit-for-bit the old behavior.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Copy)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ReflectionProbe {
    #[serde(default)]
    pub enabled: bool,
    /// World-space center of the projection box (usually also where the
    /// probe cubemap was authored/captured from).
    #[serde(default)]
    pub center: [f32; 3],
    /// Half-extents of the projection box, in meters. Must be > 0 on every
    /// axis when enabled.
    #[serde(default = "default_probe_half_extents")]
    pub half_extents: [f32; 3],
}

fn default_probe_half_extents() -> [f32; 3] {
    [10.0, 10.0, 10.0]
}

impl Default for ReflectionProbe {
    fn default() -> Self {
        Self {
            enabled: false,
            center: [0.0; 3],
            half_extents: default_probe_half_extents(),
        }
    }
}

/// A single environment slot (skybox / specular / irradiance). All three slots
/// share this type; the *role* (and therefore the generated resolution for the
/// procedural variants) is decided by which field it fills, not by the enum.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Copy, Default)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum EnvSlot {
    /// The baked-in "Default sky" — a procedural [`CubemapSkyGradient`] default.
    /// Referenced by no asset.
    #[default]
    BuiltInDefault,
    /// A KTX2 cubemap asset (skybox faces, or a prefiltered/irradiance map).
    Ktx { asset_id: AssetId },
    /// Agent-authored two-color sky gradient (zenith→nadir), linear RGB. The
    /// generic "environment from agent data" hook (§18): pick a zenith (sky) and
    /// nadir (ground) color to author dusk / overcast / night / studio — no
    /// preset menu, no externally-hosted `.ktx2` required. Same generator the
    /// built-in default uses (`CubemapSkyGradient`).
    SkyGradient { zenith: [f32; 3], nadir: [f32; 3] },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `project.toml`-shaped `[environment]` block with KTX **specular +
    /// irradiance** and a gradient skybox must parse back as `Ktx` on both
    /// slots. Guards the open→save round-trip: a silent fall back to the
    /// default here loses a scene's whole HDR environment with no error
    /// (docs/plans/env-ktx-lost-on-project-load.md).
    #[test]
    fn ktx_specular_and_irradiance_parse_from_project_toml_shape() {
        let toml_src = r#"
[skybox.sky_gradient]
zenith = [0.015, 0.02, 0.05]
nadir = [0.004, 0.004, 0.008]

[specular.ktx]
asset_id = "7ac215ae-1e66-4ad1-8bf7-f3b8d5566668"

[irradiance.ktx]
asset_id = "99e98c3a-fc34-42da-88bf-fc415cf61589"

[probe]
enabled = true
center = [0.0, 1.6, 0.0]
half_extents = [3.4, 2.0, 2.3]
"#;
        let cfg: EnvironmentConfig = toml::from_str(toml_src).expect("parse [environment] block");
        assert!(
            matches!(cfg.specular, EnvSlot::Ktx { .. }),
            "specular parsed as {:?}, expected Ktx",
            cfg.specular
        );
        assert!(
            matches!(cfg.irradiance, EnvSlot::Ktx { .. }),
            "irradiance parsed as {:?}, expected Ktx",
            cfg.irradiance
        );
        assert_eq!(
            cfg.ktx_asset_ids().len(),
            2,
            "both KTX slots must be reported"
        );
        assert!(cfg.probe.enabled);
    }

    /// The three slots are fully independent: skybox / specular / irradiance can
    /// each be a different kind (built-in default, sky-gradient, or KTX) in the
    /// SAME config, and it round-trips through the scene.toml / project.toml serde
    /// shape unchanged. `ktx_asset_ids()` reports only the KTX slot, so default +
    /// gradient slots ship no side files.
    #[test]
    fn per_slot_kinds_are_independent_and_round_trip() {
        let cfg = EnvironmentConfig {
            skybox: EnvSlot::BuiltInDefault,
            specular: EnvSlot::SkyGradient {
                zenith: [0.1, 0.3, 0.9],
                nadir: [0.02, 0.02, 0.05],
            },
            irradiance: EnvSlot::Ktx {
                asset_id: AssetId::new(),
            },
            probe: Default::default(),
            // A DIFFERENT non-trivial rotation per slot — a serde shape that
            // dropped a field, reordered them, or collapsed the three back
            // into one shared value diverges here.
            rotation: EnvRotation {
                skybox: [15.0, -120.0, 7.5],
                specular: [0.0, 44.0, 0.0],
                irradiance: [-8.0, 0.0, 190.0],
            },
            stream: EnvStream {
                skybox: vec![AssetId::new(), AssetId::new()],
                specular: vec![AssetId::new()],
                irradiance: vec![],
            },
        };
        let toml = toml::to_string_pretty(&cfg).unwrap();
        let back: EnvironmentConfig = toml::from_str(&toml).unwrap();
        assert_eq!(cfg, back, "mixed per-slot env round-trips");
        assert_eq!(
            cfg.ktx_asset_ids().len(),
            4,
            "the KTX slot AND every streaming-ladder level carry bytes"
        );
    }

    /// `stream` is `#[serde(default)]`: every environment block written before
    /// streaming ladders existed still deserializes, to EMPTY ladders — no
    /// phantom levels, no parse failure.
    #[test]
    fn stream_defaults_to_empty_for_pre_feature_documents() {
        let legacy = r#"
            [skybox]
            built_in_default = {}
            [specular]
            built_in_default = {}
            [irradiance]
            built_in_default = {}
        "#;
        let cfg: EnvironmentConfig =
            toml::from_str(legacy).expect("pre-stream environment still deserializes");
        assert!(
            cfg.stream.is_empty(),
            "a document with no stream key means NO ladders on every slot"
        );
    }

    /// `rotation` is `#[serde(default)]`, so every environment block written
    /// BEFORE the field existed must still deserialize — and land on identity,
    /// not on garbage. Without this, loading an older project.toml / bundled
    /// scene.toml would fail outright or silently pick up a rotation nobody
    /// authored.
    #[test]
    fn rotation_defaults_to_identity_for_pre_feature_documents() {
        let legacy = r#"
            [skybox]
            built_in_default = {}
            [specular]
            built_in_default = {}
            [irradiance]
            built_in_default = {}
        "#;
        let cfg: EnvironmentConfig =
            toml::from_str(legacy).expect("pre-rotation environment still deserializes");
        assert!(
            cfg.rotation.is_identity(),
            "a document with no rotation key means UNROTATED on every slot"
        );
    }
}
