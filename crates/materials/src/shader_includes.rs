//! Per-material declaration of which shared shader modules and pre-shade
//! fragment inputs a shading model needs — the heart of "skinny materials".
//!
//! See `docs/plans/SKINNY-MATERIALS.md`. The principle: **no global core set**.
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

/// Abstract identities of the optional shared shader modules a material may
/// declare. A `u32` bitset (mirrors [`crate::pbr::PbrFeatures`]'s hand-rolled
/// style). Pass *scaffolding* modules (frame globals, mesh-meta routing, the
/// `materials_wgsl` host, etc.) are **not** represented here — those are emitted
/// unconditionally by each pass, independent of any material.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
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
    const BIT_UNLIT_HELPER: u32 = 1 << 9;
    const BIT_SHADOWS: u32 = 1 << 10;
    const BIT_SKYBOX: u32 = 1 << 11;
    const BIT_EXTRAS: u32 = 1 << 12;

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
    /// `lighting/light_access.wgsl` — get_light / light_to_brdf / LightsInfo /
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
    /// `lighting/unlit.wgsl` — the shared unlit output helper.
    pub const UNLIT_HELPER: Self = Self(Self::BIT_UNLIT_HELPER);
    /// Shadow sampling helpers + bindings usage.
    pub const SHADOWS: Self = Self(Self::BIT_SHADOWS);
    /// `skybox.wgsl` — `sample_skybox`. (The dedicated skybox writer pass uses
    /// this; materials generally don't.)
    pub const SKYBOX: Self = Self(Self::BIT_SKYBOX);
    /// `extras.wgsl` — the extras storage pool accessor.
    pub const EXTRAS: Self = Self(Self::BIT_EXTRAS);

    pub const fn empty() -> Self {
        Self(0)
    }
    /// Every optional module — the conservative set for author-supplied
    /// (dynamic) materials that may reference anything.
    pub const fn all() -> Self {
        Self(
            Self::BIT_MATH
                | Self::BIT_CAMERA
                | Self::BIT_COLOR_SPACE
                | Self::BIT_TEXTURES
                | Self::BIT_VERTEX_COLOR
                | Self::BIT_LIGHT_ACCESS
                | Self::BIT_APPLY_LIGHTING
                | Self::BIT_BRDF
                | Self::BIT_MATERIAL_COLOR_CALC
                | Self::BIT_UNLIT_HELPER
                | Self::BIT_SHADOWS
                | Self::BIT_SKYBOX
                | Self::BIT_EXTRAS,
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

    /// Direct (one-hop) code dependencies of a single module. The bindings a
    /// module touches are *not* modeled here — bindings stay full (§9 of the
    /// plan), so deps are purely "this WGSL calls into that WGSL".
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
            (Self::UNLIT_HELPER, "UNLIT_HELPER"),
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
#[derive(Clone, Copy, PartialEq, Eq, Default)]
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
