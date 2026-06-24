//! Particle-emitter preview. Builds an `awsm_renderer_particles::Simulator` + an
//! instanced billboard quad per `NodeKind::ParticleEmitter`, ticked each frame
//! by the render loop. Ported (opaque/emissive path, auto-playing) from the
//! archived editor's per-node particle sync; the transparent-blend path + the
//! "Play" toggle gate are the follow-on.

use std::cell::RefCell;
use std::collections::HashMap;

use awsm_renderer::instances::InstanceAttr;
use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::meshes::mesh::BillboardMode;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use awsm_renderer_editor_protocol::{
    ColorOverLifeDef, EmitterSpaceDef, ForceDef, NodeId, ParticleEmitterDef, SizeOverLifeDef,
    SpawnShapeDef,
};
use awsm_renderer_particles::{Emitter, EmitterSpace, Force, Simulator, SpawnShape};
use glam::{Quat, Vec3};

const PARTICLE_QUAD_SIZE: f32 = 1.0;

struct EmitterRuntime {
    emitter: Emitter,
    simulator: Simulator,
    mesh_key: MeshKey,
    material_key: MaterialKey,
    transform_key: TransformKey,
    emitter_transform_key: TransformKey,
    transforms_buf: Vec<Transform>,
    attrs_buf: Vec<InstanceAttr>,
    base_world_pos: Vec3,
}

thread_local! {
    static RUNTIMES: RefCell<HashMap<NodeId, EmitterRuntime>> = RefCell::new(HashMap::new());
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
            ColorOverLifeDef::Const(c) => {
                awsm_renderer_particles::emitter::ColorOverLife::Const(*c)
            }
            ColorOverLifeDef::Linear { start, end } => {
                awsm_renderer_particles::emitter::ColorOverLife::Linear {
                    start: *start,
                    end: *end,
                }
            }
        },
        size_over_life: match def.size_over_life {
            SizeOverLifeDef::Const(c) => awsm_renderer_particles::emitter::SizeOverLife::Const(c),
            SizeOverLifeDef::Linear { start, end } => {
                awsm_renderer_particles::emitter::SizeOverLife::Linear { start, end }
            }
        },
    }
}

