//! ParticleEmitter materialization — **Design A: the loader sets up, the game
//! ticks** (decided 2026-06-18).
//!
//! The loader is a one-shot pass and never advances a clock (the same boundary as
//! animation: it loads clips, the consumer drives the playhead). A
//! [`NodeKind::ParticleEmitter`](awsm_scene::NodeKind::ParticleEmitter) therefore
//! materializes into a **ready-to-drive** instanced billboard — an emissive quad
//! with GPU instancing pre-enabled at `max_alive` capacity — and the loader hands
//! back an [`EmitterHandle`]. The game runs the CPU simulation
//! ([`awsm_particles::Simulator`]) each frame and pushes the live particles to the
//! handle via [`drive_emitter`]; the loader itself never simulates.
//!
//! The build mirrors the editor's proven live-preview bridge
//! (`engine/bridge/particles.rs`): an emissive PBR quad, camera-facing
//! [`BillboardMode::Full`], shadows off, instancing seeded with `max_alive`
//! zero-scale / zero-alpha "dead" instances so nothing shows until the first tick.
//! Keeping the instance count fixed at `max_alive` means the per-frame
//! [`drive_emitter`] re-write reuses the same buffers (no reallocation).
//!
//! **Follow-ons (matching the editor bridge's own documented gaps):** the sprite
//! `texture` is not yet bound (untextured emissive dots only), and `blend` always
//! routes through the opaque-emissive path (the transparent-blend pass is the
//! follow-on). Neither is a regression — the pre-loader `materialize` dropped the
//! whole emitter.

use anyhow::Result;
use awsm_particles::{Emitter, EmitterSpace, Force, Simulator, SpawnShape};
use awsm_renderer::instances::InstanceAttr;
use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode};
use awsm_renderer::meshes::mesh::BillboardMode;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::shadows::MeshShadowFlags;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use awsm_scene::{
    ColorOverLifeDef, EmitterSpaceDef, ForceDef, ParticleEmitterDef, SizeOverLifeDef, SpawnShapeDef,
};
use glam::{Mat4, Quat, Vec3};

use crate::mesh_data_to_raw;

/// The unit billboard quad every emitter instances; per-particle `size` scales it.
const PARTICLE_QUAD_SIZE: f32 = 1.0;

/// A materialized particle emitter, returned in
/// [`NodeHandles::emitter`](crate::NodeHandles) so the game can drive it.
///
/// **Contract — loader sets up, game ticks:** the loader has built the instanced
/// billboard and pre-enabled instancing at `capacity` (`= max_alive`); it does
/// **not** simulate. Each frame the consumer:
/// 1. ticks a [`Simulator`] with [`Self::to_emitter`] + [`Self::spawn_origin`],
/// 2. pushes the result with [`drive_emitter`].
///
/// ```no_run
/// # use awsm_scene_loader::{EmitterHandle, drive_emitter};
/// # use awsm_renderer::AwsmRenderer;
/// # fn tick(renderer: &mut AwsmRenderer, handle: &EmitterHandle,
/// #         sim: &mut awsm_particles::Simulator, dt: f32) -> anyhow::Result<()> {
/// let emitter = handle.to_emitter();
/// let origin = handle.spawn_origin(renderer);
/// sim.tick(dt, &emitter, origin);
/// drive_emitter(renderer, handle, sim)?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct EmitterHandle {
    /// The instanced billboard-quad mesh. Pass to
    /// [`AwsmRenderer::set_mesh_instances`].
    pub mesh: MeshKey,
    /// The instance mesh's transform key — pass to
    /// [`AwsmRenderer::set_mesh_instance_attrs`] (per-instance colour/alpha/size).
    pub instance_transform: TransformKey,
    /// The emitter node's own transform key, used to recompute the world-space
    /// spawn origin for [`EmitterSpaceDef::World`] emitters that move at runtime.
    pub emitter_transform: TransformKey,
    /// The emitter's world-space position at load time (the spawn origin for
    /// [`EmitterSpaceDef::Local`], and the fallback for `World`).
    pub base_world_pos: Vec3,
    /// Instance capacity (`= max_alive.max(1)`); the per-frame buffers stay this
    /// length so [`drive_emitter`] never reallocates.
    pub capacity: usize,
    /// The authored simulation spec, so the consumer can build a matching
    /// [`Emitter`] via [`Self::to_emitter`] without re-reading the scene.
    pub def: ParticleEmitterDef,
}

impl EmitterHandle {
    /// Lower the authored [`ParticleEmitterDef`] into an [`awsm_particles::Emitter`]
    /// the consumer feeds to [`Simulator::tick`]. Mirrors the editor bridge's
    /// `def_to_emitter` so the player and the editor simulate identically.
    pub fn to_emitter(&self) -> Emitter {
        def_to_emitter(&self.def)
    }

