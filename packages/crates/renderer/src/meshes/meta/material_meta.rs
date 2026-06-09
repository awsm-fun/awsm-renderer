//! Material mesh metadata packing.

use std::sync::LazyLock;

use awsm_renderer_core::buffers::BufferUsage;
use slotmap::Key;

use crate::{
    materials::{MaterialKey, Materials},
    meshes::{
        buffer_info::MeshBufferInfo,
        buffer_info::{MeshBufferCustomVertexAttributeInfo, MeshBufferVertexAttributeInfo},
        morphs::{MaterialMorphKey, Morphs},
        AwsmMeshError, Mesh, MeshKey,
    },
};

/// Bitmask for normal morphing.
pub const MATERIAL_MESH_META_MORPH_MATERIAL_BITMASK_NORMAL: u32 = 1;
/// Bitmask for tangent morphing.
pub const MATERIAL_MESH_META_MORPH_MATERIAL_BITMASK_TANGENT: u32 = 1 << 1;
/// Byte size for material mesh meta struct.
///
/// Layout (in u32 indices):
///   0: mesh_key_high
///   1: mesh_key_low
///   2: morph_material_target_len
///   3: morph_material_weights_offset
///   4: morph_material_values_offset
///   5: morph_material_bitmask
///   6: material_offset
///   7: transform_offset
///   8: normal_matrix_offset
///   9: vertex_attribute_indices_offset
///   10: vertex_attribute_data_offset
///   11: vertex_attribute_stride
///   12: uv_sets_index
///   13: uv_set_count
///   14: color_set_count
///   15: visibility_geometry_data_offset
///   16: is_hud
///   17: receive_shadows
///   18: receive_decals
///   19: shadow_receiver_gate (per-frame coverage gate; see
///       `MATERIAL_MESH_META_SHADOW_RECEIVER_GATE_OFFSET`)
///   20..23: reserved / pad (keeps `padding_4` at vec4 alignment;
///       indices 22..23 formerly held the per-mesh light slice, removed
///       when shading unified on the per-pixel froxel light list)
pub const MATERIAL_MESH_META_BYTE_SIZE: usize = 96;
/// Byte alignment for material mesh meta entries.
pub const MATERIAL_MESH_META_BYTE_ALIGNMENT: usize = 256;
/// Byte offset of the `receive_shadows` u32 inside the packed struct.
/// Matches the layout in `MaterialMeshMeta::to_bytes` and the
/// `receive_shadows` field in `shared_wgsl/material_mesh_meta.wgsl`.
/// Used by `MeshMeta::set_receive_shadows` for the in-place patch
/// path so the shadow toggle doesn't need to re-pack the entire
/// struct (which would require Materials/Transforms/Morphs context).
pub const MATERIAL_MESH_META_RECEIVE_SHADOWS_OFFSET: usize = 17 * 4;
/// Byte offset of the `material_offset` u32 inside the packed struct.
/// The 4 bytes from this offset hold the *effective* material's GPU
/// offset (cheap or authored, picked by
/// `Mesh::effective_material_key`) and are patched once per coverage-
/// cross-threshold transition by `MeshMeta::set_material_offset`.
pub const MATERIAL_MESH_META_MATERIAL_OFFSET_OFFSET: usize = 6 * 4;
/// Byte offset of the `receive_decals` u32 inside the packed struct.
/// Used by `MeshMeta::set_receive_decals` for the in-place patch path
/// so the decal toggle doesn't need to re-pack the entire struct.
pub const MATERIAL_MESH_META_RECEIVE_DECALS_OFFSET: usize = 18 * 4;
/// Byte offset of the `shadow_receiver_gate` u32 inside the packed struct.
/// Per-frame coverage-driven gate that augments the authored
/// `receive_shadows` flag — `apply_lighting*` in `lights.wgsl` skips
/// shadow sampling when this is `0u`, even if `receive_shadows` is `1u`.
/// Written by `MeshMeta::set_shadow_receiver_gate` from
/// `transforms::update_transforms` after `LightMeshBuckets::mark_shadow_receivers`.
pub const MATERIAL_MESH_META_SHADOW_RECEIVER_GATE_OFFSET: usize = 19 * 4;

pub static MATERIAL_BUFFER_USAGE: LazyLock<BufferUsage> = LazyLock::new(|| {
    BufferUsage::new()
        .with_copy_dst()
        .with_storage()
        .with_uniform()
});

