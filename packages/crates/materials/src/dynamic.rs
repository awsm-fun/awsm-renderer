//! Generic dynamic-material interpreter.
//!
//! A single [`DynamicMaterial`] type backs every runtime-registered material.
//! Per-material differentiation (WGSL fragment, layout, alpha mode) lives in
//! the renderer's dynamic-material registry, keyed by [`MaterialShaderId`];
//! the interpreter looks the registration up at write-time rather than
//! carrying a `Box<dyn MaterialShader>` per instance.
//!
//! The trait methods drive the [`crate::dynamic_layout`] packer for
//! the uniform-tail + texture-tail + buffer-tail writes. The
//! buffer-tail's `(offset, length)` pairs come from the context's
//! extras-pool slice lookup; a slot with no assigned slice packs `(0, 0)`.

use crate::{
    dynamic_layout::{
        pack_buffer_offsets, pack_texture_indices, pack_uniform_values, MaterialLayout,
        UniformValue,
    },
    shader::MaterialShader,
    MaterialAlphaMode, MaterialShaderId, TextureContext,
};

/// Generic runtime-registered material instance.
///
/// All registered dynamic materials share this type — what differs per
/// material is the layout + WGSL fragment reachable via [`Self::shader_id`].
/// The instance carries the per-instance uniform values, texture bindings,
/// and buffer-slot data; the renderer's registry owns the layout + WGSL.
///
/// Construct via [`DynamicMaterial::new`] when the consumer can plumb the
/// registry through. For testing, the public fields make
/// hand-construction possible without dragging the registry into every
/// caller.
#[derive(Clone, Debug)]
pub struct DynamicMaterial {
    /// Shader id assigned by the renderer's dynamic-material registry.
    pub shader_id: MaterialShaderId,
    /// Snapshot of the registration's `alpha_mode` at instance-build
    /// time. Used by `Material::is_transparency_pass` / `alpha_mask`
    /// without an additional registry lookup — those methods take
    /// `&self` and don't have a `DynamicMaterials` reference, so the
    /// registration value is mirrored here at the point a
    /// `Material::Custom` is built. Re-registering the same shader_id
    /// with a different alpha_mode does NOT auto-update existing
    /// instances; rebuild the affected `Material::Custom` via
    /// `Materials::update` to pick up the new value.
    pub alpha_mode: MaterialAlphaMode,
    /// Snapshot of the registration's `double_sided`. Same caveat as
    /// `alpha_mode` — mirrored at construction; rebuild the instance
    /// to refresh.
    pub double_sided: bool,
    /// Per-instance uniform values, indexed in the layout's declared order.
    /// Each value's [`UniformValue::field_type`] must match the layout
    /// entry's [`FieldType`](crate::dynamic_layout::FieldType).
    pub values: Vec<UniformValue>,
    /// Per-instance texture bindings, one slot per
    /// [`TextureSlot`](crate::dynamic_layout::TextureSlotRuntime) in the
    /// layout. `None` means the slot is unbound — the renderer's bridge
    /// is responsible for falling back to the registration default at
    /// upload time (here we write `u32::MAX` as the index).
    pub textures: Vec<Option<DynamicTextureBinding>>,
    /// Per-instance buffer-slot data. Each entry is a `Vec<u32>` of raw
    /// little-endian words — the same shape the renderer's extras-pool
    /// allocator slices into. `None` falls back to the registration
    /// default at upload time. The buffer-tail `(offset, length)` come
    /// from the context's extras-pool slice lookup; a slot with no
    /// assigned slice packs `(0, 0)`.
    pub buffers: Vec<Option<Vec<u32>>>,
}

/// A per-instance texture binding for a [`DynamicMaterial`]'s
/// [`TextureSlot`](crate::dynamic_layout::TextureSlotRuntime).
#[derive(Clone, Debug)]
pub enum DynamicTextureBinding {
    /// The slot is bound to a pooled texture key together with the sampler
    /// it should be sampled through. The renderer resolves these to the two
    /// packed `u32` words the generated `MaterialData` carries per texture
    /// slot — `array_and_layer` and `uv_and_sampler` — at upload time via
    /// `TextureContext` (see [`DynamicMaterialContext::resolve_texture_index`]).
    Pooled {
        texture: awsm_renderer_core::keys::TextureKey,
        sampler: awsm_renderer_core::keys::SamplerKey,
    },
}

