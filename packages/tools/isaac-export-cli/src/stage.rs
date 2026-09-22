//! Opening a USD stage and reading it with the few primitives the export needs:
//! variant selection, world transforms, and loosely typed value decoding.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use glam::DMat4;
use openusd::sdf;
use openusd::usd::{Attribute, Prim, SchemaBase, SchemaKind, Stage, TimeCode};
use openusd_schemas::geom::{Imageable, Xformable};

/// The time every value is read at. Isaac assets author their default state as
/// default (non-time-sampled) values; a time-sampled asset is read at frame 0.
pub const TIME: TimeCode = TimeCode::new(0.0);

/// A variant selection requested on the command line: `[PRIM_PATH:]SET=VALUE`.
///
/// Without a prim path it applies to the stage's default prim, which is where
/// every Isaac robot asset authors its variant sets (`Mesh`, `Gripper`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantSelection {
    pub prim: Option<String>,
    pub set: String,
    pub value: String,
}

impl std::str::FromStr for VariantSelection {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (prim, rest) = match s.rsplit_once(':') {
            Some((p, r)) if p.starts_with('/') => (Some(p.to_string()), r),
            _ => (None, s),
        };
        let (set, value) = rest
            .split_once('=')
            .ok_or_else(|| format!("expected [PRIM_PATH:]SET=VALUE, got {s:?}"))?;
        if set.is_empty() || value.is_empty() {
            return Err(format!("expected [PRIM_PATH:]SET=VALUE, got {s:?}"));
        }
        Ok(Self {
            prim,
            set: set.to_string(),
            value: value.to_string(),
        })
    }
}

/// Open `root` with `variants` selected.
///
/// Selections are authored in a generated **session layer** — the standard USD
/// way to override a stage without touching its files — because the Isaac
/// assets author their own selections, and a selection authored in the asset
/// always beats a fallback.
pub fn open(root: &Path, variants: &[VariantSelection]) -> Result<Stage> {
    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("non-UTF-8 path {}", root.display()))?;
    if variants.is_empty() {
        return Stage::open(root_str).with_context(|| format!("opening {}", root.display()));
    }

    // The default prim is needed to place un-pathed selections, so peek at it
    // with a plain open first. Composition is lazy, so this is cheap.
    let default_prim = Stage::open(root_str)
        .with_context(|| format!("opening {}", root.display()))?
        .default_prim()
        .map(|t| t.as_str().to_string());

    let session = SessionLayer::write(variants, default_prim.as_deref())?;
    let stage = Stage::builder()
        .session_layer(session.path_str()?)
        .open(root_str)
        .with_context(|| format!("opening {} with variant selections", root.display()))?;

    // Refuse a selection that did not take — a typo'd set or value would
    // otherwise export the asset's default variant without a word.
    for v in variants {
        let path = selection_prim(v, default_prim.as_deref())?;
        let prim = stage.prim(path.as_str())?;
        // A selection names a variant; it does not create one. An authored
        // selection of a variant that does not exist still READS back as
        // selected, so check that composition actually found the variant: a
        // live variant arc at `<prim>{set=value}`.
        let site = format!("{{{}={}}}", v.set, v.value);
        let composed = prim.prim_index().graph()?.nodes().any(|n| {
            n.arc() == openusd::pcp::ArcType::Variant && n.path().as_str().contains(&site)
        });
        if !composed {
            let selections = prim.variant_sets().get_all_variant_selections()?;
            bail!(
                "variant {}={} did not apply on {path} (no such variant?); its selections are {selections:?}",
                v.set,
                v.value
            );
        }
    }
    Ok(stage)
}

fn selection_prim(v: &VariantSelection, default_prim: Option<&str>) -> Result<String> {
    match (&v.prim, default_prim) {
        (Some(p), _) => Ok(p.clone()),
        (None, Some(d)) => Ok(format!("/{d}")),
        (None, None) => bail!(
            "variant {}={} has no prim path and the stage has no defaultPrim",
            v.set,
            v.value
        ),
    }
}

