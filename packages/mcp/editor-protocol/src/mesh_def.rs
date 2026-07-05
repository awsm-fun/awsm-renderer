//! The **authoring** mesh: `MeshDef` (a modifier-stack recipe + sparse per-vertex
//! overrides) and its provenance. The editor's bake step lowers this to the
//! runtime `awsm_renderer_scene::RuntimeMesh` / `MeshBlob`. Moved out of the old
//! scene-schema so the runtime crate stays free of authoring types.

use awsm_renderer_meshgen::recipe::{ModifierStack, SweepAlongCurveDef};
use awsm_renderer_scene::{AssetId, PrimitiveShape};

/// `source` records the kind the mesh was captured from. The editor's
/// Mesh inspector renders editable copies of those params; mutating
/// them auto-regenerates the bytes against the same AssetId, so every
/// referencing `NodeKind::Mesh` picks up the change without the user
/// having to find a source node in the tree.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MeshDef {
    pub label: String,
    #[serde(default)]
    pub source: Option<CapturedSource>,
    /// When true the mesh is *editable*: its geometry is the regenerable
    /// `.mesh.bin` cache (raw-vertex-edited or collapsed) re-evaluated from
    /// `stack`. `#[serde(default)]` keeps pre-feature project.json/bin files
    /// round-tripping (older captured meshes deserialize as `editable = false`).
    #[serde(default)]
    pub editable: bool,
    /// The procedural recipe (modifier stack) this mesh regenerates from. Every
    /// `MeshDef` carries one: a purely-captured / imported blob is a stack whose
    /// `base` is [`MeshBase::Captured`] (or the source's own recipe) with no
    /// modifiers; a primitive/sweep node mints a stack with the matching
    /// [`MeshBase::Primitive`] / [`MeshBase::Sweep`] base. The `.mesh.bin`
    /// triangle buffer is the regenerable bake of evaluating this stack. See
    /// [`ModifierStack`].
    pub stack: ModifierStack,
    /// Sparse, index-keyed **per-vertex authoring overrides** layered on top of
    /// the evaluated `stack` (see [`VertexOverrides`]). Per-vertex authoring is
    /// *terminal*: the first authoring op collapses `stack` to a frozen
    /// `Captured` base (locking topology), after which these maps are the only
    /// non-destructive edit layer. Empty by default — `#[serde(default)]` keeps
    /// pre-feature project.json round-tripping.
    #[serde(default)]
    pub overrides: VertexOverrides,
}

/// Sparse, vertex-index-keyed authoring overrides applied **after** the modifier
/// stack evaluates (see [`MeshDef::overrides`]). Each map keys a vertex index
/// (into the frozen, post-eval topology) to its authored value; an index absent
/// from a map rides along with the evaluated base. Positions are *edited*
/// (sculpt); colors/normals/uvs are *authored* channels (a channel is created on
/// the baked mesh if any override for it exists). This is the data behind the
/// `PaintVertexColors` / `SetVertexNormals` / migrated `SetVertexPositions` /
/// `SoftTransformVertices` commands.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct VertexOverrides {
    #[serde(
        default,
        serialize_with = "ser_int_keyed_map",
        deserialize_with = "de_int_keyed_map"
    )]
    pub positions: std::collections::HashMap<u32, [f32; 3]>,
    #[serde(
        default,
        serialize_with = "ser_int_keyed_map",
        deserialize_with = "de_int_keyed_map"
    )]
    pub colors: std::collections::HashMap<u32, [f32; 4]>,
    #[serde(
        default,
        serialize_with = "ser_int_keyed_map",
        deserialize_with = "de_int_keyed_map"
    )]
    pub normals: std::collections::HashMap<u32, [f32; 3]>,
    #[serde(
        default,
        serialize_with = "ser_int_keyed_map",
        deserialize_with = "de_int_keyed_map"
    )]
    pub uvs: std::collections::HashMap<u32, [f32; 2]>,
}