impl DynamicMaterial {
    /// Constructs a [`DynamicMaterial`] with default values pulled from
    /// the layout. `shader_id` is the id returned by
    /// `AwsmRenderer::register_material`. `alpha_mode` and
    /// `double_sided` must mirror the registration's values so the
    /// surrounding `Material::Custom`'s transparency-pass routing /
    /// cull-mode flag pick the right path.
    ///
    /// `defaults` must match `layout.uniforms` in order + type.
    pub fn new(
        shader_id: MaterialShaderId,
        alpha_mode: MaterialAlphaMode,
        double_sided: bool,
        layout: &MaterialLayout,
        defaults: Vec<UniformValue>,
    ) -> Self {
        assert_eq!(
            defaults.len(),
            layout.uniforms.len(),
            "DynamicMaterial::new: defaults length {} != layout.uniforms length {}",
            defaults.len(),
            layout.uniforms.len(),
        );
        Self {
            shader_id,
            alpha_mode,
            double_sided,
            values: defaults,
            textures: vec![None; layout.textures.len()],
            buffers: vec![None; layout.buffers.len()],
        }
    }
}

/// Context plumbed through to [`MaterialShader::write_uniform_buffer`] for
/// dynamic-material instances.
///
/// Carries the renderer-side dynamic registry's view of the material's
/// layout (so the packer knows the field order) and the per-instance
/// extras-pool slice assignments (so the buffer-slot tail can be written).
///
/// Implemented by the renderer's `DynamicMaterials` facade so
/// `awsm-renderer-materials` stays decoupled from the renderer crate.
pub trait DynamicMaterialContext {
    /// Returns the layout for a registered dynamic material. Returns
    /// `None` if the id is unknown — the packer treats that as a
    /// pack-zero-bytes no-op rather than panicking; the renderer-side
    /// registration path validates ids before instances are built.
    fn layout(&self, shader_id: MaterialShaderId) -> Option<&MaterialLayout>;

    /// Returns the registered material's alpha mode. Mirrors
    /// [`Self::layout`]'s contract — `None` for unknown ids.
    fn alpha_mode(&self, shader_id: MaterialShaderId) -> Option<MaterialAlphaMode>;

    /// Resolves a per-instance texture binding to the two packed `u32`
    /// words the generated `MaterialData` carries per texture slot:
    /// `[array_and_layer, uv_and_sampler]` — the same bit layout as
    /// `TextureInfoRaw`'s corresponding fields (array_index | layer<<12,
    /// and uv_set | sampler<<8). Returns `[u32::MAX, 0]` for unbound slots —
    /// the generated `material_sample_<name>` helper treats the `u32::MAX`
    /// sentinel in the first word as "no texture".
    fn resolve_texture_index(&self, binding: Option<&DynamicTextureBinding>) -> [u32; 2];

    /// Returns the extras-pool slice currently assigned to
    /// `(shader_id, buffer_slot_index)`. `None` means no slice
    /// assigned, in which case the packer writes `(0, 0)` for that slot.
    fn buffer_slice(
        &self,
        shader_id: MaterialShaderId,
        buffer_slot_index: usize,
    ) -> Option<(u32, u32)>;
}

