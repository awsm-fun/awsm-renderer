//! Reading a USD material into the sidecar's material, textures included.
//!
//! Isaac assets author **MDL** materials — almost always NVIDIA's `OmniPBR` —
//! with no `UsdPreviewSurface` fallback. We never need the `.mdl` itself (a
//! built-in Omniverse module, not shipped with the assets): OmniPBR is a
//! metallic-roughness model whose inputs are plain named attributes, so reading
//! those names — with OmniPBR's own defaults for the unauthored ones — is the
//! whole job. `UsdPreviewSurface` and `OmniSurface` are read the same way.
//!
//! The sidecar material has glTF semantics (factor × texture) and the importer
//! maps `roughness = 1 - shininess`, `metallic = reflectance`, so this module
//! writes the exact inverse. Where USD keeps channels apart that glTF packs
//! (OmniPBR's separate roughness and metallic maps, its opacity map), the
//! [`texture`](crate::texture) module packs them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use awsm_renderer_mujoco_format::sidecar::{
    AlphaMode, Material, MaterialTexture, MaterialTextures, UvTransform, Wrap,
};
use image::{GrayImage, Luma, RgbaImage};
use openusd::sdf;
use openusd::usd::{Attribute, Prim, Stage};
use openusd_schemas::shade;

use crate::geometry::UvSet;
use crate::stage::{value, value_f64, value_token, value_vec3};
use crate::texture::{blend, pack, Channel, Images, Mono};

/// Reads materials, caching per material prim.
pub struct Materials {
    /// Directory of the root layer: the last-resort base for a texture path
    /// the resolver left unresolved.
    root_dir: PathBuf,
    /// `(material prim, double-sided)` → table index: one USD material bound
    /// to both one- and two-sided geometry becomes two sidecar materials,
    /// since glTF puts double-sidedness on the material.
    by_path: HashMap<(sdf::Path, bool), usize>,
    /// Sidecar material table, in first-use order.
    pub table: Vec<Material>,
    /// Per table entry, the UV sets its textures sample, in `uv` index order
    /// (set 0 first). A mesh piece carrying this material writes exactly these
    /// as `TEXCOORD_0..n`.
    pub uv_sets: Vec<Vec<UvSet>>,
    /// Texture files to write next to the sidecar.
    pub images: Images,
    /// Human-readable notes for the export report — anything approximated.
    pub notes: Vec<String>,
}

impl Materials {
    pub fn new(root: &Path) -> Self {
        Self {
            root_dir: root.parent().map(Path::to_path_buf).unwrap_or_default(),
            by_path: HashMap::new(),
            table: Vec::new(),
            uv_sets: Vec::new(),
            images: Images::default(),
            notes: Vec::new(),
        }
    }

    /// The sidecar material index for whatever is bound to `prim`, minting it
    /// on first use. `None` when nothing is bound. `double_sided` is the
    /// geometry's USD `doubleSided`.
    pub fn bound(&mut self, stage: &Stage, prim: &Prim, double_sided: bool) -> Option<usize> {
        let material = bound_material(stage, prim)?;
        let key = (material, double_sided);
        if let Some(i) = self.by_path.get(&key) {
            return Some(*i);
        }
        let (mut read, sets) = self.read(stage, &key.0);
        read.double_sided = double_sided;
        let index = self.intern(read, sets);
        self.by_path.insert(key, index);
        Some(index)
    }

    /// A material for UNBOUND geometry that must still be double-sided: the
    /// importer's own fallback (a material minted from the geom's colour) is
    /// single-sided, so a two-sided sheet with no material gets this instead.
    pub fn plain_double_sided(&mut self, rgba: [f32; 4]) -> usize {
        let mut m = Material::phong(Some("displayColor".into()), rgba, 0.5, 0.5, 0.0, 0.0);
        m.double_sided = true;
        if rgba[3] < 1.0 {
            m.alpha_mode = Some(AlphaMode::Blend);
        }
        self.intern(m, vec![UvSet::Index(0)])
    }

    fn intern(&mut self, read: Material, sets: Vec<UvSet>) -> usize {
        // Isaac assets repeat each look per link file (every Franka link has
        // its own `Looks/PlasticWhite`). Equal materials share one entry, so
        // the editor's library gets ONE PlasticWhite and editing it repaints
        // the whole arm.
        // The UV sets are part of the identity: two materials whose textures
        // both say `uv: 1` mean different primvars if their set lists differ.
        let same = |(m, s): (&Material, &Vec<UvSet>)| *m == read && *s == sets;
        match self.table.iter().zip(&self.uv_sets).position(same) {
            Some(i) => i,
            None => {
                self.table.push(read);
                self.uv_sets.push(sets);
                self.table.len() - 1
            }
        }
    }

