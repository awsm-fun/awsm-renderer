//! The reactive model for **custom WGSL materials** — the only material kind the
//! Material-mode Studio authors (decision 3). Each is a registered dynamic-WGSL
//! asset: a shader body + declared uniforms/textures/buffers + surface state.
//!
//! Unlike scene mutations (which flow through invertible `EditorCommand`s), the
//! *content* of a material — WGSL text, uniform values, slot edits — is edited
//! live through these `Mutable` fields, the way a document editor works. Only the
//! structural lifecycle (create / delete / register) goes through commands. The
//! TOML serializer (M11) snapshots these fields into `material-<id>.{toml,wgsl}`.

use crate::engine::scene::AssetId;
use awsm_scene_schema::{MaterialDef, MaterialShading};
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
    /// Declared **pass dependencies** (the v1 "skinny materials" win): which
    /// `ShaderIncludes` this material's WGSL needs. Stored as the include keys;
    /// mapped to `awsm_materials::ShaderIncludes` bits at registration. The
    /// default is **none** (opt in only what the WGSL uses, for the leanest,
    /// fastest-compiling bucket) — the Definition rail's select-all adds them.
    pub shader_includes: Mutable<Vec<String>>,
    /// Declared `FragmentInputs` (interpolants the fragment reads).
    pub fragment_inputs: Mutable<Vec<String>>,
}

/// Every `ShaderIncludes` flag, by key (order = display order).
pub const SHADER_INCLUDE_KEYS: &[&str] = &[
    "math",
    "camera",
    "color_space",
    "textures",
    "vertex_color",
    "light_access",
    "apply_lighting",
    "brdf",
    "material_color_calc",
    "shadows",
    "skybox",
    "extras",
];

/// Every `FragmentInputs` flag, by key.
pub const FRAGMENT_INPUT_KEYS: &[&str] = &[
    "normals",
    "tangents",
    "uv",
    "lights",
    "view_dir",
    "vertex_color",
];

/// The default WGSL body for a fresh opaque material (matches the prototype).
pub const NEW_MATERIAL_WGSL: &str =
    "// new material — opaque.\nreturn OpaqueShadingOutput(vec3<f32>(0.55, 0.6, 0.68), 1.0);";

impl CustomMaterial {
    pub fn new(id: AssetId, name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            id,
            name: Mutable::new(name.into()),
            builtin: Mutable::new(None),
            wgsl: Mutable::new(NEW_MATERIAL_WGSL.to_string()),
            alpha: Mutable::new(AlphaMode::Opaque),
            cutoff: Mutable::new(0.5),
            double_sided: Mutable::new(false),
            color: Mutable::new("#8aa0b8".to_string()),
            uniforms: Mutable::new(Vec::new()),
            textures: Mutable::new(Vec::new()),
            buffers: Mutable::new(Vec::new()),
            registered: Mutable::new(false),
            // Default to none selected — opt in only what the material's WGSL
            // actually references (select-all is one click in the Definition rail).
            shader_includes: Mutable::new(Vec::new()),
            fragment_inputs: Mutable::new(Vec::new()),
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
/// don't end in `;`/`{`/`}`. Real validation lands with renderer registration
/// (M10); this gives instant in-editor feedback (line + message).
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
