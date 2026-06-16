//! The reactive model for **custom WGSL materials** — the only material kind the
//! Material-mode Studio authors. Each is a registered dynamic-WGSL
//! asset: a shader body + declared uniforms/textures/buffers + surface state.
//!
//! Unlike scene mutations (which flow through invertible `EditorCommand`s), the
//! *content* of a material — WGSL text, uniform values, slot edits — is edited
//! live through these `Mutable` fields, the way a document editor works. Only the
//! structural lifecycle (create / delete / register) goes through commands. The
//! TOML serializer snapshots these fields into `material-<id>.{toml,wgsl}`.

use crate::engine::scene::AssetId;
use awsm_editor_protocol::CompileError;
use awsm_editor_protocol::{MaterialDef, MaterialShading};
use awsm_web_shared::prelude::{Mutable, MutableVec};
use std::sync::Arc;

/// Alpha/surface mode a custom material compiles for (drives the return-type
/// contract: Opaque/Mask/Blend ShadingOutput).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}

impl AlphaMode {
    pub fn key(self) -> &'static str {
        match self {
            AlphaMode::Opaque => "opaque",
            AlphaMode::Mask => "mask",
            AlphaMode::Blend => "blend",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "mask" => AlphaMode::Mask,
            "blend" => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        }
    }
    /// The `ShadingOutput` return-type signature for the contract docs.
    pub fn ret_sig(self) -> &'static str {
        match self {
            AlphaMode::Opaque => "OpaqueShadingOutput(color: vec3<f32>, ao: f32)",
            AlphaMode::Mask => "MaskShadingOutput(color: vec3<f32>, cutoff: f32)",
            AlphaMode::Blend => "BlendShadingOutput(color: vec3<f32>, alpha: f32)",
        }
    }
    pub fn ret_note(self) -> &'static str {
        match self {
            AlphaMode::Opaque => "Runs in the opaque pass. Return a lit RGB color and an ambient-occlusion scalar. No blending.",
            AlphaMode::Mask => "Alpha-tested. Fragments below cutoff are discarded — good for foliage and decals.",
            AlphaMode::Blend => "Transparent pass, back-to-front. Your alpha drives the blend. No depth write.",
        }
    }
}

/// One declared slot in the Definition rail. A uniform uses `name`/`ty`/`val`;
/// a texture or buffer uses `name`/`ty`/`debug` (the debug preview source).
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub name: String,
    pub ty: String,
    pub val: String,
    pub debug: String,
}

impl Slot {
    pub fn uniform(name: impl Into<String>, ty: impl Into<String>, val: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: ty.into(),
            val: val.into(),
            debug: String::new(),
        }
    }
    pub fn named(name: impl Into<String>, ty: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: ty.into(),
            val: String::new(),
            debug: String::new(),
        }
    }
}

