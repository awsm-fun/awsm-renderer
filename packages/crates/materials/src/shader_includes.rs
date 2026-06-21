//! Per-material declaration of which shared shader modules and pre-shade
//! fragment inputs a shading model needs — the heart of "skinny materials".
//!
//! The principle: **no global core set**.
//! Each material declares the optional shared modules its shading body uses
//! (possibly none); the renderer compiles the transitive closure of that set
//! and emits only those `{% include %}`s. The `@group/@binding` surface is *not*
//! gated — it stays full and pass-owned (bindings are a stable ABI, ~free to
//! declare). Gating targets WGSL *code* (function/struct bodies).
//!
//! These are **abstract module identities** — the contract between a material
//! ("I use the BRDF + light access") and the renderer (which maps each flag to
//! the actual `shared_wgsl/...` file in each pass host template). The materials
//! crate owns the identities + the dependency closure; the renderer owns the
//! file layout.
//!
//! # Module taxonomy (audit)
//!
//! Every WGSL module emitted into the opaque material kernel
//! (`material_opaque_wgsl/opaque_kernel_includes.wgsl`), classified so the
//! gating work (Phases 2–4) has a single source of truth. Two kinds:
//!
//! - **Tier A — generic helpers.** Reusable by ANY material incl. Custom
//!   (dynamic). These are what a custom material may opt into via
//!   [`ShaderIncludes`]. Target: emitted iff the material declares them.
//! - **Tier B — shading-model internals.** Welded to a built-in family's types
//!   (`PbrMaterial` / `PbrMaterialColor`, Toon/Unlit/Flipbook material structs).
//!   NEVER reachable by a Custom material; emitted by `base`, not the custom menu.
//! - **(scaffold)** — pass plumbing every kernel needs regardless of material
//!   (frame globals, mesh-meta routing, the material storage accessor, standard
//!   coords, MSAA sample fetch). Stays unconditional; not on the custom menu.
//!
//! | module (`*.wgsl`)            | tier      | current gate                  | target gate |
//! |------------------------------|-----------|-------------------------------|-------------|
//! | math                         | Tier A    | none (always)                 | MATH (or always — cheap) |
//! | color_space                  | Tier A    | none (always)                 | COLOR_SPACE (or always — cheap) |
//! | camera                       | Tier A    | none (always)                 | CAMERA (or always — cheap) |
//! | textures                     | Tier A    | none (always)                 | TEXTURES |
//! | texture_uvs (helper)         | Tier A    | none (always)                 | TEXTURES |
//! | vertex_color / _attrib       | Tier A    | none (always)                 | VERTEX_COLOR |
//! | light_access                 | Tier A    | none (always)                 | LIGHT_ACCESS |
//! | extras                       | Tier A    | none (always)                 | EXTRAS |
//! | skybox (helper)              | Tier A    | none (always)                 | SKYBOX |
//! | brdf — primitives half       | Tier A    | `inc.brdf` (whole file)       | split → BRDF_PRIMITIVES (Phase 2) |
//! | mipmap — UV-deriv half       | Tier A    | `mipmap` mode (whole file)    | TEXTURES (Phase 2 split) |
//! | brdf — `*_direct/_ibl` half  | Tier B    | `inc.brdf`                    | base==Pbr (Phase 2 split → brdf_pbr) |
//! | apply_lighting               | Tier B    | `inc.apply_lighting`          | base==Pbr |
//! | material_color_calc (PBR)    | Tier B    | `inc.material_color_calc`     | base==Pbr |
//! | material_color_calc (unlit)  | Tier B    | `base==Unlit`                 | base==Unlit (ok) |
//! | mipmap — `pbr_get_gradients` | Tier B    | `inc.material_color_calc`     | base==Pbr (Phase 2 split) |
//! | material_shading — pbr glue  | Tier B    | `inc.material_color_calc`     | base==Pbr |
//! | materials_wgsl: pbr body     | Tier B    | `base.canonical_shader_id()`  | base==Pbr ONLY (Phase 3 — see #1) |
//! | materials_wgsl: unlit/toon/flipbook | Tier B | (emitted for Custom too — BUG)| their base ONLY (Phase 3 — see #1) |
//! | dynamic-material wrapper     | Custom    | `base==Custom`                | base==Custom (ok) |
//! | frame_globals                | scaffold  | always                        | always |
//! | material_mesh_meta           | scaffold  | always                        | always |
//! | material (storage accessor)  | scaffold  | always                        | always |
//! | transforms                   | scaffold  | always                        | always |
//! | standard (coords)            | scaffold  | always                        | always |
//! | positions                    | scaffold  | always                        | always |
//! | debug (shared + helper)      | scaffold  | `debug.any()` (helper)        | unchanged |
//! | msaa (sample fetch)          | scaffold  | `multisampled_geometry`       | unchanged |
//!
//! Key reads:
//! - The "current gate" column is why even `ShaderIncludes::empty()` still emits
//!   ~160 KB: almost every Tier A module is currently `always`, and the
//!   first-party bodies are emitted even for Custom (the #1 bug).
//! - `brdf.wgsl` and `mipmap.wgsl` are split-tier (A+B) → Phase 2 physically
//!   splits them so each half can gate independently.
//! - The [`ShaderIncludes`] menu below mixes Tier A (legit for Custom) with the
//!   Tier B PBR bits (`APPLY_LIGHTING`, `BRDF`, `MATERIAL_COLOR_CALC`) — Phase 3
//!   removes those from the custom-facing menu.

