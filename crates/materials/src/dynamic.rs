//! Generic dynamic-material interpreter.
//!
//! A single [`DynamicMaterial`] type backs every runtime-registered material.
//! Per-material differentiation (WGSL fragment, layout, alpha mode) lives in
//! the [`registry`](crate::registry) keyed by [`MaterialShaderId`]; the
//! interpreter looks the registration up at write-time rather than carrying a
//! `Box<dyn MaterialShader>` per instance.
//!
//! Phase 0 ships the type with stub trait methods; Phase 2 fills in the layout
//! walk + texture / buffer slot packing.

use crate::{
    shader::MaterialShader, MaterialAlphaMode, MaterialShaderId, TextureContext,
};

/// Generic runtime-registered material instance.
///
/// All registered dynamic materials share this type — what differs per
/// material is the layout + WGSL fragment reachable via [`Self::shader_id`].
/// The instance carries the per-instance uniform values, texture bindings,
/// and buffer-slot data; the renderer's registry owns the layout + WGSL.
#[derive(Clone, Debug)]
pub struct DynamicMaterial {
    /// Shader id assigned by the renderer's dynamic-material registry.
    pub shader_id: MaterialShaderId,
    /// Per-instance uniform values, indexed in the layout's declared order.
    /// Phase 0 leaves the concrete type unspecified — Phase 2 wires the
    /// scene-schema [`UniformValue`] in here.
    pub values: Vec<DynamicUniformValue>,
    /// Per-instance texture bindings, one slot per
    /// [`TextureSlot`](crate::dynamic::DynamicTextureBinding) in the
    /// layout. `None` means the slot is unbound and falls back to its
    /// registration default at upload time.
    pub textures: Vec<Option<DynamicTextureBinding>>,
    /// Per-instance buffer-slot data. Each entry is a `Vec<u32>` of raw
    /// little-endian words — the same shape the renderer's extras-pool
    /// allocator slices into. `None` falls back to the registration
    /// default at upload time. Phase 6 wires the extras pool.
    pub buffers: Vec<Option<Vec<u32>>>,
}

/// Placeholder for per-uniform values until Phase 2 wires the real
/// [`UniformValue`] from the scene-schema crate. Holding the type here
/// keeps the public surface stable across the Phase 0 → Phase 2 transition.
#[derive(Clone, Debug)]
pub enum DynamicUniformValue {
    /// Single-precision float — placeholder for the full
    /// [`UniformValue`] enum landed in Phase 2.
    F32(f32),
    /// Four-component float vector.
    Vec4([f32; 4]),
}

/// Placeholder for per-texture-slot bindings until Phase 2 wires the
/// scene-schema-driven [`MaterialTexture`] in.
#[derive(Clone, Debug)]
pub enum DynamicTextureBinding {
    /// The slot is bound to a pooled texture key.
    Pooled(awsm_renderer_core::keys::TextureKey),
}

impl MaterialShader for DynamicMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        self.shader_id
    }

    fn wgsl_fragment(&self) -> &'static str {
        // The fragment is looked up from the registry at template-emit
        // time; instance-level access to the source is not part of the
        // contract. Phase 2 reworks the trait so this method is unused
        // for dynamic materials (template emission queries the registry
        // directly).
        unimplemented!("dynamic-material WGSL fragment is sourced from the registry, not the instance — see Phase 2 trait rework")
    }

    fn alpha_mode(&self) -> MaterialAlphaMode {
        unimplemented!("dynamic-material alpha_mode is sourced from the registry — wired in Phase 2")
    }

    fn is_transparency_pass(&self) -> bool {
        unimplemented!("dynamic-material is_transparency_pass derives from the registered alpha_mode — wired in Phase 2")
    }

    fn write_uniform_buffer(&self, _ctx: &dyn TextureContext, _out: &mut Vec<u8>) {
        unimplemented!("dynamic-material packing walks the registry's layout — wired in Phase 2")
    }
}
