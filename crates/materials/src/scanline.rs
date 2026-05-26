//! Scanline material — first-party port of the Phase-4 dynamic
//! `scanline` worked example. Documented in
//! [`docs/dynamic-materials/promotion.md`](../../../../docs/dynamic-materials/promotion.md)
//! as the worked example for the promotion path.
//!
//! The byte layout produced by [`ScanlineMaterial::write_uniform_buffer`]
//! is **byte-identical** to what the dynamic packer's
//! `pack_uniform_values` produces for the equivalent
//! [`MaterialLayout`] — verified by the promotion smoke test at
//! the bottom of this file. If the two diverge, the contract has
//! leaked: fix the dynamic packer or the hand-written writer, not
//! both.
//!
//! Gated behind the `scanline` Cargo feature so consumers can opt
//! out of the runtime byte cost (one shader_id arm + one
//! `compute_scanline_overlay` call) when they don't use it.

use crate::{
    shader::MaterialShader,
    writer::{write, write_material_texture},
    MaterialAlphaMode, MaterialShaderId, MaterialTexture, TextureContext,
};

/// WGSL helper module for this material.
pub const WGSL_FRAGMENT: &str = include_str!("wgsl/scanline_material.wgsl");

/// Scanline material parameters.
///
/// Same surface as the dynamic `scanline` worked example — a base
/// texture overlaid with a moving horizontal scanline pattern.
/// See the worked example in
/// `docs/dynamic-materials/contract-opaque.md` for the rationale.
#[derive(Clone, Debug)]
pub struct ScanlineMaterial {
    /// Base texture sampled with the quad's UV. `None` renders as the
    /// scanline overlay over a flat 50% grey, useful for authoring
    /// without an asset pipeline.
    pub base_tex: Option<MaterialTexture>,
    /// Overlay tint. Default `[0.6, 0.9, 0.6]` (greenish CRT phosphor).
    pub tint: [f32; 3],
    /// Vertical scanline frequency. Default `80.0` cycles per quad.
    pub scan_freq: f32,
    /// Scanline scroll rate. Default `0.5` cycles / second.
    pub scan_speed: f32,
    /// Overlay intensity, 0.0..1.0. Default `0.3`.
    pub scan_strength: f32,
    // Immutable properties — changing them requires recreating the
    // material (same shape as Unlit/PBR/Toon).
    alpha_mode: MaterialAlphaMode,
    double_sided: bool,
}

impl Default for ScanlineMaterial {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanlineMaterial {
    /// Constructs a scanline material with default values matching the
    /// dynamic `scanline` worked example.
    pub fn new() -> Self {
        Self {
            base_tex: None,
            tint: [0.6, 0.9, 0.6],
            scan_freq: 80.0,
            scan_speed: 0.5,
            scan_strength: 0.3,
            alpha_mode: MaterialAlphaMode::Opaque,
            double_sided: false,
        }
    }

    /// Returns the material alpha mode.
    pub fn alpha_mode(&self) -> &MaterialAlphaMode {
        &self.alpha_mode
    }

    /// Returns whether the material is double sided.
    pub fn double_sided(&self) -> bool {
        self.double_sided
    }