    /// The current world-space spawn origin for this emitter. For
    /// [`EmitterSpaceDef::Local`] this is the load-time position (particles are
    /// parented to the emitter transform, so they follow it for free); for
    /// [`EmitterSpaceDef::World`] it re-reads the emitter transform's world matrix
    /// so a moving emitter spawns from its live position.
    pub fn spawn_origin(&self, renderer: &AwsmRenderer) -> Vec3 {
        match self.def.space {
            EmitterSpaceDef::Local => self.base_world_pos,
            EmitterSpaceDef::World => renderer
                .transforms
                .get_world(self.emitter_transform)
                .map(|m| m.w_axis.truncate())
                .unwrap_or(self.base_world_pos),
        }
    }
}

/// Lower an authored [`ParticleEmitterDef`] into an [`awsm_particles::Emitter`].
/// Mirrors the editor bridge's `def_to_emitter` exactly.
pub(crate) fn def_to_emitter(def: &ParticleEmitterDef) -> Emitter {
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

/// Build the instanced billboard for a `ParticleEmitter` node and return its
/// [`EmitterHandle`] (the loader records it into `NodeHandles.emitter`).
///
/// `tk` is the emitter node's transform; `node_world` its accumulated world
/// matrix (for the spawn origin). Emits one fresh instance transform (the parent
/// of the billboard) plus the quad mesh — both pushed onto `loaded` for teardown.
pub(crate) fn build_emitter(
    renderer: &mut AwsmRenderer,
    def: &ParticleEmitterDef,
    tk: TransformKey,
    node_world: Mat4,
) -> Result<EmitterHandle> {
    let emitter = def_to_emitter(def);
    let capacity = emitter.max_alive.max(1) as usize;
    let base_world_pos = node_world.w_axis.truncate();

    // Local-space particles are parented to the emitter transform (they follow
    // it); world-space particles hang off the root so they persist in place.
    let instance_parent = match emitter.space {
        EmitterSpace::Local => tk,
        EmitterSpace::World => renderer.transforms.root_node,
    };

    // Emissive dots: per-instance colour/alpha (driven each frame) tints them; the
    // emissive factor seeds a glow from the curve's start colour so an untextured
    // emitter still reads as light. (Texture binding is a follow-on, matching the
    // editor bridge.)
    let base_color = match &def.color_over_life {
        ColorOverLifeDef::Const(c) => *c,
        ColorOverLifeDef::Linear { start, .. } => *start,
    };
    let mut pbr = PbrMaterial::new(MaterialAlphaMode::Opaque, true);
    pbr.base_color_factor = [1.0, 1.0, 1.0, 1.0];
    pbr.metallic_factor = 0.0;
    pbr.roughness_factor = 1.0;
    pbr.emissive_factor = [
        base_color[0] * 1.6,
        base_color[1] * 1.6,
        base_color[2] * 1.6,
    ];
    let material_key = renderer.materials.insert(
        Material::Pbr(Box::new(pbr)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );

    let md = awsm_meshgen::sprite_quad(PARTICLE_QUAD_SIZE, PARTICLE_QUAD_SIZE);
    let instance_transform = renderer
        .transforms
        .insert(Transform::IDENTITY, Some(instance_parent));
    let mesh = renderer.add_raw_mesh(mesh_data_to_raw(md), instance_transform, material_key)?;
    let _ = renderer.set_mesh_billboard_mode(mesh, BillboardMode::Full);
    let _ = renderer.set_mesh_shadow_flags(
        mesh,
        MeshShadowFlags {
            cast: false,
            receive: false,
        },
    );

    // Seed `capacity` "dead" instances (zero scale + zero alpha) so the billboard
    // is invisible until the first tick, and the per-frame re-write keeps the
    // count fixed (no reallocation).
    let dead = Transform {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ZERO,
    };
    let dead_attr = InstanceAttr::from_rgba_alpha_size([1.0, 1.0, 1.0, 0.0], 0.0, 1.0);
    let initial_transforms = vec![dead.clone(); capacity];
    let initial_attrs = vec![dead_attr; capacity];
    renderer.enable_mesh_instancing_opaque(mesh, &initial_transforms)?;
    let _ = renderer.set_mesh_instance_attrs(instance_transform, &initial_attrs);

    Ok(EmitterHandle {
        mesh,
        instance_transform,
        emitter_transform: tk,
        base_world_pos,
        capacity,
        def: def.clone(),
    })
}

/// Push a simulator's live particles to its [`EmitterHandle`]'s instanced mesh —
/// the consumer's per-frame call after [`Simulator::tick`]. Writes exactly
/// `handle.capacity` transforms + attributes (dead instances zero-scaled and
/// zero-alpha), so the buffers never reallocate.
///
/// Mirrors the editor bridge's per-frame push: a [`EmitterSpaceDef::Local`]
/// emitter's particles are stored relative to the emitter (the instance parent
/// places them), so world-space sim positions are made local by subtracting the
/// base world position; [`EmitterSpaceDef::World`] particles use their world
/// position directly.
pub fn drive_emitter(
    renderer: &mut AwsmRenderer,
    handle: &EmitterHandle,
    sim: &Simulator,
) -> Result<()> {
    let live = &sim.packed;
    let mut transforms = Vec::with_capacity(handle.capacity);
    let mut attrs = Vec::with_capacity(handle.capacity);
    for i in 0..handle.capacity {
        if let Some(p) = live.get(i) {
            let translation = match handle.def.space {
                EmitterSpaceDef::World => Vec3::from_array(p.position),
                EmitterSpaceDef::Local => Vec3::from_array(p.position) - handle.base_world_pos,
            };
            transforms.push(Transform {
                translation,
                rotation: Quat::IDENTITY,
                scale: Vec3::splat(p.size.max(1.0e-3)),
            });
            attrs.push(InstanceAttr::from_rgba_alpha_size(p.color, 1.0, 1.0));
        } else {
            transforms.push(Transform {
                translation: Vec3::ZERO,
                rotation: Quat::IDENTITY,
                scale: Vec3::ZERO,
            });
            attrs.push(InstanceAttr::from_rgba_alpha_size(
                [1.0, 1.0, 1.0, 0.0],
                0.0,
                1.0,
            ));
        }
    }
    renderer.set_mesh_instances(handle.mesh, &transforms)?;
    renderer.set_mesh_instance_attrs(handle.instance_transform, &attrs)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_def() -> ParticleEmitterDef {
        ParticleEmitterDef {
            spawn_rate: 60.0,
            burst_count: 3,
            max_alive: 128,
            one_shot: true,
            space: EmitterSpaceDef::World,
            shape: SpawnShapeDef::Cone {
                angle_radians: 0.5,
                direction: [0.0, 1.0, 0.0],
            },
            initial_speed: [1.0, 2.0],
            lifetime: [0.4, 2.0],
            size: [0.1, 0.3],
            forces: vec![
                ForceDef::Gravity {
                    acceleration: [0.0, -9.8, 0.0],
                },
                ForceDef::LinearDrag {
                    coefficient_x1000: 500,
                },
            ],
            color_over_life: ColorOverLifeDef::Linear {
                start: [1.0, 0.5, 0.2, 1.0],
                end: [1.0, 0.0, 0.0, 0.0],
            },
            size_over_life: SizeOverLifeDef::Linear {
                start: 1.0,
                end: 0.2,
            },
            texture: None,
            blend: false,
        }
    }

    #[test]
    fn def_to_emitter_maps_every_field() {
        let def = sample_def();
        let e = def_to_emitter(&def);
        assert_eq!(e.spawn_rate, 60.0);
        assert_eq!(e.burst_count, 3);
        assert_eq!(e.max_alive, 128);
        assert!(e.one_shot);
        assert!(matches!(e.space, EmitterSpace::World));
        assert!(matches!(e.shape, SpawnShape::Cone { .. }));
        assert_eq!(e.initial_speed, (1.0, 2.0));
        assert_eq!(e.lifetime, (0.4, 2.0));
        assert_eq!(e.size, (0.1, 0.3));
        assert_eq!(e.forces.len(), 2);
        // x1000 drag coefficient is decoded to a float fraction.
        assert!(matches!(
            e.forces[1],
            Force::LinearDrag { coefficient } if (coefficient - 0.5).abs() < 1.0e-6
        ));
    }

    #[test]
    fn def_to_emitter_drives_a_live_simulation() {
        // The handle's emitter spec actually ages a simulation — proving the
        // "loader sets up, game ticks" contract is wired end-to-end (no GPU).
        let def = sample_def();
        let emitter = def_to_emitter(&def);
        let mut sim = Simulator::new(0xED1700);
        sim.tick(0.01, &emitter, Vec3::ZERO);
        assert_eq!(sim.packed.len(), def.burst_count as usize);
        // Age past the max lifetime → all one-shot particles culled.
        sim.tick(5.0, &emitter, Vec3::ZERO);
        assert!(sim.packed.is_empty());
        assert!(sim.is_done());
    }
}
