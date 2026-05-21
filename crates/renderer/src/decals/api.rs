//! Public `AwsmRenderer` entry points for managing projection decals
//! (Cluster 6.4, plan §16.4). Mirrors the shape of the Lights API —
//! insert / update / remove return / take a [`DecalKey`].

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
    /// `decals` feature flag is off (plan §16.F) — the per-decal GPU
    /// buffer and shading pass don't exist in that mode, so silently
    /// accepting the decal would be a no-op that later renders as
    /// "decal missing".
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
    /// is off (plan §16.F) — there can be no live keys without an
    /// allocated [`Decals`] subsystem.
    pub fn update_decal(&mut self, key: DecalKey, f: impl FnOnce(&mut Decal)) {
        if let Some(decals) = self.decals.as_mut() {
            decals.update(key, f);
        }
    }

    /// Removes the decal. Returns `true` if it existed. Always
    /// `false` when the decals feature is off (plan §16.F).
    pub fn remove_decal(&mut self, key: DecalKey) -> bool {
        match self.decals.as_mut() {
            Some(decals) => decals.remove(key),
            None => false,
        }
    }
}
