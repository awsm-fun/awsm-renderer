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
    /// Per-light buckets, parallel to `light_order` (`buckets[i]` belongs
    /// to `light_order[i]`). Stored as a plain Vec-of-Vecs (not a keyed
    /// map) so `rebuild` reuses each bucket's capacity frame-to-frame —
    /// clearing a keyed map dropped and re-allocated every bucket every
    /// frame ([[avoid-per-frame-allocations]]). Entries past
    /// `light_order.len()` are stale spares kept for their capacity.
    buckets: Vec<Vec<MeshKey>>,
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
    /// Reused scratch for `mark_shadow_receivers`' two-pass walk
    /// (cleared, capacity kept).
    to_mark_scratch: Vec<MeshKey>,
}

impl LightMeshBuckets {
    /// Rebuilds the per-light buckets from scratch by walking every
    /// active punctual light and asking the spatial index for the
    /// meshes it overlaps. O(n_lights × visible_per_light), no GPU
    /// work. Called from the per-frame `write_gpu` path.
    pub fn rebuild(&mut self, lights: &Lights, spatial: &SceneSpatial) {
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
                    // Reuse the pooled bucket at this slot (clear keeps
                    // capacity); grow the pool only when the light count
                    // exceeds any previous frame's.
                    let slot = self.light_order.len();
                    if self.buckets.len() == slot {
                        self.buckets.push(Vec::new());
                    }
                    let bucket = &mut self.buckets[slot];
                    bucket.clear();
                    bucket.extend(spatial.query_envelope(&aabb).map(|node| node.mesh_key));
                    self.last_max_bucket = self.last_max_bucket.max(bucket.len());

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
        // that we need to mark, then write to `shadow_receiver`. The
        // scratch is reused across frames (cleared, capacity kept).
        let mut to_mark = std::mem::take(&mut self.to_mark_scratch);
        to_mark.clear();
        for (light_key, bucket) in self.iter_punctual() {
            if !casts_shadow_for(light_key) {
                continue;
            }
            to_mark.extend(bucket.iter().copied());
        }
        for mesh_key in to_mark.iter().copied() {
            self.shadow_receiver.entry(mesh_key).unwrap().or_insert(());
        }
        self.to_mark_scratch = to_mark;
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
        self.light_order.iter().copied().zip(self.buckets.iter())
    }

    /// Largest single-light bucket seen in the most recent rebuild.
    /// Surfaced to the measurement/debug overlay.
    pub fn last_max_bucket(&self) -> usize {
        self.last_max_bucket
    }
}