    fn read(&mut self, stage: &Stage, path: &sdf::Path) -> (Material, Vec<UvSet>) {
        let name = path.name().map(str::to_string);
        let grey = || {
            (
                Material::phong(name.clone(), [0.5, 0.5, 0.5, 1.0], 0.5, 0.5, 0.0, 0.0),
                vec![UvSet::Index(0)],
            )
        };
        if !matches!(shade::Material::get(stage, path.clone()), Ok(Some(_))) {
            // A dangling binding — it happens in shipped assets (the Franka's
            // `Quality` decals bind a material no layer defines).
            self.notes
                .push(format!("{path}: bound material does not exist, using grey"));
            return grey();
        }
        let Some(shader) = surface_shader(stage, path) else {
            self.notes
                .push(format!("{path}: no surface shader, using grey"));
            return grey();
        };
        let mut ctx = Ctx {
            path: path.clone(),
            shader,
            root_dir: &self.root_dir,
            images: &mut self.images,
            notes: &mut self.notes,
            uv_sets: vec![UvSet::Index(0)],
        };
        let look = match shader_kind(&ctx.shader).as_deref() {
            Some(
                "OmniPBR" | "OmniPBR_ClearCoat" | "OmniPBR_Opacity" | "OmniPBR_ClearCoat_Opacity",
            ) => ctx.omni_pbr(),
            Some("OmniSurface" | "OmniSurfaceBase" | "OmniSurfaceLite") => ctx.omni_surface(),
            Some("OmniGlass") => Look {
                base: ctx.color("glass_color").unwrap_or([1.0; 3]),
                alpha: 0.3,
                roughness: 0.05,
                alpha_mode: Some(AlphaMode::Blend),
                ..Look::default()
            },
            Some("UsdPreviewSurface") => ctx.preview_surface(),
            other => {
                self.notes
                    .push(format!("{path}: unsupported shader {other:?}, using grey"));
                return grey();
            }
        };
        let sets = std::mem::take(&mut ctx.uv_sets);
        (look.into_material(name), sets)
    }
}

/// What a surface shader boils down to before it becomes a sidecar material.
struct Look {
    base: [f32; 3],
    alpha: f32,
    roughness: f32,
    metallic: f32,
    emissive: Option<[f32; 3]>,
    alpha_mode: Option<AlphaMode>,
    textures: MaterialTextures,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            base: [1.0; 3],
            alpha: 1.0,
            roughness: 0.5,
            metallic: 0.0,
            emissive: None,
            alpha_mode: None,
            textures: MaterialTextures::default(),
        }
    }
}

impl Look {
    fn into_material(self, name: Option<String>) -> Material {
        let clamp = |v: f32| v.clamp(0.0, 1.0);
        let mut m = Material::phong(
            name,
            [
                clamp(self.base[0]),
                clamp(self.base[1]),
                clamp(self.base[2]),
                clamp(self.alpha),
            ],
            // Unused by the importer (a dielectric's specular is fixed in
            // metallic-roughness); MuJoCo's own default.
            0.5,
            clamp(1.0 - self.roughness),
            clamp(self.metallic),
            0.0,
        );
        if let Some(e) = self.emissive.filter(|e| e.iter().any(|v| *v > 0.0)) {
            // No physical units in the renderer: a glTF emissive factor is
            // 0..1, so an over-bright colour keeps its hue at full strength.
            let peak = e.iter().copied().fold(0.0f32, f32::max);
            let e = if peak > 1.0 { e.map(|v| v / peak) } else { e };
            // `emission` stays 0: an older reader, which knows only
            // `rgba × emission`, would otherwise glow in the BASE colour (the
            // Franka's white LED housing glowing white) — no glow is closer.
            m.emissive = Some(e);
        }
        m.alpha_mode = self.alpha_mode;
        m.textures = self.textures;
        m
    }
}

/// One shader being read.
struct Ctx<'a> {
    path: sdf::Path,
    shader: Prim,
    root_dir: &'a Path,
    images: &'a mut Images,
    notes: &'a mut Vec<String>,
    /// The UV sets this material's textures sample, in `uv` index order.
    uv_sets: Vec<UvSet>,
}

/// A texture feeding an input: the file, which channel the input reads, and
/// how it is sampled.
#[derive(Clone)]
struct Tex {
    authored: String,
    file: PathBuf,
    channel: Channel,
    transform: Option<UvTransform>,
    wrap: [Wrap; 2],
    /// A UsdUVTexture's `scale.rgb` — a colour multiplier on what it reads.
    /// OmniPBR maps have none (1).
    scale: [f32; 3],
    /// The UV set it samples.
    uv: UvSet,
}

