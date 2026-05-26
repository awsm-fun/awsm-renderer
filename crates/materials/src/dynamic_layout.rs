//! WGSL layout + packing helpers shared by [`DynamicMaterial`] and the
//! Phase-3+ renderer-side template substitution.
//!
//! [`DynamicMaterial`]: crate::dynamic::DynamicMaterial
//!
//! Two outputs from one source of truth — the [`MaterialLayout`]:
//!
//! 1. [`generate_wgsl_struct`] emits the WGSL `struct MaterialData { ... }`
//!    that the renderer injects above the author's WGSL fragment.
//! 2. [`pack_uniform_values`] / [`pack_texture_indices`] /
//!    [`pack_buffer_offsets`] pack a per-instance value list into the byte
//!    layout the shader expects.
//!
//! ## WGSL alignment rules
//!
//! Implements the [WGSL spec §13.4 memory layout][wgsl-layout] for the
//! field types [`FieldType`] supports:
//!
//! | Type              | Align | Size | Notes                           |
//! |-------------------|-------|------|---------------------------------|
//! | scalar (f32/u32/i32/bool) | 4 | 4 | bool packed as u32 (0/1)      |
//! | `vec2<T>`         | 8     | 8    |                                 |
//! | `vec3<T>`         | 16    | 12   | 4 bytes trailing padding        |
//! | `vec4<T>`         | 16    | 16   |                                 |
//! | `mat3x3<f32>`     | 16    | 48   | 3 columns × 16-byte-aligned vec3|
//! | `mat4x4<f32>`     | 16    | 64   |                                 |
//!
//! Struct align = `max(align(field_i))`; struct size is rounded up to
//! struct align. Each member's offset is rounded up to its align before
//! writing.
//!
//! [wgsl-layout]: https://www.w3.org/TR/WGSL/#memory-layouts
//!
//! ## Field order for the generated `MaterialData` struct
//!
//! 1. Every [`UniformField`](Self) in declaration order (alignment-respecting).
//! 2. One `<name>_index: u32` per [`TextureSlot`](Self).
//! 3. One `<name>_offset: u32` + `<name>_length: u32` per
//!    [`BufferSlot`](Self).
//!
//! The byte packing produced by `pack_*` mirrors this order exactly, so
//! the kernel's `materials[]` read indices line up byte-for-byte with the
//! generated struct's field offsets.
//!
//! ## Phase 2 status
//!
//! This module ships the helpers + unit tests. Wiring the helpers into
//! `DynamicMaterial::write_uniform_buffer` and the renderer-side
//! substitution lands in Phases 3 / 4. The unit tests are the **first
//! line of defense** against silent rendering garbage — covering every
//! [`FieldType`] in isolation and the load-bearing mixed-alignment
//! corner cases (vec3 trailing padding, mat3 stride, bool→u32, mixed
//! uniform/texture/buffer tails).

/// Field type for one entry in a [`MaterialLayout::uniforms`] list.
/// Mirrors `awsm_scene_schema::FieldType` — kept duplicated here so
/// `awsm-materials` doesn't depend on `awsm-scene-schema`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    /// `f32`
    F32,
    /// `vec2<f32>`
    Vec2,
    /// `vec3<f32>` — 16-byte aligned, 12-byte payload + 4 trailing bytes.
    Vec3,
    /// `vec4<f32>`
    Vec4,
    /// `u32`
    U32,
    /// `vec2<i32>`
    IVec2,
    /// `vec3<i32>` — 16-byte aligned, 12-byte payload + 4 trailing bytes.
    IVec3,
    /// `vec4<i32>`
    IVec4,
    /// `mat3x3<f32>` — 16-byte aligned, 48-byte payload.
    Mat3,
    /// `mat4x4<f32>`
    Mat4,
    /// `vec3<f32>` semantically, displayed as a color picker. Layout
    /// identical to `Vec3`.
    Color3,
    /// `vec4<f32>` semantically, displayed as a color picker. Layout
    /// identical to `Vec4`.
    Color4,
    /// Stored as a `u32` (0 / 1) in WGSL.
    Bool,
}

impl FieldType {
    /// WGSL type name as it appears in the generated struct.
    pub fn wgsl_type(self) -> &'static str {
        match self {
            FieldType::F32 => "f32",
            FieldType::Vec2 => "vec2<f32>",
            FieldType::Vec3 | FieldType::Color3 => "vec3<f32>",
            FieldType::Vec4 | FieldType::Color4 => "vec4<f32>",
            FieldType::U32 | FieldType::Bool => "u32",
            FieldType::IVec2 => "vec2<i32>",
            FieldType::IVec3 => "vec3<i32>",
            FieldType::IVec4 => "vec4<i32>",
            FieldType::Mat3 => "mat3x3<f32>",
            FieldType::Mat4 => "mat4x4<f32>",
        }
    }

    /// WGSL alignment in bytes.
    pub fn align(self) -> usize {
        match self {
            FieldType::F32 | FieldType::U32 | FieldType::Bool => 4,
            FieldType::Vec2 | FieldType::IVec2 => 8,
            FieldType::Vec3
            | FieldType::Vec4
            | FieldType::IVec3
            | FieldType::IVec4
            | FieldType::Color3
            | FieldType::Color4
            | FieldType::Mat3
            | FieldType::Mat4 => 16,
        }
    }

    /// WGSL field size in bytes (payload — does NOT include trailing
    /// padding for vec3 / mat3).
    pub fn size(self) -> usize {
        match self {
            FieldType::F32 | FieldType::U32 | FieldType::Bool => 4,
            FieldType::Vec2 | FieldType::IVec2 => 8,
            FieldType::Vec3 | FieldType::IVec3 | FieldType::Color3 => 12,
            FieldType::Vec4 | FieldType::IVec4 | FieldType::Color4 => 16,
            FieldType::Mat3 => 48,
            FieldType::Mat4 => 64,
        }
    }
}