impl MaterialShader for DynamicMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        self.shader_id
    }

    fn wgsl_fragment(&self) -> &'static str {
        // Dynamic materials don't carry a `&'static str` WGSL fragment —
        // the fragment lives in the registry as a `String` (owned per
        // registration) and the template substitution emits it directly.
        // First-party materials override this method to return their
        // `WGSL_FRAGMENT` constant; the registry walks first-party entries
        // through `registry::build_materials_wgsl` and dynamic entries
        // through a separate code path.
        //
        // Calling this on a dynamic material is a renderer-internal bug
        // (the dispatch table should never reach for a `&'static str`
        // for a dynamic id).
        unreachable!(
            "MaterialShader::wgsl_fragment is not callable on DynamicMaterial — \
             dynamic-material WGSL is sourced from the renderer's registry, \
             not the instance"
        )
    }

    fn shader_includes(&self) -> crate::ShaderIncludes {
        // Conservative: author-supplied WGSL may reference any shared symbol,
        // so until the dynamic-registration API carries a per-material
        // declaration, dynamic materials opt into the full optional surface
        // (same as the pre-skinny behaviour). The renderer's dynamic shader
        // cache key is the future home for a tighter, author-declared set.
        crate::ShaderIncludes::all()
    }

    fn fragment_inputs(&self) -> crate::FragmentInputs {
        crate::FragmentInputs::all()
    }

    fn alpha_mode(&self) -> MaterialAlphaMode {
        // The renderer-side dispatch never asks a `DynamicMaterial` for
        // its alpha mode directly — it calls
        // `DynamicMaterials::get(shader_id).map(|reg| reg.alpha_mode)`
        // — but `MaterialShader` is the shared trait so we need a
        // sensible default. Opaque is the conservative pick (routes
        // through the opaque kernel; doesn't surprise the transparent
        // sort).
        MaterialAlphaMode::Opaque
    }

    fn is_transparency_pass(&self) -> bool {
        // Same caveat as `alpha_mode` — the renderer side routes
        // dynamic-material transparency through
        // `DynamicMaterials::get(shader_id).map(...)`.
        false
    }

    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, out: &mut Vec<u8>) {
        // Writes the shader_id prefix then defers to
        // `write_uniform_buffer_with_layout` once the caller has plumbed
        // the layout context. The plain `TextureContext`-only signature
        // can't reach the layout — the renderer-side bridge calls
        // `write_uniform_buffer_with_layout` directly (passing the
        // `DynamicMaterialContext` view).
        //
        // For callers that hit this path through
        // `Material::uniform_buffer_data`, we panic so the missed
        // wiring surfaces loudly rather than producing garbage bytes.
        // The renderer's `Material::Custom` arm in `uniform_buffer_data`
        // is marked `unreachable!` for the same reason — see
        // crates/renderer/src/materials.rs.
        let _ = (ctx, out);
        unreachable!(
            "DynamicMaterial::write_uniform_buffer is wired via \
             `write_uniform_buffer_with_layout`; the bare \
             `MaterialShader::write_uniform_buffer` path is not reachable \
             for dynamic materials"
        );
    }
}

impl DynamicMaterial {
    /// Pack this instance into `out`, including the shader_id prefix
    /// word, the alignment-respecting uniform tail, the texture-index
    /// tail, and the buffer `(offset, length)` tail.
    ///
    /// The `ctx` provides the layout + per-instance buffer-slice
    /// assignments. The renderer's bridge owns it.
    pub fn write_uniform_buffer_with_layout(
        &self,
        ctx: &dyn DynamicMaterialContext,
        out: &mut Vec<u8>,
    ) {
        // 1. shader_id prefix word — matches the first-party convention
        //    (`flipbook_get_material` reads `byte_offset / 4 + 1` to
        //    skip this).
        out.extend_from_slice(&self.shader_id.as_u32().to_le_bytes());

        let Some(layout) = ctx.layout(self.shader_id) else {
            // Unknown registration — nothing else to pack. The renderer
            // side should never get here for live instances (a stale
            // material reference is caught at bridge time), but it's
            // safer than panicking from a per-frame upload path.
            return;
        };

        // 2. Round to struct align (so the auto-generated MaterialData
        //    struct's first field lands at the alignment WGSL expects).
        //    The struct's natural alignment is at least 4 (the shader_id
        //    is 4 bytes), and at most 16 (anything with a vec3/vec4/mat).
        //    The template substitution emits the access pattern that
        //    matches this.
        let struct_align = layout_align(layout);
        let pre_struct_len = out.len();
        let pad = align_up(pre_struct_len, struct_align) - pre_struct_len;
        for _ in 0..pad {
            out.push(0);
        }
        let struct_start = out.len();

        // 3. Uniform values.
        pack_uniform_values(layout, &self.values, out);

        // 4. Texture indices — two words per slot
        //    (`array_and_layer`, `uv_and_sampler`).
        let texture_indices: Vec<[u32; 2]> = self
            .textures
            .iter()
            .map(|binding| ctx.resolve_texture_index(binding.as_ref()))
            .collect();
        pack_texture_indices(layout, &texture_indices, out);

        // 5. Buffer (offset, length) pairs. The ctx returns the
        //    extras-pool slice per slot; unassigned slots write (0, 0).
        let buffer_pairs: Vec<(u32, u32)> = (0..layout.buffers.len())
            .map(|i| ctx.buffer_slice(self.shader_id, i).unwrap_or((0, 0)))
            .collect();
        pack_buffer_offsets(layout, &buffer_pairs, out);

        // 6. Trailing struct padding.
        crate::dynamic_layout::pad_tail_to_struct_size(layout, out, struct_start);
    }
}