impl Ctx<'_> {
    fn note(&mut self, msg: impl std::fmt::Display) {
        self.notes.push(format!("{}: {msg}", self.path));
    }

    /// `set`'s index in this material's UV set list, adding it if new.
    fn uv_index(&mut self, set: &UvSet) -> u32 {
        match self.uv_sets.iter().position(|s| s == set) {
            Some(i) => i as u32,
            None => {
                self.uv_sets.push(set.clone());
                (self.uv_sets.len() - 1) as u32
            }
        }
    }

    fn slot(&mut self, image: String, t: &Tex) -> MaterialTexture {
        MaterialTexture {
            image,
            transform: t.transform,
            wrap: t.wrap,
            uv: self.uv_index(&t.uv),
        }
    }

    // ── OmniPBR ──────────────────────────────────────────────────────────────
    //
    // Defaults are OmniPBR.mdl's own (v2.1), for inputs the asset leaves
    // unauthored.

    fn omni_pbr(&mut self) -> Look {
        let transform = self.omni_transform("texture_scale", "texture_rotate", "texture_translate");
        // Every OmniPBR map samples one UV set, `uv_space_index`.
        let uv = UvSet::Index(self.int("uv_space_index").unwrap_or(0).max(0) as u32);
        let tex = |ctx: &mut Self, name: &str, channel: Channel| {
            ctx.asset_texture(name, channel, transform, [Wrap::Repeat; 2], uv.clone())
        };
        for (input, what) in [
            ("detail_normalmap_texture", "detail normal map"),
            ("clearcoat_normalmap_texture", "clearcoat normal map"),
        ] {
            if self.asset_input(input).is_some() {
                self.note(format!("{what} dropped (no glTF slot)"));
            }
        }
        for (input, default) in [("albedo_desaturation", 0.0), ("albedo_add", 0.0)] {
            if self
                .float(input)
                .is_some_and(|v| (v - default).abs() > 1e-6)
            {
                self.note(format!("{input} ignored (no glTF equivalent)"));
            }
        }

        // Base colour: a bound diffuse_texture REPLACES the constant; tint and
        // brightness multiply either.
        let tint = self.color("diffuse_tint").unwrap_or([1.0; 3]);
        let brightness = self.float("albedo_brightness").unwrap_or(1.0);
        let diffuse = tex(self, "diffuse_texture", Channel::Rgb);
        let base_constant = self.color("diffuse_color_constant").unwrap_or([0.2; 3]);

        // Opacity.
        let mut alpha = 1.0;
        let mut alpha_map = None;
        let mut alpha_mode = None;
        if self.bool("enable_opacity").unwrap_or(false) {
            let threshold = self.float("opacity_threshold").unwrap_or(0.0);
            let mono = Channel::from_mono_mode(self.int("opacity_mode").unwrap_or(1));
            let textured = self.bool("enable_opacity_texture").unwrap_or(false);
            alpha_map = if textured {
                tex(self, "opacity_texture", mono)
            } else {
                None
            };
            alpha = self.float("opacity_constant").unwrap_or(1.0);
            alpha_mode = Some(if threshold > 0.0 {
                AlphaMode::Mask {
                    cutoff: threshold.min(1.0),
                }
            } else if alpha_map.is_some() || alpha < 1.0 {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            });
            if alpha_map.is_some() {
                // The map is the opacity; the constant only applies without one.
                alpha = 1.0;
            }
        }

        let (base, base_color) = self.base_color(diffuse, base_constant, alpha_map);
        let base = mul(mul(base, tint), [brightness; 3]);

        // Metallic-roughness: ORM when enabled, else each map blended over its
        // constant by its influence (which defaults to 0 — an authored map
        // with no influence does nothing, in OmniPBR as here).
        let mut roughness = self.float("reflection_roughness_constant").unwrap_or(0.5);
        let mut metallic = self.float("metallic_constant").unwrap_or(0.0);
        let mut textures = MaterialTextures {
            base_color,
            ..MaterialTextures::default()
        };
        let orm = if self.bool("enable_ORM_texture").unwrap_or(false) {
            tex(self, "ORM_texture", Channel::Rgb)
        } else {
            None
        };
        if let Some(orm) = &orm {
            // ORM is glTF's own packing: R occlusion, G roughness, B metallic.
            textures.metallic_roughness = self.copy(orm);
            roughness = 1.0;
            metallic = 1.0;
        } else {
            let rough_map = tex(self, "reflectionroughness_texture", Channel::R);
            let metal_map = tex(self, "metallic_texture", Channel::R);
            let rough_inf = self
                .float("reflection_roughness_texture_influence")
                .unwrap_or(0.0);
            let metal_inf = self.float("metallic_texture_influence").unwrap_or(0.0);
            if (rough_map.is_some() && rough_inf > 0.0) || (metal_map.is_some() && metal_inf > 0.0)
            {
                let rough = blend(roughness, self.gray(rough_map.as_ref()), rough_inf);
                let metal = blend(metallic, self.gray(metal_map.as_ref()), metal_inf);
                let packed = pack(
                    None,
                    [&Mono::Constant(1.0), &rough, &metal, &Mono::Constant(1.0)],
                );
                let sampling = rough_map.as_ref().or(metal_map.as_ref());
                textures.metallic_roughness =
                    self.write_png("metallic_roughness", &packed, sampling);
                roughness = 1.0;
                metallic = 1.0;
            } else {
                for (map, name) in [
                    (&rough_map, "reflectionroughness_texture"),
                    (&metal_map, "metallic_texture"),
                ] {
                    if map.is_some() {
                        self.note(format!(
                            "{name} has influence 0, so (as in OmniPBR) it has no effect"
                        ));
                    }
                }
            }
        }

        // Occlusion: OmniPBR applies AO only to the diffuse term, weighted by
        // ao_to_diffuse (default 0 = off) — glTF's occlusion strength.
        let ao_strength = self.float("ao_to_diffuse").unwrap_or(0.0);
        if ao_strength > 0.0 {
            let ao = tex(self, "ao_texture", Channel::R).or_else(|| orm.clone());
            if let Some(ao) = ao {
                textures.occlusion = self.copy(&ao);
                textures.occlusion_strength = ao_strength.min(1.0);
            }
        }

        // Normal map. OmniPBR's defaults (flip_tangent_u = false,
        // flip_tangent_v = true) read an OpenGL-convention (+Y) map, which is
        // glTF's; a non-default flip inverts that channel.
        if let Some(n) = tex(self, "normalmap_texture", Channel::Rgb) {
            let flip_u = self.bool("flip_tangent_u").unwrap_or(false);
            let flip_v = !self.bool("flip_tangent_v").unwrap_or(true);
            textures.normal = if flip_u || flip_v {
                self.flipped_normal(&n, flip_u, flip_v)
            } else {
                self.copy(&n)
            };
            textures.normal_scale = self.float("bump_factor").unwrap_or(1.0);
        }

        // Emission: colour (or a colour map) × an optional mask. Intensity is
        // in nits in Omniverse; the renderer has no physical units, so the
        // colour ships at full strength and intensity is dropped.
        let mut emissive = None;
        if self.bool("enable_emission").unwrap_or(false) {
            let color = self.color("emissive_color").unwrap_or([1.0, 0.1, 0.1]);
            let color_map = tex(self, "emissive_color_texture", Channel::Rgb);
            let mask = tex(self, "emissive_mask_texture", Channel::Average);
            emissive = Some(if color_map.is_some() { [1.0; 3] } else { color });
            textures.emissive = self.emissive_map(color_map, mask);
        }

        Look {
            base,
            alpha,
            roughness,
            metallic,
            emissive,
            alpha_mode,
            textures,
        }
    }

    // ── OmniSurface ──────────────────────────────────────────────────────────

    fn omni_surface(&mut self) -> Look {
        let transform = self.omni_transform("texture_scale", "texture_rotate", "texture_translate");
        let diffuse = self.asset_texture(
            "diffuse_reflection_color_image",
            Channel::Rgb,
            transform,
            [Wrap::Repeat; 2],
            UvSet::Index(0),
        );
        let weight = self.float("diffuse_reflection_weight").unwrap_or(0.8);
        let constant = self.color("diffuse_reflection_color").unwrap_or([1.0; 3]);
        let (base, base_color) = self.base_color(diffuse, constant, None);
        let emission_weight = self.float("emission_weight").unwrap_or(0.0);
        let emissive =
            (emission_weight > 0.0).then(|| self.color("emission_color").unwrap_or([1.0; 3]));
        let opacity = self.float("geometry_opacity").unwrap_or(1.0);
        Look {
            base: base.map(|c| c * weight),
            alpha: opacity,
            alpha_mode: (opacity < 1.0).then_some(AlphaMode::Blend),
            roughness: self.float("specular_reflection_roughness").unwrap_or(0.2),
            metallic: self.float("metalness").unwrap_or(0.0),
            emissive,
            textures: MaterialTextures {
                base_color,
                ..MaterialTextures::default()
            },
        }
    }

    // ── UsdPreviewSurface ────────────────────────────────────────────────────

    fn preview_surface(&mut self) -> Look {
        if self.int("useSpecularWorkflow").unwrap_or(0) != 0 {
            self.note("specular workflow read as metallic-roughness (specularColor dropped)");
        }
        let mut textures = MaterialTextures::default();

        // Base colour + opacity.
        let (diffuse, mut base) = match self.input("diffuseColor") {
            Some(Input::Texture(t)) => (Some(t), [1.0; 3]),
            Some(Input::Value(v)) => (None, vec3(&v).unwrap_or([0.18; 3])),
            None => (None, [0.18; 3]),
        };
        let (opacity_map, alpha) = match self.input("opacity") {
            Some(Input::Texture(t)) => (Some(t), 1.0),
            Some(Input::Value(v)) => (None, value_f64(&v).unwrap_or(1.0) as f32),
            None => (None, 1.0),
        };
        let threshold = self.float("opacityThreshold").unwrap_or(0.0);
        let alpha_mode = if threshold > 0.0 {
            Some(AlphaMode::Mask {
                cutoff: threshold.min(1.0),
            })
        } else if opacity_map.is_some() || alpha < 1.0 {
            Some(AlphaMode::Blend)
        } else {
            None
        };
        let diffuse_scale = diffuse.as_ref().map_or([1.0; 3], |t| t.scale);
        let (b, base_color) = self.base_color(diffuse, base, opacity_map);
        // The texture node's scale multiplies what it reads: glTF's factor.
        base = if base_color.is_some() {
            mul(b, diffuse_scale)
        } else {
            b
        };
        textures.base_color = base_color;

        // Roughness + metallic: either may be a map (any channel).
        let (rough_map, roughness) = self.mono_input("roughness", 0.5);
        let (metal_map, metallic) = self.mono_input("metallic", 0.0);
        let (roughness, metallic) = if rough_map.is_some() || metal_map.is_some() {
            let rough = match &rough_map {
                Some(t) => Mono::Image(self.gray(Some(t)).unwrap_or_else(|| flat(roughness))),
                None => Mono::Constant(roughness),
            };
            let metal = match &metal_map {
                Some(t) => Mono::Image(self.gray(Some(t)).unwrap_or_else(|| flat(metallic))),
                None => Mono::Constant(metallic),
            };
            let packed = pack(
                None,
                [&Mono::Constant(1.0), &rough, &metal, &Mono::Constant(1.0)],
            );
            if let (Some(r), Some(m)) = (&rough_map, &metal_map) {
                if r.uv != m.uv || r.transform != m.transform {
                    self.note("roughness and metallic maps sample different UVs; packed on the roughness map's");
                }
            }
            let sampling = rough_map.as_ref().or(metal_map.as_ref());
            textures.metallic_roughness = self.write_png("metallic_roughness", &packed, sampling);
            (1.0, 1.0)
        } else {
            (roughness, metallic)
        };

        // Normal: the image is the tangent-space map; UsdUVTexture's scale/bias
        // only decode it to [-1, 1], which glTF does implicitly.
        if let Some(Input::Texture(t)) = self.input("normal") {
            textures.normal = self.copy(&t);
        }
        if let Some(Input::Texture(t)) = self.input("occlusion") {
            textures.occlusion = if t.channel == Channel::R || t.channel == Channel::Rgb {
                self.copy(&t)
            } else {
                let gray = self.gray(Some(&t));
                let packed = gray.map(|g| {
                    pack(
                        None,
                        [
                            &Mono::Image(g),
                            &Mono::Constant(1.0),
                            &Mono::Constant(1.0),
                            &Mono::Constant(1.0),
                        ],
                    )
                });
                packed.and_then(|p| self.write_png("occlusion", &p, Some(&t)))
            };
        }
        let emissive = match self.input("emissiveColor") {
            Some(Input::Texture(t)) => {
                let factor = t.scale;
                textures.emissive = self.copy(&t);
                Some(factor)
            }
            Some(Input::Value(v)) => vec3(&v).filter(|c| c.iter().any(|x| *x > 0.0)),
            None => None,
        };

        Look {
            base,
            alpha,
            roughness,
            metallic,
            emissive,
            alpha_mode,
            textures,
        }
    }

    // ── assembling slots ─────────────────────────────────────────────────────

    /// The base-colour factor and texture from a colour map, a constant, and
    /// an optional opacity map (packed into alpha).
    fn base_color(
        &mut self,
        diffuse: Option<Tex>,
        constant: [f32; 3],
        alpha_map: Option<Tex>,
    ) -> ([f32; 3], Option<MaterialTexture>) {
        if let (Some(d), Some(a)) = (&diffuse, &alpha_map) {
            if d.uv != a.uv || d.transform != a.transform {
                self.note(
                    "colour and opacity maps sample different UVs; packed on the colour map's",
                );
            }
        }
        let diffuse_img = diffuse.as_ref().and_then(|t| self.decode(t));
        let alpha_img = alpha_map.as_ref().and_then(|t| self.gray(Some(t)));
        match (diffuse_img, alpha_img) {
            (Some(_), None) => {
                let d = diffuse.expect("decoded above");
                ([1.0; 3], self.copy(&d))
            }
            (Some(rgb), Some(a)) => {
                let d = diffuse.expect("decoded above");
                // The same RGBA file for colour and alpha (a PNG whose alpha IS
                // the opacity) ships untouched.
                if alpha_map
                    .as_ref()
                    .is_some_and(|m| m.file == d.file && m.channel == Channel::A)
                {
                    return ([1.0; 3], self.copy(&d));
                }
                let one = Mono::Constant(1.0);
                let packed = pack(Some(&rgb), [&one, &one, &one, &Mono::Image(a)]);
                ([1.0; 3], self.write_png("base_color", &packed, Some(&d)))
            }
            (None, Some(a)) => {
                let one = Mono::Constant(1.0);
                let packed = pack(None, [&one, &one, &one, &Mono::Image(a)]);
                (
                    constant,
                    self.write_png("opacity", &packed, alpha_map.as_ref()),
                )
            }
            (None, None) => (constant, None),
        }
    }

    fn emissive_map(&mut self, color: Option<Tex>, mask: Option<Tex>) -> Option<MaterialTexture> {
        let mask_img = mask.as_ref().and_then(|t| self.gray(Some(t)));
        match (color, mask_img) {
            (Some(c), None) => self.copy(&c),
            (color, Some(m)) => {
                let rgb = color.as_ref().and_then(|t| self.decode(t));
                let rgb = match rgb {
                    Some(img) => {
                        let m = image::imageops::resize(
                            &m,
                            img.width(),
                            img.height(),
                            image::imageops::FilterType::Triangle,
                        );
                        RgbaImage::from_fn(img.width(), img.height(), |x, y| {
                            let k = m.get_pixel(x, y).0[0] as u16;
                            let p = img.get_pixel(x, y).0;
                            let s = |c: u8| ((c as u16 * k) / 255) as u8;
                            image::Rgba([s(p[0]), s(p[1]), s(p[2]), 255])
                        })
                    }
                    None => {
                        let mono = Mono::Image(m);
                        pack(None, [&mono, &mono, &mono, &Mono::Constant(1.0)])
                    }
                };
                let sampling = color.as_ref().or(mask.as_ref()).cloned();
                self.write_png("emissive", &rgb, sampling.as_ref())
            }
            (None, None) => None,
        }
    }

    fn flipped_normal(&mut self, n: &Tex, flip_u: bool, flip_v: bool) -> Option<MaterialTexture> {
        let mut img = self.decode(n)?;
        for p in img.pixels_mut() {
            if flip_u {
                p.0[0] = 255 - p.0[0];
            }
            if flip_v {
                p.0[1] = 255 - p.0[1];
            }
        }
        self.write_png("normal", &img, Some(n))
    }

    // ── images ───────────────────────────────────────────────────────────────

    fn copy(&mut self, t: &Tex) -> Option<MaterialTexture> {
        match self.images.copy(&t.file) {
            Ok(image) => Some(self.slot(image, t)),
            Err(e) => {
                self.note(format!("texture {} could not be read ({e:#})", t.authored));
                None
            }
        }
    }

    fn write_png(
        &mut self,
        stem: &str,
        img: &RgbaImage,
        sampling: Option<&Tex>,
    ) -> Option<MaterialTexture> {
        let stem = format!("{}_{stem}", self.path.name().unwrap_or("material"));
        match self.images.png(&stem, img) {
            Ok(image) => Some(match sampling {
                Some(t) => self.slot(image, t),
                None => MaterialTexture::new(image),
            }),
            Err(e) => {
                self.note(format!("could not encode {stem} ({e:#})"));
                None
            }
        }
    }

    fn decode(&mut self, t: &Tex) -> Option<RgbaImage> {
        match self.images.decode(&t.file) {
            Ok(Some(img)) => Some(img),
            _ => {
                self.note(format!(
                    "texture {} could not be read ({})",
                    t.authored,
                    t.file.display()
                ));
                None
            }
        }
    }

    fn gray(&mut self, t: Option<&Tex>) -> Option<GrayImage> {
        let t = t?;
        match self.images.channel(&t.file, t.channel) {
            Ok(Some(img)) => Some(img),
            _ => {
                self.note(format!(
                    "texture {} could not be read ({})",
                    t.authored,
                    t.file.display()
                ));
                None
            }
        }
    }

    // ── inputs ───────────────────────────────────────────────────────────────

    fn input(&mut self, name: &str) -> Option<Input> {
        let input = resolve_input(&self.shader, name, 0)?;
        Some(match input {
            Resolved::Value(v) => Input::Value(v),
            Resolved::Connected {
                asset,
                channel,
                node,
            } => {
                let file = self.resolve_file(&asset)?;
                let (transform, wrap, uv) = self.node_sampling(&node);
                let four = |n: &str, d: f32| match value(&node.attribute(format!("inputs:{n}"))) {
                    Some(sdf::Value::Vec4f(v)) => [v.x, v.y, v.z, v.w],
                    Some(sdf::Value::Vec4d(v)) => [v.x as f32, v.y as f32, v.z as f32, v.w as f32],
                    _ => [d; 4],
                };
                let (scale, bias) = (four("scale", 1.0), four("bias", 0.0));
                // A normal map's scale/bias only decode [0,1] to [-1,1], which
                // glTF does implicitly; anywhere else a bias is an offset glTF
                // cannot express.
                if name != "normal" && bias.iter().any(|b| *b != 0.0) {
                    self.note(format!("{name}: UsdUVTexture bias {bias:?} ignored"));
                }
                Input::Texture(Tex {
                    authored: asset.authored_path.clone(),
                    file,
                    channel,
                    transform,
                    wrap,
                    scale: [scale[0], scale[1], scale[2]],
                    uv,
                })
            }
        })
    }

    /// A mono input that may be a constant or a map (UsdPreviewSurface).
    fn mono_input(&mut self, name: &str, default: f32) -> (Option<Tex>, f32) {
        match self.input(name) {
            Some(Input::Texture(t)) => (Some(t), default),
            Some(Input::Value(v)) => (None, value_f64(&v).map_or(default, |x| x as f32)),
            None => (None, default),
        }
    }

    /// An asset-valued input (OmniPBR's `diffuse_texture`), as a texture.
    fn asset_input(&self, name: &str) -> Option<sdf::AssetPath> {
        match resolve_input(&self.shader, name, 0)? {
            Resolved::Value(sdf::Value::AssetPath(a)) if !a.authored_path.is_empty() => Some(a),
            Resolved::Connected { asset, .. } => Some(asset),
            _ => None,
        }
    }

    fn asset_texture(
        &mut self,
        name: &str,
        channel: Channel,
        transform: Option<UvTransform>,
        wrap: [Wrap; 2],
        uv: UvSet,
    ) -> Option<Tex> {
        let asset = self.asset_input(name)?;
        let file = self.resolve_file(&asset)?;
        Some(Tex {
            authored: asset.authored_path,
            file,
            channel,
            transform,
            wrap,
            scale: [1.0; 3],
            uv,
        })
    }

    fn resolve_file(&mut self, asset: &sdf::AssetPath) -> Option<PathBuf> {
        let file = match asset.resolved_path() {
            Some(r) if !r.is_empty() => PathBuf::from(r),
            _ => self.root_dir.join(&asset.authored_path),
        };
        if file.is_file() {
            Some(file)
        } else {
            self.note(format!(
                "texture {} not found ({}), slot dropped",
                asset.authored_path,
                file.display()
            ));
            None
        }
    }

    /// OmniPBR/OmniSurface's shader-level UV transform, applied to every map.
    fn omni_transform(
        &mut self,
        scale: &str,
        rotate: &str,
        translate: &str,
    ) -> Option<UvTransform> {
        if self.bool("project_uvw").unwrap_or(false) {
            self.note(
                "project_uvw (world/object-space projection) is not supported; mesh UVs used",
            );
        }
        let s = self.vec2(scale).unwrap_or([1.0, 1.0]);
        let r = self.float(rotate).unwrap_or(0.0);
        let t = self.vec2(translate).unwrap_or([0.0, 0.0]);
        usd_uv_transform(s, r, t)
    }

    /// A UsdUVTexture's wrap modes, its `st` input's UsdTransform2d, and the
    /// UV set its primvar reader names.
    fn node_sampling(&mut self, node: &Prim) -> (Option<UvTransform>, [Wrap; 2], UvSet) {
        let wrap = |name: &str, notes: &mut Vec<String>, path: &sdf::Path| match value(
            &node.attribute(format!("inputs:{name}")),
        )
        .and_then(|v| value_token(&v))
        .as_deref()
        {
            Some("mirror") => Wrap::Mirror,
            Some("clamp") => Wrap::Clamp,
            Some("black") => {
                notes.push(format!("{path}: wrap `black` approximated as clamp"));
                Wrap::Clamp
            }
            _ => Wrap::Repeat,
        };
        let wrap = [
            wrap("wrapS", self.notes, &self.path),
            wrap("wrapT", self.notes, &self.path),
        ];
        // `st` ← [UsdTransform2d `in` ←] UsdPrimvarReader_float2 `varname`.
        let st = connected_prim(node, "st");
        let is = |p: &Prim, id: &str| {
            value(&p.attribute("info:id"))
                .and_then(|v| value_token(&v))
                .is_some_and(|t| t.starts_with(id))
        };
        let reader = match &st {
            Some(p) if is(p, "UsdTransform2d") => connected_prim(p, "in"),
            other => other.clone(),
        };
        let uv = reader
            .filter(|r| is(r, "UsdPrimvarReader"))
            .and_then(|r| match resolve_input(&r, "varname", 0) {
                Some(Resolved::Value(v)) => value_token(&v),
                _ => None,
            })
            .map_or(UvSet::Index(0), UvSet::Name);
        let transform = st.and_then(|t| {
            let id = value(&t.attribute("info:id")).and_then(|v| value_token(&v));
            (id.as_deref() == Some("UsdTransform2d")).then(|| {
                let v2 = |n: &str, d: [f32; 2]| match value(&t.attribute(format!("inputs:{n}"))) {
                    Some(sdf::Value::Vec2f(v)) => [v.x, v.y],
                    Some(sdf::Value::Vec2d(v)) => [v.x as f32, v.y as f32],
                    _ => d,
                };
                let rot = value(&t.attribute("inputs:rotation"))
                    .and_then(|v| value_f64(&v))
                    .unwrap_or(0.0) as f32;
                usd_uv_transform(v2("scale", [1.0, 1.0]), rot, v2("translation", [0.0, 0.0]))
            })
        });
        (transform.flatten(), wrap, uv)
    }

    fn color(&self, name: &str) -> Option<[f32; 3]> {
        match resolve_input(&self.shader, name, 0)? {
            Resolved::Value(v) => vec3(&v),
            Resolved::Connected { .. } => None,
        }
    }

    fn vec2(&self, name: &str) -> Option<[f32; 2]> {
        match resolve_input(&self.shader, name, 0)? {
            Resolved::Value(sdf::Value::Vec2f(v)) => Some([v.x, v.y]),
            Resolved::Value(sdf::Value::Vec2d(v)) => Some([v.x as f32, v.y as f32]),
            _ => None,
        }
    }

    fn float(&self, name: &str) -> Option<f32> {
        match resolve_input(&self.shader, name, 0)? {
            Resolved::Value(v) => value_f64(&v).map(|x| x as f32),
            Resolved::Connected { .. } => None,
        }
    }

    fn int(&self, name: &str) -> Option<i64> {
        match resolve_input(&self.shader, name, 0)? {
            Resolved::Value(sdf::Value::Int(i)) => Some(i as i64),
            Resolved::Value(sdf::Value::Int64(i)) => Some(i),
            Resolved::Value(sdf::Value::Uint(i)) => Some(i as i64),
            Resolved::Value(v) => value_f64(&v).map(|x| x as i64),
            Resolved::Connected { .. } => None,
        }
    }

    fn bool(&self, name: &str) -> Option<bool> {
        match resolve_input(&self.shader, name, 0)? {
            Resolved::Value(sdf::Value::Bool(b)) => Some(b),
            Resolved::Value(v) => value_f64(&v).map(|x| x != 0.0),
            Resolved::Connected { .. } => None,
        }
    }
}

