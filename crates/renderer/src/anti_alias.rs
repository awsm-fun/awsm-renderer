//! Anti-aliasing configuration.

use crate::{bind_groups::BindGroupCreate, error::Result, AwsmRenderer};

/// Anti-aliasing configuration for the renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AntiAliasing {
    // if None, no MSAA
    // Some(4) is the only supported option for now
    pub msaa_sample_count: Option<u32>,
    pub smaa: bool,
    pub mipmap: bool,
}

impl AntiAliasing {
    /// Returns whether MSAA is enabled and supported.
    pub fn has_msaa_checked(&self) -> crate::error::Result<bool> {
        match self.msaa_sample_count {
            Some(4) => Ok(true),
            None => Ok(false),
            Some(sample_count) => Err(crate::error::AwsmError::UnsupportedMsaaCount(sample_count)),
        }
    }
}

impl Default for AntiAliasing {
    fn default() -> Self {
        Self {
            // Some(4) is the only supported option for now
            msaa_sample_count: Some(4),
            //msaa_sample_count: None,
            smaa: false,
            mipmap: true,
        }
    }
}

impl AwsmRenderer {
    /// Updates the anti-aliasing settings and rebuilds dependent pipelines.
    pub async fn set_anti_aliasing(&mut self, aa: AntiAliasing) -> Result<()> {
        self.anti_aliasing = aa;
        self.bind_groups
            .mark_create(BindGroupCreate::AntiAliasingChange);
        self.bind_groups
            .mark_create(BindGroupCreate::TextureViewRecreate);

        // OPAQUE: No pipeline updates needed here. All MSAA/mipmap variants are pre-created at init,
        // and the correct one is selected at render time via get_compute_pipeline_key(&anti_aliasing).
        //
        // TRANSPARENT: Pipelines depend on per-mesh attributes AND anti-aliasing settings,
        // so we must recreate them when anti-aliasing changes. Build one request per mesh and
        // route through set_render_pipeline_keys_batched — Shaders::ensure_keys +
        // RenderPipelines::ensure_keys dedupe by cache key internally, so the OR-style
        // (buffer_info OR material) dedup we used to do here misses pairs like
        // (A,M1)(B,M2)(A,M2) when M1 and M2 differ in has_transmission and leaves some
        // meshes with stale pipeline-key map entries. See the matching fix in textures.rs.
        let mut requests: Vec<
            crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest,
        > = Vec::new();
        for (mesh_key, mesh) in self.meshes.iter() {
            let buffer_info_key = self.meshes.buffer_info_key(mesh_key)?;
            let has_transmission = self.materials.has_transmission(mesh.material_key);
            requests.push(
                crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest {
                    mesh,
                    mesh_key,
                    buffer_info_key,
                    has_transmission,
                },
            );
        }
        self.render_passes
            .material_transparent
            .pipelines
            .set_render_pipeline_keys_batched(
                &self.gpu,
                requests,
                &mut self.shaders,
                &mut self.pipelines,
                &self.render_passes.material_transparent.bind_groups,
                &self.pipeline_layouts,
                &self.meshes.buffer_infos,
                &self.anti_aliasing,
                &self.textures,
                &self.render_textures.formats,
            )
            .await?;

        self.render_passes
            .effects
            .pipelines
            .set_render_pipeline_keys(
                &self.anti_aliasing,
                &self.post_processing,
                &self.gpu,
                &mut self.shaders,
                &mut self.pipelines,
                &self.pipeline_layouts,
                &self.render_textures.formats,
            )
            .await?;
        Ok(())
    }
}
