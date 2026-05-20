//! Public `AwsmRenderer` entry points for managing projection decals
//! (Cluster 6.4, plan §16.4). Mirrors the shape of the Lights API —
//! insert / update / remove return / take a [`DecalKey`].

use glam::Mat4;

use crate::{
    decals::{Decal, DecalKey, gpu::AwsmDecalError},
    AwsmRenderer,
};

impl AwsmRenderer {
    /// Inserts a projection decal. The decal is an oriented unit
    /// cube in world space (`transform` × `(-1..1)^3`) projecting its
    /// texture down its local -Z axis. Returns a stable
    /// [`DecalKey`] handle for later mutation / removal.
    pub fn insert_decal(
        &mut self,
        transform: Mat4,
        texture_index: u32,
        alpha: f32,
    ) -> Result<DecalKey, AwsmDecalError> {
        let decal = Decal::new(transform, texture_index, alpha);
        self.decals.insert(decal)
    }

    /// Mutates a decal in place. The closure receives a `&mut Decal`
    /// — if the caller changes `transform`, they should re-derive
    /// `inverse_transform` + `world_aabb` (use [`Decal::new`] as the
    /// canonical constructor instead).
    pub fn update_decal(&mut self, key: DecalKey, f: impl FnOnce(&mut Decal)) {
        self.decals.update(key, f);
    }

    /// Removes the decal. Returns `true` if it existed.
    pub fn remove_decal(&mut self, key: DecalKey) -> bool {
        self.decals.remove(key)
    }
}