/// One uniform field in a [`MaterialLayout`].
#[derive(Clone, Debug, PartialEq)]
pub struct UniformFieldRuntime {
    /// Field name. Becomes the field name in the generated WGSL struct.
    pub name: String,
    /// WGSL field type.
    pub ty: FieldType,
}

/// One texture slot. Becomes `<name>_index: u32` in the generated
/// struct.
#[derive(Clone, Debug, PartialEq)]
pub struct TextureSlotRuntime {
    /// Slot name.
    pub name: String,
}

/// One variable-length buffer slot. Becomes `<name>_offset: u32` and
/// `<name>_length: u32` in the generated struct.
#[derive(Clone, Debug, PartialEq)]
pub struct BufferSlotRuntime {
    /// Slot name.
    pub name: String,
}

/// The complete layout of a registered material — the source of truth
/// for [`generate_wgsl_struct`] + the `pack_*` helpers.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MaterialLayout {
    /// Per-instance uniform fields, in declaration order. Alignment is
    /// respected at packing time.
    pub uniforms: Vec<UniformFieldRuntime>,
    /// Per-instance texture slots, in declaration order.
    pub textures: Vec<TextureSlotRuntime>,
    /// Per-instance variable-length buffer slots, in declaration order.
    pub buffers: Vec<BufferSlotRuntime>,
}

/// One concrete per-instance uniform value. Mirrors
/// `awsm_scene_schema::UniformValue` — kept duplicated here so
/// `awsm-materials` doesn't depend on `awsm-scene-schema`.
#[derive(Clone, Debug, PartialEq)]
pub enum UniformValue {
    /// `f32` value.
    F32(f32),
    /// `vec2<f32>` value.
    Vec2([f32; 2]),
    /// `vec3<f32>` value.
    Vec3([f32; 3]),
    /// `vec4<f32>` value.
    Vec4([f32; 4]),
    /// `u32` value.
    U32(u32),
    /// `vec2<i32>` value.
    IVec2([i32; 2]),
    /// `vec3<i32>` value.
    IVec3([i32; 3]),
    /// `vec4<i32>` value.
    IVec4([i32; 4]),
    /// `mat3x3<f32>` packed as 9 column-major f32s.
    Mat3([f32; 9]),
    /// `mat4x4<f32>` packed as 16 column-major f32s.
    Mat4([f32; 16]),
    /// 3-channel color (same layout as Vec3).
    Color3([f32; 3]),
    /// 4-channel color (same layout as Vec4).
    Color4([f32; 4]),
    /// Becomes a `u32` (0 / 1) in WGSL.
    Bool(bool),
}

impl UniformValue {
    /// The [`FieldType`] this value satisfies.
    pub fn field_type(&self) -> FieldType {
        match self {
            UniformValue::F32(_) => FieldType::F32,
            UniformValue::Vec2(_) => FieldType::Vec2,
            UniformValue::Vec3(_) => FieldType::Vec3,
            UniformValue::Vec4(_) => FieldType::Vec4,
            UniformValue::U32(_) => FieldType::U32,
            UniformValue::IVec2(_) => FieldType::IVec2,
            UniformValue::IVec3(_) => FieldType::IVec3,
            UniformValue::IVec4(_) => FieldType::IVec4,
            UniformValue::Mat3(_) => FieldType::Mat3,
            UniformValue::Mat4(_) => FieldType::Mat4,
            UniformValue::Color3(_) => FieldType::Color3,
            UniformValue::Color4(_) => FieldType::Color4,
            UniformValue::Bool(_) => FieldType::Bool,
        }
    }
}

