//! Cluster-LOD cut compute shader template (Phase B, B.2).

use askama::Template;

use crate::{
    render_passes::cluster_lod::shader::cache_key::{
        ShaderCacheKeyClusterCompaction, ShaderCacheKeyClusterCut,
    },
    shaders::{AwsmShaderError, Result},
};

/// Renders `cluster_lod_wgsl/cluster_cut.wgsl` — the on-device per-cluster cut
/// (mirror of `cluster_lod::select_cut_per_cluster`). No template variables; the
/// camera/instance params arrive via a uniform buffer.
#[derive(Template, Debug, Default)]
#[template(path = "cluster_lod_wgsl/cluster_cut.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateClusterCut;

impl TryFrom<&ShaderCacheKeyClusterCut> for ShaderTemplateClusterCut {
    type Error = AwsmShaderError;

    fn try_from(_value: &ShaderCacheKeyClusterCut) -> Result<Self> {
        Ok(Self)
    }
}

impl ShaderTemplateClusterCut {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Cluster Cut")
    }
}

/// Renders `cluster_lod_wgsl/cluster_compaction.wgsl` — packs the cut's selected
/// clusters' index pages into one compacted stream + drawIndexedIndirect args.
#[derive(Template, Debug, Default)]
#[template(path = "cluster_lod_wgsl/cluster_compaction.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateClusterCompaction;

impl TryFrom<&ShaderCacheKeyClusterCompaction> for ShaderTemplateClusterCompaction {
    type Error = AwsmShaderError;

    fn try_from(_value: &ShaderCacheKeyClusterCompaction) -> Result<Self> {
        Ok(Self)
    }
}

impl ShaderTemplateClusterCompaction {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Cluster Compaction")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_cut_shader_source() {
        // askama embeds + renders the .wgsl at build time; confirm the registered
        // template produces the compute entry point (and matches the const the
        // layout tests pin).
        let src = ShaderTemplateClusterCut.into_source().expect("render");
        assert!(src.contains("@compute"));
        assert!(src.contains("fn cs_main"));
        assert!(src.contains("ClusterCutParams"));
    }
}