async fn build_runtime(
    renderer: &mut AwsmRenderer,
    parent_transform: TransformKey,
    parent_world_pos: Vec3,
    def: &ParticleEmitterDef,
) -> Option<EmitterRuntime> {
    let emitter = def_to_emitter(def);
    let max = emitter.max_alive.max(1) as usize;
    let instance_parent = match emitter.space {
        EmitterSpace::Local => parent_transform,
        EmitterSpace::World => renderer.transforms.root_node,
    };

    let base_color = match &def.color_over_life {
        ColorOverLifeDef::Const(c) => *c,
        ColorOverLifeDef::Linear { start, .. } => *start,
    };
    // §14: bind the emitter's billboard SPRITE (def.texture) so particles sample
    // its alpha instead of rendering as hard squares. Two routes by `def.blend`:
    //   - blend=false: alpha-TEST (Mask, cutoff 0.5) — the sprite's transparent
    //     rim is discarded so the particle takes the sprite's SHAPE, but the edge
    //     is a hard cutout and the fade-out pops at the cutoff. Cheaper (opaque
    //     instancing pass).
    //   - blend=true: transparent BLEND — the sprite's per-texel alpha (and the
    //     per-particle color.a, ramped by color_over_life) drive a real
    //     (src.a, 1-src.a) blend → SOFT-GRADIENT edges + a smooth fade with no
    //     Mask pop. Routes through the transparent instancing pipeline (async).
    // The sprite drives base-color (the blended/masked alpha) AND emissive (the
    // glow keeps the sprite shape).
    let sprite = def.texture.as_ref().and_then(|tref| {
        super::material::resolve_texture_binding(renderer, tref).map(|(key, sampler_key)| {
            awsm_renderer::materials::MaterialTexture {
                key,
                sampler_key: Some(sampler_key),
                uv_index: Some(0),
                transform_key: None,
            }
        })
    });
    let alpha_mode = if def.blend {
        MaterialAlphaMode::Blend
    } else if sprite.is_some() {
        MaterialAlphaMode::Mask { cutoff: 0.5 }
    } else {
        MaterialAlphaMode::Opaque
    };
    let mut pbr = PbrMaterial::new(alpha_mode, true);
    pbr.base_color_factor = [1.0, 1.0, 1.0, 1.0];
    pbr.metallic_factor = 0.0;
    pbr.roughness_factor = 1.0;
    pbr.emissive_factor = [
        base_color[0] * 1.6,
        base_color[1] * 1.6,
        base_color[2] * 1.6,
    ];
    // The sprite's ALPHA drives the mask cutout (→ the particle takes the
    // sprite's shape); binding it to emissive too keeps the kept disc brightly
    // glowing (emissive-only base reads washed-out against a bright IBL).
    pbr.base_color_tex = sprite.clone();
    pbr.emissive_tex = sprite;
    let material_key = renderer.materials.insert(
        Material::Pbr(Box::new(pbr)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );

    let m = awsm_renderer_meshgen::sprite_quad(PARTICLE_QUAD_SIZE, PARTICLE_QUAD_SIZE);
    let raw = RawMeshData {
        positions: m.positions,
        normals: m.normals,
        uv_sets: m.uvs,
        colors: m.colors,
        indices: m.indices,
        ..Default::default()
    };
    let transform_key = renderer
        .transforms
        .insert(Transform::IDENTITY, Some(instance_parent));
    let mesh_key = match renderer.add_raw_mesh(raw, transform_key, material_key) {
        Ok(k) => k,
        Err(err) => {
            tracing::warn!("particles build_runtime: add_raw_mesh failed: {err}");
            renderer.remove_material(material_key);
            renderer.transforms.remove(transform_key);
            return None;
        }
    };
    let _ = renderer.set_mesh_billboard_mode(mesh_key, BillboardMode::Full);
    let _ = renderer.set_mesh_shadow_flags(
        mesh_key,
        awsm_renderer::shadows::MeshShadowFlags {
            cast: false,
            receive: false,
        },
    );

    let dead = Transform {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ZERO,
    };
    let dead_attr = InstanceAttr::from_rgba_alpha_size([1.0, 1.0, 1.0, 0.0], 0.0, 1.0);
    let initial_transforms = vec![dead.clone(); max];
    let initial_attrs = vec![dead_attr; max];
    // Async instancing builds the TRANSPARENT pipeline when the material is Blend;
    // for Opaque/Mask it early-returns after the transform insert (same as the old
    // `enable_mesh_instancing_opaque`), so this one call serves both routes.
    if let Err(err) = renderer
        .enable_mesh_instancing(mesh_key, &initial_transforms)
        .await
    {
        tracing::warn!("particles: enable_mesh_instancing failed: {err}");
        renderer.remove_mesh(mesh_key);
        renderer.remove_material(material_key);
        renderer.transforms.remove(transform_key);
        return None;
    }
    let _ = renderer.set_mesh_instance_attrs(transform_key, &initial_attrs);

    Some(EmitterRuntime {
        emitter,
        simulator: Simulator::new(0xED1700),
        mesh_key,
        material_key,
        transform_key,
        emitter_transform_key: parent_transform,
        transforms_buf: vec![dead.clone(); max],
        attrs_buf: vec![dead_attr; max],
        base_world_pos: parent_world_pos,
    })
}

/// Build + register an auto-playing emitter runtime for a node. Async because the
/// transparent-blend route (§14, `def.blend`) compiles a transparent instancing
/// pipeline; the opaque/mask route awaits a cheap early-return.
pub async fn materialize(
    renderer: &mut AwsmRenderer,
    node_id: NodeId,
    parent_transform: TransformKey,
    parent_world_pos: Vec3,
    def: &ParticleEmitterDef,
) {
    teardown(renderer, node_id);
    if let Some(rt) = build_runtime(renderer, parent_transform, parent_world_pos, def).await {
        RUNTIMES.with(|m| m.borrow_mut().insert(node_id, rt));
    }
}

/// Free a node's emitter runtime (GPU mesh/material/transform).
pub fn teardown(renderer: &mut AwsmRenderer, node_id: NodeId) {
    let rt = RUNTIMES.with(|m| m.borrow_mut().remove(&node_id));
    if let Some(rt) = rt {
        renderer.remove_mesh(rt.mesh_key);
        renderer.remove_material(rt.material_key);
        renderer.transforms.remove(rt.transform_key);
    }
}

/// Advance every emitter one frame + push the live particles to its instanced
/// mesh. Called from the render loop before `render`.
pub fn tick_all(renderer: &mut AwsmRenderer) {
    let dt = renderer.frame_globals().delta_time;
    RUNTIMES.with(|map| {
        for runtime in map.borrow_mut().values_mut() {
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
                tracing::warn!("particles tick: set_mesh_instances failed: {err}");
            }
            let _ = renderer.set_mesh_instance_attrs(runtime.transform_key, &runtime.attrs_buf);
        }
    });
}