/// Generate the WGSL `struct <struct_name> { ... }` declaration that
/// goes above the author's WGSL fragment.
///
/// Field order:
/// 1. Every uniform in declaration order (alignment-respecting; padding
///    fields named `_pad_<n>` are inserted where WGSL requires them).
/// 2. One `<tex>_index: u32` per texture slot.
/// 3. One `<buf>_offset: u32` + one `<buf>_length: u32` per buffer slot.
///
/// The struct's *trailing* padding is handled by WGSL automatically when
/// the struct is read from a storage buffer — we don't emit a trailing
/// `_pad` field; the byte packer ([`pack_uniform_values`] etc.) does
/// pad the tail so [`layout_size`] reports the WGSL-correct total.
pub fn generate_wgsl_struct(struct_name: &str, layout: &MaterialLayout) -> String {
    let mut out = String::new();
    out.push_str(&format!("struct {} {{\n", struct_name));

    let mut offset: usize = 0;
    let mut pad_counter: usize = 0;

    let emit_field = |out: &mut String,
                      offset: &mut usize,
                      pad_counter: &mut usize,
                      name: &str,
                      ty: FieldType| {
        let needs_pad = align_up(*offset, ty.align()) - *offset;
        if needs_pad > 0 {
            // Emit one `u32` padding field per 4 bytes of gap so naga
            // accepts the layout regardless of which scalar alignment it
            // landed at. (We never need more than 3×4 = 12 bytes of
            // padding for the WGSL types we support; vec3 → vec4
            // transitions need 0, vec2 → vec4 needs 8, and so on.)
            let mut remaining = needs_pad;
            while remaining > 0 {
                let chunk = remaining.min(4);
                if chunk == 4 {
                    out.push_str(&format!("    _pad_{}: u32,\n", *pad_counter));
                    *pad_counter += 1;
                } else {
                    // Sub-u32 padding shouldn't occur for our type set
                    // (every field is at least 4-byte aligned and 4-byte
                    // sized). Bail loudly if it ever does.
                    panic!(
                        "[dynamic_layout] sub-u32 padding ({} bytes) before `{}`: WGSL types should always align to 4-byte boundaries",
                        chunk, name,
                    );
                }
                remaining = remaining.saturating_sub(chunk);
            }
            *offset += needs_pad;
        }
        out.push_str(&format!("    {}: {},\n", name, ty.wgsl_type()));
        *offset += ty.size();
    };

    for field in &layout.uniforms {
        emit_field(
            &mut out,
            &mut offset,
            &mut pad_counter,
            &field.name,
            field.ty,
        );
    }
    for tex in &layout.textures {
        emit_field(
            &mut out,
            &mut offset,
            &mut pad_counter,
            &format!("{}_index", tex.name),
            FieldType::U32,
        );
    }
    for buf in &layout.buffers {
        emit_field(
            &mut out,
            &mut offset,
            &mut pad_counter,
            &format!("{}_offset", buf.name),
            FieldType::U32,
        );
        emit_field(
            &mut out,
            &mut offset,
            &mut pad_counter,
            &format!("{}_length", buf.name),
            FieldType::U32,
        );
    }

    out.push_str("}\n");
    out
}