fn flat(v: f32) -> GrayImage {
    GrayImage::from_pixel(1, 1, Luma([(v.clamp(0.0, 1.0) * 255.0).round() as u8]))
}

fn vec3(v: &sdf::Value) -> Option<[f32; 3]> {
    value_vec3(v).map(|c| c.map(|x| x as f32))
}

fn mul(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2]]
}

/// A USD UV transform (scale, then rotate `rotation_deg` counter-clockwise,
/// then translate — `UsdTransform2d`'s order) as a glTF `KHR_texture_transform`
/// in the GLB's UV space, or `None` for the identity.
///
/// The GLB's V runs top-down (`v_gl = 1 - v_usd`, flipped at export), so the
/// transform is conjugated by that flip `F`: `M_gl = F · M_usd · F`. Its linear
/// part is `R(-θ)·S`, which is exactly glTF's rotation matrix at the SAME angle
/// (glTF rotates "counter-clockwise with V pointing down"), and its offset is
/// `F(R·S·e + t)` with `e = (0, 1)`.
pub fn usd_uv_transform(
    scale: [f32; 2],
    rotation_deg: f32,
    translation: [f32; 2],
) -> Option<UvTransform> {
    let identity = scale == [1.0, 1.0] && rotation_deg == 0.0 && translation == [0.0, 0.0];
    if identity {
        return None;
    }
    let theta = rotation_deg.to_radians();
    let (s, c) = theta.sin_cos();
    // R·S·e for e = (0, 1): the rotated, scaled V axis.
    let rse = [-s * scale[1], c * scale[1]];
    let offset = [translation[0] + rse[0], 1.0 - (translation[1] + rse[1])];
    Some(UvTransform {
        offset,
        rotation: theta,
        scale,
    })
}

