//! Reading a USD material down to the sidecar's flat material.
//!
//! Isaac assets author **MDL** materials — almost always NVIDIA's `OmniPBR` —
//! with no `UsdPreviewSurface` fallback. We never need the `.mdl` itself (it is a
//! built-in Omniverse module, not shipped with the assets): `OmniPBR` is a
//! metallic-roughness model whose inputs are plain named attributes, so reading
//! those names is the whole job. `UsdPreviewSurface` and `OmniSurface` are read
//! the same way.
//!
//! The sidecar material is MuJoCo-shaped (`rgba`, `shininess`, `reflectance`,
//! `emission`), and the importer maps it to PBR as `roughness = 1 - shininess`,
//! `metallic = reflectance`, `emissive = rgba * emission`. So this module writes
//! the exact inverse, and a USD roughness/metallic survives the round trip.
//!
//! Textures do not cross the seam — the sidecar has no texture slot. A texture
//! driving base colour is instead **averaged** into the constant colour, which
//! keeps a textured part (ANYmal's shells) its overall tint rather than turning
//! it grey. Every texture that was flattened this way is reported.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use awsm_renderer_mujoco_format::sidecar::Material;
use openusd::sdf;
use openusd::usd::{Attribute, Prim, Stage};
use openusd_schemas::shade;

use crate::stage::{value, value_f64, value_token, value_vec3};

/// Reads materials, caching per material prim and per texture file.
pub struct Materials {
    /// Directory of the root layer: the last-resort base for a texture path the
    /// resolver left unresolved.
    root_dir: PathBuf,
    by_path: HashMap<sdf::Path, (usize, Material)>,
    texture_means: HashMap<PathBuf, Option<[f32; 3]>>,
    /// Sidecar material table, in first-use order.
    pub table: Vec<Material>,
    /// Human-readable notes for the export report (flattened textures, unknown
    /// shaders).
    pub notes: Vec<String>,
}

