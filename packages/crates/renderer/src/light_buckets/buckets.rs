//! Per-light → per-mesh AABB-overlap buckets, used by the
//! visibility-buffer-native lighting path.
//!
//! Instead of per-tile / per-cluster light lists (the classical
//! forward+ / clustered-deferred shape), we exploit the visibility
//! buffer's per-pixel mesh identity and build **per-mesh** light
//! lists. For each active punctual light, one
//! `SceneSpatial::query_envelope` produces the meshes it can possibly
//! affect; the transpose then gives every mesh its own short list of
//! overlapping lights.

use slotmap::SecondaryMap;

use crate::lights::{Light, LightKey, Lights};
use crate::meshes::MeshKey;
use crate::scene_spatial::SceneSpatial;

/// Per-light bucket of mesh keys whose world AABB overlaps the light's
/// influence volume. Directional lights are NOT entered here — they
/// affect every mesh and travel through a separate global-prefix path
/// in the shader.
#[derive(Default)]
pub struct LightMeshBuckets {
    per_light: SecondaryMap<LightKey, Vec<MeshKey>>,
    /// Stable iteration order over the punctual lights we processed
    /// this frame. Matches `Lights::iter_active_punctual` ordering.
    light_order: Vec<LightKey>,
    /// Count of last-frame's largest bucket. Surfaced via
    /// `last_max_bucket()` for the measurement/debug overlay.
    last_max_bucket: usize,
    /// Per-mesh shadow-receiver flag: true when at least
    /// one shadow-casting punctual light overlaps the mesh's AABB this
    /// frame. Used by the shading path to AND with the per-mesh
    /// `receive_shadows` gate and skip the shadow-sample branch entirely
    /// for meshes no shadow-caster reaches. Defaults to false; populated
    /// by `mark_shadow_receivers`.
    shadow_receiver: SecondaryMap<MeshKey, ()>,
    /// Whether any directional light is currently casting shadows. If
    /// so, every mesh is conservatively flagged shadow-receiver. Set by
    /// `mark_shadow_receivers`.
    any_directional_shadow_caster: bool,
}

impl LightMeshBuckets {
    /// Rebuilds the per-light buckets from scratch by walking every
    /// active punctual light and asking the spatial index for the
    /// meshes it overlaps. O(n_lights × visible_per_light), no GPU
    /// work. Called from the per-frame `write_gpu` path.
    pub fn rebuild(&mut self, lights: &Lights, spatial: &SceneSpatial) {
        self.per_light.clear();
        self.light_order.clear();
        self.last_max_bucket = 0;
        self.shadow_receiver.clear();
        self.any_directional_shadow_caster = false;

        // Directional lights affect every mesh and travel through a
        // separate global-prefix path in the shader (see
        // `LightsInfoPacked`), so they're not entered into the per-light
        // buckets here.
        for (light_key, light) in lights.iter() {
            match light {
                Light::Directional { .. } => {}
                Light::Point { .. } | Light::Spot { .. } => {
                    let Some(aabb) = light.world_aabb() else {
                        continue;
                    };
                    let bucket: Vec<MeshKey> = spatial
                        .query_envelope(&aabb)
                        .map(|node| node.mesh_key)
                        .collect();
                    self.last_max_bucket = self.last_max_bucket.max(bucket.len());

                    self.per_light.insert(light_key, bucket);
                    self.light_order.push(light_key);
                }
            }
        }
    }

    /// Walk the bucket set and stamp the per-mesh
    /// "shadow-receiver" flag. `casts_shadow_for` is a callback that
    /// answers "does this light have a shadow descriptor this frame?"
    /// — typically `|key| shadows.params.get(key).map(|p| p.cast).unwrap_or(false)`.
    /// Call after `rebuild`.
    pub fn mark_shadow_receivers(
        &mut self,
        lights: &Lights,
        casts_shadow_for: impl Fn(LightKey) -> bool,
    ) {
        // Any directional shadow-caster lights up every mesh — the
        // global prefix never participates in the per-mesh bucket, so
        // we OR in this signal separately.
        self.any_directional_shadow_caster = lights
            .iter_directional()
            .any(|(key, _)| casts_shadow_for(key));

        if self.any_directional_shadow_caster {
            // Whole-scene receiver case: we don't bother populating the
            // map — `is_shadow_receiver` short-circuits on the flag.
            return;
        }

        // Two-pass to avoid a self-borrow conflict: snapshot the keys
        // that we need to mark, then write to `shadow_receiver`.
        let mut to_mark: Vec<MeshKey> = Vec::new();
        for (light_key, bucket) in self.iter_punctual() {
            if !casts_shadow_for(light_key) {
                continue;
            }
            to_mark.extend(bucket.iter().copied());
        }
        for mesh_key in to_mark {
            self.shadow_receiver.entry(mesh_key).unwrap().or_insert(());
        }
    }

    /// Whether this mesh sees at least one shadow-casting light this
    /// frame. False when no light reaches it; the shading path can
    /// skip its shadow-sample branch entirely.
    pub fn is_shadow_receiver(&self, mesh_key: MeshKey) -> bool {
        self.any_directional_shadow_caster || self.shadow_receiver.contains_key(mesh_key)
    }

    /// Iterate per-punctual-light buckets in stable order. The
    /// `Vec<MeshKey>` is the set of meshes whose AABB overlaps the
    /// light's influence volume this frame.
    pub fn iter_punctual(&self) -> impl Iterator<Item = (LightKey, &Vec<MeshKey>)> {
        self.light_order
            .iter()
            .copied()
            .filter_map(move |key| self.per_light.get(key).map(|bucket| (key, bucket)))
    }

    /// Largest single-light bucket seen in the most recent rebuild.
    /// Surfaced to the measurement/debug overlay.
    pub fn last_max_bucket(&self) -> usize {
        self.last_max_bucket
    }
}
