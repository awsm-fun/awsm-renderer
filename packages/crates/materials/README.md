# awsm-renderer-materials

Pluggable material shaders for the `awsm-renderer` visibility-buffer pipeline.

## What this crate is

A small Rust + WGSL package that defines the `MaterialShader` trait and the
first-party material implementations (`PbrMaterial`, `UnlitMaterial`,
`ToonMaterial`, `FlipBookMaterial`) that the renderer dispatches against in its
visibility-buffer compute pass and transparent fragment shader. It also ships a
generic `DynamicMaterial` interpreter for runtime-registered custom WGSL
materials (see "Compile-time first-party + runtime dynamic materials" below).

Each first-party material is a Cargo feature:

| Feature        | Default | Material            |
| -------------- | ------- | ------------------- |
| `pbr-standard` | yes     | `PbrMaterial`       |
| `unlit`        | yes     | `UnlitMaterial`     |
| `toon`         | yes     | `ToonMaterial`      |
| `flipbook`     | yes     | `FlipBookMaterial`  |

Adding a new first-party material is **one new module + one feature entry + one
`MaterialEntry` in `registry::enabled_materials()`**, with zero edits to
`awsm-renderer`.

## Why this crate exists

`awsm-renderer`'s visibility-buffer architecture is its superpower: many opaque
materials shade in a single compute dispatch, switching on a per-fragment
`shader_id`. The historical cost was that the switch and every WGSL helper /
Rust struct / buffer-writer / bind-group entry were hardcoded across
`awsm-renderer`. Moving materials into their own crate behind a trait + Cargo
features turns that into a registry the renderer walks at template time.

The visibility-buffer scaling property is preserved — materials are dispatched
against by `shader_id`, with the WGSL fragments and the `SHADER_ID_X` consts now
**generated from the registry** (`registry::build_materials_wgsl*` /
`build_shader_id_consts`) rather than hand-written.

## The trait contract

`MaterialShader` is the public ABI of every shading model:

```rust
pub trait MaterialShader {
    fn shader_id(&self) -> MaterialShaderId;
    fn shader_includes(&self) -> ShaderIncludes;
    fn fragment_inputs(&self) -> FragmentInputs;
    fn wgsl_fragment(&self) -> &'static str;
    fn alpha_mode(&self) -> MaterialAlphaMode;
    fn is_transparency_pass(&self) -> bool;
    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, out: &mut Vec<u8>);
}
```

- `shader_id` is the per-material stable id baked into the first u32 of the
  packed uniform buffer payload and dispatched against in WGSL.
- `shader_includes` declares the shared shader modules the material's body
  uses; the renderer compiles the transitive closure and emits only those
  `{% include %}`s — a material that returns `ShaderIncludes::empty()` pulls
  no shared shading code at all.
- `fragment_inputs` declares the pre-shade fragment inputs the material
  consumes, so the pass scaffolding only unpacks/computes the declared ones
  (TBN, lights, …).
- `wgsl_fragment` returns the material's WGSL helper module (its
  `*_get_material` accessor + `compute_*_color` / `compute_*_lit_color`
  functions). The renderer feeds these into the shader template as the
  `{{ materials_wgsl }}` substitution variable. Each opaque/transparent
  pipeline is specialized to one `shader_id`, so the renderer typically emits
  only the matching material's fragment (`build_materials_wgsl_filtered`); the
  unfiltered concat of every enabled material (`build_materials_wgsl`) is used
  only by the no-geometry empty kernel. A fragment must be self-contained for
  its own pipeline.
- `alpha_mode` + `is_transparency_pass` classify the material for the
  opaque-vs-transparent render pass split.
- `write_uniform_buffer` packs the material's authored parameters into the
  byte buffer the visibility-buffer dispatch reads. The `TextureContext`
  trait abstracts the renderer's `Textures` slotmap so this crate doesn't
  depend on `awsm-renderer`.

Texture bindings flow through the renderer's shared texture pool: a
material stores `MaterialTexture` keys in its uniform payload, the renderer
maps each key to `(array_index, layer_index)` at pack time
(via `writer::pack_texture_info_raw`), and the WGSL accessor samples the
pool's bind-group entry at that index. The pool's bind-group layout is
data-driven by the textures actually loaded — not by per-material slot
declarations.

## Compile-time first-party + runtime dynamic materials

There are two registration paths, with a clean split:

**First-party materials — compile-time, via Cargo features.** `pbr-standard`,
`unlit`, `toon`, and `flipbook` are gated behind features and listed in
`registry::enabled_materials()` as `MaterialEntry` descriptors (each carrying
the material's `&'static str` WGSL fragment, `ShaderIncludes`, and
`FragmentInputs`). The renderer walks this set during shader templating to
build the `{{ materials_wgsl }}` and `{{ shader_id_consts }}` substitution
variables. Their layout and WGSL are baked into the binary.

**Dynamic materials — runtime, via a generic interpreter.** A single
`DynamicMaterial` type (see `dynamic.rs`) backs *every* runtime-registered
custom material, keyed by a `MaterialShaderId`. The per-material layout +
WGSL fragment + alpha mode are **not** owned by this crate — they live in the
renderer's dynamic-material registry, plumbed back through the
`DynamicMaterialContext` trait. What this crate provides is:

- the `DynamicMaterial` instance type (per-instance uniform values, texture
  bindings, and buffer-slot data), and
- the `dynamic_layout` packer/codegen (`MaterialLayout` → WGSL `struct` +
  loader + per-texture sampler helpers via `generate_wgsl_struct` /
  `generate_wgsl_loader` / `generate_wgsl_texture_helpers`, and the
  matching byte packers `pack_uniform_values` / `pack_texture_indices` /
  `pack_buffer_offsets`).

`DynamicMaterial::write_uniform_buffer_with_layout` interprets a layout at
write time to pack a shader_id prefix, an alignment-respecting uniform tail, a
texture-index tail, and a buffer `(offset, length)` tail — the same byte
layout the generated WGSL loader reads back. (The bare
`MaterialShader::write_uniform_buffer` / `wgsl_fragment` methods are
`unreachable!` for `DynamicMaterial`; the renderer routes dynamic materials
through the layout-aware path instead.)

So: first-party = compile-time feature + `const`-ish registry, fixed layout;
dynamic = runtime-registered, layout + WGSL live in the renderer's registry,
this crate supplies the interpreter + packer.

## Why askama-substitution, not askama iteration

The renderer uses [askama](https://djc.github.io/askama/) for compile-time
WGSL templating. askama's `{% include %}` directive requires literal path
strings — it cannot iterate over a registry. So the registry feeds the
shader via a different path: **Rust-side concatenation + askama variable
substitution**.

The renderer collects the relevant material `wgsl_fragment()`s (filtered to the
pipeline's own base, or all of them for the empty kernel), concatenates them,
generates the `const SHADER_ID_X: u32 = N;` lines from the registry, and passes
both to the existing askama templates as two variables (`{{ materials_wgsl }}`,
`{{ shader_id_consts }}`). Fixed scaffolding (camera, lighting, math helpers)
stays as `{% include %}` lines — no change. End-user DX is identical to "askama
iterates a registry"; only the mechanism differs.

## Dependencies

`awsm-renderer-materials` depends on `awsm-renderer-core` only. It does **not**
depend on `awsm-renderer`. The opaque slotmap key types
(`TextureKey` / `SamplerKey` / `TextureTransformKey`) live in
`awsm-renderer-core::keys` so this crate can reference textures + samplers
without dragging in the GPU device or scene graph.

`awsm-renderer` depends on `awsm-renderer-materials`. No circularity.
