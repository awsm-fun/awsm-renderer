//! Native locks on the CPU→GPU uniform byte layout each first-party material
//! packs in `write_uniform_buffer`. The WGSL side reads these payloads at hard-
//! coded word offsets (e.g. `flipbook_get_material` reads cols at base+11), so a
//! silent drift between the Rust writer and the WGSL reader corrupts shading
//! with no compile error and no panic — only a wrong-looking render. These
//! tests pin the layout (offsets + the clamp/validation behavior) so the
//! contract can't slip unnoticed.

#![cfg(test)]

use awsm_renderer_core::keys::{SamplerKey, TextureKey, TextureTransformKey};
use awsm_renderer_core::sampler::AddressMode;
use awsm_renderer_core::texture::texture_pool::{TexturePoolArray, TexturePoolEntryInfo};

use crate::{MaterialAlphaMode, MaterialShader, MaterialShaderId, TextureContext};

/// A `TextureContext` for a TEXTURELESS material: every lookup misses, so
/// `write_material_texture` emits the 20-byte `SkipTexture` placeholder. Enough
/// to exercise the scalar field layout without a GPU / texture pool.
struct NullTextureContext;

impl TextureContext for NullTextureContext {
    fn pool_array_by_index(&self, _index: usize) -> Option<&TexturePoolArray<TextureKey>> {
        None
    }
    fn texture_entry(&self, _key: TextureKey) -> Option<&TexturePoolEntryInfo<TextureKey>> {
        None
    }
    fn sampler_index(&self, _key: SamplerKey) -> Option<u32> {
        None
    }
    fn sampler_address_modes(
        &self,
        _key: SamplerKey,
    ) -> (Option<AddressMode>, Option<AddressMode>) {
        (None, None)
    }
    fn texture_transform_offset(&self, _key: TextureTransformKey) -> Option<usize> {
        None
    }
    fn texture_transform_identity_offset(&self) -> usize {
        0
    }
}

fn pack(m: &dyn MaterialShader) -> Vec<u8> {
    let mut out = Vec::new();
    m.write_uniform_buffer(&NullTextureContext, &mut out);
    out
}

fn u32_at(bytes: &[u8], word: usize) -> u32 {
    let b = word * 4;
    u32::from_le_bytes(bytes[b..b + 4].try_into().unwrap())
}
fn f32_at(bytes: &[u8], word: usize) -> f32 {
    let b = word * 4;
    f32::from_le_bytes(bytes[b..b + 4].try_into().unwrap())
}

/// Every first-party material MUST write its `shader_id` as word 0 — the kernel
/// dispatches on it. A material that forgot (or wrote it elsewhere) would be
/// shaded by the wrong branch.
#[test]
fn shader_id_is_always_word_zero() {
    use crate::flipbook::FlipBookMaterial;
    use crate::pbr::PbrMaterial;
    use crate::toon::ToonMaterial;
    use crate::unlit::UnlitMaterial;
    let a = MaterialAlphaMode::Opaque;
    let cases: [(&dyn MaterialShader, MaterialShaderId); 4] = [
        (&PbrMaterial::new(a, false), MaterialShaderId::PBR),
        (&UnlitMaterial::new(a, false), MaterialShaderId::UNLIT),
        (&ToonMaterial::new(a, false), MaterialShaderId::TOON),
        (&FlipBookMaterial::new(a, false), MaterialShaderId::FLIPBOOK),
    ];
    for (m, id) in cases {
        let bytes = pack(m);
        assert!(bytes.len() >= 4, "payload too short to hold a shader_id");
        assert_eq!(
            u32_at(&bytes, 0),
            id.as_u32(),
            "material {id:?} must write its shader_id as word 0 (got {})",
            u32_at(&bytes, 0),
        );
        assert_eq!(bytes.len() % 4, 0, "payload must be u32-aligned");
    }
}

/// FlipBook's full word layout, matching `wgsl/flipbook_material.wgsl`
/// (`flipbook_get_material`): shader_id, alpha_mode, alpha_cutoff,
/// atlas_tex(5), tint(4), cols, rows, frame_count, fps, time_offset, mode,
/// flip_y → 19 words / 76 bytes.
#[test]
fn flipbook_uniform_word_layout() {
    use crate::flipbook::{FlipBookMaterial, FlipBookMode};
    let mut m = FlipBookMaterial::new(MaterialAlphaMode::Mask { cutoff: 0.25 }, false);
    m.tint = [0.1, 0.2, 0.3, 0.4];
    m.cols = 3;
    m.rows = 2;
    m.frame_count = 5;
    m.fps = 12.5;
    m.time_offset = 1.5;
    m.mode = FlipBookMode::PingPong;
    m.flip_y = true;
    let b = pack(&m);

    assert_eq!(b.len(), 19 * 4, "flipbook payload is 19 u32 words");
    assert_eq!(u32_at(&b, 0), MaterialShaderId::FLIPBOOK.as_u32());
    // word 1 alpha_mode (Mask), word 2 alpha_cutoff.
    assert_eq!(f32_at(&b, 2), 0.25, "alpha_cutoff @ word 2");
    // words 3..8 are the 20-byte SkipTexture placeholder (no atlas) → all zero.
    for w in 3..8 {
        assert_eq!(u32_at(&b, w), 0, "atlas SkipTexture word {w} must be zero");
    }
    // words 8..12 tint rgba.
    assert_eq!(
        [f32_at(&b, 8), f32_at(&b, 9), f32_at(&b, 10), f32_at(&b, 11)],
        [0.1, 0.2, 0.3, 0.4],
        "tint @ words 8..12",
    );
    assert_eq!(u32_at(&b, 12), 3, "cols @ word 12");
    assert_eq!(u32_at(&b, 13), 2, "rows @ word 13");
    assert_eq!(u32_at(&b, 14), 5, "frame_count @ word 14");
    assert_eq!(f32_at(&b, 15), 12.5, "fps @ word 15");
    assert_eq!(f32_at(&b, 16), 1.5, "time_offset @ word 16");
    assert_eq!(
        u32_at(&b, 17),
        FlipBookMode::PingPong.as_u32(),
        "mode @ word 17"
    );
    assert_eq!(u32_at(&b, 18), 1, "flip_y(true) @ word 18");
}

/// FlipBook clamps invalid grid/frame dims at pack time so the WGSL never
/// divides by zero or indexes past the atlas: cols/rows floor at 1, and
/// frame_count is clamped into `[1, cols*rows]`.
#[test]
fn flipbook_clamps_degenerate_grid() {
    use crate::flipbook::FlipBookMaterial;
    // cols=0,rows=0 → both floor to 1; frame_count=0 → 1.
    let mut m = FlipBookMaterial::new(MaterialAlphaMode::Opaque, false);
    m.cols = 0;
    m.rows = 0;
    m.frame_count = 0;
    let b = pack(&m);
    assert_eq!(u32_at(&b, 12), 1, "cols=0 floors to 1");
    assert_eq!(u32_at(&b, 13), 1, "rows=0 floors to 1");
    assert_eq!(u32_at(&b, 14), 1, "frame_count=0 floors to 1");

    // frame_count beyond cols*rows clamps down to the grid capacity.
    let mut m2 = FlipBookMaterial::new(MaterialAlphaMode::Opaque, false);
    m2.cols = 2;
    m2.rows = 2;
    m2.frame_count = 99;
    let b2 = pack(&m2);
    assert_eq!(u32_at(&b2, 14), 4, "frame_count clamps to cols*rows = 4");
}
