//! Public `AwsmRenderer` entry points for managing projection decals.
//! Mirrors the shape of the Lights API — insert / update / remove
//! return / take a [`DecalKey`].

use glam::Mat4;

use crate::{
    decals::{gpu::AwsmDecalError, Decal, DecalKey},
    AwsmRenderer,
};

impl AwsmRenderer {
    /// Inserts a projection decal. The decal is an oriented unit
    /// cube in world space (`transform` × `(-1..1)^3`) projecting its
    /// texture down its local -Z axis. Returns a stable
    /// [`DecalKey`] handle for later mutation / removal.
    ///
    /// Returns [`AwsmDecalError::FeatureNotEnabled`] when the
    /// `decals` feature flag is off — the per-decal GPU buffer and
    /// shading pass don't exist in that mode, so silently accepting
    /// the decal would be a no-op that later renders as "decal
    /// missing".
    pub fn insert_decal(
        &mut self,
        transform: Mat4,
        texture_index: u32,
        alpha: f32,
    ) -> Result<DecalKey, AwsmDecalError> {
        let decal = Decal::new(transform, texture_index, alpha);
        match self.decals.as_mut() {
            Some(decals) => decals.insert(decal),
            None => Err(AwsmDecalError::FeatureNotEnabled),
        }
    }

    /// Mutates a decal in place. The closure receives a `&mut Decal`
    /// — if the caller changes `transform`, they should re-derive
    /// `inverse_transform` + `world_aabb` (use [`Decal::new`] as the
    /// canonical constructor instead). No-op when the decals feature
    /// is off — there can be no live keys without an allocated
    /// [`crate::decals::Decals`] subsystem.
    pub fn update_decal(&mut self, key: DecalKey, f: impl FnOnce(&mut Decal)) {
        if let Some(decals) = self.decals.as_mut() {
            decals.update(key, f);
        }
    }

    /// Removes the decal. Returns `true` if it existed. Always
    /// `false` when the decals feature is off.
    pub fn remove_decal(&mut self, key: DecalKey) -> bool {
        match self.decals.as_mut() {
            Some(decals) => decals.remove(key),
            None => false,
        }
    }

    /// Render-loop auto-drive (step 1) for the content-lazy decal pipelines —
    /// the decal analog of `LineRenderer::kick_compile`. A LIVE
    /// [`Self::insert_decal`] (e.g. the editor's node-kind observer) doesn't
    /// run a `commit_load`, so the commit-path compile
    /// (`ensure_decal_pipelines_compiled`) never fires for it; this per-frame
    /// kick covers that path. Early-outs to a couple of boolean checks when
    /// there are no decals, everything is compiled, or a compile is already
    /// in flight — projects that never place a decal pay effectively nothing.
    pub(crate) fn kick_decal_pipelines_compile(&mut self) -> crate::error::Result<()> {
        let needs = self.decals.as_ref().is_some_and(|d| !d.is_empty())
            && self.render_passes.material_decal.as_ref().is_some_and(|d| {
                d.pipelines.is_none()
                    || d.classify_pass.pipelines.is_none()
                    || d.composite.is_none()
            });
        if !needs {
            return Ok(());
        }
        let mut ctx = crate::render_passes::RenderPassInitContext {
            gpu: &self.gpu,
            bind_group_layouts: &mut self.bind_group_layouts,
            pipeline_layouts: &mut self.pipeline_layouts,
            pipelines: &mut self.pipelines,
            shaders: &mut self.shaders,
            render_texture_formats: &mut self.render_textures.formats,
            textures: &mut self.textures,
            features: &self.features,
            anti_aliasing: &self.anti_aliasing,
            post_processing: &self.post_processing,
            prep_config: &self.prep_config,
            max_edge_budget: self
                .material_edge_buffers
                .as_ref()
                .map(|b| b.max_edge_budget)
                .unwrap_or(
                    crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
                ),
        };
        // Disjoint field borrows: `ctx` holds cache fields; `render_passes`
        // is not part of it (same shape as `ensure_material_prep_pipelines`).
        if let Some(decal) = self.render_passes.material_decal.as_mut() {
            decal.kick_compile(&mut ctx)?;
        }
        Ok(())
    }

    /// Render-loop auto-drive (step 2): install any resolved decal-pipeline
    /// compiles. No-op when nothing is in flight. When the composite lands,
    /// mark `TextureViewRecreate` so its bind group is created by THIS
    /// frame's recreate drain (which runs after this preamble and before the
    /// decal dispatch) — a fresh composite starts unbound and skip-renders
    /// until bound.
    pub(crate) fn poll_decal_pipelines_compile(&mut self) -> crate::error::Result<()> {
        if let Some(decal) = self.render_passes.material_decal.as_mut() {
            let composite_installed = decal.poll_compile(&mut self.pipelines.compute)?;
            if composite_installed {
                self.bind_groups
                    .mark_create(crate::bind_groups::BindGroupCreate::TextureViewRecreate);
            }
        }
        Ok(())
    }
}