/// Serialize a `u32`-keyed override map. The complement of [`de_int_keyed_map`]:
/// human-readable formats (TOML / JSON) require **string** map keys — TOML's
/// serializer outright errors with "map key was not a string" on an integer key,
/// which used to make any project carrying per-vertex overrides unsaveable — so
/// stringify the index there. Non-self-describing binary formats (bitcode — the
/// `.mesh.bin` / project persistence) keep the native `u32` key, so existing
/// `.mesh.bin` files keep round-tripping byte-for-byte. Keys are emitted in
/// sorted order to keep `project.toml` diffs deterministic.
fn ser_int_keyed_map<S, V>(map: &std::collections::HashMap<u32, V>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    V: serde::Serialize,
{
    use serde::ser::SerializeMap;
    let mut entries: Vec<(&u32, &V)> = map.iter().collect();
    entries.sort_by_key(|(k, _)| **k);
    let human_readable = s.is_human_readable();
    let mut m = s.serialize_map(Some(entries.len()))?;
    for (k, v) in entries {
        if human_readable {
            m.serialize_entry(&k.to_string(), v)?;
        } else {
            m.serialize_entry(k, v)?;
        }
    }
    m.end()
}

/// Deserialize a `u32`-keyed map whose keys may arrive as integers (bitcode /
/// native) **or** as integer-strings (JSON). This is what makes the per-vertex
/// override commands drivable over JSON dispatch: serde's `#[serde(tag="cmd")]`
/// internally-tagged `EditorCommand` buffers each variant into a `Content` value
/// before deserializing it, and `Content` (like `serde_json::from_value`) can't
/// coerce a JSON string object-key into `u32` — so a plain `HashMap<u32,_>` field
/// rejects `{"0":[…]}` with "invalid type: string, expected u32". A key visitor
/// using `deserialize_any` accepts both shapes and survives the `Content` round
/// (and bitcode, which feeds the key back as an integer).
fn de_int_keyed_map<'de, D, V>(d: D) -> Result<std::collections::HashMap<u32, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: serde::Deserialize<'de>,
{
    use std::collections::HashMap;
    use std::marker::PhantomData;

    struct U32Key(u32);
    impl<'de> serde::Deserialize<'de> for U32Key {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            // Branch on the format: human-readable (serde_json / serde's tagged-enum
            // `Content` buffer) hands map keys back as strings and supports
            // `deserialize_any`; non-self-describing binary formats (bitcode — the
            // `.mesh.bin` / project persistence) wrote the key as a real `u32` and
            // reject `deserialize_any`, so read it natively there.
            if d.is_human_readable() {
                struct KeyVisitor;
                impl serde::de::Visitor<'_> for KeyVisitor {
                    type Value = u32;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("a u32 vertex index (integer or integer-string)")
                    }
                    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u32, E> {
                        u32::try_from(v).map_err(serde::de::Error::custom)
                    }
                    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<u32, E> {
                        u32::try_from(v).map_err(serde::de::Error::custom)
                    }
                    fn visit_u32<E: serde::de::Error>(self, v: u32) -> Result<u32, E> {
                        Ok(v)
                    }
                    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<u32, E> {
                        v.parse().map_err(serde::de::Error::custom)
                    }
                }
                d.deserialize_any(KeyVisitor).map(U32Key)
            } else {
                u32::deserialize(d).map(U32Key)
            }
        }
    }

    struct MapVisitor<V>(PhantomData<V>);
    impl<'de, V: serde::Deserialize<'de>> serde::de::Visitor<'de> for MapVisitor<V> {
        type Value = HashMap<u32, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a map keyed by vertex index")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut a: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = HashMap::new();
            while let Some((k, v)) = a.next_entry::<U32Key, V>()? {
                out.insert(k.0, v);
            }
            Ok(out)
        }
    }

    d.deserialize_map(MapVisitor(PhantomData))
}

impl VertexOverrides {
    /// True when no override of any channel is present.
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
            && self.colors.is_empty()
            && self.normals.is_empty()
            && self.uvs.is_empty()
    }
}

/// Where a captured mesh's geometry came from. Stored on `MeshDef`
/// so the Mesh inspector can render the source params + re-capture
/// without a separate source node.
///
/// `Sweep`'s `curve_node` is a `NodeId` reference into the live
/// scene; if that node is deleted between captures the inspector
/// falls back to the legacy "pick a source from scene" picker.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum CapturedSource {
    Primitive(PrimitiveShape),
    Sweep(SweepAlongCurveDef),
    /// Raw-vertex-edited / collapsed geometry — there is **no** recipe to
    /// regenerate from; the `.mesh.bin` triangle buffer *is* the source of truth.
    Editable,
    /// Geometry baked from an imported model. The original `.glb` on disk
    /// (referenced by `source`) remains the editable source of truth; the
    /// `.mesh.bin` is a bake for editing/export.
    Imported {
        source: AssetId,
    },
}

