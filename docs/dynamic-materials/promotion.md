# Promoting a dynamic material to first-party

Once a custom material has proven itself, you promote it to first-party
by porting the on-disk `material.json` layout to a typed Rust struct
and the WGSL fragment to a `&str` constant. This document walks the
mechanics using the **scanline** material as the worked example.

The end shape mirrors the most-recent first-party material —
[`FlipBookMaterial`](../../crates/materials/src/flipbook.rs) — so
referencing that file alongside this walkthrough makes the moving
parts concrete.

---

## Why promote at all?

| Concern              | Dynamic                                  | First-party                                     |
|----------------------|------------------------------------------|-------------------------------------------------|
| Dispatch cost        | One shader_id lookup → dynamic-arm match | Static per-shader-id pipeline                   |
| Type safety          | Opaque byte buffer driven by JSON layout | Typed Rust struct with `impl MaterialShader`    |
| Cargo feature gating | Always compiled in                       | Behind a Cargo feature; can be turned off       |
| Shader-cache key     | `dispatch_hash` changes on every reg     | Stable; bit-identical across builds             |

The performance delta is small (the dynamic-arm match is one if/else
on `shader_id.is_dynamic()`); the durability + clarity win is the
real driver.

---

## The contract is the same

Both dynamic and promoted versions implement
[`MaterialShader`](../../crates/materials/src/shader.rs) and write into
the same per-material storage buffer with the same shader_id prefix
word. The dispatch table on the renderer side calls the matching
shading function for the material's `shader_id` — promoted materials
get their own const-named arm; dynamic materials share the
`else if shader_id.is_dynamic()` fallback.

The promotion is mechanical *exactly because* the contract doesn't
change:

1. **Storage layout** — the byte order
   [`generate_wgsl_struct`](../../crates/materials/src/dynamic_layout.rs)
   would have produced is what the promoted material's
   `write_uniform_buffer` should match. The dynamic-material packer is
   the reference; the hand-written one is the optimised version.
2. **WGSL fragment** — paste the author's `shader.wgsl` body into the
   promoted material's `WGSL_FRAGMENT` `const &str`, wrapping any
   author-declared helpers at module scope.
3. **Cargo feature** — gate the new module on
   `awsm-materials/<material-name>` so consumers can opt out.

---

## Worked example: promoting `scanline`

The `scanline` material was authored as:

```
scanline/
├── material.json
├── shader.wgsl
└── assets/base.png
```

with `material.json`:

```json
{
  "name": "scanline",
  "version": 1,
  "alpha_mode": "opaque",
  "double_sided": false,
  "uniforms": [
    { "name": "tint",          "ty": "color3", "default": { "kind": "color3", "value": [0.6, 0.9, 0.6] } },
    { "name": "scan_freq",     "ty": "f32",    "default": { "kind": "f32",    "value": 80.0 } },
    { "name": "scan_speed",    "ty": "f32",    "default": { "kind": "f32",    "value": 0.5 } },
    { "name": "scan_strength", "ty": "f32",    "default": { "kind": "f32",    "value": 0.3 } }
  ],
  "textures": [{ "name": "base", "default": "assets/base.png" }],
  "buffers":  []
}
```

### Step 1 — typed Rust struct + `MaterialShader` impl

Add `crates/materials/src/scanline.rs` (behind a `scanline` Cargo
feature):

```rust
use crate::{
    shader::MaterialShader,
    writer::{write, write_material_texture},
    MaterialAlphaMode, MaterialShaderId, MaterialTexture, TextureContext,
};

pub const WGSL_FRAGMENT: &str = include_str!("wgsl/scanline_material.wgsl");

#[derive(Clone, Debug)]
pub struct ScanlineMaterial {
    pub base_tex: Option<MaterialTexture>,
    pub tint: [f32; 3],
    pub scan_freq: f32,
    pub scan_speed: f32,
    pub scan_strength: f32,
    alpha_mode: MaterialAlphaMode,
    double_sided: bool,
}

impl ScanlineMaterial {
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
    pub fn double_sided(&self) -> bool { self.double_sided }
    pub fn alpha_cutoff(&self) -> Option<f32> { None }
}

impl MaterialShader for ScanlineMaterial {
    fn shader_id(&self) -> MaterialShaderId { /* see MaterialShaderId promotion below */ }
    fn wgsl_fragment(&self) -> &'static str { WGSL_FRAGMENT }
    fn alpha_mode(&self) -> MaterialAlphaMode { self.alpha_mode }
    fn is_transparency_pass(&self) -> bool { false }
    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, data: &mut Vec<u8>) {
        // BYTE LAYOUT — must produce bit-identical output to the
        // dynamic packer's pack_uniform_values walk for the same
        // material.json. See the smoke test below.
        write(data, self.shader_id().as_u32().into()); // word 0: shader_id
        // Padding to 16-byte alignment because the first field
        // (tint: Color3 → vec3) requires it.
        for _ in 0..3 { write(data, 0u32.into()); }
        // tint (vec3, 12 bytes) + 4-byte trailing pad
        write(data, self.tint[0].into());
        write(data, self.tint[1].into());
        write(data, self.tint[2].into());
        write(data, 0u32.into()); // pad
        write(data, self.scan_freq.into());
        write(data, self.scan_speed.into());
        write(data, self.scan_strength.into());
        write_material_texture(data, self.base_tex.as_ref(), ctx);
    }
}
```

