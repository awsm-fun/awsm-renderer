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
            .collect()
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
        };
        let toml = toml::to_string_pretty(&cfg).unwrap();
        let back: EnvironmentConfig = toml::from_str(&toml).unwrap();
        assert_eq!(cfg, back, "mixed per-slot env round-trips");
        assert_eq!(
            cfg.ktx_asset_ids().len(),
            1,
            "only the KTX slot carries bytes"
        );
    }
}