/// Material meta fields used by shaders.
/// See `meta.wgsl` for the corresponding struct.
pub struct MaterialMeshMeta<'a> {
    pub mesh_key: MeshKey,
    pub material_key: MaterialKey,
    pub material_morph_key: Option<MaterialMorphKey>,
    pub custom_attribute_indices_offset: usize,
    pub custom_attribute_data_offset: usize,
    pub visibility_geometry_data_offset: Option<usize>,
    pub transform_offset: usize,
    pub normal_matrix_offset: usize,
    pub buffer_info: &'a MeshBufferInfo,
    pub materials: &'a Materials,
    pub morphs: &'a Morphs,
    pub mesh: &'a Mesh,
}

/// Calculate the offset (in floats) to TEXCOORD_0 within the vertex attribute data.
/// This accounts for any COLOR_n attributes that come before texture coordinates.
fn calculate_uv_sets_index(buffer_info: &MeshBufferInfo) -> u32 {
    let mut offset_floats = 0;
    for attr in &buffer_info.triangles.vertex_attributes {
        if let MeshBufferVertexAttributeInfo::Custom(custom) = attr {
            match custom {
                MeshBufferCustomVertexAttributeInfo::Colors { .. } => {
                    // vertex_size() returns bytes, divide by 4 to get float count
                    offset_floats += attr.vertex_size() / 4;
                }
                MeshBufferCustomVertexAttributeInfo::TexCoords { .. } => {
                    // Found TexCoords, stop counting
                    break;
                }
            }
        }
    }
    offset_floats as u32
}

/// Calculate the offset (in floats) to COLOR_0 within the vertex attribute data.
/// Symmetric with [`calculate_uv_sets_index`]: accounts for any TEXCOORD_n (or
/// other) attributes packed before the first color set. The opaque compute
/// kernel fetches vertex colours manually from the visibility buffer (unlike the
/// transparent raster path, which gets rasterizer-interpolated attributes), so
/// it needs the real per-vertex offset rather than assuming colours sit at 0.
fn calculate_color_sets_index(buffer_info: &MeshBufferInfo) -> u32 {
    let mut offset_floats = 0;
    for attr in &buffer_info.triangles.vertex_attributes {
        if let MeshBufferVertexAttributeInfo::Custom(custom) = attr {
            match custom {
                MeshBufferCustomVertexAttributeInfo::TexCoords { .. } => {
                    offset_floats += attr.vertex_size() / 4;
                }
                MeshBufferCustomVertexAttributeInfo::Colors { .. } => {
                    // Found the first color set, stop counting.
                    break;
                }
            }
        }
    }
    offset_floats as u32
}

/// Calculate how many UV sets and color sets this mesh has.
/// Returns (uv_set_count, color_set_count).
fn calculate_attribute_counts(buffer_info: &MeshBufferInfo) -> (u32, u32) {
    let mut uv_set_count = 0u32;
    let mut color_set_count = 0u32;

    for attr in &buffer_info.triangles.vertex_attributes {
        if let MeshBufferVertexAttributeInfo::Custom(custom) = attr {
            match custom {
                MeshBufferCustomVertexAttributeInfo::TexCoords { index, .. } => {
                    uv_set_count = uv_set_count.max(*index + 1);
                }
                MeshBufferCustomVertexAttributeInfo::Colors { index, .. } => {
                    color_set_count = color_set_count.max(*index + 1);
                }
            }
        }
    }

    (uv_set_count, color_set_count)
}