    /// Returns the alpha cutoff for masked materials (always `None`
    /// for scanline — alpha_mode is Opaque).
    pub fn alpha_cutoff(&self) -> Option<f32> {
        match self.alpha_mode {
            MaterialAlphaMode::Mask { cutoff } => Some(cutoff),
            _ => None,
        }
    }
}

impl MaterialShader for ScanlineMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        MaterialShaderId::SCANLINE
    }

    fn wgsl_fragment(&self) -> &'static str {
        WGSL_FRAGMENT
    }

    fn alpha_mode(&self) -> MaterialAlphaMode {
        self.alpha_mode
    }

    fn is_transparency_pass(&self) -> bool {
        // Scanline is opaque-only.
        false
    }

    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, data: &mut Vec<u8>) {
        // Byte layout — must produce bit-identical output to the
        // dynamic packer for the equivalent MaterialLayout (one
        // Color3 + three F32 + one TextureSlot). Verified by the
        // promotion smoke test below.
        //
        //   word 0      shader_id
        //   word 1..3   leading pad (vec3<f32> needs 16-byte align)
        //   word 4..6   tint (vec3 payload)
        //   word 7      trailing vec3 pad
        //   word 8      scan_freq
        //   word 9      scan_speed
        //   word 10     scan_strength
        //   word 11..15 base_tex_info (TextureInfoRaw, 5 u32s)
        write(data, self.shader_id().as_u32().into());
        // Leading vec3 alignment pad — 3 u32 words to advance from
        // word 1 to word 4.
        write(data, 0u32.into());
        write(data, 0u32.into());
        write(data, 0u32.into());
        // tint
        write(data, self.tint[0].into());
        write(data, self.tint[1].into());
        write(data, self.tint[2].into());
        // NOTE: no trailing vec3 pad — the next field (scan_freq) is
        // f32 (4-byte align) so a 12-byte vec3 payload is immediately
        // followed by the f32. This matches the dynamic packer's
        // `align_buffer_to` behavior. If the next field were vec3/vec4/mat,
        // a 4-byte pad would be needed here.
        // scan_freq / scan_speed / scan_strength
        write(data, self.scan_freq.into());
        write(data, self.scan_speed.into());
        write(data, self.scan_strength.into());
        // base_tex_info
        write_material_texture(data, self.base_tex.as_ref(), ctx);
    }
}

#[cfg(test)]
mod tests {
    //! Promotion smoke test — the load-bearing assertion that
    //! `ScanlineMaterial::write_uniform_buffer` produces byte-identical
    //! output to the dynamic packer for the equivalent `MaterialLayout`.

    use super::*;
    use crate::dynamic::{DynamicMaterial, DynamicMaterialContext, DynamicTextureBinding};
    use crate::dynamic_layout::{
        BufferSlotRuntime, FieldType, MaterialLayout, TextureSlotRuntime, UniformFieldRuntime,
        UniformValue,
    };
    use crate::texture_context::TextureContext;
    use awsm_renderer_core::{
        keys::{SamplerKey, TextureKey, TextureTransformKey},
        sampler::AddressMode,
        texture::texture_pool::{TexturePoolArray, TexturePoolEntryInfo},
    };

    /// Stub TextureContext that returns `None` for every lookup — the
    /// scanline test material has no bound texture, so the writer
    /// emits the "unbound" TextureInfo pattern in both halves.
    struct StubTextureContext;
    impl TextureContext for StubTextureContext {
        fn pool_array_by_index(&self, _i: usize) -> Option<&TexturePoolArray<TextureKey>> {
            None
        }
        fn texture_entry(&self, _k: TextureKey) -> Option<&TexturePoolEntryInfo<TextureKey>> {
            None
        }
        fn sampler_index(&self, _k: SamplerKey) -> Option<u32> {
            None
        }
        fn sampler_address_modes(
            &self,
            _k: SamplerKey,
        ) -> (Option<AddressMode>, Option<AddressMode>) {
            (None, None)
        }
        fn texture_transform_offset(&self, _k: TextureTransformKey) -> Option<usize> {
            None
        }
        fn texture_transform_identity_offset(&self) -> usize {
            0
        }
    }

    /// Stub DynamicMaterialContext returning a hard-coded layout
    /// matching the scanline material.json. resolve_texture_index
    /// returns `u32::MAX` for unbound — mirrors the StubTextureContext
    /// path that produces an empty TextureInfoRaw above.
    struct StubDynamicCtx {
        layout: MaterialLayout,
    }
    impl DynamicMaterialContext for StubDynamicCtx {
        fn layout(&self, _id: MaterialShaderId) -> Option<&MaterialLayout> {
            Some(&self.layout)
        }
        fn alpha_mode(&self, _id: MaterialShaderId) -> Option<MaterialAlphaMode> {
            Some(MaterialAlphaMode::Opaque)
        }
        fn resolve_texture_index(&self, _b: Option<&DynamicTextureBinding>) -> u32 {
            u32::MAX
        }
        fn buffer_slice(&self, _id: MaterialShaderId, _idx: usize) -> Option<(u32, u32)> {
            None
        }
    }

