# awsm-materials

Pluggable material shaders for the `awsm-renderer` visibility-buffer pipeline.

## What this crate is

A small Rust + WGSL package that defines the `MaterialShader` trait and the
material implementations (`PbrMaterial`, `UnlitMaterial`, `ToonMaterial`, …)
that the renderer dispatches against in its visibility-buffer compute pass
and transparent fragment shader.

Each material is a Cargo feature:

| Feature        | Default | Material         |
| -------------- | ------- | ---------------- |
| `pbr-standard` | yes     | `PbrMaterial`    |
| `unlit`        | yes     | `UnlitMaterial`  |
| `toon`         | yes     | `ToonMaterial`   |

Adding a new material is **one new file + one feature entry + one trait impl**,
with zero edits to `awsm-renderer`.

## Why this crate exists

`awsm-renderer`'s visibility-buffer architecture is its superpower: many opaque
materials shade in a single compute dispatch, switching on a per-fragment
`shader_id`. The historical cost was that the switch and every WGSL helper /
Rust struct / buffer-writer / bind-group entry were hardcoded across
`awsm-renderer`. Moving materials into their own crate behind a trait + Cargo
features turns that into a registry the renderer walks at template time.

The visibility-buffer scaling property is preserved — all enabled materials
still shade in one compute dispatch via the same `shader_id` switch, now
**generated from the registry** rather than hand-written.

## The trait contract

`MaterialShader` is the public ABI of every shading model:

```rust
pub trait MaterialShader {
    fn shader_id(&self) -> MaterialShaderId;
    fn wgsl_fragment(&self) -> &'static str;
    fn alpha_mode(&self) -> MaterialAlphaMode;
    fn is_transparency_pass(&self) -> bool;
    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, out: &mut Vec<u8>);
    fn texture_slots(&self) -> &'static [TextureSlotDecl];
}
```

- `shader_id` is the per-material stable id baked into the first u32 of the
  packed uniform buffer payload and dispatched against in WGSL.
- `wgsl_fragment` returns the material's WGSL helper module
  (`compute_*_color`, `apply_*_lighting`, etc.). The renderer concatenates
  every enabled material's fragment into one buffer and feeds it to the
  shader template as the `{{ materials_wgsl }}` substitution variable.
- `alpha_mode` + `is_transparency_pass` classify the material for the
  opaque-vs-transparent render pass split.
- `write_uniform_buffer` packs the material's authored parameters into the
  byte buffer the visibility-buffer dispatch reads. The `TextureContext`
  trait abstracts the renderer's `Textures` slotmap so this crate doesn't
  depend on `awsm-renderer`.
- `texture_slots` declares which textures the material binds. The renderer
  builds the union of declared slots across enabled materials when laying
  out bind groups.

## How registration works (compile-time, not dynamic)

This crate registers materials at **compile time** via Cargo features. The
registry is a `const` slice the renderer walks during shader templating.

We do not support runtime / user-supplied materials in this crate. Paths to
add that later are documented in the overhaul plan (`docs/plans/editor-
renderer-overhaul.md`): editor-time WGSL hot-reload, or tile-classified
material shading. Neither is built speculatively.

## Why askama-substitution, not askama iteration

The renderer uses [askama](https://djc.github.io/askama/) for compile-time
WGSL templating. askama's `{% include %}` directive requires literal path
strings — it cannot iterate over a registry. So the registry feeds the
shader via a different path: **Rust-side concatenation + askama variable
substitution**.

The renderer collects every enabled material's `wgsl_fragment()`,
concatenates them, generates the `if shader_id == X { … }` dispatch table
similarly, and passes both to the existing askama templates as two new
variables (`{{ materials_wgsl }}`, `{{ shader_id_dispatch }}`). Fixed
scaffolding (camera, lighting, math helpers) stays as `{% include %}` lines
— no change. End-user DX is identical to "askama iterates a registry";
only the mechanism differs.

## Dependencies

`awsm-materials` depends on `awsm-renderer-core` only. It does **not**
depend on `awsm-renderer`. The opaque slotmap key types
(`TextureKey` / `SamplerKey` / `TextureTransformKey`) live in
`awsm-renderer-core::keys` so this crate can reference textures + samplers
without dragging in the GPU device or scene graph.

`awsm-renderer` depends on `awsm-materials`. No circularity.