/// The material bound to `prim` (all-purpose), resolved through ancestors.
///
/// `compute_bound_material` walks the ancestor chain itself, but it is reached
/// through a `MaterialBindingAPI` view, which only exists on prims that apply
/// the API — so start from the nearest ancestor-or-self that does.
fn bound_material(stage: &Stage, prim: &Prim) -> Option<sdf::Path> {
    let mut path = Some(prim.path().clone());
    while let Some(p) = path {
        if p.is_abs_root() {
            break;
        }
        if let Ok(Some(api)) = shade::MaterialBindingAPI::get(stage, p.clone()) {
            if let Ok(Some(m)) = api.compute_bound_material("") {
                return Some(m);
            }
        }
        path = p.parent();
    }
    None
}

/// The shader driving the material's surface: the MDL terminal when there is
/// one (what Isaac authors), otherwise the universal (UsdPreviewSurface) one.
fn surface_shader(stage: &Stage, path: &sdf::Path) -> Option<Prim> {
    let material = shade::Material::get(stage, path.clone()).ok()??;
    let terminal = material.compute_surface_source(&["mdl"]).ok()??;
    let source = terminal.sources().first()?;
    source.shader().map(|s| {
        use openusd::usd::SchemaBase;
        s.prim().clone()
    })
}

/// `OmniPBR`, `UsdPreviewSurface`, ...: the MDL sub-identifier when the shader
/// is an MDL source asset, else its `info:id`.
fn shader_kind(shader: &Prim) -> Option<String> {
    value(&shader.attribute("info:mdl:sourceAsset:subIdentifier"))
        .and_then(|v| value_token(&v))
        .or_else(|| {
            // An MDL module with no sub-identifier: the module file name is the
            // material name (`OmniPBR.mdl` → `OmniPBR`).
            match value(&shader.attribute("info:mdl:sourceAsset")) {
                Some(sdf::Value::AssetPath(a)) => Path::new(&a.authored_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string),
                _ => None,
            }
        })
        .or_else(|| value(&shader.attribute("info:id")).and_then(|v| value_token(&v)))
}