/// Abstract identities of the optional shared shader modules a material may
/// declare. A `u32` bitset (mirrors [`crate::pbr::PbrFeatures`]'s hand-rolled
/// style). Pass *scaffolding* modules (frame globals, mesh-meta routing, the
/// `materials_wgsl` host, etc.) are **not** represented here — those are emitted
/// unconditionally by each pass, independent of any material.
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct ShaderIncludes(u32);

impl ShaderIncludes {
    // Bit positions — stable + append-only.
    const BIT_MATH: u32 = 1 << 0;
    const BIT_CAMERA: u32 = 1 << 1;
    const BIT_COLOR_SPACE: u32 = 1 << 2;
    const BIT_TEXTURES: u32 = 1 << 3;
    const BIT_VERTEX_COLOR: u32 = 1 << 4;
    const BIT_LIGHT_ACCESS: u32 = 1 << 5;
    const BIT_APPLY_LIGHTING: u32 = 1 << 6;
    const BIT_BRDF: u32 = 1 << 7;
    const BIT_MATERIAL_COLOR_CALC: u32 = 1 << 8;
    // bit 9 retired (was BIT_UNLIT_HELPER): the unlit output helper lives in
    // the unlit material fragment, not a shared module, so there was nothing
    // to gate. Append-only — don't reuse bit 9.
    const BIT_SHADOWS: u32 = 1 << 10;
    const BIT_SKYBOX: u32 = 1 << 11;
    const BIT_EXTRAS: u32 = 1 << 12;
    const BIT_IBL: u32 = 1 << 13;
    const BIT_NORMAL_MAP: u32 = 1 << 14;

