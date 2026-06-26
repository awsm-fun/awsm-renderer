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
pub struct ShaderTemplateClusterCut {
    /// Gap-B dynamic paging: bind the `resident` table + cull absent clusters.
    pub paging: bool,
}

impl TryFrom<&ShaderCacheKeyClusterCut> for ShaderTemplateClusterCut {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyClusterCut) -> Result<Self> {
        Ok(Self {
            paging: value.paging,
        })
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
#[template(
    path = "cluster_lod_wgsl/cluster_compaction.wgsl",
    whitespace = "minimize"
)]
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
        let src = ShaderTemplateClusterCut::default()
            .into_source()
            .expect("render");
        assert!(src.contains("@compute"));
        assert!(src.contains("fn cs_main"));
        assert!(src.contains("ClusterCutParams"));
    }

    /// Gap-B paging variant: the `resident` binding + absent-cluster cull appear
    /// ONLY when `paging` is set, so the default (non-paging) cut is byte-identical.
    #[test]
    fn paging_variant_gates_resident_binding() {
        let off = ShaderTemplateClusterCut { paging: false }
            .into_source()
            .expect("render off");
        let on = ShaderTemplateClusterCut { paging: true }
            .into_source()
            .expect("render on");
        // Non-paging variant must NOT reference the resident table at all.
        assert!(
            !off.contains("resident"),
            "non-paging cut must not bind/read resident (byte-identical to shipped)"
        );
        // Paging variant binds resident at @binding(3) and culls absent clusters.
        assert!(on.contains("@binding(3)"), "paging cut binds resident at 3");
        assert!(on.contains("resident"), "paging cut reads resident");
        assert!(
            on.contains("selected[i] = 0u") || on.contains("selected[i]=0u"),
            "paging cut culls absent (resident<0) clusters"
        );
        // Both still produce a valid compute entry point.
        assert!(on.contains("@compute") && on.contains("fn cs_main"));
    }
}