impl<'a> MaterialMeshMeta<'a> {
    /// Packs material meta into bytes.
    pub fn to_bytes(
        self,
    ) -> std::result::Result<[u8; MATERIAL_MESH_META_BYTE_SIZE], AwsmMeshError> {
        let Self {
            mesh_key,
            material_key,
            material_morph_key,
            buffer_info,
            custom_attribute_indices_offset,
            custom_attribute_data_offset,
            visibility_geometry_data_offset,
            transform_offset,
            normal_matrix_offset,
            materials,
            morphs,
            mesh,
        } = self;

        let mut result = [0u8; MATERIAL_MESH_META_BYTE_SIZE];
        let mut offset = 0;

        let mut push_u32 = |value: u32| {
            result[offset..offset + 4].copy_from_slice(&value.to_le_bytes());

            offset += 4;
        };

        let mesh_key_u64 = mesh_key.data().as_ffi();
        let (mesh_key_u32_high, mesh_key_u32_low) = (
            (mesh_key_u64 >> 32) as u32,
            (mesh_key_u64 & 0xFFFFFFFF) as u32,
        );

        // Mesh Key (8 bytes)
        push_u32(mesh_key_u32_high);
        push_u32(mesh_key_u32_low);

        // Morph (20 bytes)
        if let Some(morph_key) = material_morph_key {
            let info = morphs.material.get_info(morph_key)?;
            push_u32(info.targets_len as u32);
            push_u32(morphs.material.weights_buffer_offset(morph_key)? as u32);
            push_u32(morphs.material.values_buffer_offset(morph_key)? as u32);
            let mut bitmask = 0;
            if info.attributes.normal {
                bitmask |= MATERIAL_MESH_META_MORPH_MATERIAL_BITMASK_NORMAL;
            }
            if info.attributes.tangent {
                bitmask |= MATERIAL_MESH_META_MORPH_MATERIAL_BITMASK_TANGENT;
            }
            push_u32(bitmask);
        } else {
            push_u32(0);
            push_u32(0);
            push_u32(0);
            push_u32(0);
        }

        // Material (4 bytes)
        push_u32(materials.buffer_offset(material_key)? as u32);

        // Transform offset (4 bytes)
        push_u32(transform_offset as u32);
        // Normal matrix offset (4 bytes)
        push_u32(normal_matrix_offset as u32);

        // Vertex attribute offsets (8 bytes)
        push_u32(custom_attribute_indices_offset as u32);
        push_u32(custom_attribute_data_offset as u32);

        // Vertex attribute stride (4 bytes)
        push_u32(buffer_info.triangles.vertex_attribute_stride() as u32);

        // UV sets index - offset in floats to TEXCOORD_0 within vertex attribute data (4 bytes)
        let uv_sets_index = calculate_uv_sets_index(buffer_info);
        push_u32(uv_sets_index);

        // UV set count and color set count (8 bytes)
        let (uv_set_count, color_set_count) = calculate_attribute_counts(buffer_info);
        push_u32(uv_set_count);
        push_u32(color_set_count);

        // Geometry data offset (4 bytes)
        push_u32(visibility_geometry_data_offset.unwrap_or_default() as u32);

        // is hud
        push_u32(if mesh.hud { 1 } else { 0 });

        // receive_shadows — consumed by `apply_lighting` in
        // `lights.wgsl` to skip the shadow modulation when the mesh
        // opted out. Matches the `receive_shadows` u32 in
        // `material_mesh_meta.wgsl`.
        push_u32(if mesh.receive_shadows { 1 } else { 0 });

        // receive_decals (index 18) — consumed by the decal compute pass
        // to skip the per-decal volume test when the mesh opted out.
        // Matches the `receive_decals` u32 in `material_mesh_meta.wgsl`.
        push_u32(if mesh.receive_decals { 1 } else { 0 });
        // shadow_receiver_gate (index 19) — per-frame coverage-driven
        // gate. Initial pack is `1` (conservative: assume receiver until
        // proven otherwise) so a freshly-inserted mesh shaded before the
        // next `LightMeshBuckets::mark_shadow_receivers` pass still
        // samples shadows correctly. Live updates run through
        // `MeshMeta::set_shadow_receiver_gate`.
        push_u32(1);
        // color_sets_index (index 20) — offset in floats to COLOR_0 within the
        // vertex attribute data, symmetric with `uv_sets_index`. The opaque
        // compute kernel fetches vertex colours manually, so it needs the real
        // offset (colours pack after UVs, not at 0). Formerly `_reserved0`.
        push_u32(calculate_color_sets_index(buffer_info));
        // Reserved trailing u32s (indices 21-23). The last two formerly
        // held the per-mesh light slice (`offset`/`count`) for the old
        // per-mesh lighting path, removed when shading unified on the
        // per-pixel froxel light list; kept as inert tail padding (each
        // entry is 256-byte aligned, so removing them reclaims nothing).
        // The four u32s keep the populated region at a vec4 multiple so
        // `padding_4: array<vec4<u32>, _>` stays vec4-aligned.
        push_u32(0);
        push_u32(0);
        push_u32(0);

        Ok(result)
    }
}