impl Materials {
    pub fn new(root: &Path) -> Self {
        Self {
            root_dir: root.parent().map(Path::to_path_buf).unwrap_or_default(),
            by_path: HashMap::new(),
            texture_means: HashMap::new(),
            table: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// The sidecar material index for whatever is bound to `prim`, minting it on
    /// first use. `None` when nothing is bound.
    pub fn bound(&mut self, stage: &Stage, prim: &Prim) -> Option<usize> {
        let material = bound_material(stage, prim)?;
        if let Some((i, _)) = self.by_path.get(&material) {
            return Some(*i);
        }
        let read = self.read(stage, &material);
        let index = self.table.len();
        self.table.push(read.clone());
        self.by_path.insert(material, (index, read));
        Some(index)
    }

    fn read(&mut self, stage: &Stage, path: &sdf::Path) -> Material {
        let name = path.name().map(str::to_string);
        let fallback = Material {
            name: name.clone(),
            rgba: [0.5, 0.5, 0.5, 1.0],
            specular: 0.5,
            shininess: 0.5,
            reflectance: 0.0,
            emission: 0.0,
        };
        if !matches!(shade::Material::get(stage, path.clone()), Ok(Some(_))) {
            // A dangling binding — it happens in shipped assets (the Franka's
            // `Quality` decals bind a material no layer defines).
            self.notes
                .push(format!("{path}: bound material does not exist, using grey"));
            return fallback;
        }
        let Some(shader) = surface_shader(stage, path) else {
            self.notes
                .push(format!("{path}: no surface shader, using grey"));
            return fallback;
        };
        let kind = shader_kind(&shader);
        let look = match kind.as_deref() {
            Some("OmniPBR") | Some("OmniPBR_ClearCoat") | Some("OmniPBR_Opacity") => {
                self.omni_pbr(path, &shader)
            }
            Some("OmniSurface") | Some("OmniSurfaceBase") | Some("OmniSurfaceLite") => {
                self.omni_surface(path, &shader)
            }
            Some("OmniGlass") => Look {
                base: self.color(path, &shader, "glass_color").unwrap_or([1.0; 3]),
                alpha: 0.3,
                roughness: 0.05,
                metallic: 0.0,
                emissive: None,
            },
            Some("UsdPreviewSurface") => self.preview_surface(path, &shader),
            other => {
                self.notes
                    .push(format!("{path}: unsupported shader {other:?}, using grey"));
                return fallback;
            }
        };
        look.into_material(name)
    }

    fn omni_pbr(&mut self, path: &sdf::Path, shader: &Prim) -> Look {
        // OmniPBR: a bound diffuse_texture REPLACES the constant (the constant is
        // only the untextured colour), and diffuse_tint multiplies either.
        let base = match self.texture_mean(path, shader, "diffuse_texture") {
            Some(t) => t,
            None => self
                .color(path, shader, "diffuse_color_constant")
                .unwrap_or([0.2; 3]),
        };
        let tint = self.color(path, shader, "diffuse_tint").unwrap_or([1.0; 3]);
        for tex in [
            "reflectionroughness_texture",
            "metallic_texture",
            "normalmap_texture",
            "ORM_texture",
        ] {
            self.note_texture(path, shader, tex);
        }
        let emissive = self
            .bool(shader, "enable_emission")
            .unwrap_or(false)
            .then(|| {
                self.color(path, shader, "emissive_color")
                    .unwrap_or([1.0, 0.1, 0.1])
            });
        let opacity = if self.bool(shader, "enable_opacity").unwrap_or(false) {
            self.float(shader, "opacity_constant").unwrap_or(1.0)
        } else {
            1.0
        };
        Look {
            base: mul(base, tint),
            alpha: opacity,
            roughness: self
                .float(shader, "reflection_roughness_constant")
                .unwrap_or(0.5),
            metallic: self.float(shader, "metallic_constant").unwrap_or(0.0),
            emissive,
        }
    }

    fn omni_surface(&mut self, path: &sdf::Path, shader: &Prim) -> Look {
        let color = match self.texture_mean(path, shader, "diffuse_reflection_color_image") {
            Some(t) => t,
            None => self
                .color(path, shader, "diffuse_reflection_color")
                .unwrap_or([1.0; 3]),
        };
        let weight = self
            .float(shader, "diffuse_reflection_weight")
            .unwrap_or(0.8);
        let emission_weight = self.float(shader, "emission_weight").unwrap_or(0.0);
        let emissive = (emission_weight > 0.0).then(|| {
            self.color(path, shader, "emission_color")
                .unwrap_or([1.0; 3])
        });
        Look {
            base: color.map(|c| c * weight),
            alpha: self.float(shader, "geometry_opacity").unwrap_or(1.0),
            roughness: self
                .float(shader, "specular_reflection_roughness")
                .unwrap_or(0.2),
            metallic: self.float(shader, "metalness").unwrap_or(0.0),
            emissive,
        }
    }

    fn preview_surface(&mut self, path: &sdf::Path, shader: &Prim) -> Look {
        let base = match self.texture_mean(path, shader, "diffuseColor") {
            Some(t) => t,
            None => self
                .color(path, shader, "diffuseColor")
                .unwrap_or([0.18; 3]),
        };
        for tex in ["roughness", "metallic", "normal", "occlusion"] {
            self.note_texture(path, shader, tex);
        }
        let emissive = self
            .color(path, shader, "emissiveColor")
            .filter(|c| c.iter().any(|v| *v > 0.0));
        Look {
            base,
            alpha: self.float(shader, "opacity").unwrap_or(1.0),
            roughness: self.float(shader, "roughness").unwrap_or(0.5),
            metallic: self.float(shader, "metallic").unwrap_or(0.0),
            emissive,
        }
    }

    // ── inputs ───────────────────────────────────────────────────────────────

    fn color(&mut self, _path: &sdf::Path, shader: &Prim, name: &str) -> Option<[f32; 3]> {
        match resolve_input(shader, name, 0)? {
            Input::Value(v) => value_vec3(&v).map(|c| c.map(|x| x as f32)),
            Input::Texture(_) => None,
        }
    }

    fn float(&self, shader: &Prim, name: &str) -> Option<f32> {
        match resolve_input(shader, name, 0)? {
            Input::Value(v) => value_f64(&v).map(|x| x as f32),
            Input::Texture(_) => None,
        }
    }

    fn bool(&self, shader: &Prim, name: &str) -> Option<bool> {
        match resolve_input(shader, name, 0)? {
            Input::Value(sdf::Value::Bool(b)) => Some(b),
            Input::Value(v) => value_f64(&v).map(|x| x != 0.0),
            Input::Texture(_) => None,
        }
    }

    /// Record a texture this export cannot carry, so the report says what was
    /// dropped rather than silently looking different.
    fn note_texture(&mut self, path: &sdf::Path, shader: &Prim, name: &str) {
        if let Some(Input::Texture(a)) = resolve_input(shader, name, 0) {
            self.notes.push(format!(
                "{path}: {name} texture {} dropped (the sidecar has no texture slots)",
                a.authored_path
            ));
        }
    }

    /// The mean linear colour of the texture feeding `name`, if one does.
    fn texture_mean(&mut self, path: &sdf::Path, shader: &Prim, name: &str) -> Option<[f32; 3]> {
        let asset = match resolve_input(shader, name, 0)? {
            Input::Texture(a) => a,
            // An asset-typed input (OmniPBR's `diffuse_texture`) is itself the
            // texture, not a connection to one.
            Input::Value(sdf::Value::AssetPath(a)) => a,
            Input::Value(_) => return None,
        };
        if asset.authored_path.is_empty() {
            return None;
        }
        let file = match asset.resolved_path() {
            Some(r) if !r.is_empty() => PathBuf::from(r),
            _ => self.root_dir.join(&asset.authored_path),
        };
        let mean = *self
            .texture_means
            .entry(file.clone())
            .or_insert_with(|| mean_linear_color(&file));
        match mean {
            Some(m) => {
                self.notes.push(format!(
                    "{path}: {name} texture {} averaged to base colour",
                    asset.authored_path
                ));
                Some(m)
            }
            None => {
                self.notes.push(format!(
                    "{path}: {name} texture {} could not be read ({})",
                    asset.authored_path,
                    file.display()
                ));
                None
            }
        }
    }
}

/// What a surface shader boils down to before it is squeezed into the sidecar.
struct Look {
    base: [f32; 3],
    alpha: f32,
    roughness: f32,
    metallic: f32,
    emissive: Option<[f32; 3]>,
}

impl Look {
    fn into_material(self, name: Option<String>) -> Material {
        // The sidecar has one colour, and the importer derives emissive as
        // `rgba * emission`. An emissive part (the Franka's blue status LEDs)
        // is therefore drawn in its EMISSIVE colour at full emission — the
        // colour you see when it glows — rather than its unlit base colour.
        let (rgb, emission) = match self.emissive {
            Some(e) if e.iter().any(|v| *v > 0.0) => {
                let peak = e.iter().copied().fold(0.0f32, f32::max);
                (e.map(|v| v / peak), 1.0)
            }
            _ => (self.base, 0.0),
        };
        let clamp = |v: f32| v.clamp(0.0, 1.0);
        Material {
            name,
            rgba: [
                clamp(rgb[0]),
                clamp(rgb[1]),
                clamp(rgb[2]),
                clamp(self.alpha),
            ],
            // Unused by the importer (a dielectric's specular is fixed in
            // metallic-roughness); MuJoCo's own default.
            specular: 0.5,
            shininess: clamp(1.0 - self.roughness),
            reflectance: clamp(self.metallic),
            emission,
        }
    }
}

fn mul(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2]]
}

