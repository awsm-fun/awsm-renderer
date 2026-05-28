//! In-memory edit state for the material editor.
//!
//! Holds the currently-edited [`MaterialDefinition`] + WGSL source,
//! the last successful compile (for fallback when a new compile
//! fails), and a list of compile errors to surface in the Errors pane.
//!
//! Phase 8 ships this as a `Mutable<EditState>` initialised from a
//! hard-coded scanline material. Phase 9 wires it to a real renderer
//! preview.

use std::collections::HashMap;
use std::sync::Arc;

use futures_signals::signal::Mutable;

use awsm_scene_schema::dynamic_material::{
    BufferSlot, FieldType, MaterialDefinition, TextureSlot, UniformField, UniformValue,
};
use awsm_scene_schema::material::MaterialAlphaMode;

/// One compile error reported by the renderer. Phase 11 parses these
/// for line/column from naga's diagnostic format.
#[derive(Clone, Debug)]
pub struct CompileError {
    /// Full naga diagnostic message (multi-line).
    pub message: String,
    /// Best-effort line number, parsed from the message.
    pub line: Option<u32>,
    /// Best-effort column number, parsed from the message.
    pub column: Option<u32>,
}

/// Edit state for the material editor.
#[derive(Clone)]
pub struct EditState {
    /// The currently-edited material definition (uniforms, textures,
    /// buffer slots, alpha mode, etc.).
    pub definition: Arc<Mutable<MaterialDefinition>>,
    /// The WGSL source text. Owned separately from `definition` since
    /// it's the bigger, more frequently-edited surface.
    pub wgsl_source: Arc<Mutable<String>>,
    /// The current list of compile errors (empty when the live shader
    /// compiled cleanly). Phase 9 populates this from
    /// `register_material` results.
    pub errors: Arc<Mutable<Vec<CompileError>>>,
    /// Number of pipeline groups currently in `Pending` state from
    /// the renderer's pipeline scheduler. Driven by the RAF tick which
    /// calls `drain_pipeline_status_events` each frame. Block A.4: a
    /// modal overlay shows while this is `> 0`.
    pub compile_pending: Arc<Mutable<usize>>,
    /// Last error string from a `Failed` status event (if any). Reset
    /// when a fresh compile batch opens — keeps the modal's
    /// "Last error" subsection scoped to the current compile cycle.
    pub compile_last_error: Arc<Mutable<Option<String>>>,
    /// Preview-canvas mesh shape. Default `Plane` matches the historic
    /// 2×2 stub; the selector in the Preview pane header lets the
    /// author switch to a curved or volumetric shape so materials
    /// that read `world_normal` / `world_tangent` non-trivially can
    /// be inspected. Updates trigger a debounced re-apply path that
    /// regenerates the preview mesh (see `recompile.rs` and
    /// `host::apply_quad_for_current_registration`).
    pub preview_mesh: Arc<Mutable<PreviewMeshKind>>,
    /// In-memory bytes for each `BufferSlot` default, keyed by slot
    /// name. Populated by the Definition pane's Buffer Converter modal
    /// (file drop / file picker / JSON paste). The recompile pipeline
    /// threads these through `MaterialRegistration.buffer_defaults`
    /// so the live preview reflects what the author dropped in,
    /// without requiring a disk write to `assets/materials/<name>/<slot>.bin`
    /// first.
    ///
    /// Slots without an entry default to empty (the
    /// `MaterialData.<slot>_length` field reads 0 in the shader,
    /// matching the contract docs).
    pub buffer_defaults: Arc<Mutable<HashMap<String, Vec<u32>>>>,
    /// The slot name currently being edited by the Buffer Converter
    /// modal (`Some("frames")` while the modal is open, `None` when
    /// the modal is closed). The Definition pane renders the modal as
    /// a child of the root layout; it watches this signal to know
    /// whether to display itself.
    pub converter_open_for_slot: Arc<Mutable<Option<String>>>,
}

