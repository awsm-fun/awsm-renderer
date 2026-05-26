//! In-memory edit state for the material editor.
//!
//! Holds the currently-edited [`MaterialDefinition`] + WGSL source,
//! the last successful compile (for fallback when a new compile
//! fails), and a list of compile errors to surface in the Errors pane.
//!
//! Phase 8 ships this as a `Mutable<EditState>` initialised from a
//! hard-coded scanline material. Phase 9 wires it to a real renderer
//! preview.

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
}

impl EditState {
    /// Creates the initial edit state with a hard-coded `scanline`
    /// material — the worked example from the contract docs.
    pub fn new_scanline() -> Self {
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
        Self {
            definition: Arc::new(Mutable::new(def)),
            wgsl_source: Arc::new(Mutable::new(SCANLINE_WGSL.to_string())),
            errors: Arc::new(Mutable::new(Vec::new())),
        }
    }
}

/// Hard-coded WGSL fragment for the scanline material — matches the
/// worked example in `docs/dynamic-materials/contract-opaque.md`.
const SCANLINE_WGSL: &str = r#"// scanline material
// Layout: tint, scan_freq, scan_speed, scan_strength, base texture.

let fg = frame_globals_from_raw(frame_globals_raw);
let uv = vec2<f32>(f32(input.coords.x), f32(input.coords.y))
       / vec2<f32>(f32(input.screen_dims.x), f32(input.screen_dims.y));

let scan = sin(uv.y * input.material.scan_freq
             + fg.time * input.material.scan_speed);
let overlay = mix(vec3<f32>(0.0), input.material.tint,
                  scan * input.material.scan_strength);

// (No texture sampling in this stub — the full version reads
// input.material.base_index via material_load_texture_info_raw +
// texture_pool_sample_no_mips. See contract-opaque.md.)
let color = vec3<f32>(0.5, 0.5, 0.5) + overlay;
return OpaqueShadingOutput(color, 1.0);
"#;
