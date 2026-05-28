# Test materials

Procedurally-authored dynamic materials that exercise the renderer's
`BufferSlot` + extras-pool path, the transparent dispatch routing,
and the side-by-side dynamic-vs-promoted comparison. **No external
artwork is required** — each material's shader generates its visual
output algorithmically from uniforms + (where applicable) the
extras-pool slice.

Each subfolder is the on-disk shape a `scene-editor` "Import Material"
flow or a `material-editor` `?folder=<name>` deep-link expects:

```
test-materials/
  scanline/
    material.json         schema definition (uniforms + alpha mode + …)
    shader.wgsl           authored WGSL fragment
  irregular-atlas/
    material.json
    shader.wgsl
    frames.bin            packed Vec<vec4<f32>> of UV rects + colors
  soft-glass/
    material.json
    shader.wgsl
```

## Materials

### `scanline` — opaque

The worked example from `docs/dynamic-materials/contract-opaque.md`.
Mid-gray base color with animated horizontal scanlines tinted by a
configurable color + frequency + speed + strength.

Uniforms:
- `tint: Color3` — overlay color (default mid-green)
- `scan_freq: F32` — scanline cycles per screen height
- `scan_speed: F32` — animation speed in cycles per second
- `scan_strength: F32` — overlay intensity (0..1)

### `irregular-atlas` — opaque, BufferSlot

Exercises the `BufferSlot` + extras-pool path end-to-end. Animates
through a packed list of UV rects (one per atlas cell); for each
frame, picks a cell and shades the visible quad with a procedurally-
derived color keyed on the cell index.

The procedural color generation lets this material ship without an
actual atlas texture — every cell gets a deterministic distinct hue.
A real consumer would replace the `cell_color()` helper with a
texture sample.

Uniforms:
- `frame_rate: F32` — frames per second (default 4)

BufferSlots:
- `frames: Vec<vec4<f32>>` — UV rects packed `(x, y, w, h)`.
  `frames.bin` ships 16 cells in a 4×4 grid.

### `soft-glass` — blend

First dynamic transparent material. View-angle-modulated alpha
(Schlick-style edge alpha tint, no opaque-background sampling), so
the sphere looks like a tinted glass shell.

Uniforms:
- `tint: Color3` — body tint (default pale cyan)
- `edge_alpha: F32` — alpha at grazing angles (default 0.95)
- `face_alpha: F32` — alpha at facing angles (default 0.25)

Alpha mode: `Blend`. Double-sided.

## Loading these into the editors

### material-editor (single material at a time)

Direct URL:
```
http://localhost:<port>/?folder=irregular-atlas
```

The deep-link banner at the top of the editor prompts for a one-click
"Open" gesture; pick the corresponding `assets/test-materials/<name>/`
folder from disk.

### scene-editor (all three side-by-side)

Use the Custom Materials pane → "Import Material…" three times,
picking each subfolder in turn. Then place three meshes in the scene
(sphere is ideal for soft-glass; plane / quad for scanline +
irregular-atlas) and assign one custom material to each.

## Side-by-side comparison

Once `scanline` is promoted to first-party, the side-by-side scene
puts the dynamic version next to the promoted version. The expected
byte-identical render (since the same shader runs through both code
paths) drives the visual diff test in
`docs/dynamic-materials/promotion.md`.