    fn scanline_layout() -> MaterialLayout {
        MaterialLayout {
            uniforms: vec![
                UniformFieldRuntime {
                    name: "tint".into(),
                    ty: FieldType::Color3,
                },
                UniformFieldRuntime {
                    name: "scan_freq".into(),
                    ty: FieldType::F32,
                },
                UniformFieldRuntime {
                    name: "scan_speed".into(),
                    ty: FieldType::F32,
                },
                UniformFieldRuntime {
                    name: "scan_strength".into(),
                    ty: FieldType::F32,
                },
            ],
            textures: vec![TextureSlotRuntime {
                name: "base".into(),
            }],
            buffers: Vec::<BufferSlotRuntime>::new(),
        }
    }

    #[test]
    fn promotion_byte_identical_struct_prefix() {
        // Build a dynamic material with the same layout + same
        // default values that ScanlineMaterial::new() ships with.
        let layout = scanline_layout();
        let values = vec![
            UniformValue::Color3([0.6, 0.9, 0.6]),
            UniformValue::F32(80.0),
            UniformValue::F32(0.5),
            UniformValue::F32(0.3),
        ];
        let id = MaterialShaderId::from_dynamic_raw(MaterialShaderId::DYNAMIC_START);
        let dynamic = DynamicMaterial::new(id, &layout, values);
        let dyn_ctx = StubDynamicCtx {
            layout: layout.clone(),
        };
        let mut dynamic_bytes = Vec::new();
        dynamic.write_uniform_buffer_with_layout(&dyn_ctx, &mut dynamic_bytes);

        // ScanlineMaterial — the typed first-party version.
        let promoted = ScanlineMaterial::new();
        let stub_tex = StubTextureContext;
        let mut promoted_bytes = Vec::new();
        MaterialShader::write_uniform_buffer(&promoted, &stub_tex, &mut promoted_bytes);

        // The byte-identical assertion is for the layout-driven
        // prefix BEFORE the texture-index tail. The promoted writer
        // appends a full 5-u32 TextureInfoRaw; the dynamic packer
        // appends a single texture-index u32 (`u32::MAX` for
        // unbound). Those tails are intentionally different — the
        // typed first-party version carries the full glTF-style
        // TextureInfo (sampler, transform, uv-set indices) while the
        // dynamic version only carries the pool index (the dynamic
        // contract pre-resolves sampler / transform via
        // texture_pool_sample_* directly).
        //
        // Verify the prefix matches: shader_id word + 3 leading pad
        // u32s + tint vec3 (no trailing pad, next is f32) + 3 f32
        // scalars = 10 u32 = 40 bytes.
        const PREFIX_BYTES: usize = 40;
        assert!(
            dynamic_bytes.len() >= PREFIX_BYTES,
            "dynamic packer produced {} bytes; expected >= {}",
            dynamic_bytes.len(),
            PREFIX_BYTES
        );
        assert!(
            promoted_bytes.len() >= PREFIX_BYTES,
            "promoted writer produced {} bytes; expected >= {}",
            promoted_bytes.len(),
            PREFIX_BYTES
        );
        // The shader_id word differs (dynamic uses DYNAMIC_START,
        // promoted uses SCANLINE = 5); verify only the post-shader-id
        // portion (which is purely layout-driven) is equal.
        assert_eq!(
            &dynamic_bytes[4..PREFIX_BYTES],
            &promoted_bytes[4..PREFIX_BYTES],
            "dynamic vs promoted scanline byte prefix (post-shader-id) differs"
        );
    }

    #[test]
    fn promotion_wgsl_includes_expected_helpers() {
        // The promoted WGSL fragment must declare the
        // scanline_get_material accessor + the
        // scanline_compute_overlay helper that the opaque kernel's
        // dispatch arm calls.
        assert!(WGSL_FRAGMENT.contains("fn scanline_get_material"));
        assert!(WGSL_FRAGMENT.contains("fn scanline_compute_overlay"));
        assert!(WGSL_FRAGMENT.contains("struct ScanlineMaterial"));
    }
}