### Step 2 — promote `MaterialShaderId`

Add a new associated constant to
[`MaterialShaderId`](../../crates/materials/src/shader_id.rs) in the
reserved `5..DYNAMIC_START` range:

```rust
pub const SCANLINE: Self = Self(5);
```

Update the registry entry list to include the new id when the
`scanline` Cargo feature is enabled:

```rust
// crates/materials/src/registry.rs
#[cfg(feature = "scanline")]
MaterialEntry {
    shader_id: MaterialShaderId::SCANLINE,
    wgsl_fragment: crate::scanline::WGSL_FRAGMENT,
    name: "scanline",
},
```

And extend the wgsl_const_name match:

```rust
} else if self == Self::SCANLINE {
    Some("SHADER_ID_SCANLINE")
}
```

### Step 3 — WGSL fragment lives at `crates/materials/src/wgsl/scanline_material.wgsl`

The dynamic version's `shader.wgsl` body becomes the contents of the
file. Move any helper functions to the top of the file (outside the
shading function). The first-party version is called directly by the
opaque kernel — there's no wrapper to live inside, so all declarations
are module-scope.

### Step 4 — Material enum + dispatch routing

Add a variant to
[`Material`](../../crates/renderer/src/materials.rs):

```rust
pub enum Material {
    Pbr(Box<PbrMaterial>),
    Unlit(UnlitMaterial),
    Toon(Box<ToonMaterial>),
    FlipBook(Box<FlipBookMaterial>),
    #[cfg(feature = "scanline")]
    Scanline(Box<ScanlineMaterial>),
    Custom(Box<DynamicMaterial>),
}
```

Add the new arm to every match site (`shader_id`,
`is_transparency_pass`, `double_sided`, `has_transmission`,
`uniform_buffer_data`).

Extend the opaque compute.wgsl template with a new `{% else if %}`
arm calling `compute_scanline_lit_color(...)` (mirroring the
existing PBR / Unlit / Toon / FlipBook arms).

### Step 5 — Migration path for existing projects

Projects that previously referenced the material as
`custom_material: { material: "scanline", ... }` need a one-time
migration: change the scene's per-mesh material reference from the
`custom_material` field to the new typed equivalent. There is **no
runtime auto-detection** — the registration-time name-collision
check would add complexity (the dynamic id and the first-party id
would both exist for the same `name`); manual migration is the
simpler convention.

```diff
- "custom_material": { "material": "scanline", "uniform_overrides": {} },
+ // After promotion: switch the inline_material's shading or add a
+ // Scanline variant to MaterialShading and reference it as the
+ // primary material — see scene-schema/src/material.rs for the
+ // typed-material schema's shape.
```

The scene-editor's per-mesh material picker will surface the
promoted material under its typed name automatically (the
`MaterialShading` enum's discriminator drives the dropdown).

### Step 6 — Promotion smoke test

The load-bearing assertion that promotion is mechanical:
**byte-identical write_uniform_buffer output** between the dynamic
and promoted versions for the same input parameters.

```rust
#[test]
fn promotion_byte_identical() {
    let layout = /* MaterialLayout matching the scanline material.json */;
    let values = vec![
        UniformValue::Color3([0.6, 0.9, 0.6]),
        UniformValue::F32(80.0),
        UniformValue::F32(0.5),
        UniformValue::F32(0.3),
    ];
    let dynamic = DynamicMaterial::new(
        MaterialShaderId::SCANLINE,
        &layout,
        values,
    );
    let promoted = ScanlineMaterial::new();

    let stub_ctx = StubTextureContext::new();
    let stub_dyn_ctx = StubDynamicMaterialContext::new(&layout);

    let mut dynamic_bytes = Vec::new();
    dynamic.write_uniform_buffer_with_layout(&stub_dyn_ctx, &mut dynamic_bytes);

    let mut promoted_bytes = Vec::new();
    MaterialShader::write_uniform_buffer(&promoted, &stub_ctx, &mut promoted_bytes);

    assert_eq!(dynamic_bytes, promoted_bytes,
        "promoted scanline byte layout differs from dynamic packer");
}

#[test]
fn promotion_wgsl_identical() {
    // The dynamic's shader.wgsl source and the promoted's WGSL_FRAGMENT
    // const should be byte-identical text.
    assert_eq!(SCANLINE_DYNAMIC_WGSL, ScanlineMaterial::WGSL_FRAGMENT);
}
```

If either test fails, the byte order / WGSL text drifted — the
promotion isn't mechanical anymore and you've found a contract leak
worth fixing in the dynamic packer.

---

## What promotion does NOT change

- The renderer's per-shader-id pipeline architecture — the promoted
  material gets its own static pipeline arm where the dynamic version
  had the `is_dynamic()` fallback. Same dispatch shape.
- The shader cache key — promoted materials' `dispatch_hash`
  contribution moves from the dynamic-registry side to the
  first-party side, but the resulting hash is stable across builds
  (no longer changes when other dynamic materials register / leave).
- Authored scenes — projects that referenced the material under its
  registered name need the one-step rename above; the data shape
  (`uniform_overrides`, `texture_overrides`, `buffer_overrides`) is
  identical because both versions read from the same per-material
  storage layout.