/// Captured procedural-mesh geometry, bitcode-serialized into the
/// project's `assets/<asset-id>.mesh.bin` side file. Mirrors the
/// in-memory shape of `awsm_renderer_meshgen::MeshData` so the materializer can
/// hand the data straight to the renderer without massaging.
///
/// The consuming crates own conversion helpers (editor bake → runtime MeshBlob).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CapturedMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Option<Vec<[f32; 3]>>,
    pub uvs: Option<Vec<[f32; 2]>>,
    /// Optional 2nd UV set (`TEXCOORD_1`), vertex-aligned with `uvs`. Lets a
    /// captured (imported static) mesh feed the renderer's UV set 1 so custom
    /// materials can read `material_uv(in, 1u)`. `#[serde(default)]` so the field
    /// is omitted from existing single-UV captures.
    #[serde(default)]
    pub uvs1: Option<Vec<[f32; 2]>>,
    pub colors: Option<Vec<[f32; 4]>>,
    /// Optional authored per-vertex `TANGENT` (vec4: xyz + handedness), vertex-aligned
    /// with `positions`. Carried from an imported glTF's TANGENT attribute so the
    /// captured mesh preserves the EXACT tangent basis a normal map was baked against
    /// across save→reload (else the renderer regenerates via MikkTSpace and shades
    /// differently — the dark-patch bug). `#[serde(default)]` so pre-feature
    /// `.mesh.bin` files (and edited/procedural meshes, which carry none) round-trip.
    #[serde(default)]
    pub tangents: Option<Vec<[f32; 4]>>,
    pub indices: Vec<u32>,
}

impl CapturedMesh {
    /// Validate geometry before a `SetMeshData` stores it. Rejects the silent
    /// mesh-wipe footgun (`{positions:[], indices:[]}` used to overwrite a real
    /// mesh and return `ok`) and structurally-broken input. With `allow_empty`,
    /// empty geometry is permitted (a deliberate clear / the undo-restore path)
    /// but the structural invariants below still hold for any non-empty buffer.
    ///
    /// Always enforced: `indices.len()` a multiple of 3; every index in range for
    /// `positions`; any present optional channel (`normals`/`uvs`/`uvs1`/`colors`)
    /// vertex-aligned with `positions`.
    pub fn validate(&self, allow_empty: bool) -> Result<(), String> {
        if !allow_empty && (self.positions.is_empty() || self.indices.is_empty()) {
            return Err(format!(
                "set_mesh_data: refusing to store empty/degenerate geometry \
                 ({} positions, {} indices) — pass allow_empty:true to clear a mesh deliberately",
                self.positions.len(),
                self.indices.len()
            ));
        }
        if self.indices.len() % 3 != 0 {
            return Err(format!(
                "set_mesh_data: indices length {} is not a multiple of 3 (not a triangle list)",
                self.indices.len()
            ));
        }
        let vcount = self.positions.len() as u32;
        if let Some(&max) = self.indices.iter().max() {
            if max >= vcount {
                return Err(format!(
                    "set_mesh_data: index {max} out of range for {vcount} vertices"
                ));
            }
        }
        let check = |name: &str, len: Option<usize>| -> Result<(), String> {
            if let Some(n) = len {
                if n != self.positions.len() {
                    return Err(format!(
                        "set_mesh_data: {name} length {n} != positions length {}",
                        self.positions.len()
                    ));
                }
            }
            Ok(())
        };
        check("normals", self.normals.as_ref().map(|v| v.len()))?;
        check("uvs", self.uvs.as_ref().map(|v| v.len()))?;
        check("uvs1", self.uvs1.as_ref().map(|v| v.len()))?;
        check("colors", self.colors.as_ref().map(|v| v.len()))?;
        check("tangents", self.tangents.as_ref().map(|v| v.len()))?;
        Ok(())
    }
}
