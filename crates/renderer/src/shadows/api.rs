//! `AwsmRenderer` public setters / getters for shadow state.
//!
//! These wrap the per-`Shadows` API so callers don't have to hold a
//! mutable borrow of two fields at once, and so the lights buffer
//! (which bakes the shadow descriptor index into `LightPacked.row4.z`
//! at pack time) can be marked dirty in the same call that mutates
//! the source-of-truth shadow params.

use crate::{
    lights::LightKey,
    shadows::{
        config::ShadowsConfig,
        error::AwsmShadowError,
        light_shadow::{LightShadowParams, MeshShadowFlags},
    },
    AwsmRenderer,
};

impl AwsmRenderer {
    /// Replaces the renderer-wide shadow config. Player / runtime
    /// equivalent of the editor's "shadows" inspector — load the
    /// `ShadowsConfig` from disk (via `awsm_scene_schema` → `into()`
    /// or a custom build pipeline) and push it in at startup.
    ///
    /// Resource-shaped fields (`atlas_size`, `max_point_shadows`,
    /// `point_shadow_resolution`, `evsm_atlas_size`) are baked into
    /// `Shadows::new` at construction time, so changing them at
    /// runtime requires recreating the renderer. The other tunables
    /// (SSCS toggle, blur radius, exponent, debug overlay) take
    /// effect on the next `render()` call.
    pub fn set_shadows_config(&mut self, config: ShadowsConfig) {
        self.shadows.set_config(config);
    }

    /// Returns the current renderer-wide shadow config.
    pub fn shadows_config(&self) -> &ShadowsConfig {
        self.shadows.config()
    }

    /// Sets a light's shadow parameters. Pass
    /// `LightShadowParams { cast: false, .. }` to disable shadows for a
    /// specific light while keeping the light itself. Takes effect on
    /// the next `render()` call.
    pub fn set_light_shadow_params(
        &mut self,
        key: LightKey,
        params: LightShadowParams,
    ) -> Result<(), AwsmShadowError> {
        self.shadows.params.insert(key, params);
        // The light's `shadow_index` is baked into `LightPacked.row4.z`
        // at pack time via the `shadow_index_for` callback in
        // `Lights::write_gpu`. Changing shadow params can change that
        // index (cast=false → SHADOW_INDEX_NONE, or a freshly assigned
        // descriptor_base when shadows toggle on), so the cached pack
        // must be invalidated even though the light itself didn't move.
        self.lights.mark_punctual_dirty();
        Ok(())
    }

    /// Returns the current shadow parameters for a light, or `None` if
    /// the light has never had shadow params set.
    pub fn light_shadow_params(&self, key: LightKey) -> Option<&LightShadowParams> {
        self.shadows.params.get(key)
    }

    /// Removes a light AND every piece of shadow state keyed on it:
    /// the authored shadow params, the cube-pool slot cache (and the
    /// slot's owner field), and any throttle history. Without this
    /// coordinated removal, `params` would keep a stale entry with
    /// `cast = true` forever — `caster_count` / `any_active` would
    /// stay nonzero, and `write_gpu`'s per-frame caster-AABB sweep
    /// would keep running for a light that no longer exists.
    pub fn remove_light(&mut self, key: LightKey) {
        self.shadows.on_light_removed(key);
        self.lights.remove(key);
    }

    /// Mutates a light's shadow params in place. Convenience over the
    /// get-clone-mutate-set pattern.
    pub fn update_light_shadow<F: FnOnce(&mut LightShadowParams)>(
        &mut self,
        key: LightKey,
        f: F,
    ) -> Result<(), AwsmShadowError> {
        if let Some(params) = self.shadows.params.get_mut(key) {
            f(params);
            // See `set_light_shadow_params` — the baked `shadow_index`
            // in the lights buffer must be reconciled.
            self.lights.mark_punctual_dirty();
            Ok(())
        } else {
            Err(AwsmShadowError::UnknownLight)
        }
    }

    /// Sets a mesh's shadow flags. Takes effect on the next `render()`.
    pub fn set_mesh_shadow_flags(
        &mut self,
        key: crate::meshes::MeshKey,
        flags: MeshShadowFlags,
    ) -> Result<(), AwsmShadowError> {
        let mesh = self
            .meshes
            .get_mut(key)
            .map_err(|_| AwsmShadowError::UnknownMesh)?;
        let receive_changed = mesh.receive_shadows != flags.receive;
        mesh.cast_shadows = flags.cast;
        mesh.receive_shadows = flags.receive;
        // `cast_shadows` is read CPU-side by the shadow render pass at
        // draw time — no GPU state to update. `receive_shadows` is
        // packed into `MaterialMeshMeta.receive_shadows` and read by
        // the lighting shader; patch it in place so the GPU buffer
        // doesn't keep the stale value.
        if receive_changed {
            self.meshes
                .meta
                .set_receive_shadows(key, flags.receive)
                .map_err(|_| AwsmShadowError::UnknownMesh)?;
        }
        Ok(())
    }

    /// Returns the current shadow flags for a mesh.
    pub fn mesh_shadow_flags(&self, key: crate::meshes::MeshKey) -> MeshShadowFlags {
        match self.meshes.get(key) {
            Ok(mesh) => MeshShadowFlags {
                cast: mesh.cast_shadows,
                receive: mesh.receive_shadows,
            },
            Err(_) => MeshShadowFlags::default(),
        }
    }
}
