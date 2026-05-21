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

/// Provisional default for the oversized-mesh thresholds. A mesh is
/// "oversized" when both:
///   - its AABB diagonal exceeds `OVERSIZED_AABB_DIAGONAL_METERS`
///   - and it appears in at least one light bucket whose list-count
///     exceeds `OVERSIZED_LIST_COUNT_THRESHOLD`.
///
/// The future cluster-light-list fallback path reads these flags and
/// routes the offending meshes through the fallback list rather than
/// the per-mesh slice.
pub const OVERSIZED_LIST_COUNT_THRESHOLD: usize = 16;
pub const OVERSIZED_AABB_DIAGONAL_METERS: f32 = 50.0;

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
    /// Indices into `Lights::iter()` order for every directional light
    /// active this frame — the global prefix the shader applies to
    /// every mesh.
    directional_light_indices: Vec<u32>,
    /// Count of last-frame's largest bucket. Useful as a Vec capacity
    /// hint when allocating the per-mesh transpose.
    last_max_bucket: usize,
    /// Mesh keys flagged "oversized" this frame — they belong to at
    /// least one bucket that crossed `OVERSIZED_LIST_COUNT_THRESHOLD`
    /// AND have an AABB diagonal > `OVERSIZED_AABB_DIAGONAL_METERS`.
    /// The cluster-light-list fallback routes these
    /// through a coarser path. Empty when no oversized mesh is present
    /// — the cluster path stays unconstructed in the common case.
    oversized_meshes: Vec<MeshKey>,
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
        self.directional_light_indices.clear();
        self.last_max_bucket = 0;
        self.oversized_meshes.clear();
        self.shadow_receiver.clear();
        self.any_directional_shadow_caster = false;

        // Track the global index alongside the iteration. The shader
        // consumes light indices in the same order `Lights::write_gpu`
        // packs the storage buffer (= `Lights::iter()` order), so we
        // mirror that ordering here.
        for (light_index, (light_key, light)) in lights.iter().enumerate() {
            match light {
                Light::Directional { .. } => {
                    self.directional_light_indices.push(light_index as u32);
                }
                Light::Point { .. } | Light::Spot { .. } => {
                    let Some(aabb) = light.world_aabb() else {
                        continue;
                    };
                    let bucket: Vec<MeshKey> = spatial
                        .query_envelope(&aabb)
                        .map(|node| node.mesh_key)
                        .collect();
                    self.last_max_bucket = self.last_max_bucket.max(bucket.len());

                    // Oversized-mesh detection. A bucket
                    // larger than the threshold means *something* in it
                    // is acting like an "every-mesh" target. Any mesh in
                    // such a bucket with a diagonal > 50 m is the likely
                    // offender; flag it for the cluster-light-list
                    // fallback. Cheap — we already have `node.aabb` in
                    // memory from the BVH query above.
                    if bucket.len() >= OVERSIZED_LIST_COUNT_THRESHOLD {
                        for mesh_key in &bucket {
                            if let Some(node) = spatial.get(*mesh_key) {
                                let diag = (node.aabb.max - node.aabb.min).length();
                                if diag > OVERSIZED_AABB_DIAGONAL_METERS
                                    && !self.oversized_meshes.contains(mesh_key)
                                {
                                    self.oversized_meshes.push(*mesh_key);
                                }
                            }
                        }
                    }

                    self.per_light.insert(light_key, bucket);
                    self.light_order.push(light_key);
                }
            }
        }
    }

    /// Mesh keys flagged "oversized" for the current frame. A future
    /// cluster-light-list path would route these through a fallback
    /// shader; until that lands the field is consultative
    /// (instrumentation / debug overlay).
    pub fn oversized_meshes(&self) -> &[MeshKey] {
        &self.oversized_meshes
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

    /// Whether any oversized mesh is present this frame. When false,
    /// the cluster-light-list fallback path stays cold — building it is
    /// pure overhead for typical scenes.
    pub fn has_oversized(&self) -> bool {
        !self.oversized_meshes.is_empty()
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

    /// Directional-light indices (positions in `Lights::iter()` order).
    /// The shader applies these to every mesh unconditionally — they
    /// don't participate in the per-mesh slice.
    pub fn directional_light_indices(&self) -> &[u32] {
        &self.directional_light_indices
    }

    /// Total mesh→light edges across every punctual bucket. Equal to
    /// the upcoming `mesh_light_indices` length after the transpose.
    pub fn total_edges(&self) -> usize {
        self.per_light.values().map(|b| b.len()).sum()
    }

    /// Number of distinct punctual lights with at least one affected
    /// mesh. Used by the debug overlay.
    pub fn punctual_light_count(&self) -> usize {
        self.light_order.len()
    }

    /// Number of meshes the given light overlaps this frame. Returns 0
    /// for unknown / non-punctual / disabled lights.
    pub fn meshes_for_light(&self, key: LightKey) -> usize {
        self.per_light.get(key).map(|b| b.len()).unwrap_or(0)
    }

    /// Largest single-light bucket seen in the most recent rebuild.
    /// The oversized-mesh fallback consults this to decide when to
    /// flip the cluster path on.
    pub fn last_max_bucket(&self) -> usize {
        self.last_max_bucket
    }

    /// Transpose: turn the per-light buckets into a per-mesh list of
    /// affected light indices. The returned map is keyed by `MeshKey`
    /// and each `Vec<u32>` is a list of indices into the packed
    /// `lights` storage buffer (matching `Lights::iter()` order). The
    /// directional prefix is NOT included — those are global.
    ///
    /// Consumed to populate `mesh_light_slices` and
    /// `mesh_light_indices`.
    pub fn transpose_per_mesh(&self, lights: &Lights) -> SecondaryMap<MeshKey, Vec<u32>> {
        let mut per_mesh: SecondaryMap<MeshKey, Vec<u32>> = SecondaryMap::new();
        for (light_key, bucket) in self.iter_punctual() {
            let Some(light_index) = lights.index_of(light_key) else {
                continue;
            };
            for mesh_key in bucket {
                per_mesh
                    .entry(*mesh_key)
                    .unwrap()
                    .or_default()
                    .push(light_index);
            }
        }
        per_mesh
    }
}
