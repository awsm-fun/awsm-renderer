//! Editor-side particle-emitter preview.
//!
//! Mirrors the player's `scene::particles` paths (both opaque and
//! transparent-blend) behind a per-node "Playing" toggle. When the
//! inspector flips the toggle on, `materialize_or_replace` builds the
//! same instanced-billboard mesh + `awsm_particles::Simulator` runtime
//! as the player would; the editor's render loop ticks every registered
//! runtime each frame via [`tick_all`]. Toggling off tears the runtime
//! down.
//!
//! `def.blend` selects the path: `false` → opaque (sync `add_raw_mesh`,
//! emissive-only material), `true` → transparent (async
//! `add_raw_mesh_transparent`, alpha-blend material). Mirrors the
//! player's `build_particle_emitter` / `build_particle_emitter_blend_async`
//! split.

use std::{collections::HashMap, sync::Mutex};

use crate::context::{renderer_handle, with_renderer_mut};
use crate::scene::NodeId;
use crate::state::app_state;
use awsm_meshgen::sprite_quad;
use awsm_particles::{Emitter, EmitterSpace, Force, Simulator, SpawnShape};
use awsm_renderer::{
    instances::InstanceAttr,
    materials::{pbr::PbrMaterial, Material, MaterialAlphaMode},
    meshes::{mesh::BillboardMode, MeshKey},
    raw_mesh::RawMeshData,
    transforms::{Transform, TransformKey},
    AwsmRenderer,
};
use awsm_scene_schema::{
    ColorOverLifeDef, EmitterSpaceDef, ForceDef, ParticleEmitterDef, SizeOverLifeDef, SpawnShapeDef,
};
use futures_signals::signal::{Mutable, SignalExt};
use glam::{Quat, Vec3};

const PARTICLE_QUAD_SIZE: f32 = 1.0;

struct EmitterRuntime {
    emitter: Emitter,
    simulator: Simulator,
    mesh_key: MeshKey,
    material_key: awsm_renderer::materials::MaterialKey,
    /// Per-instance transform key for the billboard mesh. In `Local`
    /// space this is parented to `emitter_transform_key`; in `World`
    /// space it's parented to the scene root.
    transform_key: TransformKey,
    /// The emitter node's own transform — held so `tick_all` can
    /// re-read the live emitter world position every frame and feed
    /// it to the simulator. Without this, `World` mode would spawn
    /// new particles at the build-time snapshot location even after
    /// the user moves the emitter.
    emitter_transform_key: TransformKey,
    transforms_buf: Vec<Transform>,
    attrs_buf: Vec<InstanceAttr>,
    /// Snapshot of the emitter's world position at build time.
    /// Used as the spawn-origin reference in `Local` space (so
    /// new particles spawn at the parent's origin and the parent
    /// transform sweeps the whole cloud as the emitter moves).
    /// `World` mode ignores this and uses the live position instead.
    base_world_pos: Vec3,
    last_ts_ms: f64,
}

static RUNTIMES: Mutex<Option<HashMap<NodeId, EmitterRuntime>>> = Mutex::new(None);

