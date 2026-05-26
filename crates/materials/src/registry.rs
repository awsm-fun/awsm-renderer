//! Registry of enabled materials.
//!
//! The renderer walks this at shader-template construction time to:
//! - Build the `{{ materials_wgsl }}` substitution variable (the concat of
//!   every enabled material's `wgsl_fragment`).
//! - Build the `{{ shader_id_consts }}` substitution variable (the generated
//!   `const SHADER_ID_X: u32 = N;` lines).
//!
//! Materials register via Cargo features in this crate. Adding a new
//! material is one new module + one feature entry + one `MaterialEntry`
//! push in `enabled_materials()` — no edits anywhere else.

use crate::MaterialShaderId;

/// Static descriptor of a registered material.
pub struct MaterialEntry {
    /// Stable shader id assigned to this material.
    pub shader_id: MaterialShaderId,
    /// WGSL helper module (loader + accessor + shading function bodies).
    pub wgsl_fragment: &'static str,
    /// Human-readable name used in generated WGSL comments / debug labels.
    pub name: &'static str,
}

/// Returns the list of materials enabled in this build.
pub fn enabled_materials() -> Vec<MaterialEntry> {
    vec![
        #[cfg(feature = "pbr-standard")]
        MaterialEntry {
            shader_id: MaterialShaderId::PBR,
            wgsl_fragment: crate::pbr::WGSL_FRAGMENT,
            name: "pbr",
        },
        #[cfg(feature = "unlit")]
        MaterialEntry {
            shader_id: MaterialShaderId::UNLIT,
            wgsl_fragment: crate::unlit::WGSL_FRAGMENT,
            name: "unlit",
        },
        #[cfg(feature = "toon")]
        MaterialEntry {
            shader_id: MaterialShaderId::TOON,
            wgsl_fragment: crate::toon::WGSL_FRAGMENT,
            name: "toon",
        },
        #[cfg(feature = "flipbook")]
        MaterialEntry {
            shader_id: MaterialShaderId::FLIPBOOK,
            wgsl_fragment: crate::flipbook::WGSL_FRAGMENT,
            name: "flipbook",
        },
        #[cfg(feature = "scanline")]
        MaterialEntry {
            shader_id: MaterialShaderId::SCANLINE,
            wgsl_fragment: crate::scanline::WGSL_FRAGMENT,
            name: "scanline",
        },
    ]
}

/// Builds the `{{ materials_wgsl }}` substitution: the concatenation of
/// every enabled material's `wgsl_fragment()`, wrapped in human-readable
/// `START/END` comment fences for debugging the generated shader.
pub fn build_materials_wgsl() -> String {
    let entries = enabled_materials();
    let mut out = String::new();
    for entry in &entries {
        out.push_str(&format!(
            "/*************** START {name}_material.wgsl ******************/\n",
            name = entry.name
        ));
        out.push_str(entry.wgsl_fragment);
        if !entry.wgsl_fragment.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!(
            "/*************** END {name}_material.wgsl ******************/\n\n",
            name = entry.name
        ));
    }
    out
}

/// Builds the `{{ shader_id_consts }}` substitution: one
/// `const SHADER_ID_X: u32 = N;` line per enabled material.
pub fn build_shader_id_consts() -> String {
    let entries = enabled_materials();
    let mut out = String::new();
    for entry in &entries {
        // `enabled_materials()` returns only first-party entries, all of
        // which have a canonical WGSL const name. The dynamic-material
        // registry emits its own consts via a separate code path (see
        // `MaterialRegistry::build_shader_id_consts` in Phase 3).
        let const_name = entry.shader_id.wgsl_const_name().unwrap_or_else(|| {
            panic!(
                "[awsm-materials] first-party material {} (id {}) is missing a canonical WGSL const name",
                entry.name,
                entry.shader_id.as_u32(),
            )
        });
        out.push_str(&format!(
            "const {name}: u32 = {value}u;\n",
            name = const_name,
            value = entry.shader_id.as_u32(),
        ));
    }
    out
}