/// Generate the WGSL function that loads a `MaterialData`
/// (`struct_name`) struct from the renderer-wide `materials: array<u32>`
/// storage buffer, given the byte offset of the material's slot.
///
/// Walks the same field offsets [`generate_wgsl_struct`] +
/// [`pack_uniform_values`] use so the loader reads back exactly what
/// the packer wrote. The function signature is:
///
/// ```wgsl
/// fn <fn_name>(byte_offset: u32) -> <struct_name> { ... }
/// ```
///
/// Requires `material_load_u32(idx)` / `material_load_f32(idx)` to be
/// in scope (they are — `shared_wgsl/material.wgsl` declares them
/// alongside the `materials` binding).
///
/// The slot's byte 0 is the `shader_id` u32. The struct begins at
/// the first byte aligned to the struct's natural alignment, which
/// is the max of all field alignments (≥ 4). The loader emits the
/// `base` index = `byte_offset / 4u + pre_struct_pad_words + 1u`
/// (the `+ 1u` skips the shader_id word; `pre_struct_pad_words` is
/// the alignment pad written by
/// [`crate::dynamic::DynamicMaterial::write_uniform_buffer_with_layout`]).
pub fn generate_wgsl_loader(struct_name: &str, fn_name: &str, layout: &MaterialLayout) -> String {
    let struct_align = struct_alignment(layout);
    // The shader_id is one u32 at the slot's start. The struct
    // begins at the next struct_align-aligned byte. Words from the
    // start of the slot: 1 (shader_id) + pre_pad_words.
    let pre_pad_words = (align_up(4, struct_align) - 4) / 4;
    let leading_skip = 1 + pre_pad_words; // u32 words to skip from slot start

    let mut out = String::new();
    out.push_str(&format!(
        "fn {}(byte_offset: u32) -> {} {{\n",
        fn_name, struct_name
    ));
    out.push_str(&format!(
        "    let base = byte_offset / 4u + {}u;\n",
        leading_skip
    ));
    out.push_str(&format!("    return {}(\n", struct_name));

    let mut field_byte_offset: usize = 0;

    let emit = |out: &mut String, name: &str, ty: FieldType, byte_offset: &mut usize| {
        *byte_offset = align_up(*byte_offset, ty.align());
        let word = *byte_offset / 4;
        match ty {
            FieldType::F32 => {
                out.push_str(&format!("        material_load_f32(base + {}u), // {name}\n", word));
            }
            FieldType::U32 | FieldType::Bool => {
                out.push_str(&format!("        material_load_u32(base + {}u), // {name}\n", word));
            }
            FieldType::Vec2 => {
                out.push_str(&format!(
                    "        vec2<f32>(material_load_f32(base + {}u), material_load_f32(base + {}u)), // {name}\n",
                    word, word + 1
                ));
            }
            FieldType::Vec3 | FieldType::Color3 => {
                out.push_str(&format!(
                    "        vec3<f32>(material_load_f32(base + {}u), material_load_f32(base + {}u), material_load_f32(base + {}u)), // {name}\n",
                    word, word + 1, word + 2
                ));
            }
            FieldType::Vec4 | FieldType::Color4 => {
                out.push_str(&format!(
                    "        vec4<f32>(material_load_f32(base + {}u), material_load_f32(base + {}u), material_load_f32(base + {}u), material_load_f32(base + {}u)), // {name}\n",
                    word, word + 1, word + 2, word + 3
                ));
            }
            FieldType::IVec2 => {
                out.push_str(&format!(
                    "        vec2<i32>(i32(material_load_u32(base + {}u)), i32(material_load_u32(base + {}u))), // {name}\n",
                    word, word + 1
                ));
            }
            FieldType::IVec3 => {
                out.push_str(&format!(
                    "        vec3<i32>(i32(material_load_u32(base + {}u)), i32(material_load_u32(base + {}u)), i32(material_load_u32(base + {}u))), // {name}\n",
                    word, word + 1, word + 2
                ));
            }
            FieldType::IVec4 => {
                out.push_str(&format!(
                    "        vec4<i32>(i32(material_load_u32(base + {}u)), i32(material_load_u32(base + {}u)), i32(material_load_u32(base + {}u)), i32(material_load_u32(base + {}u))), // {name}\n",
                    word, word + 1, word + 2, word + 3
                ));
            }
            FieldType::Mat3 => {
                // 3 columns × vec3 with 16-byte stride per column
                // (4 u32 words per column including the trailing pad).
                out.push_str(&format!(
                    "        mat3x3<f32>(\n            vec3<f32>(material_load_f32(base + {0}u), material_load_f32(base + {1}u), material_load_f32(base + {2}u)),\n            vec3<f32>(material_load_f32(base + {3}u), material_load_f32(base + {4}u), material_load_f32(base + {5}u)),\n            vec3<f32>(material_load_f32(base + {6}u), material_load_f32(base + {7}u), material_load_f32(base + {8}u)),\n        ), // {name}\n",
                    word, word + 1, word + 2, word + 4, word + 5, word + 6, word + 8, word + 9, word + 10
                ));
            }
            FieldType::Mat4 => {
                // 4 columns × vec4, 16 u32 words contiguous.
                out.push_str(&format!(
                    "        mat4x4<f32>(\n            vec4<f32>(material_load_f32(base + {0}u), material_load_f32(base + {1}u), material_load_f32(base + {2}u), material_load_f32(base + {3}u)),\n            vec4<f32>(material_load_f32(base + {4}u), material_load_f32(base + {5}u), material_load_f32(base + {6}u), material_load_f32(base + {7}u)),\n            vec4<f32>(material_load_f32(base + {8}u), material_load_f32(base + {9}u), material_load_f32(base + {10}u), material_load_f32(base + {11}u)),\n            vec4<f32>(material_load_f32(base + {12}u), material_load_f32(base + {13}u), material_load_f32(base + {14}u), material_load_f32(base + {15}u)),\n        ), // {name}\n",
                    word, word + 1, word + 2, word + 3,
                    word + 4, word + 5, word + 6, word + 7,
                    word + 8, word + 9, word + 10, word + 11,
                    word + 12, word + 13, word + 14, word + 15
                ));
            }
        }
        *byte_offset += ty.size();
    };

    for field in &layout.uniforms {
        emit(&mut out, &field.name, field.ty, &mut field_byte_offset);
    }
    for tex in &layout.textures {
        emit(
            &mut out,
            &format!("{}_index", tex.name),
            FieldType::U32,
            &mut field_byte_offset,
        );
    }
    for buf in &layout.buffers {
        emit(
            &mut out,
            &format!("{}_offset", buf.name),
            FieldType::U32,
            &mut field_byte_offset,
        );
        emit(
            &mut out,
            &format!("{}_length", buf.name),
            FieldType::U32,
            &mut field_byte_offset,
        );
    }

    out.push_str("    );\n}\n");
    out
}

/// The struct alignment derived from a layout — `max(align(field_i))`,
/// floored at 4. Used by [`generate_wgsl_loader`] to compute the
/// pre-struct padding count.
pub fn struct_alignment(layout: &MaterialLayout) -> usize {
    let mut align: usize = 4;
    for f in &layout.uniforms {
        if f.ty.align() > align {
            align = f.ty.align();
        }
    }
    // Texture / buffer slot fields are u32 (align 4) — they don't
    // bump the struct alignment.
    align
}