fn layout_align(layout: &MaterialLayout) -> usize {
    let mut align: usize = 4;
    for f in &layout.uniforms {
        if f.ty.align() > align {
            align = f.ty.align();
        }
    }
    // Texture / buffer slots are u32 (align 4) so they don't bump the
    // struct align further.
    align
}

fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_layout::FieldType;
    use crate::dynamic_layout::{BufferSlotRuntime, TextureSlotRuntime, UniformFieldRuntime};

    struct StubCtx {
        layout: MaterialLayout,
    }
    impl DynamicMaterialContext for StubCtx {
        fn layout(&self, _id: MaterialShaderId) -> Option<&MaterialLayout> {
            Some(&self.layout)
        }
        fn alpha_mode(&self, _id: MaterialShaderId) -> Option<MaterialAlphaMode> {
            Some(MaterialAlphaMode::Opaque)
        }
        fn resolve_texture_index(&self, _binding: Option<&DynamicTextureBinding>) -> [u32; 2] {
            [42, 43]
        }
        fn buffer_slice(&self, _id: MaterialShaderId, _slot: usize) -> Option<(u32, u32)> {
            None
        }
    }

    #[test]
    fn write_uniform_buffer_with_layout_produces_expected_bytes() {
        let layout = MaterialLayout {
            uniforms: vec![UniformFieldRuntime {
                name: "a".into(),
                ty: FieldType::F32,
            }],
            textures: vec![TextureSlotRuntime {
                name: "tex".into(),
                srgb: true,
                mipmap_kind: awsm_renderer_core::texture::mipmap::MipmapTextureKind::Albedo,
            }],
            buffers: vec![BufferSlotRuntime { name: "buf".into() }],
        };
        let id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
        let material = DynamicMaterial::new(
            id,
            MaterialAlphaMode::Opaque,
            false,
            &layout,
            vec![UniformValue::F32(3.5)],
        );

        let ctx = StubCtx { layout };
        let mut out = Vec::new();
        material.write_uniform_buffer_with_layout(&ctx, &mut out);

        // shader_id (4) + struct (20: f32 + u32 tex_index + u32 tex_uv_sampler
        // + u32 buf_off + u32 buf_len)
        assert_eq!(out.len(), 4 + 20);
        let id_bytes = MaterialShaderId::DYNAMIC_START.to_le_bytes();
        assert_eq!(&out[0..4], &id_bytes);
        assert_eq!(&out[4..8], &3.5_f32.to_le_bytes());
        assert_eq!(&out[8..12], &42u32.to_le_bytes()); // tex_index (array_and_layer)
        assert_eq!(&out[12..16], &43u32.to_le_bytes()); // tex_uv_sampler
        assert_eq!(&out[16..20], &0u32.to_le_bytes()); // buf_offset stub
        assert_eq!(&out[20..24], &0u32.to_le_bytes()); // buf_length stub
    }

    #[test]
    fn vec3_first_field_aligns_after_shader_id() {
        let layout = MaterialLayout {
            uniforms: vec![UniformFieldRuntime {
                name: "v".into(),
                ty: FieldType::Vec3,
            }],
            ..Default::default()
        };
        let id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
        let material = DynamicMaterial::new(
            id,
            MaterialAlphaMode::Opaque,
            false,
            &layout,
            vec![UniformValue::Vec3([1.0, 2.0, 3.0])],
        );
        let ctx = StubCtx { layout };
        let mut out = Vec::new();
        material.write_uniform_buffer_with_layout(&ctx, &mut out);

        // shader_id (4) + 12 bytes of pre-struct pad (to reach 16-byte
        // align) + 12 bytes of Vec3 payload + 4 bytes of trailing struct
        // padding (slot rounded to align 16) = 32.
        assert_eq!(out.len(), 32);
        // The Vec3 payload starts at byte 16.
        assert_eq!(&out[16..20], &1.0_f32.to_le_bytes());
        assert_eq!(&out[20..24], &2.0_f32.to_le_bytes());
        assert_eq!(&out[24..28], &3.0_f32.to_le_bytes());
    }
}
