//! Anti-aliasing configuration.

use slotmap::SecondaryMap;

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
        // so we must recreate them when anti-aliasing changes.
        //
        // DISPLAY: Pipelines depend on anti-aliasing settings, so we must recreate them when anti-aliasing changes.
        let mut has_seen_buffer_info = SecondaryMap::new();
        let mut has_seen_material = SecondaryMap::new();
        for (key, mesh) in self.meshes.iter() {
            let buffer_info_key = self.meshes.buffer_info_key(key)?;
            if has_seen_buffer_info.insert(buffer_info_key, ()).is_none()
                || has_seen_material.insert(mesh.material_key, ()).is_none()
            {
                let has_transmission = self.materials.has_transmission(mesh.material_key);
                self.render_passes
                    .material_transparent
                    .pipelines
                    .set_render_pipeline_key(
                        &self.gpu,
                        mesh,
                        key,
                        buffer_info_key,
                        &mut self.shaders,
                        &mut self.pipelines,
                        &self.render_passes.material_transparent.bind_groups,
                        &self.pipeline_layouts,
                        &self.meshes.buffer_infos,
                        &self.anti_aliasing,
                        &self.textures,
                        &self.render_textures.formats,
                        has_transmission,
                    )
                    .await?;
            }
        }

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