/// Total layout size in bytes — including trailing padding required by
/// WGSL's struct-size rounding (struct size is rounded up to its
/// alignment). Used as the size of one material's slice in the
/// renderer's `materials` storage buffer.
pub fn layout_size(layout: &MaterialLayout) -> usize {
    let mut offset: usize = 0;
    let mut struct_align: usize = 4;

    let mut walk = |ty: FieldType| {
        offset = align_up(offset, ty.align());
        offset += ty.size();
        if ty.align() > struct_align {
            struct_align = ty.align();
        }
    };

    for field in &layout.uniforms {
        walk(field.ty);
    }
    for _ in &layout.textures {
        walk(FieldType::U32);
    }
    for _ in &layout.buffers {
        walk(FieldType::U32);
        walk(FieldType::U32);
    }

    align_up(offset, struct_align)
}

/// Pack a list of per-instance [`UniformValue`]s into `out` at the
/// alignment offsets the WGSL struct expects.
///
/// `values.len()` must equal `layout.uniforms.len()` and each value's
/// [`UniformValue::field_type`] must match the layout entry's
/// [`FieldType`] (panics on mismatch — these are caller bugs).
///
/// Does NOT write the texture / buffer tails — call
/// [`pack_texture_indices`] and [`pack_buffer_offsets`] afterwards to
/// append those.
///
/// Does NOT write trailing struct padding — call [`pad_tail_to_struct_size`]
/// to round the buffer up to [`layout_size`] when needed.
pub fn pack_uniform_values(layout: &MaterialLayout, values: &[UniformValue], out: &mut Vec<u8>) {
    assert_eq!(
        values.len(),
        layout.uniforms.len(),
        "pack_uniform_values: got {} values for a layout of {} uniforms",
        values.len(),
        layout.uniforms.len(),
    );
    let start = out.len();
    for (field, value) in layout.uniforms.iter().zip(values) {
        assert_eq!(
            field.ty,
            value.field_type(),
            "pack_uniform_values: layout field `{}` is {:?} but value is {:?}",
            field.name,
            field.ty,
            value.field_type(),
        );
        align_buffer_to(out, field.ty.align(), start);
        write_uniform_bytes(out, value);
    }
}

/// Append one `u32` per texture slot at the byte position the WGSL
/// struct's `<tex>_index` field sits at.
///
/// `indices.len()` must equal `layout.textures.len()`.
///
/// The caller is responsible for having packed the uniform tail first
/// via [`pack_uniform_values`]; the writer aligns to `u32` at the slot's
/// natural alignment relative to its own struct-start offset.
pub fn pack_texture_indices(layout: &MaterialLayout, indices: &[u32], out: &mut Vec<u8>) {
    assert_eq!(
        indices.len(),
        layout.textures.len(),
        "pack_texture_indices: got {} indices for a layout of {} textures",
        indices.len(),
        layout.textures.len(),
    );
    // The caller is expected to have already packed the uniform tail
    // starting at `out`'s base; align_buffer_to uses the *current* out
    // length as a proxy for "next field offset within the struct".
    let start = struct_start(out, layout);
    for &index in indices {
        align_buffer_to(out, FieldType::U32.align(), start);
        out.extend_from_slice(&index.to_le_bytes());
    }
}

/// Append `(offset, length)` u32 pairs in buffer-slot declaration
/// order.
///
/// `offsets.len()` must equal `layout.buffers.len()`. Each tuple is
/// `(offset_in_extras_pool, length_in_u32_words)`.
pub fn pack_buffer_offsets(layout: &MaterialLayout, offsets: &[(u32, u32)], out: &mut Vec<u8>) {
    assert_eq!(
        offsets.len(),
        layout.buffers.len(),
        "pack_buffer_offsets: got {} pairs for a layout of {} buffers",
        offsets.len(),
        layout.buffers.len(),
    );
    let start = struct_start(out, layout);
    for &(offset, length) in offsets {
        align_buffer_to(out, FieldType::U32.align(), start);
        out.extend_from_slice(&offset.to_le_bytes());
        align_buffer_to(out, FieldType::U32.align(), start);
        out.extend_from_slice(&length.to_le_bytes());
    }
}

/// Pad `out` so its current length minus `struct_start_offset` equals
/// [`layout_size`] — i.e. the slot's trailing WGSL padding is in place.
pub fn pad_tail_to_struct_size(
    layout: &MaterialLayout,
    out: &mut Vec<u8>,
    struct_start_offset: usize,
) {
    let expected = struct_start_offset + layout_size(layout);
    while out.len() < expected {
        out.push(0);
    }
}

// ─────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────

fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

fn align_buffer_to(out: &mut Vec<u8>, align: usize, struct_start: usize) {
    let cur_struct_offset = out.len() - struct_start;
    let pad = align_up(cur_struct_offset, align) - cur_struct_offset;
    for _ in 0..pad {
        out.push(0);
    }
}

