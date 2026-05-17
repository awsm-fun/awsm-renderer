//! CPU particle simulator. See [`README.md`](../README.md).

pub mod emitter;
pub mod simulator;
pub mod spawn;

pub use emitter::{Emitter, EmitterSpace};
pub use simulator::{InstanceAttr, Simulator};
pub use spawn::{Force, SpawnShape};