fn with_runtimes<R>(f: impl FnOnce(&mut HashMap<NodeId, EmitterRuntime>) -> R) -> R {
    let mut guard = RUNTIMES.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Look up (or lazily create) the `Mutable<bool>` "playing" flag for the
/// given emitter node. The inspector's "Play" button writes to it; the
/// observer started by [`start_observer`] materializes / tears down the
/// runtime in response.
pub fn playing_state(node_id: NodeId) -> Mutable<bool> {
    let state = app_state();
    let mut map = state.playing_emitters.lock().unwrap();
    map.entry(node_id)
        .or_insert_with(|| Mutable::new(false))
        .clone()
}

/// Spawn a signal observer for the given emitter node's `playing` flag.
/// Each time the flag toggles, the runtime is created / destroyed. Called
/// once when the bridge entry first sees a `ParticleEmitter` kind.
///
/// The spawn handle is parked on the caller's `RendererNode::tasks`
/// (`entry`) so the standard per-node teardown drops it on kind change
/// or node removal — without that, every emitter ever inserted would
/// leave a dormant `for_each` task holding the per-node Mutable.
pub fn start_observer(
    entry: &std::sync::Arc<crate::renderer_bridge::node_sync::RendererNode>,
    node_id: NodeId,
    transform_key: TransformKey,
) {
    let playing = playing_state(node_id);
    let loader = awsm_web_shared::prelude::AsyncLoader::new();
    loader.load(async move {
        playing
            .signal()
            .for_each(move |is_playing| async move {
                if is_playing {
                    materialize_or_replace(node_id, transform_key).await;
                } else {
                    tear_down(node_id).await;
                }
            })
            .await;
    });
    entry.tasks.lock().unwrap().push(loader);
}

async fn materialize_or_replace(node_id: NodeId, transform_key: TransformKey) {
    // Tear down any prior runtime so we always start from a clean slate
    // (e.g. user edited an emitter param while playing — the next play
    // press should reflect the new value).
    tear_down(node_id).await;

    // Snapshot the emitter's current authored def.
    let Some(def) = read_emitter_def(node_id) else {
        return;
    };

    if def.blend {
        // Transparent path needs an await on `add_raw_mesh_transparent`,
        // so we hold the lock manually across it instead of using the
        // sync `with_renderer_mut` closure.
        let handle = renderer_handle();
        let mut renderer = handle.lock().await;
        let base_world = renderer
            .transforms
            .get_world(transform_key)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        let base_world_pos = base_world.w_axis.truncate();
        if let Some(runtime) =
            build_runtime_blend(&mut renderer, transform_key, base_world_pos, &def).await
        {
            with_runtimes(|m| {
                m.insert(node_id, runtime);
            });
        }
        return;
    }

    // Opaque path is sync per-step, but we still need `finalize_gpu_textures`
    // (async) afterwards so the pipeline + bind groups see any freshly-uploaded
    // raster texture the material binds — same reason as the blend path.
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    let base_world = renderer
        .transforms
        .get_world(transform_key)
        .copied()
        .unwrap_or(glam::Mat4::IDENTITY);
    let base_world_pos = base_world.w_axis.truncate();
    if let Some(runtime) = build_runtime(&mut renderer, transform_key, base_world_pos, &def) {
        with_runtimes(|m| {
            m.insert(node_id, runtime);
        });
    }
    if let Err(err) = renderer.finalize_gpu_textures().await {
        tracing::warn!("particles_sync (opaque): finalize_gpu_textures failed: {err}");
    }
}

async fn tear_down(node_id: NodeId) {
    let removed = with_runtimes(|m| m.remove(&node_id));
    let Some(runtime) = removed else { return };
    let mesh_key = runtime.mesh_key;
    let material_key = runtime.material_key;
    let tk = runtime.transform_key;
    with_renderer_mut(move |r| {
        r.remove_mesh(mesh_key);
        // The material is owned by this runtime only — free it after the
        // mesh so no live draw still points at the slot.
        r.remove_material(material_key);
        r.transforms.remove(tk);
    })
    .await;
}

fn read_emitter_def(node_id: NodeId) -> Option<ParticleEmitterDef> {
    let state = app_state();
    let scene_nodes = state.scene.nodes.lock_ref();
    let node = find_node_recursive(&scene_nodes, node_id)?;
    match node.kind.get_cloned() {
        crate::scene::NodeKind::ParticleEmitter(def) => Some(def),
        _ => None,
    }
}

fn find_node_recursive(
    nodes: &[std::sync::Arc<crate::scene::Node>],
    target: NodeId,
) -> Option<std::sync::Arc<crate::scene::Node>> {
    for n in nodes.iter() {
        if n.id == target {
            return Some(n.clone());
        }
        let children = n.children.lock_ref();
        if let Some(found) = find_node_recursive(&children, target) {
            return Some(found);
        }
    }
    None
}

fn build_runtime(
    renderer: &mut AwsmRenderer,
    parent_transform: TransformKey,
    parent_world_pos: Vec3,
    def: &ParticleEmitterDef,
) -> Option<EmitterRuntime> {
    let emitter = def_to_emitter(def);
    let max = emitter.max_alive.max(1) as usize;
    // For `World` mode the simulator emits in absolute world space and
    // we expect particles to *stay* where they were spawned even when
    // the emitter moves — so parent the instanced mesh's transform to
    // the scene root instead of the emitter. Parenting to the emitter
    // would multiply the world-space instance positions by the
    // emitter's current world transform every frame, dragging the
    // whole cloud along.
    let instance_parent = match emitter.space {
        EmitterSpace::Local => parent_transform,
        EmitterSpace::World => renderer.transforms.root_node,
    };

    let base_color = match &def.color_over_life {
        ColorOverLifeDef::Const(c) => *c,
        ColorOverLifeDef::Linear { start, .. } => *start,
    };
    // Resolve `def.texture` (if any) so the PBR material samples the
    // user's smoke / fire / spark sprite instead of a flat white quad.
    // base_color slot is the right choice: the texture's RGB tints the
    // emissive (computed below) and its alpha modulates the
    // per-instance alpha at fragment time — exactly what a soft-falloff
    // sprite needs. Same plumbing in the blend path below.
    let base_color_tex = resolve_particle_texture(renderer, def.texture);
    let mut pbr_opaque = PbrMaterial::new(MaterialAlphaMode::Opaque, true);
    pbr_opaque.base_color_factor = [1.0, 1.0, 1.0, 1.0];
    pbr_opaque.base_color_tex = base_color_tex;
    pbr_opaque.metallic_factor = 0.0;
    pbr_opaque.roughness_factor = 1.0;
    pbr_opaque.emissive_factor = [
        base_color[0] * 1.6,
        base_color[1] * 1.6,
        base_color[2] * 1.6,
    ];
    let material_key = renderer
        .materials
        .insert(Material::Pbr(Box::new(pbr_opaque)), &renderer.textures);

    let m = sprite_quad(PARTICLE_QUAD_SIZE, PARTICLE_QUAD_SIZE);
    let raw = RawMeshData {
        positions: m.positions,
        normals: m.normals,
        uvs: m.uvs,
        colors: m.colors,
        indices: m.indices,
    };
    let transform_key = renderer
        .transforms
        .insert(Transform::IDENTITY, Some(instance_parent));
    let mesh_key = match renderer.add_raw_mesh(raw, transform_key, material_key) {
        Ok(k) => k,
        Err(err) => {
            tracing::warn!("particles_sync::build_runtime: add_raw_mesh failed: {err}");
            // Mesh insert failed — free the just-inserted material +
            // transform so we don't leak on the error path.
            renderer.remove_material(material_key);
            renderer.transforms.remove(transform_key);
            return None;
        }
    };

    if let Err(err) = renderer.set_mesh_billboard_mode(mesh_key, BillboardMode::Full) {
        tracing::warn!("particles_sync: set_mesh_billboard_mode failed: {err}");
    }

    let dead_transform = Transform {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ZERO,
    };
    let dead_attr = InstanceAttr::from_rgba_alpha_size([1.0, 1.0, 1.0, 0.0], 0.0, 1.0);
    let initial_transforms = vec![dead_transform.clone(); max];
    let initial_attrs = vec![dead_attr; max];

    if let Err(err) = renderer.enable_mesh_instancing_opaque(mesh_key, &initial_transforms) {
        tracing::warn!("particles_sync: enable_mesh_instancing_opaque failed: {err}");
        renderer.remove_mesh(mesh_key);
        renderer.remove_material(material_key);
        renderer.transforms.remove(transform_key);
        return None;
    }
    if let Err(err) = renderer.set_mesh_instance_attrs(transform_key, &initial_attrs) {
        tracing::warn!("particles_sync: set_mesh_instance_attrs failed: {err}");
    }

    Some(EmitterRuntime {
        emitter,
        simulator: Simulator::new(0xED1700 ^ (renderer.materials.iter().count() as u32)),
        mesh_key,
        material_key,
        transform_key,
        emitter_transform_key: parent_transform,
        transforms_buf: vec![dead_transform; max],
        attrs_buf: vec![dead_attr; max],
        base_world_pos: parent_world_pos,
        last_ts_ms: 0.0,
    })
}

/// Transparent-blend mirror of [`build_runtime`]. Spawns the alpha-blend
/// material via `add_raw_mesh_transparent` (async — pipeline-key
/// registration runs on the renderer's task queue). Otherwise identical
/// in shape to the opaque path: same simulator, same per-instance
/// buffers, same per-frame `tick_all` loop.
async fn build_runtime_blend(
    renderer: &mut AwsmRenderer,
    parent_transform: TransformKey,
    parent_world_pos: Vec3,
    def: &ParticleEmitterDef,
) -> Option<EmitterRuntime> {
    let emitter = def_to_emitter(def);
    let max = emitter.max_alive.max(1) as usize;
    // See `build_runtime` for the rationale — `World` mode emits in
    // absolute world space and the instanced mesh's parent must be
    // the scene root so the emitter's transform doesn't drag already-
    // spawned particles along when the user moves it.
    let instance_parent = match emitter.space {
        EmitterSpace::Local => parent_transform,
        EmitterSpace::World => renderer.transforms.root_node,
    };

    // Alpha-blend material so per-instance alpha (Stage-3b) fades on
    // screen instead of writing as alpha-0 into the opaque texture.
    //
    // Unlike the opaque path, we deliberately leave `emissive_factor`
    // at zero here. The opaque path needs an emissive glow because
    // its pipeline can't actually alpha-blend (it writes to the
    // visibility buffer), so the only way to make a particle visible
    // at all is to push the lit base into the emissive add. With real
    // alpha blending available on this path, that workaround backfires:
    // emissive is added *after* the texture sample in the BRDF, so a
    // white `emissive_factor` saturates the output and the texture's
    // RGB never makes it to screen — particles render as solid colored
    // squares regardless of which sprite is bound. Texture drives
    // base color; per-instance tint (carrying `color_over_life`)
    // modulates it at fragment time.
    let base_color_tex = resolve_particle_texture(renderer, def.texture);
    let mut pbr_blend = PbrMaterial::new(MaterialAlphaMode::Blend, true);
    pbr_blend.base_color_factor = [1.0, 1.0, 1.0, 1.0];
    pbr_blend.base_color_tex = base_color_tex;
    pbr_blend.metallic_factor = 0.0;
    pbr_blend.roughness_factor = 1.0;
    pbr_blend.emissive_factor = [0.0, 0.0, 0.0];
    let material_key = renderer
        .materials
        .insert(Material::Pbr(Box::new(pbr_blend)), &renderer.textures);

    let m = sprite_quad(PARTICLE_QUAD_SIZE, PARTICLE_QUAD_SIZE);
    let raw = RawMeshData {
        positions: m.positions,
        normals: m.normals,
        uvs: m.uvs,
        colors: m.colors,
        indices: m.indices,
    };
    let transform_key = renderer
        .transforms
        .insert(Transform::IDENTITY, Some(instance_parent));
    let mesh_key = match renderer
        .add_raw_mesh_transparent(raw, transform_key, material_key)
        .await
    {
        Ok(k) => k,
        Err(err) => {
            tracing::warn!(
                "particles_sync::build_runtime_blend: add_raw_mesh_transparent failed: {err}"
            );
            renderer.remove_material(material_key);
            renderer.transforms.remove(transform_key);
            return None;
        }
    };

    if let Err(err) = renderer.set_mesh_billboard_mode(mesh_key, BillboardMode::Full) {
        tracing::warn!("particles_sync (blend): set_mesh_billboard_mode failed: {err}");
    }

    let dead_transform = Transform {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ZERO,
    };
    let dead_attr = InstanceAttr::from_rgba_alpha_size([1.0, 1.0, 1.0, 0.0], 0.0, 1.0);
    let initial_transforms = vec![dead_transform.clone(); max];
    let initial_attrs = vec![dead_attr; max];

    // Transparent path must use the async `enable_mesh_instancing` so
    // the transparent pipeline gets re-keyed with the instanced shader
    // variant. The opaque/sync variant flips `mesh.instanced` without
    // rebuilding the transparent pipeline; since the transparent shader
    // cache key includes `instancing_transforms` (see
    // `material_transparent::pipeline::set_render_pipeline_key`), that
    // leaves the mesh on the non-instanced shader and only the base
    // quad ever renders — no per-instance transforms, no per-instance
    // attrs. Visible as a single fading sprite instead of a smoke cloud.
    if let Err(err) = renderer
        .enable_mesh_instancing(mesh_key, &initial_transforms)
        .await
    {
        tracing::warn!("particles_sync (blend): enable_mesh_instancing failed: {err}");
        renderer.remove_mesh(mesh_key);
        renderer.remove_material(material_key);
        renderer.transforms.remove(transform_key);
        return None;
    }
    if let Err(err) = renderer.set_mesh_instance_attrs(transform_key, &initial_attrs) {
        tracing::warn!("particles_sync (blend): set_mesh_instance_attrs failed: {err}");
    }

    // If `resolve_particle_texture` just uploaded a fresh raster
    // texture into the pool (the typical first-play case for a
    // smoke / fire / spark sprite), the bind groups + transparent
    // pipelines need to be rebuilt to see the new pool entry. Without
    // this the shader samples an empty array slot and the particles
    // render fully invisible. Mirrors what `instance_template` does
    // at the end of the gltf instance-flow.
    if let Err(err) = renderer.finalize_gpu_textures().await {
        tracing::warn!("particles_sync (blend): finalize_gpu_textures failed: {err}");
    }

    Some(EmitterRuntime {
        emitter,
        simulator: Simulator::new(0xED1700 ^ (renderer.materials.iter().count() as u32)),
        mesh_key,
        material_key,
        transform_key,
        emitter_transform_key: parent_transform,
        transforms_buf: vec![dead_transform; max],
        attrs_buf: vec![dead_attr; max],
        base_world_pos: parent_world_pos,
        last_ts_ms: 0.0,
    })
}

/// Per-frame: tick every "playing" emitter runtime. Mirrors the player's
/// `scene::particles::tick_all` minus the multi-emitter map ordering quirks.
/// Caller is the editor's render loop, with the renderer locked.
pub fn tick_all(now_ms: f64, renderer: &mut AwsmRenderer) {
    with_runtimes(|map| {
        for (_, runtime) in map.iter_mut() {
            let dt = if runtime.last_ts_ms > 0.0 {
                ((now_ms - runtime.last_ts_ms) / 1000.0).clamp(0.0, 0.1) as f32
            } else {
                0.016
            };
            runtime.last_ts_ms = now_ms;

            // In `World` space new spawns need to come out of the
            // emitter's CURRENT world position so a moving emitter
            // leaves a trail; in `Local` space we keep using the
            // build-time snapshot because the parent transform
            // already moves the entire cloud and re-spawning at
            // the live position would double-displace.
            let spawn_origin = match runtime.emitter.space {
                EmitterSpace::Local => runtime.base_world_pos,
                EmitterSpace::World => renderer
                    .transforms
                    .get_world(runtime.emitter_transform_key)
                    .map(|m| m.w_axis.truncate())
                    .unwrap_or(runtime.base_world_pos),
            };
            runtime.simulator.tick(dt, &runtime.emitter, spawn_origin);

            let live = &runtime.simulator.packed;
            let max = runtime.transforms_buf.len();
            for i in 0..max {
                if i < live.len() {
                    let p = &live[i];
                    let translation = match runtime.emitter.space {
                        EmitterSpace::World => Vec3::from_array(p.position),
                        EmitterSpace::Local => {
                            Vec3::from_array(p.position) - runtime.base_world_pos
                        }
                    };
                    runtime.transforms_buf[i] = Transform {
                        translation,
                        rotation: Quat::IDENTITY,
                        scale: Vec3::splat(p.size.max(1.0e-3)),
                    };
                    runtime.attrs_buf[i] = InstanceAttr::from_rgba_alpha_size(p.color, 1.0, 1.0);
                } else {
                    runtime.transforms_buf[i].scale = Vec3::ZERO;
                    runtime.attrs_buf[i] =
                        InstanceAttr::from_rgba_alpha_size([1.0, 1.0, 1.0, 0.0], 0.0, 1.0);
                }
            }

            if let Err(err) = renderer.set_mesh_instances(runtime.mesh_key, &runtime.transforms_buf)
            {
                tracing::warn!("particles_sync::tick_all: set_mesh_instances failed: {err}");
            }
            if let Err(err) =
                renderer.set_mesh_instance_attrs(runtime.transform_key, &runtime.attrs_buf)
            {
                tracing::warn!("particles_sync::tick_all: set_mesh_instance_attrs failed: {err}");
            }
        }
    });
}

/// L5 fast path: replace the runtime's `Emitter` snapshot in place
/// with one freshly derived from `new_def`. Per-particle state stays
/// intact, so the user can drag inspector sliders and watch the
/// simulator react smoothly. Returns `true` if a live runtime existed
/// for `node_id` and was updated; `false` if there was nothing to
/// update (emitter not currently playing — `apply_kind`'s caller
/// falls through to the standard rebuild path so the next Play
/// snapshots the new def via `read_emitter_def`).
///
/// Structural fields (`blend`, `max_alive`, `texture`) MUST already
/// match between old and new defs — the caller (`node_sync`) gates
/// on that. Hot-swapping with a mismatched `max_alive`, for example,
/// would leave the per-frame instance buffers sized for the old
/// value.
pub fn hot_swap_emitter(node_id: NodeId, new_def: &ParticleEmitterDef) -> bool {
    with_runtimes(|map| {
        if let Some(rt) = map.get_mut(&node_id) {
            rt.emitter = def_to_emitter(new_def);
            true
        } else {
            false
        }
    })
}

/// L5+ fast path for structural edits (blend / max_alive / texture
/// flips) while the emitter is playing. Lifts the live `Simulator`
/// state out of the existing runtime, tears down only the
/// renderer-side handles (mesh + material), rebuilds them on the new
/// def's path, then grafts the preserved simulator state back on so
/// the per-particle positions / velocities / ages / RNG survive the
/// rebuild. The user sees the new render-pass (or new texture, or
/// resized instance buffer) take effect without restarting the
/// particle stream.
///
/// Returns `true` if a live runtime was preserved; `false` if there
/// was no live runtime to preserve (the caller falls through to the
/// standard rebuild path so the next Play snapshots the new def
/// via `read_emitter_def`).
pub async fn try_rebuild_preserving_simulator(
    node_id: NodeId,
    transform_key: TransformKey,
    new_def: &ParticleEmitterDef,
) -> bool {
    // Lift the existing runtime out of the global map. If nothing's
    // there, this fast path doesn't apply — caller takes the regular
    // rebuild route.
    let salvaged = match with_runtimes(|m| m.remove(&node_id)) {
        Some(rt) => rt,
        None => return false,
    };

    // Free the old renderer-side resources. `transform_key` belongs to
    // the RendererNode and is intentionally NOT touched here.
    let old_mesh = salvaged.mesh_key;
    let old_material = salvaged.material_key;
    with_renderer_mut(move |r| {
        r.remove_mesh(old_mesh);
        r.remove_material(old_material);
    })
    .await;

    let base_world_pos = salvaged.base_world_pos;

    // Build fresh renderer-side handles on the new def's pass path,
    // mirroring `materialize_or_replace`'s blend/opaque fork.
    let new_built = if new_def.blend {
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        build_runtime_blend(&mut r, transform_key, base_world_pos, new_def).await
    } else {
        let mut out = None;
        with_renderer_mut(|r| {
            out = build_runtime(r, transform_key, base_world_pos, new_def);
        })
        .await;
        out
    };

    let Some(mut new_runtime) = new_built else {
        // build_* failed — without the salvaged runtime to put back
        // we'd leave the node in an "expected playing" state with no
        // runtime. Best we can do is log and return true so caller
        // doesn't tear down further. The next Play toggle will rebuild
        // from scratch.
        tracing::warn!(
            "particles_sync: structural rebuild for {node_id:?} failed; runtime dropped"
        );
        return true;
    };

    // Graft the preserved simulator state. Per-frame `tick_all`
    // assumes `simulator.packed` and the runtime's `attrs_buf` /
    // `transforms_buf` are consistent on size — `build_*` already
    // sized those to new_def.max_alive, and `tick_all` clamps by the
    // smaller of the two each frame, so a max_alive shrink hides
    // overflow particles cleanly while a max_alive grow shows them
    // immediately.
    new_runtime.simulator = salvaged.simulator;
    new_runtime.last_ts_ms = salvaged.last_ts_ms;

    with_runtimes(|m| {
        m.insert(node_id, new_runtime);
    });
    true
}

/// Tear down a runtime + drop the per-node "playing" Mutable. Called from
/// `apply_kind` (kind transitioned away from `ParticleEmitter`) and
/// `remove_node` so a deleted emitter node doesn't keep a stale
/// runtime alive.
pub async fn forget(node_id: NodeId) {
    tear_down(node_id).await;
    let state = app_state();
    state.playing_emitters.lock().unwrap().remove(&node_id);
}

fn def_to_emitter(def: &ParticleEmitterDef) -> Emitter {
    Emitter {
        spawn_rate: def.spawn_rate,
        burst_count: def.burst_count,
        max_alive: def.max_alive,
        one_shot: def.one_shot,
        space: match def.space {
            EmitterSpaceDef::World => EmitterSpace::World,
            EmitterSpaceDef::Local => EmitterSpace::Local,
        },
        shape: match def.shape {
            SpawnShapeDef::Point => SpawnShape::Point,
            SpawnShapeDef::Sphere { radius } => SpawnShape::Sphere { radius },
            SpawnShapeDef::Cone {
                angle_radians,
                direction,
            } => SpawnShape::Cone {
                angle_radians,
                direction,
            },
        },
        initial_speed: (def.initial_speed[0], def.initial_speed[1]),
        lifetime: (def.lifetime[0], def.lifetime[1]),
        size: (def.size[0], def.size[1]),
        forces: def
            .forces
            .iter()
            .map(|f| match *f {
                ForceDef::Gravity { acceleration } => Force::Gravity { acceleration },
                ForceDef::LinearDrag { coefficient_x1000 } => Force::LinearDrag {
                    coefficient: coefficient_x1000 as f32 / 1000.0,
                },
            })
            .collect(),
        color_over_life: match &def.color_over_life {
            ColorOverLifeDef::Const(c) => awsm_particles::emitter::ColorOverLife::Const(*c),
            ColorOverLifeDef::Linear { start, end } => {
                awsm_particles::emitter::ColorOverLife::Linear {
                    start: *start,
                    end: *end,
                }
            }
        },
        size_over_life: match def.size_over_life {
            SizeOverLifeDef::Const(c) => awsm_particles::emitter::SizeOverLife::Const(c),
            SizeOverLifeDef::Linear { start, end } => {
                awsm_particles::emitter::SizeOverLife::Linear { start, end }
            }
        },
    }
}

/// Resolve `def.texture` to a `MaterialTexture` for the particle's
/// base-color slot. Mirrors `procedural_sync::resolve_material_texture`
/// but is local because the particle path doesn't go through a
/// `MaterialDef`. Tagged sRGB because particle sprites are authored as
/// gamma-encoded PNGs (smoke, fire, sparks) — same convention as
/// every other base_color binding in the renderer.
fn resolve_particle_texture(
    renderer: &mut AwsmRenderer,
    texture_ref: Option<awsm_scene_schema::TextureRef>,
) -> Option<awsm_renderer::materials::MaterialTexture> {
    use super::texture_cache::{asset_source, get_or_upload, TextureColorRole};
    use awsm_renderer::textures::SamplerCacheKey;
    let texture_ref = texture_ref?;
    let source = asset_source(texture_ref.0)?;
    let key = get_or_upload(renderer, texture_ref.0, &source, TextureColorRole::Srgb)?;
    let sampler_key = renderer
        .textures
        .get_sampler_key(&renderer.gpu, SamplerCacheKey::default())
        .ok()?;
    Some(awsm_renderer::materials::MaterialTexture {
        key,
        sampler_key: Some(sampler_key),
        uv_index: Some(0),
        transform_key: None,
    })
}
