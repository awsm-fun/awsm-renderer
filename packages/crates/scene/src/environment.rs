use super::assets::AssetId;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub struct EnvironmentConfig {
    #[serde(default)]
    pub skybox: SkyboxConfig,
    #[serde(default)]
    pub ibl: IblConfig,
}

impl EnvironmentConfig {
    /// Every KTX2 cubemap asset id this environment references (skybox +
    /// IBL-prefiltered + IBL-irradiance, when file-based). These are exactly the
    /// ids whose BYTES must accompany the config — the editor's Save/export
    /// write them to [`crate::project_dir::env_ktx_path`] and the player's
    /// `apply_environment` reads them back from the same path. Procedural
    /// variants (built-in default / sky-gradient) reference no assets.
    pub fn ktx_asset_ids(&self) -> Vec<AssetId> {
        let mut ids = Vec::new();
        if let SkyboxConfig::Ktx { asset_id } = self.skybox {
            ids.push(asset_id);
        }
        if let IblConfig::Ktx {
            prefiltered_asset_id,
            irradiance_asset_id,
        } = self.ibl
        {
            ids.push(prefiltered_asset_id);
            ids.push(irradiance_asset_id);
        }
        ids
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Copy, Default)]
pub enum SkyboxConfig {
    #[default]
    BuiltInDefault,
    Ktx {
        asset_id: AssetId,
    },
    /// Agent-authored two-color sky gradient (zenith→nadir), linear RGB. The
    /// generic "environment from agent data" hook (§18): pick a zenith (sky) and
    /// nadir (ground) color to author dusk / overcast / night / studio — no
    /// preset menu, no externally-hosted `.ktx2` required. Same generator the
    /// built-in default uses (`CubemapSkyGradient`).
    SkyGradient {
        zenith: [f32; 3],
        nadir: [f32; 3],
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Copy, Default)]
pub enum IblConfig {
    #[default]
    BuiltInDefault,
    Ktx {
        prefiltered_asset_id: AssetId,
        irradiance_asset_id: AssetId,
    },
    /// Agent-authored two-color sky-gradient IBL (zenith→nadir), linear RGB —
    /// the IBL counterpart of [`SkyboxConfig::SkyGradient`] (§18). Drives both the
    /// prefiltered-env and irradiance from the same gradient the built-in default
    /// uses, so a custom sky also lights the scene consistently.
    SkyGradient { zenith: [f32; 3], nadir: [f32; 3] },
}