/// Preview-canvas mesh shape selectable from the Preview pane header.
/// `Plane` is the default; the other variants are used when a material's
/// visual behavior depends on geometry. The meshgen primitives ship the
/// underlying mesh data — this enum is just the user-facing tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewMeshKind {
    /// 2×2 plane, the default. Matches the historic stub.
    Plane,
    /// Unit-radius sphere — the workhorse for any material that reads
    /// world_normal.
    Sphere,
    /// 1×1×1 box centered at origin.
    Box,
    /// Unit-radius × 1.5 height cylinder.
    Cylinder,
    /// Major-radius 1, tube-radius 0.3 torus.
    Torus,
}

impl PreviewMeshKind {
    /// Human-readable label for the Preview-pane dropdown.
    pub fn label(self) -> &'static str {
        match self {
            PreviewMeshKind::Plane => "Plane",
            PreviewMeshKind::Sphere => "Sphere",
            PreviewMeshKind::Box => "Box",
            PreviewMeshKind::Cylinder => "Cylinder",
            PreviewMeshKind::Torus => "Torus",
        }
    }

    /// Every kind in display order. Drives the dropdown rendering.
    pub fn all() -> &'static [PreviewMeshKind] {
        &[
            PreviewMeshKind::Plane,
            PreviewMeshKind::Sphere,
            PreviewMeshKind::Box,
            PreviewMeshKind::Cylinder,
            PreviewMeshKind::Torus,
        ]
    }
}

impl EditState {
    /// Creates the initial edit state with a hard-coded `scanline`
    /// material — the worked example from the contract docs.
    pub fn new_scanline() -> Self {
        let (def, wgsl) = starter_scanline();
        Self {
            definition: Arc::new(Mutable::new(def)),
            wgsl_source: Arc::new(Mutable::new(wgsl)),
            errors: Arc::new(Mutable::new(Vec::new())),
            compile_pending: Arc::new(Mutable::new(0)),
            compile_last_error: Arc::new(Mutable::new(None)),
            preview_mesh: Arc::new(Mutable::new(PreviewMeshKind::Plane)),
            buffer_defaults: Arc::new(Mutable::new(HashMap::new())),
            converter_open_for_slot: Arc::new(Mutable::new(None)),
        }
    }

    /// Resets the live state to the chosen starter template. Mutates
    /// `definition` + `wgsl_source` in place (instead of swapping out
    /// the whole `EditState`) so the panes' signal subscriptions stay
    /// live; the debounced-recompile loop picks the changes up on the
    /// next tick. Errors and last-error are cleared eagerly so the
    /// fresh starter doesn't show a stale-from-prior-edit list.
    pub fn reset_to(&self, starter: Starter) {
        let (def, wgsl) = match starter {
            Starter::Scanline => starter_scanline(),
            Starter::ConstantRed => starter_constant_red(),
            Starter::UnlitBaseline => starter_unlit_baseline(),
        };
        self.definition.set(def);
        self.wgsl_source.set(wgsl);
        self.errors.set(Vec::new());
        self.compile_last_error.set(None);
        self.buffer_defaults.set(HashMap::new());
        self.converter_open_for_slot.set(None);
    }
}

/// Available starter templates exposed by the File → New menu. Each
/// variant maps to a hand-authored `(MaterialDefinition, wgsl)` pair
/// below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Starter {
    /// The Phase-8 worked example — animated horizontal scanlines over
    /// a mid-gray base. Demonstrates `frame_globals.time` + multiple
    /// uniforms.
    Scanline,
    /// Minimal "constant red" body — the smallest possible runnable
    /// opaque material. Useful as a starting point for somebody who
    /// just wants the boilerplate out of the way.
    ConstantRed,
    /// Unlit pass-through of a single base color uniform — the
    /// "hello world" for a material that exposes a single tint.
    UnlitBaseline,
}

impl Starter {
    /// Human-readable label for the File → New menu entry.
    pub fn label(self) -> &'static str {
        match self {
            Starter::Scanline => "Scanline (animated)",
            Starter::ConstantRed => "Constant red (minimal)",
            Starter::UnlitBaseline => "Unlit tint (single color)",
        }
    }
}

