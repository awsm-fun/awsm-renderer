use super::{
    assets::AssetTable, environment::EnvironmentConfig, shadows::ShadowsConfig, tree::EditorNode,
};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EditorProject {
    /// Human-readable project name. When empty (the default), the
    /// editor falls back to the on-disk directory name for display +
    /// build-artifact filename. Set by the in-header rename input;
    /// preserved across save/load so rename is durable. Existing
    /// project.json files without a `name` round-trip via
    /// `#[serde(default)]`.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub environment: EnvironmentConfig,
    /// Renderer-wide shadow settings. Read at startup by the
    /// editor / player and pushed into the renderer via
    /// `AwsmRenderer::set_shadows_config`; subsequent edits push the
    /// updated config the same way and take effect on the next
    /// rendered frame.
    #[serde(default)]
    pub shadows: ShadowsConfig,
    #[serde(default)]
    pub assets: AssetTable,
    #[serde(default)]
    pub nodes: Vec<EditorNode>,
}