    /// `math.wgsl` — basic math helpers.
    pub const MATH: Self = Self(Self::BIT_MATH);
    /// `camera.wgsl` — the Camera uniform + view/proj helpers.
    pub const CAMERA: Self = Self(Self::BIT_CAMERA);
    /// `color_space.wgsl` — linear/sRGB + tonemapping-adjacent helpers.
    pub const COLOR_SPACE: Self = Self(Self::BIT_COLOR_SPACE);
    /// `textures.wgsl` — texture-pool sampling infrastructure.
    pub const TEXTURES: Self = Self(Self::BIT_TEXTURES);
    /// `vertex_color.wgsl` (+ the pass's vertex-color attrib helper).
    pub const VERTEX_COLOR: Self = Self(Self::BIT_VERTEX_COLOR);
    /// `lighting/light_access.wgsl` — get_light / light_sample / LightsInfo /
    /// attenuation. Needed by anything that walks punctual lights (PBR + toon).
    pub const LIGHT_ACCESS: Self = Self(Self::BIT_LIGHT_ACCESS);
    /// `lighting/apply_lighting.wgsl` — the `apply_lighting*` orchestration that
    /// drives the PBR BRDF. PBR only.
    pub const APPLY_LIGHTING: Self = Self(Self::BIT_APPLY_LIGHTING);
    /// `lighting/brdf.wgsl` — the PBR BRDF lobes + IBL split-sum. PBR only.
    pub const BRDF: Self = Self(Self::BIT_BRDF);
    /// The PBR material-color builder (`material_color_calc.wgsl`) that samples
    /// all the PBR textures/extensions into a `PbrMaterialColor`. PBR only.
    pub const MATERIAL_COLOR_CALC: Self = Self(Self::BIT_MATERIAL_COLOR_CALC);
    /// Shadow sampling helpers + bindings usage.
    pub const SHADOWS: Self = Self(Self::BIT_SHADOWS);
    /// `skybox.wgsl` — `sample_skybox`. (The dedicated skybox writer pass uses
    /// this; materials generally don't.)
    pub const SKYBOX: Self = Self(Self::BIT_SKYBOX);
    /// `extras.wgsl` — the extras storage pool accessor.
    pub const EXTRAS: Self = Self(Self::BIT_EXTRAS);
    /// `lighting/ibl.wgsl` — image-based-lighting primitive `sample_ibl(...)`
    /// (diffuse irradiance + split-sum specular prefilter + BRDF LUT) over the
    /// scene's always-bound environment cubemaps + BRDF LUT. Tier A (generic):
    /// the single biggest "make a custom material first-class in an IBL-lit
    /// scene" primitive — without it a dynamic material with no punctual lights
    /// renders ~black. NOT a PBR re-implementation; just the ambient/environment
    /// term. Depends on LIGHT_ACCESS (for the IBL mip-count info).
    pub const IBL: Self = Self(Self::BIT_IBL);

    /// Tier A (generic): normal mapping for custom materials. Exposes the
    /// per-pixel orthonormal world tangent frame the engine already reconstructs
    /// (the prep G-buffer) as `material_tbn(input)` + `apply_normal_map(input,
    /// sampled_rgb)`, so a dynamic material perturbs its normal from a normal-map
    /// sample WITHOUT re-deriving a TBN. Tiny (two helpers over always-present
    /// `OpaqueShadingInput` fields, no extra bindings); opt-in like IBL so a lean
    /// material that doesn't normal-map carries nothing. No hard dep (uses only
    /// always-on `math`).
    pub const NORMAL_MAP: Self = Self(Self::BIT_NORMAL_MAP);

    pub const fn empty() -> Self {
        Self(0)
    }
    /// Every **Tier A (generic)** module — the conservative set a custom
    /// (dynamic) material may safely opt into. This deliberately EXCLUDES the
    /// Tier B PBR-internal modules (`APPLY_LIGHTING` / `BRDF` /
    /// `MATERIAL_COLOR_CALC`): those are welded to the `PbrMaterial` /
    /// `PbrMaterialColor` types and are emitted only for the built-in PBR base,
    /// never reachable from a custom material. A custom material that wants
    /// PBR-like shading supplies its own WGSL (optionally built on the generic
    /// `brdf_primitives` helpers). So `all()` now means "all generic helpers",
    /// a safe lazy default that no longer drags ~87 KB of PBR code into a shader
    /// that doesn't use it. First-party PBR declares its Tier B modules
    /// explicitly via `pbr::SHADER_INCLUDES` (the first-party-internal set).
    pub const fn all() -> Self {
        Self(
            Self::BIT_MATH
                | Self::BIT_CAMERA
                | Self::BIT_COLOR_SPACE
                | Self::BIT_TEXTURES
                | Self::BIT_VERTEX_COLOR
                | Self::BIT_LIGHT_ACCESS
                | Self::BIT_SHADOWS
                | Self::BIT_SKYBOX
                | Self::BIT_EXTRAS,
            // NOTE: `IBL` is deliberately NOT in `all()`. It pulls ~40 KB of
            // split-sum sampling into the kernel, so — per the report's "costed
            // only when used" — it's the one Tier-A helper that a custom material
            // must opt into EXPLICITLY (declare `"ibl"` via shader includes). It's
            // still `tier_a: true` in `KEY_TABLE` (offered on the custom menu) and
            // reaches the kernel through the declared set (`ShaderIncludeFlags::
            // for_custom`), independent of this default; it's just not auto-on.
        )
    }