/// The material bound to `prim` (all-purpose), resolved through ancestors.
///
/// `compute_bound_material` walks the ancestor chain itself, but it is reached
/// through a `MaterialBindingAPI` view, which only exists on prims that apply the
/// API — so start from the nearest ancestor-or-self that does.
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

enum Input {
    Value(sdf::Value),
    Texture(sdf::AssetPath),
}

/// An input's effective value, following connections: to a texture node (its
/// `inputs:file`), or to an interface input on the material / a node graph.
fn resolve_input(prim: &Prim, name: &str, depth: usize) -> Option<Input> {
    let attr = prim.attribute(format!("inputs:{name}"));
    resolve_attr(prim, &attr, depth)
}

fn resolve_attr(prim: &Prim, attr: &Attribute, depth: usize) -> Option<Input> {
    // A malformed network can connect in a cycle; real ones are a few hops.
    if depth > 8 {
        return None;
    }
    let connections = attr.connections().ok().unwrap_or_default();
    if let Some(target) = connections.first() {
        let owner = target.prim_path();
        let source = prim.stage().prim(owner.clone()).ok()?;
        let prop = target.name().unwrap_or_default().to_string();
        if prop.starts_with("outputs:") {
            // Connected to a node's OUTPUT: a texture reader (UsdUVTexture,
            // or any node with a `file` input) — or a node we cannot evaluate.
            return match value(&source.attribute("inputs:file")) {
                Some(sdf::Value::AssetPath(a)) => Some(Input::Texture(a)),
                _ => None,
            };
        }
        // Connected to an interface INPUT: read that one instead.
        return resolve_attr(&source, &source.attribute(prop), depth + 1);
    }
    value(attr).map(Input::Value)
}

/// Mean colour of an image, in LINEAR space (base colour factors are linear;
/// colour textures are sRGB-encoded). Downsampled first: a mean does not need
/// every pixel of a 4K albedo.
fn mean_linear_color(file: &Path) -> Option<[f32; 3]> {
    let img = image::open(file).ok()?.thumbnail(64, 64).to_rgb8();
    let n = (img.width() * img.height()) as f64;
    if n == 0.0 {
        return None;
    }
    let mut acc = [0.0f64; 3];
    for p in img.pixels() {
        for (a, c) in acc.iter_mut().zip(p.0) {
            *a += srgb_to_linear(c as f64 / 255.0);
        }
    }
    Some(acc.map(|a| (a / n) as f32))
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