/// The byte offset where the current layout's struct started in `out`.
///
/// In Phase 2 the struct always starts at the *beginning* of `out` —
/// the renderer's `write_uniform_buffer` will hand us a fresh
/// `Vec<u8>` (after writing the shader_id + alignment prefix) and we
/// pack from offset 0. We compute it from a known invariant rather
/// than threading it through every helper to keep the signatures
/// stable for Phases 3 / 4.
fn struct_start(_out: &[u8], _layout: &MaterialLayout) -> usize {
    0
}

fn write_uniform_bytes(out: &mut Vec<u8>, value: &UniformValue) {
    match value {
        UniformValue::F32(v) => out.extend_from_slice(&v.to_le_bytes()),
        UniformValue::Vec2(v) => {
            out.extend_from_slice(&v[0].to_le_bytes());
            out.extend_from_slice(&v[1].to_le_bytes());
        }
        UniformValue::Vec3(v) | UniformValue::Color3(v) => {
            // vec3 payload is 12 bytes; trailing 4-byte padding is added
            // by the NEXT field's alignment step, not here. This means
            // a trailing vec3 leaves the buffer at offset (struct_start +
            // ...12); `pad_tail_to_struct_size` rounds the slot to its
            // struct alignment when called.
            out.extend_from_slice(&v[0].to_le_bytes());
            out.extend_from_slice(&v[1].to_le_bytes());
            out.extend_from_slice(&v[2].to_le_bytes());
        }
        UniformValue::Vec4(v) | UniformValue::Color4(v) => {
            for &c in v {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        UniformValue::U32(v) => out.extend_from_slice(&v.to_le_bytes()),
        UniformValue::IVec2(v) => {
            out.extend_from_slice(&v[0].to_le_bytes());
            out.extend_from_slice(&v[1].to_le_bytes());
        }
        UniformValue::IVec3(v) => {
            // Same trailing-padding rule as Vec3.
            out.extend_from_slice(&v[0].to_le_bytes());
            out.extend_from_slice(&v[1].to_le_bytes());
            out.extend_from_slice(&v[2].to_le_bytes());
        }
        UniformValue::IVec4(v) => {
            for &c in v {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        UniformValue::Mat3(v) => {
            // mat3x3<f32> is laid out as 3 columns; each column is a
            // vec3<f32> at 16-byte alignment — so we write 3 f32s then
            // 4 bytes of padding, three times. Authors pass the matrix
            // as 9 column-major f32s.
            for col in 0..3 {
                for row in 0..3 {
                    out.extend_from_slice(&v[col * 3 + row].to_le_bytes());
                }
                // 4-byte column padding to round to 16.
                out.extend_from_slice(&0u32.to_le_bytes());
            }
        }
        UniformValue::Mat4(v) => {
            for &c in v {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        UniformValue::Bool(v) => out.extend_from_slice(&(*v as u32).to_le_bytes()),
    }
}

// ─────────────────────────────────────────────────────────────────────
// Unit tests — load-bearing. Wrong alignment math = silent rendering
// garbage. Cover every FieldType in isolation, then exercise the
// mixed-alignment corner cases (vec3 padding, mat3 stride, bool → u32,
// mixed uniform-texture-buffer tails).
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ufield(name: &str, ty: FieldType) -> UniformFieldRuntime {
        UniformFieldRuntime {
            name: name.to_string(),
            ty,
        }
    }
    fn tslot(name: &str) -> TextureSlotRuntime {
        TextureSlotRuntime {
            name: name.to_string(),
        }
    }
    fn bslot(name: &str) -> BufferSlotRuntime {
        BufferSlotRuntime {
            name: name.to_string(),
        }
    }

    #[test]
    fn align_helpers() {
        assert_eq!(align_up(0, 4), 0);
        assert_eq!(align_up(1, 4), 4);
        assert_eq!(align_up(4, 4), 4);
        assert_eq!(align_up(5, 4), 8);
        assert_eq!(align_up(13, 16), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
    }

    #[test]
    fn field_type_align_size_table() {
        for (ty, align, size) in [
            (FieldType::F32, 4, 4),
            (FieldType::Vec2, 8, 8),
            (FieldType::Vec3, 16, 12),
            (FieldType::Vec4, 16, 16),
            (FieldType::U32, 4, 4),
            (FieldType::IVec2, 8, 8),
            (FieldType::IVec3, 16, 12),
            (FieldType::IVec4, 16, 16),
            (FieldType::Mat3, 16, 48),
            (FieldType::Mat4, 16, 64),
            (FieldType::Color3, 16, 12),
            (FieldType::Color4, 16, 16),
            (FieldType::Bool, 4, 4),
        ] {
            assert_eq!(ty.align(), align, "align({ty:?})");
            assert_eq!(ty.size(), size, "size({ty:?})");
        }
    }

    #[test]
    fn pack_each_field_type_in_isolation() {
        for (ty, value, expected_size) in [
            (FieldType::F32, UniformValue::F32(1.5), 4),
            (FieldType::Vec2, UniformValue::Vec2([1.0, 2.0]), 8),
            (FieldType::Vec3, UniformValue::Vec3([1.0, 2.0, 3.0]), 12),
            (FieldType::Vec4, UniformValue::Vec4([1.0; 4]), 16),
            (FieldType::U32, UniformValue::U32(42), 4),
            (FieldType::IVec2, UniformValue::IVec2([1, 2]), 8),
            (FieldType::IVec3, UniformValue::IVec3([1, 2, 3]), 12),
            (FieldType::IVec4, UniformValue::IVec4([1, 2, 3, 4]), 16),
            (FieldType::Mat3, UniformValue::Mat3([0.0; 9]), 48),
            (FieldType::Mat4, UniformValue::Mat4([0.0; 16]), 64),
            (FieldType::Color3, UniformValue::Color3([0.0; 3]), 12),
            (FieldType::Color4, UniformValue::Color4([0.0; 4]), 16),
            (FieldType::Bool, UniformValue::Bool(true), 4),
        ] {
            let layout = MaterialLayout {
                uniforms: vec![ufield("f", ty)],
                ..Default::default()
            };
            let mut out = Vec::new();
            pack_uniform_values(&layout, &[value], &mut out);
            assert_eq!(
                out.len(),
                expected_size,
                "{ty:?} packed to wrong byte length"
            );
        }
    }

    #[test]
    fn vec3_padding_against_following_field() {
        // [F32, Vec3, F32] — the Vec3 lands at offset 16 (next align),
        // the trailing F32 lands at offset 28 (12 bytes for the vec3
        // payload, no padding because F32 needs only 4-byte align).
        let layout = MaterialLayout {
            uniforms: vec![
                ufield("a", FieldType::F32),
                ufield("v", FieldType::Vec3),
                ufield("b", FieldType::F32),
            ],
            ..Default::default()
        };

        let values = [
            UniformValue::F32(1.0),
            UniformValue::Vec3([2.0, 3.0, 4.0]),
            UniformValue::F32(5.0),
        ];
        let mut out = Vec::new();
        pack_uniform_values(&layout, &values, &mut out);
        // F32 at 0, 12 bytes of padding (4 ➜ 16), Vec3 payload (12),
        // F32 at 28 — total 32 bytes.
        assert_eq!(out.len(), 32);
        assert_eq!(&out[0..4], &1.0_f32.to_le_bytes());
        // padding between 4..16 is all zeros
        assert!(out[4..16].iter().all(|&b| b == 0));
        assert_eq!(&out[16..20], &2.0_f32.to_le_bytes());
        assert_eq!(&out[20..24], &3.0_f32.to_le_bytes());
        assert_eq!(&out[24..28], &4.0_f32.to_le_bytes());
        assert_eq!(&out[28..32], &5.0_f32.to_le_bytes());
    }

    #[test]
    fn two_vec3_total_size() {
        // [Vec3, Vec3] → 12 + 4 padding + 12 = 28 packed payload;
        // struct rounds to next 16 (struct align = 16) = 32.
        let layout = MaterialLayout {
            uniforms: vec![ufield("a", FieldType::Vec3), ufield("b", FieldType::Vec3)],
            ..Default::default()
        };
        let mut out = Vec::new();
        pack_uniform_values(
            &layout,
            &[UniformValue::Vec3([1.0; 3]), UniformValue::Vec3([2.0; 3])],
            &mut out,
        );
        // Packer emits 12 + 4 + 12 = 28 bytes (no trailing pad — that's
        // the slot writer's job).
        assert_eq!(out.len(), 28);
        // layout_size rounds up to struct align (16) so the slot is 32.
        assert_eq!(layout_size(&layout), 32);
    }

    #[test]
    fn mat3_then_f32() {
        // [Mat3, F32] — Mat3 is 48 bytes (3 cols × vec3 at 16-byte
        // stride, including each column's trailing pad), F32 follows
        // immediately (4-byte align).
        let layout = MaterialLayout {
            uniforms: vec![ufield("m", FieldType::Mat3), ufield("a", FieldType::F32)],
            ..Default::default()
        };
        let mut out = Vec::new();
        pack_uniform_values(
            &layout,
            &[
                UniformValue::Mat3([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]),
                UniformValue::F32(99.0),
            ],
            &mut out,
        );
        assert_eq!(out.len(), 48 + 4);
        // Column 0 = [1,2,3] then padding
        assert_eq!(&out[0..4], &1.0_f32.to_le_bytes());
        assert_eq!(&out[4..8], &2.0_f32.to_le_bytes());
        assert_eq!(&out[8..12], &3.0_f32.to_le_bytes());
        assert_eq!(&out[12..16], &[0; 4]); // column-stride padding
                                           // Column 1 = [4,5,6] then padding
        assert_eq!(&out[16..20], &4.0_f32.to_le_bytes());
        assert_eq!(&out[28..32], &[0; 4]);
        // Column 2 = [7,8,9] then padding
        assert_eq!(&out[32..36], &7.0_f32.to_le_bytes());
        assert_eq!(&out[44..48], &[0; 4]);
        // Trailing F32
        assert_eq!(&out[48..52], &99.0_f32.to_le_bytes());
    }

    #[test]
    fn bool_packs_as_u32() {
        // [Bool, F32] — bool → u32 (4 bytes), F32 immediately after.
        let layout = MaterialLayout {
            uniforms: vec![ufield("b", FieldType::Bool), ufield("f", FieldType::F32)],
            ..Default::default()
        };
        let mut out = Vec::new();
        pack_uniform_values(
            &layout,
            &[UniformValue::Bool(true), UniformValue::F32(7.5)],
            &mut out,
        );
        assert_eq!(out.len(), 8);
        assert_eq!(&out[0..4], &1u32.to_le_bytes());
        assert_eq!(&out[4..8], &7.5_f32.to_le_bytes());

        let mut out2 = Vec::new();
        pack_uniform_values(
            &layout,
            &[UniformValue::Bool(false), UniformValue::F32(0.0)],
            &mut out2,
        );
        assert_eq!(&out2[0..4], &0u32.to_le_bytes());
    }

    #[test]
    fn mixed_uniform_texture_buffer_tail() {
        // [F32 "a"] uniform + [TextureSlot "tex"] + [BufferSlot "buf"]
        // → struct is { a: f32, tex_index: u32, buf_offset: u32, buf_length: u32 }
        // → 16 bytes total, naturally tight.
        let layout = MaterialLayout {
            uniforms: vec![ufield("a", FieldType::F32)],
            textures: vec![tslot("tex")],
            buffers: vec![bslot("buf")],
        };
        assert_eq!(layout_size(&layout), 16);

        let mut out = Vec::new();
        pack_uniform_values(&layout, &[UniformValue::F32(3.5)], &mut out);
        pack_texture_indices(&layout, &[7u32], &mut out);
        pack_buffer_offsets(&layout, &[(100, 4)], &mut out);
        assert_eq!(out.len(), 16);
        assert_eq!(&out[0..4], &3.5_f32.to_le_bytes());
        assert_eq!(&out[4..8], &7u32.to_le_bytes());
        assert_eq!(&out[8..12], &100u32.to_le_bytes());
        assert_eq!(&out[12..16], &4u32.to_le_bytes());
    }

    #[test]
    fn generated_struct_minimal_case() {
        let layout = MaterialLayout {
            uniforms: vec![ufield("a", FieldType::F32)],
            textures: vec![tslot("tex")],
            buffers: vec![bslot("buf")],
        };
        let src = generate_wgsl_struct("MaterialData", &layout);
        assert!(src.contains("struct MaterialData {"));
        assert!(src.contains("    a: f32,"));
        assert!(src.contains("    tex_index: u32,"));
        assert!(src.contains("    buf_offset: u32,"));
        assert!(src.contains("    buf_length: u32,"));
    }

    #[test]
    fn generated_struct_emits_padding_for_vec3_neighbor() {
        // [F32, Vec3] needs 12 bytes of pad between F32 and Vec3 (3 u32 pads).
        let layout = MaterialLayout {
            uniforms: vec![ufield("a", FieldType::F32), ufield("v", FieldType::Vec3)],
            ..Default::default()
        };
        let src = generate_wgsl_struct("MaterialData", &layout);
        assert!(src.contains("    a: f32,"));
        assert!(src.contains("_pad_0: u32"));
        assert!(src.contains("_pad_1: u32"));
        assert!(src.contains("_pad_2: u32"));
        assert!(src.contains("    v: vec3<f32>,"));
    }

    #[test]
    fn layout_size_struct_alignment_rounding() {
        // [F32] alone → struct align is 4 → size is 4.
        let layout_f32 = MaterialLayout {
            uniforms: vec![ufield("a", FieldType::F32)],
            ..Default::default()
        };
        assert_eq!(layout_size(&layout_f32), 4);

        // [F32, Vec3] → packs to 16 + 12 = 28; struct align 16 → 32.
        let layout_padded = MaterialLayout {
            uniforms: vec![ufield("a", FieldType::F32), ufield("v", FieldType::Vec3)],
            ..Default::default()
        };
        assert_eq!(layout_size(&layout_padded), 32);
    }

    #[test]
    fn pad_tail_rounds_to_struct_size() {
        let layout = MaterialLayout {
            uniforms: vec![ufield("a", FieldType::F32), ufield("v", FieldType::Vec3)],
            ..Default::default()
        };
        let mut out = Vec::new();
        pack_uniform_values(
            &layout,
            &[UniformValue::F32(1.0), UniformValue::Vec3([0.0; 3])],
            &mut out,
        );
        assert_eq!(out.len(), 28);
        pad_tail_to_struct_size(&layout, &mut out, 0);
        assert_eq!(out.len(), 32);
    }

    #[test]
    fn empty_layout_is_zero_sized() {
        let layout = MaterialLayout::default();
        assert_eq!(layout_size(&layout), 0);
        let src = generate_wgsl_struct("MaterialData", &layout);
        assert!(src.contains("struct MaterialData {"));
        assert!(src.trim_end().ends_with("}"));
    }
}