    /// The single-source key table: `(key, bit, tier_a, description)`. The
    /// authoring string keys, their bits, whether they're offered on the
    /// custom-material (Tier-A) menu, and a one-line description. Editor pickers,
    /// the scene-loader / editor-bridge string parsers, and the MCP catalog all
    /// derive from THIS — no duplicated key lists. Tier-B entries (the PBR
    /// internals) stay in the table for back-compat parsing of old saved
    /// projects, but `tier_a` is false so they're never offered to custom
    /// materials (and `ShaderIncludeFlags::for_custom` masks them anyway).
    pub const KEY_TABLE: &'static [(&'static str, Self, bool, &'static str)] = &[
        (
            "math",
            Self::MATH,
            true,
            "Basic math helpers (constants, clamping, etc.)",
        ),
        (
            "camera",
            Self::CAMERA,
            true,
            "Camera uniform + view/projection access",
        ),
        (
            "color_space",
            Self::COLOR_SPACE,
            true,
            "Linear/sRGB + tonemap-adjacent helpers",
        ),
        (
            "textures",
            Self::TEXTURES,
            true,
            "Texture-pool sampling + UV helpers",
        ),
        (
            "vertex_color",
            Self::VERTEX_COLOR,
            true,
            "Per-vertex COLOR_n attribute access",
        ),
        (
            "light_access",
            Self::LIGHT_ACCESS,
            true,
            "Punctual light access: get_lights_info / get_light / light_sample",
        ),
        (
            "shadows",
            Self::SHADOWS,
            true,
            "Shadow-map sampling helpers",
        ),
        (
            "skybox",
            Self::SKYBOX,
            true,
            "Skybox cubemap sampling (sample_skybox)",
        ),
        (
            "extras",
            Self::EXTRAS,
            true,
            "Extras storage-pool accessors (extras_load_*)",
        ),
        (
            "ibl",
            Self::IBL,
            true,
            "Image-based lighting: sample_ibl(albedo, normal, view, roughness, metallic) — environment irradiance + specular + BRDF LUT",
        ),
        (
            "normal_map",
            Self::NORMAL_MAP,
            true,
            "Normal mapping: apply_normal_map(input, sampled_rgb) + material_tbn(input) — perturb the normal from a normal-map sample using the engine's reconstructed tangent frame",
        ),
        (
            "apply_lighting",
            Self::APPLY_LIGHTING,
            false,
            "(PBR-internal) apply_lighting orchestration — NOT available to custom materials",
        ),
        (
            "brdf",
            Self::BRDF,
            false,
            "(PBR-internal) PBR BRDF orchestrators — NOT available to custom materials",
        ),
        (
            "material_color_calc",
            Self::MATERIAL_COLOR_CALC,
            false,
            "(PBR-internal) PbrMaterialColor builder — NOT available to custom materials",
        ),
    ];

    /// Parse an authoring string key (e.g. `"light_access"`) to its include bit.
    /// `None` for unknown keys. The single source for the scene-loader + editor
    /// bridge string→include parsers (back-compat: Tier-B keys still parse, but
    /// `for_custom` masks them on the custom path).
    pub fn from_key(key: &str) -> Option<Self> {
        Self::KEY_TABLE
            .iter()
            .find(|(k, ..)| *k == key)
            .map(|(_, bit, ..)| *bit)
    }

    /// The custom-material-facing **Tier-A** helper catalog: `(key, description)`
    /// for every generic module a custom material may opt into. Drives the editor
    /// picker + the MCP helper catalog. Excludes the Tier-B PBR internals.
    pub fn tier_a_catalog() -> impl Iterator<Item = (&'static str, &'static str)> {
        Self::KEY_TABLE
            .iter()
            .filter(|(.., tier_a, _)| *tier_a)
            .map(|(k, _, _, desc)| (*k, *desc))
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
    pub const fn from_bits(b: u32) -> Self {
        Self(b)
    }
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Direct (one-hop) code dependencies of a single module. The bindings a
    /// module touches are *not* modeled here — bindings stay full, so deps
    /// are purely "this WGSL calls into that WGSL".
    const fn direct_deps(bit: u32) -> Self {
        match bit {
            Self::BIT_APPLY_LIGHTING => Self::BRDF
                .union(Self::LIGHT_ACCESS)
                .union(Self::MATH)
                .union(Self::CAMERA),
            Self::BIT_BRDF => Self::MATH.union(Self::CAMERA),
            Self::BIT_LIGHT_ACCESS => Self::MATH,
            Self::BIT_MATERIAL_COLOR_CALC => Self::TEXTURES.union(Self::CAMERA).union(Self::MATH),
            Self::BIT_SHADOWS => Self::MATH.union(Self::CAMERA),
            Self::BIT_SKYBOX => Self::CAMERA.union(Self::MATH),
            Self::BIT_TEXTURES => Self::MATH,
            // IBL needs the LightsInfo/IblInfo accessor (mip counts) + math/camera.
            Self::BIT_IBL => Self::LIGHT_ACCESS.union(Self::MATH).union(Self::CAMERA),
            // normal_map only calls normalize() (math) over always-present fields.
            Self::BIT_NORMAL_MAP => Self::MATH,
            _ => Self::empty(),
        }
    }

    /// The transitive closure of this set: every module plus everything its
    /// modules (transitively) depend on. Fixpoint over [`Self::direct_deps`];
    /// bounded by the bit count so it always terminates.
    pub fn resolve(self) -> Self {
        let mut acc = self;
        loop {
            let mut next = acc;
            let mut bit = 1u32;
            while bit != 0 {
                if (acc.0 & bit) != 0 {
                    next = next.union(Self::direct_deps(bit));
                }
                bit <<= 1;
            }
            if next.0 == acc.0 {
                return acc;
            }
            acc = next;
        }
    }
}

