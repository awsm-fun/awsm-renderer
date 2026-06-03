//! Particle simulator: advances state, produces packed instance-attribute slice.

use glam::Vec3;

use crate::emitter::{Emitter, EmitterSpace};
use crate::spawn::Force;

/// Per-particle data sent to the renderer's per-instance attribute path.
///
/// Memory layout matches what a future GPU compute shader would write:
/// `[position(3) | size(1) | color(4)]` per particle. `alpha` is multiplied into
/// `color.a` upstream of the slice. 32 bytes / particle.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InstanceAttr {
    pub position: [f32; 3],
    pub size: f32,
    pub color: [f32; 4],
}

#[derive(Default)]
pub struct Simulator {
    positions: Vec<Vec3>,
    velocities: Vec<Vec3>,
    ages: Vec<f32>,
    lifetimes: Vec<f32>,
    base_sizes: Vec<f32>,
    spawn_accumulator: f32,
    rng_state: u32,
    burst_fired: bool,
    one_shot_done: bool,
    pub packed: Vec<InstanceAttr>,
}

impl Simulator {
    pub fn new(seed: u32) -> Self {
        Self {
            rng_state: if seed == 0 { 0xDEADBEEF } else { seed },
            ..Self::default()
        }
    }

    fn next_unit(&mut self) -> f32 {
        // xorshift32
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        (x as f32) / (u32::MAX as f32)
    }

    fn alive_count(&self) -> usize {
        self.ages.len()
    }

    pub fn reset(&mut self) {
        self.positions.clear();
        self.velocities.clear();
        self.ages.clear();
        self.lifetimes.clear();
        self.base_sizes.clear();
        self.spawn_accumulator = 0.0;
        self.burst_fired = false;
        self.one_shot_done = false;
        self.packed.clear();
    }

    /// Spawn `count` particles immediately with `emitter`'s parameters. Used by
    /// gameplay-driven one-shot bursts (e.g. hit sparks).
    pub fn fire_burst(&mut self, emitter: &Emitter, count: u32, emitter_pos: Vec3) {
        let max = emitter.max_alive as usize;
        for _ in 0..count {
            if self.alive_count() >= max {
                break;
            }
            self.spawn_one(emitter, emitter_pos);
        }
    }

    fn spawn_one(&mut self, emitter: &Emitter, emitter_pos: Vec3) {
        let mut rng = || self.rng_state_local();
        let (offset, dir) = emitter.shape.sample(&mut rng);
        let speed_t = self.next_unit();
        let speed =
            emitter.initial_speed.0 + (emitter.initial_speed.1 - emitter.initial_speed.0) * speed_t;
        let life_t = self.next_unit();
        let lifetime = emitter.lifetime.0 + (emitter.lifetime.1 - emitter.lifetime.0) * life_t;
        let size_t = self.next_unit();
        let size = emitter.size.0 + (emitter.size.1 - emitter.size.0) * size_t;

        let position = match emitter.space {
            EmitterSpace::World => emitter_pos + offset,
            EmitterSpace::Local => emitter_pos + offset,
        };
        self.positions.push(position);
        self.velocities.push(dir * speed);
        self.ages.push(0.0);
        self.lifetimes.push(lifetime.max(0.001));
        self.base_sizes.push(size);
    }

    fn rng_state_local(&mut self) -> f32 {
        self.next_unit()
    }

    /// Advance simulation by `dt` seconds. `emitter_world_pos` is the current world
    /// position of the emitter (used for `EmitterSpace::World` spawn positioning and
    /// nothing else once particles are born).
    pub fn tick(&mut self, dt: f32, emitter: &Emitter, emitter_world_pos: Vec3) {
        // Spawn
        if emitter.one_shot {
            if !self.burst_fired {
                for _ in 0..emitter.burst_count {
                    if self.alive_count() >= emitter.max_alive as usize {
                        break;
                    }
                    self.spawn_one(emitter, emitter_world_pos);
                }
                self.burst_fired = true;
            }
        } else {
            // Continuous emission at spawn_rate per second
            self.spawn_accumulator += dt * emitter.spawn_rate;
            while self.spawn_accumulator >= 1.0 {
                if self.alive_count() >= emitter.max_alive as usize {
                    self.spawn_accumulator = 0.0;
                    break;
                }
                self.spawn_one(emitter, emitter_world_pos);
                self.spawn_accumulator -= 1.0;
            }
        }

        // Integrate
        let mut i = 0;
        while i < self.ages.len() {
            self.ages[i] += dt;
            if self.ages[i] >= self.lifetimes[i] {
                // swap-remove
                let last = self.ages.len() - 1;
                self.ages.swap_remove(i);
                self.lifetimes.swap_remove(i);
                self.positions.swap_remove(i);
                self.velocities.swap_remove(i);
                self.base_sizes.swap_remove(i);
                let _ = last;
                continue;
            }
            let mut accel = Vec3::ZERO;
            let mut drag = 0.0_f32;
            for force in &emitter.forces {
                match force {
                    Force::Gravity { acceleration } => {
                        accel += Vec3::from_array(*acceleration);
                    }
                    Force::LinearDrag { coefficient } => {
                        drag += *coefficient;
                    }
                }
            }
            self.velocities[i] += accel * dt;
            if drag > 0.0 {
                self.velocities[i] *= (1.0 - drag * dt).max(0.0);
            }
            let v = self.velocities[i];
            self.positions[i] += v * dt;
            i += 1;
        }

        // Pack
        self.packed.clear();
        self.packed.reserve(self.ages.len());
        for i in 0..self.ages.len() {
            let t = (self.ages[i] / self.lifetimes[i]).clamp(0.0, 1.0);
            let base_color = emitter.color_over_life.sample(t);
            let size_factor = emitter.size_over_life.sample(t);
            let pos = self.positions[i];
            self.packed.push(InstanceAttr {
                position: pos.to_array(),
                size: self.base_sizes[i] * size_factor,
                color: base_color,
            });
        }
        if emitter.one_shot && self.ages.is_empty() && self.burst_fired {
            self.one_shot_done = true;
        }
    }

    pub fn is_done(&self) -> bool {
        self.one_shot_done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emitter::{ColorOverLife, Emitter};
    use crate::spawn::SpawnShape;

    #[test]
    fn continuous_emission_respects_max_alive() {
        let mut sim = Simulator::new(1);
        let emitter = Emitter {
            spawn_rate: 1000.0,
            max_alive: 10,
            lifetime: (10.0, 10.0),
            ..Default::default()
        };
        sim.tick(1.0, &emitter, Vec3::ZERO);
        assert!(sim.alive_count() <= 10);
    }

    #[test]
    fn one_shot_burst_completes() {
        let mut sim = Simulator::new(2);
        let emitter = Emitter {
            spawn_rate: 0.0,
            burst_count: 5,
            one_shot: true,
            max_alive: 10,
            lifetime: (0.05, 0.05),
            initial_speed: (0.0, 0.0),
            shape: SpawnShape::Point,
            color_over_life: ColorOverLife::Const([1.0, 0.0, 0.0, 1.0]),
            ..Default::default()
        };
        sim.tick(0.01, &emitter, Vec3::ZERO);
        assert_eq!(sim.alive_count(), 5);
        sim.tick(1.0, &emitter, Vec3::ZERO);
        assert_eq!(sim.alive_count(), 0);
        assert!(sim.is_done());
    }
}