fn starter_scanline() -> (MaterialDefinition, String) {
    let def = MaterialDefinition {
        name: "scanline".into(),
        version: 1,
        alpha_mode: MaterialAlphaMode::Opaque,
        double_sided: false,
        uniforms: vec![
            UniformField {
                name: "tint".into(),
                ty: FieldType::Color3,
                default: UniformValue::Color3([0.6, 0.9, 0.6]),
            },
            UniformField {
                name: "scan_freq".into(),
                ty: FieldType::F32,
                default: UniformValue::F32(80.0),
            },
            UniformField {
                name: "scan_speed".into(),
                ty: FieldType::F32,
                default: UniformValue::F32(0.5),
            },
            UniformField {
                name: "scan_strength".into(),
                ty: FieldType::F32,
                default: UniformValue::F32(0.3),
            },
        ],
        textures: vec![TextureSlot {
            name: "base".into(),
            default: None,
        }],
        buffers: Vec::<BufferSlot>::new(),
    };
    (def, SCANLINE_WGSL.to_string())
}

fn starter_constant_red() -> (MaterialDefinition, String) {
    let def = MaterialDefinition {
        name: "constant-red".into(),
        version: 1,
        alpha_mode: MaterialAlphaMode::Opaque,
        double_sided: false,
        uniforms: Vec::new(),
        textures: Vec::new(),
        buffers: Vec::new(),
    };
    (def, CONSTANT_RED_WGSL.to_string())
}

fn starter_unlit_baseline() -> (MaterialDefinition, String) {
    let def = MaterialDefinition {
        name: "unlit-tint".into(),
        version: 1,
        alpha_mode: MaterialAlphaMode::Opaque,
        double_sided: false,
        uniforms: vec![UniformField {
            name: "tint".into(),
            ty: FieldType::Color3,
            default: UniformValue::Color3([0.8, 0.4, 0.2]),
        }],
        textures: Vec::new(),
        buffers: Vec::new(),
    };
    (def, UNLIT_BASELINE_WGSL.to_string())
}

/// Hard-coded WGSL fragment for the scanline material — a minimal
/// stub that uses only the input fields the current
/// `OpaqueShadingInput` provides + `frame_globals_raw` (in scope from
/// the kernel's bind group). The full per-material-data access path
/// `input.material.<field>` lands once `generate_wgsl_loader`
/// emits the typed accessor; until then the author manually pulls
/// uniforms via material_load_* using input.material_offset.
const SCANLINE_WGSL: &str = r#"// scanline material — references input.material.* per the contract.
// The wrapper auto-loads MaterialData from material_offset before
// calling this fragment, so the author has typed access to every
// uniform declared in the Definition pane.
let coords_f = vec2<f32>(f32(input.coords.x), f32(input.coords.y));
let dims_f = vec2<f32>(f32(input.screen_dims.x), f32(input.screen_dims.y));
let uv = coords_f / dims_f;
let fg = frame_globals_from_raw(frame_globals_raw);
let scan = sin(uv.y * input.material.scan_freq
             + fg.time * input.material.scan_speed);
let overlay = mix(vec3<f32>(0.0), input.material.tint,
                  scan * input.material.scan_strength);
let color = vec3<f32>(0.5, 0.5, 0.5) + overlay;
return OpaqueShadingOutput(color, 1.0);
"#;

/// Minimal runnable opaque shader — no uniforms, just a constant color.
/// The smallest possible body that lights up the preview.
const CONSTANT_RED_WGSL: &str = r#"// constant-red — the minimal opaque material.
// No uniforms, no textures, no buffer slots: just a single
// hard-coded color. Use as the boilerplate-out-of-the-way starting
// point when authoring a brand-new material from scratch.
return OpaqueShadingOutput(vec3<f32>(0.9, 0.15, 0.15), 1.0);
"#;

/// Unlit pass-through that returns the single `tint` uniform as the
/// final color. One step up from `constant-red` — demonstrates the
/// `input.material.<field>` accessor.
const UNLIT_BASELINE_WGSL: &str = r#"// unlit-tint — single-uniform unlit material.
// Demonstrates the `input.material.<field>` access pattern with the
// minimum surface area. Add more uniforms in the Definition pane to
// extend; the wrapper regenerates `MaterialData` on every edit.
return OpaqueShadingOutput(input.material.tint, 1.0);
"#;