impl core::ops::BitOr for ShaderIncludes {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl core::fmt::Debug for ShaderIncludes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut names: Vec<&str> = Vec::new();
        let table = [
            (Self::MATH, "MATH"),
            (Self::CAMERA, "CAMERA"),
            (Self::COLOR_SPACE, "COLOR_SPACE"),
            (Self::TEXTURES, "TEXTURES"),
            (Self::VERTEX_COLOR, "VERTEX_COLOR"),
            (Self::LIGHT_ACCESS, "LIGHT_ACCESS"),
            (Self::APPLY_LIGHTING, "APPLY_LIGHTING"),
            (Self::BRDF, "BRDF"),
            (Self::MATERIAL_COLOR_CALC, "MATERIAL_COLOR_CALC"),
            (Self::SHADOWS, "SHADOWS"),
            (Self::SKYBOX, "SKYBOX"),
            (Self::EXTRAS, "EXTRAS"),
        ];
        for (flag, name) in table {
            if self.contains(flag) {
                names.push(name);
            }
        }
        write!(f, "ShaderIncludes({})", names.join(" | "))
    }
}

/// Abstract identities of the pre-shade fragment inputs a material's shading
/// body consumes. The pass scaffolding computes/unpacks only the declared ones
/// (a solid-color material needs none, so it skips TBN unpack, lights read, …).
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash, Debug)]
pub struct FragmentInputs(u32);

impl FragmentInputs {
    const BIT_NORMALS: u32 = 1 << 0;
    const BIT_TANGENTS: u32 = 1 << 1;
    const BIT_UV: u32 = 1 << 2;
    const BIT_LIGHTS: u32 = 1 << 3;
    const BIT_VIEW_DIR: u32 = 1 << 4;
    const BIT_VERTEX_COLOR: u32 = 1 << 5;

    /// World-space shading normal (perturbed by normal mapping if used).
    pub const NORMALS: Self = Self(Self::BIT_NORMALS);
    /// World-space tangent/bitangent (normal mapping, anisotropy).
    pub const TANGENTS: Self = Self(Self::BIT_TANGENTS);
    /// Interpolated UV(s) for texture sampling.
    pub const UV: Self = Self(Self::BIT_UV);
    /// The per-fragment light list (`get_lights_info()` + the froxel slice).
    pub const LIGHTS: Self = Self(Self::BIT_LIGHTS);
    /// Surface→camera vector (specular, rim, view-dependent terms).
    pub const VIEW_DIR: Self = Self(Self::BIT_VIEW_DIR);
    /// Per-vertex color attribute.
    pub const VERTEX_COLOR: Self = Self(Self::BIT_VERTEX_COLOR);