/// A live, reactive material in the library (`EditorController::custom_materials`).
///
/// Two kinds share this struct: a **dynamic** WGSL material (`builtin == None` —
/// authored via the Studio's code/slots/includes) and a **built-in** material
/// (`builtin == Some(def)` — PBR/Unlit/Toon whose shared *variant* settings are
/// the `MaterialDef`; its `wgsl`/`uniforms`/`includes` fields are unused, and per
/// the model its uniform *values* are set per-mesh). Both are assignable, renamable,
/// and deletable; only dynamics carry editable shader code.
pub struct CustomMaterial {
    pub id: AssetId,
    pub name: Mutable<String>,
    /// `Some` ⇒ this is a built-in material carrying shared variant settings.
    /// `None` ⇒ a dynamic WGSL material.
    pub builtin: Mutable<Option<MaterialDef>>,
    pub wgsl: Mutable<String>,
    /// The **second**, alpha-only WGSL window — only meaningful when
    /// `alpha == AlphaMode::Mask`. Returns just an `f32` alpha; it is compiled
    /// into the masked visibility-raster variant so the cutout is cheap
    /// (no color/lighting) and casts hole-shaped shadows + shows through to
    /// transmission. Empty → no masked variant is built (the mesh renders solid
    /// through the opaque path). See `MaterialRegistration::alpha_wgsl`.
    pub alpha_wgsl: Mutable<String>,
    pub alpha: Mutable<AlphaMode>,
    pub cutoff: Mutable<f64>,
    pub double_sided: Mutable<bool>,
    /// Debug base color as a `#rrggbb` hex string (preview-only).
    pub color: Mutable<String>,
    pub uniforms: Mutable<Vec<Slot>>,
    pub textures: Mutable<Vec<Slot>>,
    pub buffers: Mutable<Vec<Slot>>,
    /// Whether the material has been registered (compiled to a renderer bucket).
    /// A content edit after registration flips this back to `false` (draft).
    pub registered: Mutable<bool>,
    /// The outstanding compile diagnostics from the last (auto- or manual-)
    /// register attempt — empty when the WGSL compiled cleanly. Surfaced over
    /// MCP via `MaterialDiagnostics` so a caller can tell a compile failure from
    /// a successful-but-dark shader (the original §A failure). Populated by the
    /// register path (both the lightweight syntax check and the real GPU/naga
    /// error, which is otherwise dropped).
    pub last_diagnostics: Mutable<Vec<CompileError>>,
    /// Declared **pass dependencies** (the v1 "skinny materials" win): which
    /// `ShaderIncludes` this material's WGSL needs. Stored as the include keys;
    /// mapped to `awsm_materials::ShaderIncludes` bits at registration. The
    /// default is **none** (opt in only what the WGSL uses, for the leanest,
    /// fastest-compiling bucket) — the Definition rail's select-all adds them.
    pub shader_includes: Mutable<Vec<String>>,
    /// Declared `FragmentInputs` (interpolants the fragment reads).
    pub fragment_inputs: Mutable<Vec<String>>,
    /// Monotonic counter bumped by *any* compile-affecting edit (WGSL, alpha,
    /// double-sided, layout, includes/inputs). The auto-register observer watches
    /// this — so a layout/alpha edit recompiles too, not just a WGSL edit (the
    /// pre-reroute observer only watched `wgsl`). Not persisted.
    pub recompile_rev: Mutable<u64>,
}

/// The custom-material-facing `ShaderIncludes` menu — the **Tier-A generic
/// helpers** a custom material may opt into (display order). Derived from the
/// single source of truth, `awsm_materials::ShaderIncludes::tier_a_catalog()`,
/// so it can't drift from the engine's actual gate set. The Tier-B PBR-internal
/// modules (`apply_lighting`/`brdf`/`material_color_calc`) are excluded there:
/// `ShaderIncludeFlags::for_custom` masks them, so offering them would do
/// nothing. A custom material that wants PBR-like shading writes its own WGSL
/// (building on `light_access` + the generic `brdf_primitives` helpers).
pub static SHADER_INCLUDE_KEYS: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| {
        awsm_materials::ShaderIncludes::tier_a_catalog()
            .map(|(key, _desc)| key)
            .collect()
    });

/// Every `FragmentInputs` flag, by key.
pub const FRAGMENT_INPUT_KEYS: &[&str] = &[
    "normals",
    "tangents",
    "uv",
    "lights",
    "view_dir",
    "vertex_color",
];