/// An input as the material sees it.
enum Input {
    Value(sdf::Value),
    Texture(Tex),
}

/// An input after following connections.
enum Resolved {
    Value(sdf::Value),
    /// Connected to a texture node's output (UsdUVTexture `outputs:rgb`, ...).
    Connected {
        asset: sdf::AssetPath,
        channel: Channel,
        node: Prim,
    },
}

fn resolve_input(prim: &Prim, name: &str, depth: usize) -> Option<Resolved> {
    let attr = prim.attribute(format!("inputs:{name}"));
    resolve_attr(prim, &attr, depth)
}

/// Follow connections: to a texture node's output (its `inputs:file`), or to
/// an interface input on the material / a node graph.
fn resolve_attr(prim: &Prim, attr: &Attribute, depth: usize) -> Option<Resolved> {
    // A malformed network can connect in a cycle; real ones are a few hops.
    if depth > 8 {
        return None;
    }
    let connections = attr.connections().ok().unwrap_or_default();
    if let Some(target) = connections.first() {
        let source = prim.stage().prim(target.prim_path()).ok()?;
        let prop = property_name(target)?;
        if prop.starts_with("outputs:") {
            return match value(&source.attribute("inputs:file")) {
                Some(sdf::Value::AssetPath(asset)) => Some(Resolved::Connected {
                    asset,
                    channel: Channel::from_output(&prop),
                    node: source,
                }),
                // A node we cannot evaluate (a procedural, a math node).
                _ => None,
            };
        }
        return resolve_attr(&source, &source.attribute(prop), depth + 1);
    }
    value(attr).map(Resolved::Value)
}