    pub const fn empty() -> Self {
        Self(0)
    }
    /// Every input — conservative set for author-supplied (dynamic) materials.
    pub const fn all() -> Self {
        Self(
            Self::BIT_NORMALS
                | Self::BIT_TANGENTS
                | Self::BIT_UV
                | Self::BIT_LIGHTS
                | Self::BIT_VIEW_DIR
                | Self::BIT_VERTEX_COLOR,
        )
    }
    pub const fn bits(self) -> u32 {
        self.0
    }
    pub const fn from_bits(b: u32) -> Self {
        Self(b)
    }
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for FragmentInputs {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_table_is_single_source_of_truth() {
        // from_key round-trips every table key.
        for (k, bit, _, _) in ShaderIncludes::KEY_TABLE {
            assert_eq!(
                ShaderIncludes::from_key(k),
                Some(*bit),
                "from_key({k}) drifted"
            );
        }
        assert_eq!(ShaderIncludes::from_key("nonsense"), None);

        // The Tier-A catalog (the custom-facing menu) == the DEFAULT-ON set
        // `all()` PLUS the explicit opt-in extras. Most Tier-A helpers are
        // default-on, but a few heavy ones are offered + declarable yet NOT
        // auto-on ("costed only when used"): `ibl` pulls split-sum env sampling
        // into the kernel, so it must be declared. Any NEW generic module must
        // either go in `all()` (default-on) or be added to `OPT_IN_TIER_A` here —
        // this guard forces that decision rather than silent drift.
        const OPT_IN_TIER_A: ShaderIncludes = ShaderIncludes::IBL.union(ShaderIncludes::NORMAL_MAP);
        let catalog_union = ShaderIncludes::tier_a_catalog()
            .map(|(k, _)| ShaderIncludes::from_key(k).unwrap())
            .fold(ShaderIncludes::empty(), |a, b| a.union(b));
        assert_eq!(
            catalog_union.bits(),
            ShaderIncludes::all().union(OPT_IN_TIER_A).bits(),
            "tier_a_catalog must == all() (default-on) ∪ OPT_IN_TIER_A (offered-but-opt-in)"
        );
        // The opt-in extras are offered (Tier-A) but NOT default-on — i.e. not
        // already in `all()` (otherwise "opt-in" is meaningless).
        assert_eq!(
            ShaderIncludes::all().bits() & OPT_IN_TIER_A.bits(),
            0,
            "OPT_IN_TIER_A must be disjoint from all() (offered but not default-on)"
        );

        // The Tier-B PBR internals are in the table (back-compat parsing) but NOT
        // in the Tier-A catalog (never offered to custom materials).
        let tier_a_keys: Vec<_> = ShaderIncludes::tier_a_catalog().map(|(k, _)| k).collect();
        for forbidden in ["apply_lighting", "brdf", "material_color_calc"] {
            assert!(
                ShaderIncludes::from_key(forbidden).is_some(),
                "{forbidden} must still parse (back-compat)"
            );
            assert!(
                !tier_a_keys.contains(&forbidden),
                "{forbidden} must NOT be on the custom Tier-A menu"
            );
        }
    }

    #[test]
    fn closure_pulls_transitive_deps() {
        // APPLY_LIGHTING → BRDF, LIGHT_ACCESS, MATH, CAMERA (and their deps).
        let resolved = ShaderIncludes::APPLY_LIGHTING.resolve();
        assert!(resolved.contains(ShaderIncludes::APPLY_LIGHTING));
        assert!(resolved.contains(ShaderIncludes::BRDF));
        assert!(resolved.contains(ShaderIncludes::LIGHT_ACCESS));
        assert!(resolved.contains(ShaderIncludes::MATH));
        assert!(resolved.contains(ShaderIncludes::CAMERA));
    }

    #[test]
    fn empty_resolves_empty() {
        assert!(ShaderIncludes::empty().resolve().is_empty());
    }

    #[test]
    fn material_color_calc_pulls_textures() {
        let resolved = ShaderIncludes::MATERIAL_COLOR_CALC.resolve();
        assert!(resolved.contains(ShaderIncludes::TEXTURES));
        assert!(resolved.contains(ShaderIncludes::MATH));
    }

    #[test]
    fn unrelated_modules_not_pulled() {
        // A pure unlit/textures material must NOT drag in the BRDF.
        let resolved = ShaderIncludes::TEXTURES.resolve();
        assert!(!resolved.contains(ShaderIncludes::BRDF));
        assert!(!resolved.contains(ShaderIncludes::APPLY_LIGHTING));
    }
}