/// The default WGSL body for a fresh opaque material: a view-dependent rim that
/// renders **non-black out of the box** and demonstrates the input ABI (the
/// `normals` + `view_dir` fragment inputs are seeded for it in
/// [`CustomMaterial::new`]). See `docs/dynamic-materials/contract-opaque.md`.
pub const NEW_MATERIAL_WGSL: &str = "\
// Opaque material. Inputs arrive as fields on `input` (declare them in Pass
// Dependencies): input.world_normal, input.surface_to_camera (normalized),
// input.world_position, input.coords. Time: frame_globals_from_raw(frame_globals_raw).
// `color` is linear HDR (tonemap is a later pass); it is UNLIT — to light it,
// declare `light_access` and walk lights yourself (get_lights_info / get_light /
// light_sample). Must end in `return OpaqueShadingOutput(color, ao)`.
let n = normalize(input.world_normal);
let v = input.surface_to_camera;          // already normalized (surface -> camera)
let fresnel = pow(1.0 - max(dot(n, v), 0.0), 3.0);
let base = vec3<f32>(0.1, 0.12, 0.2);
let color = base + vec3<f32>(0.6, 0.7, 1.0) * fresnel;
return OpaqueShadingOutput(color, 1.0);";

impl CustomMaterial {
    pub fn new(id: AssetId, name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            id,
            name: Mutable::new(name.into()),
            builtin: Mutable::new(None),
            wgsl: Mutable::new(NEW_MATERIAL_WGSL.to_string()),
            alpha_wgsl: Mutable::new(String::new()),
            alpha: Mutable::new(AlphaMode::Opaque),
            cutoff: Mutable::new(0.5),
            double_sided: Mutable::new(false),
            color: Mutable::new("#8aa0b8".to_string()),
            uniforms: Mutable::new(Vec::new()),
            textures: Mutable::new(Vec::new()),
            buffers: Mutable::new(Vec::new()),
            registered: Mutable::new(false),
            last_diagnostics: Mutable::new(Vec::new()),
            // Seed the inputs the default rim shader (`NEW_MATERIAL_WGSL`)
            // references, so a fresh material compiles + renders non-black out of
            // the box. Pare these back in the Definition rail for a leaner bucket
            // once the WGSL no longer needs them.
            shader_includes: Mutable::new(Vec::new()),
            fragment_inputs: Mutable::new(vec!["normals".to_string(), "view_dir".to_string()]),
            recompile_rev: Mutable::new(0),
        })
    }

    /// A fresh **built-in** library material of the given shading. Carries the
    /// shared variant settings as a `MaterialDef`; needs no compile, so it's
    /// immediately registered/usable.
    pub fn new_builtin(
        id: AssetId,
        name: impl Into<String>,
        shading: MaterialShading,
    ) -> Arc<Self> {
        let mat = Self::new(id, name);
        mat.builtin.set(Some(MaterialDef {
            shading,
            ..MaterialDef::default()
        }));
        mat.registered.set_neq(true);
        mat
    }

    /// Whether this is a built-in (PBR/Unlit/Toon) rather than a dynamic WGSL material.
    pub fn is_builtin(&self) -> bool {
        self.builtin.lock_ref().is_some()
    }
}

/// Find a material in the live list by id.
pub fn find_material(
    materials: &MutableVec<Arc<CustomMaterial>>,
    id: AssetId,
) -> Option<Arc<CustomMaterial>> {
    materials
        .lock_ref()
        .iter()
        .find(|m| m.id == id)
        .map(Arc::clone)
}

/// A *very* lightweight WGSL "compile" check (mirrors the prototype's
/// `compileWGSL`): flags statements that begin with `let`/`var`/`return` but
/// don't end in `;`/`{`/`}`. Real validation lands with renderer registration;
/// this gives instant in-editor feedback (line + message).
pub fn compile_wgsl(code: &str) -> Vec<(usize, String)> {
    let mut errs = Vec::new();
    for (i, raw) in code.lines().enumerate() {
        // strip line comment
        let line = match raw.find("//") {
            Some(c) => &raw[..c],
            None => raw,
        };
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let starts = t.starts_with("let ")
            || t.starts_with("var ")
            || t.starts_with("return ")
            || t == "return";
        let ends = t.ends_with(';') || t.ends_with('{') || t.ends_with('}');
        if starts && !ends {
            errs.push((i + 1, "expected ';' at end of statement".to_string()));
        }
    }
    errs
}