/// A temporary `.usda` holding the variant selections, deleted on drop. It must
/// outlive `Stage::open`, which reads it; the composed stage keeps its own copy.
struct SessionLayer(PathBuf);

impl SessionLayer {
    fn write(variants: &[VariantSelection], default_prim: Option<&str>) -> Result<Self> {
        // prim path → its selections, so one prim's sets share one `over`.
        let mut by_prim: Vec<(String, Vec<&VariantSelection>)> = Vec::new();
        for v in variants {
            let path = selection_prim(v, default_prim)?;
            match by_prim.iter_mut().find(|(p, _)| *p == path) {
                Some((_, list)) => list.push(v),
                None => by_prim.push((path, vec![v])),
            }
        }
        let mut text = String::from("#usda 1.0\n");
        for (path, list) in &by_prim {
            let names: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if names.is_empty() {
                bail!("variant selection on the pseudo-root is not a thing");
            }
            for (depth, name) in names.iter().enumerate() {
                let indent = "    ".repeat(depth);
                if depth + 1 == names.len() {
                    let body: Vec<String> = list
                        .iter()
                        .map(|v| format!("string {} = {:?}", v.set, v.value))
                        .collect();
                    text.push_str(&format!(
                        "{indent}over {name:?} (\n{indent}    variants = {{ {} }}\n{indent})\n{indent}{{\n",
                        body.join("; ")
                    ));
                } else {
                    text.push_str(&format!("{indent}over {name:?}\n{indent}{{\n"));
                }
            }
            for depth in (0..names.len()).rev() {
                text.push_str(&format!("{}}}\n", "    ".repeat(depth)));
            }
        }
        let path = std::env::temp_dir().join(format!(
            "awsm-isaac-export-session-{}-{}.usda",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(Self(path))
    }

    fn path_str(&self) -> Result<&str> {
        self.0
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF-8 temp path {}", self.0.display()))
    }
}

impl Drop for SessionLayer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Any prim, viewed as `Xformable` + `Imageable`.
///
/// The typed views (`Xform`, `Mesh`, `Cube`, ...) only wrap prims of their own
/// type, but the transform stack and `purpose`/`visibility` are plain
/// attributes every prim can carry — and the default trait methods only read
/// those attributes. An un-transformable prim (a `Scope`, a `Material`) simply
/// has no `xformOpOrder` and reads as identity, which is also what USD says.
pub struct AnyPrim(pub Prim);

impl SchemaBase for AnyPrim {
    const KIND: SchemaKind = SchemaKind::ConcreteTyped;
    fn prim(&self) -> &Prim {
        &self.0
    }
}
impl Imageable for AnyPrim {}
impl Xformable for AnyPrim {}

/// Memoized local-to-world transforms, as column-vector glam matrices.
#[derive(Default)]
pub struct WorldXforms {
    cache: HashMap<sdf::Path, DMat4>,
}

impl WorldXforms {
    pub fn get(&mut self, stage: &Stage, path: &sdf::Path) -> Result<DMat4> {
        if let Some(m) = self.cache.get(path) {
            return Ok(*m);
        }
        let prim = stage.prim(path.clone())?;
        let view = AnyPrim(prim);
        let local = to_dmat4(
            view.local_to_parent_transform(TIME)
                .map_err(|e| anyhow!("{path}: {e}"))?,
        );
        let resets = view.resets_xform_stack()?;
        let world = match path.parent() {
            Some(parent) if !parent.is_abs_root() && !resets => self.get(stage, &parent)? * local,
            _ => local,
        };
        self.cache.insert(path.clone(), world);
        Ok(world)
    }
}

/// USD stores matrices row-major for ROW vectors (`p' = p · M`). Read as
/// column-major, the same 16 numbers are exactly the column-vector matrix glam
/// uses, so this is a reinterpretation, not a transpose.
pub fn to_dmat4(m: openusd::gf::Matrix4d) -> DMat4 {
    DMat4::from_cols_array(&m.0)
}

/// The stage's `upAxis` and `metersPerUnit`, with USD's fallbacks (`Y`, `0.01`)
/// when unauthored.
pub fn stage_units(stage: &Stage) -> (String, f64) {
    let layer = stage.root_layer();
    let Some(root) = layer.pseudo_root() else {
        return ("Y".into(), 0.01);
    };
    let up = match root.get::<sdf::Value>("upAxis") {
        Some(sdf::Value::Token(t)) => t.as_str().to_string(),
        Some(sdf::Value::String(s)) => s,
        _ => "Y".into(),
    };
    let mpu = match root.get::<sdf::Value>("metersPerUnit") {
        Some(v) => value_f64(&v).unwrap_or(0.01),
        None => 0.01,
    };
    (up, mpu)
}

// ── loosely typed value reading ─────────────────────────────────────────────
//
// USD lets an author pick float or double (and sometimes half) for most
// attributes, so every reader here accepts any of them rather than failing on a
// precision it did not expect.

pub fn value(attr: &Attribute) -> Option<sdf::Value> {
    attr.get::<sdf::Value>().ok().flatten()
}

pub fn value_f64(v: &sdf::Value) -> Option<f64> {
    match v {
        sdf::Value::Float(f) => Some(*f as f64),
        sdf::Value::Double(f) => Some(*f),
        sdf::Value::Half(f) => Some(f.to_f64()),
        sdf::Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

pub fn value_vec3(v: &sdf::Value) -> Option<[f64; 3]> {
    match v {
        sdf::Value::Vec3f(v) => Some([v.x as f64, v.y as f64, v.z as f64]),
        sdf::Value::Vec3d(v) => Some([v.x, v.y, v.z]),
        sdf::Value::Vec3h(v) => Some([v.x.to_f64(), v.y.to_f64(), v.z.to_f64()]),
        // `color3f[] primvars:displayColor` is an array; its constant form is
        // one element.
        sdf::Value::Vec3fVec(v) if !v.is_empty() => {
            Some([v[0].x as f64, v[0].y as f64, v[0].z as f64])
        }
        sdf::Value::Vec3dVec(v) if !v.is_empty() => Some([v[0].x, v[0].y, v[0].z]),
        _ => None,
    }
}

pub fn value_vec3_array(v: &sdf::Value) -> Option<Vec<[f64; 3]>> {
    match v {
        sdf::Value::Vec3fVec(v) => Some(
            v.iter()
                .map(|p| [p.x as f64, p.y as f64, p.z as f64])
                .collect(),
        ),
        sdf::Value::Vec3dVec(v) => Some(v.iter().map(|p| [p.x, p.y, p.z]).collect()),
        sdf::Value::Vec3hVec(v) => Some(
            v.iter()
                .map(|p| [p.x.to_f64(), p.y.to_f64(), p.z.to_f64()])
                .collect(),
        ),
        _ => None,
    }
}

pub fn value_vec2_array(v: &sdf::Value) -> Option<Vec<[f64; 2]>> {
    match v {
        sdf::Value::Vec2fVec(v) => Some(v.iter().map(|p| [p.x as f64, p.y as f64]).collect()),
        sdf::Value::Vec2dVec(v) => Some(v.iter().map(|p| [p.x, p.y]).collect()),
        sdf::Value::Vec2hVec(v) => Some(v.iter().map(|p| [p.x.to_f64(), p.y.to_f64()]).collect()),
        _ => None,
    }
}

pub fn value_int_array(v: &sdf::Value) -> Option<Vec<i64>> {
    match v {
        sdf::Value::IntVec(v) => Some(v.iter().map(|i| *i as i64).collect()),
        sdf::Value::UintVec(v) => Some(v.iter().map(|i| *i as i64).collect()),
        sdf::Value::Int64Vec(v) => Some(v.clone()),
        _ => None,
    }
}

pub fn value_token(v: &sdf::Value) -> Option<String> {
    match v {
        sdf::Value::Token(t) => Some(t.as_str().to_string()),
        sdf::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// An attribute's `interpolation` metadata (`vertex`, `faceVarying`, ...).
pub fn interpolation(attr: &Attribute) -> Option<String> {
    attr.get_metadata::<sdf::Value>("interpolation")
        .ok()
        .flatten()
        .and_then(|v| value_token(&v))
}