/// The property part of a property path (`outputs:rgb` of
/// `/Looks/M/Tex.outputs:rgb`). `sdf::Path::name` returns the whole final
/// segment, prim name included.
fn property_name(path: &sdf::Path) -> Option<String> {
    let segment = path.as_str().rsplit('/').next()?;
    segment.split_once('.').map(|(_, prop)| prop.to_string())
}

/// The prim connected to `node`'s `inputs:<name>`.
fn connected_prim(node: &Prim, name: &str) -> Option<Prim> {
    let target = node
        .attribute(format!("inputs:{name}"))
        .connections()
        .ok()?
        .into_iter()
        .next()?;
    node.stage().prim(target.prim_path()).ok()
}

#[cfg(test)]
mod tests {
    use super::usd_uv_transform;

    /// Apply a glTF transform exactly as the renderer does (`[c s; -s c]·S`,
    /// then the offset).
    fn gl(t: &awsm_renderer_mujoco_format::sidecar::UvTransform, uv: [f32; 2]) -> [f32; 2] {
        let (s, c) = t.rotation.sin_cos();
        let (x, y) = (uv[0] * t.scale[0], uv[1] * t.scale[1]);
        [c * x + s * y + t.offset[0], -s * x + c * y + t.offset[1]]
    }

    /// Apply the USD transform in USD's UV space (scale, rotate CCW, translate).
    fn usd(scale: [f32; 2], deg: f32, t: [f32; 2], uv: [f32; 2]) -> [f32; 2] {
        let (s, c) = deg.to_radians().sin_cos();
        let (x, y) = (uv[0] * scale[0], uv[1] * scale[1]);
        [c * x - s * y + t[0], s * x + c * y + t[1]]
    }

    #[test]
    fn uv_transforms_survive_the_v_flip() {
        for (scale, deg, t) in [
            ([2.0, 3.0], 0.0, [0.25, 0.5]),
            ([1.0, 1.0], 30.0, [0.0, 0.0]),
            ([0.5, 2.0], -75.0, [0.1, -0.3]),
        ] {
            let g = usd_uv_transform(scale, deg, t).expect("not identity");
            for uv_usd in [[0.0, 0.0], [1.0, 0.0], [0.3, 0.8], [1.0, 1.0]] {
                // The same surface point in each space samples the same texel.
                let uv_gl = [uv_usd[0], 1.0 - uv_usd[1]];
                let want = usd(scale, deg, t, uv_usd);
                let want_gl = [want[0], 1.0 - want[1]];
                let got = gl(&g, uv_gl);
                assert!(
                    (got[0] - want_gl[0]).abs() < 1e-5 && (got[1] - want_gl[1]).abs() < 1e-5,
                    "{scale:?} {deg}° {t:?} at {uv_usd:?}: {got:?} vs {want_gl:?}"
                );
            }
        }
        assert!(usd_uv_transform([1.0, 1.0], 0.0, [0.0, 0.0]).is_none());
    }
}
