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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Eq, Copy, Default)]
pub enum SkyboxConfig {
    #[default]
    BuiltInDefault,
    Ktx {
        asset_id: AssetId,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Eq, Copy, Default)]
pub enum IblConfig {
    #[default]
    BuiltInDefault,
    Ktx {
        prefiltered_asset_id: AssetId,
        irradiance_asset_id: AssetId,
    },
}
