//! The rmcp tool layer. Each tool is a thin typed wrapper that builds a protocol
//! [`Request`] and relays it to the attached editor over the WebSocket link,
//! then shapes the [`Response`] into an MCP result. All editor mutation flows
//! through `EditorController` on the far side (the "all via controller" rule);
//! this layer only translates.
//!
//! Coverage spans typed tools for the common families (scene /
//! nodes, project / import, materials incl. WGSL, view / camera, animation,
//! queries / screenshots) plus two generic escape hatches (`dispatch_command` /
//! `run_query`) that take a raw protocol JSON value, so *every* `EditorCommand` /
//! `EditorQuery` variant is reachable even when its payload references
//! scene-schema types without a JSON schema.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, GetPromptRequestParams, GetPromptResult,
    ListPromptsResult, ListResourcesResult, LoggingLevel, LoggingMessageNotificationParam,
    PaginatedRequestParams, Prompt, PromptMessage, PromptMessageRole, RawResource,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use serde_json::Value;

use awsm_renderer_editor_protocol::{
    CameraAxis, CompileError, CustomAlphaMode, EditorCommand, EditorMode, EditorQuery, InsertSpec,
    ProceduralKind, QueryResult, Request, Response, SlotSpec, StepKind,
};
use awsm_renderer_scene::animation::{
    BuiltinParamKind, ClipLoop, Interp, LightParamKind, SamplerKind, TexSlot, TexTransformProp,
    TrackTarget, TrackValue, TransformProp,
};
use awsm_renderer_scene::{
    AssetId, EnvSlot, EnvironmentConfig, LightKind, MaterialShading, MeshLodConfig,
    MeshShadowConfig, NodeId, NodeKind, PrimitiveShape, ToneMappingConfig, Trs,
};

use crate::link::{AgentSession, EditorLink, LinkError};

/// The MCP tool provider — one per MCP session. Cheap to clone (handles are
/// `Arc`s); clones share the same [`AgentSession`], so a session's editor binding
/// is stable across clones.
#[derive(Clone)]
pub struct EditorMcp {
    link: EditorLink,
    /// This session's identity + editor binding. Every request routes only to the
    /// bound editor tab.
    agent: Arc<AgentSession>,
    // Populated by `Self::tool_router()` and consumed by the `#[tool_handler]`
    // generated routing; rmcp 1.7's macro reads it through a trait impl the
    // dead-code lint can't see, hence the allow.
    #[allow(dead_code)]
    tool_router: ToolRouter<EditorMcp>,
}

// ───────────────────────────── parameter types ──────────────────────────────

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NodeArg {
    /// Target node UUID (from `get_snapshot`'s `scene_tree` ids).
    pub node: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OptionalNodeParams {
    /// Root node UUID, or omit for every scene root.
    #[serde(default)]
    pub node: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetTransformParams {
    /// Target node UUID.
    pub node: String,
    /// Local translation `[x, y, z]` (meters, relative to the parent; the scene
    /// is right-handed Y-up).
    pub translation: [f32; 3],
    /// Local rotation quaternion `[x, y, z, w]`.
    pub rotation: [f32; 4],
    /// Per-axis scale `[x, y, z]`.
    pub scale: [f32; 3],
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct Vec3Params {
    /// Target node UUID.
    pub node: String,
    /// `[x, y, z]`.
    pub value: [f32; 3],
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LookAtParams {
    /// Target node UUID (typically a light or camera).
    pub node: String,
    /// World-space point to aim at, `[x, y, z]`.
    pub target: [f32; 3],
    /// Optional up-hint for the roll-free frame (default `[0,1,0]`;
    /// auto-falls back when the aim direction is parallel to it).
    pub up: Option<[f32; 3]>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EulerParams {
    /// Target node UUID.
    pub node: String,
    /// Euler angles `[x, y, z]` in **radians**.
    pub euler: [f32; 3],
    /// Rotation order (default `xyz`): xyz | xzy | yxz | yzx | zxy | zyx.
    #[serde(default)]
    pub order: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NodesParams {
    /// Node UUIDs to read; empty/omitted = every node in the scene.
    #[serde(default)]
    pub nodes: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportNodeParams {
    /// UUID of the node (subtree) to bake to GLB.
    pub node: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MeshCrossSectionParams {
    /// UUID of the geometry node.
    pub node: String,
    /// Profile axis: 0=X, 1=Y, 2=Z. Defaults to Y.
    #[serde(default = "default_axis_y")]
    pub axis: u8,
    /// Number of height bins. Defaults to 16.
    #[serde(default = "default_cross_samples")]
    pub samples: u32,
}

fn default_axis_y() -> u8 {
    1
}
fn default_cross_samples() -> u32 {
    16
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SelectVerticesParams {
    /// UUID of the geometry node.
    pub node: String,
    /// Strongly-typed selection predicate (the schema lists every `kind`).
    pub predicate: Flexible<awsm_renderer_editor_protocol::VertexPredicate>,
    /// §10: `true` ⇒ keep the indices SERVER-SIDE and return a reusable
    /// `{ id, count }` handle (pass `selection: <id>` to the paint/sculpt verbs)
    /// instead of the index array. Use this for full-res selections that would
    /// overflow the tool-result token cap.
    #[serde(default)]
    pub store: bool,
    /// Return just `{ count }` (no indices).
    #[serde(default)]
    pub count_only: bool,
    /// Page the returned `indices` (when not storing): start index.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Page the returned `indices` (when not storing): max returned.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PaintWhereParams {
    /// UUID of the geometry node.
    pub node: String,
    /// Selection predicate (same shapes as `select_vertices_where`).
    pub predicate: Flexible<awsm_renderer_editor_protocol::VertexPredicate>,
    /// Linear RGBA `[r,g,b,a]` painted on every selected vertex.
    pub color: [f32; 4],
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TransformWhereParams {
    /// UUID of the geometry node.
    pub node: String,
    /// Selection predicate (same shapes as `select_vertices_where`).
    pub predicate: Flexible<awsm_renderer_editor_protocol::VertexPredicate>,
    /// World-space translation `[x,y,z]` applied to the selection.
    pub translation: [f32; 3],
    /// Smooth radial falloff radius (0 = rigid move of exactly the selection).
    #[serde(default)]
    pub falloff: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportBundleParams {
    /// Bundle name (publish dir / manifest label).
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MeshIdParams {
    /// UUID of the editable mesh asset.
    pub mesh: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SeparateMeshParams {
    /// UUID of the source mesh node to detach a region from.
    pub node: String,
    /// Vertex indices of the region (a face moves when all 3 verts are selected).
    /// Omit when using `selection`.
    #[serde(default)]
    pub indices: Vec<u32>,
    /// §10: a stored selection HANDLE supplying the region indices.
    #[serde(default)]
    pub selection: Option<u32>,
    /// When true, also REMOVE the extracted faces from the source (source ←
    /// remainder). Default false (source untouched; the new node is a copy).
    #[serde(default)]
    pub keep_remainder: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetVertexPositionsParams {
    pub mesh: String,
    /// Vertex indices to move (omit when using `selection`).
    #[serde(default)]
    pub indices: Vec<u32>,
    /// New positions, aligned with `indices` (or with the `selection` handle's
    /// stored order — read it back with `get_vertex_data { selection }`).
    pub positions: Vec<[f32; 3]>,
    /// §10: a selection HANDLE id (from `select_vertices_where { store: true }`)
    /// supplying the target indices instead of `indices`.
    #[serde(default)]
    pub selection: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SoftTransformParams {
    pub mesh: String,
    /// The selected vertex indices (the move's full-weight center; omit when
    /// using `selection`).
    #[serde(default)]
    pub indices: Vec<u32>,
    /// Translation applied at the selection, fading over the falloff radius.
    pub translation: [f32; 3],
    /// Falloff radius (world units); 0 = hard move of exactly the selection.
    pub falloff: f32,
    /// §10: a selection HANDLE id supplying the target indices instead of `indices`.
    #[serde(default)]
    pub selection: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetMeshModifiersParams {
    /// UUID of the editable mesh asset.
    pub mesh: String,
    /// Strongly-typed modifier stack (the schema lists every base + modifier).
    /// See the `awsm://docs/mesh-tools` resource for worked examples.
    pub stack: Flexible<awsm_renderer_editor_protocol::ModifierStack>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddModifierParams {
    /// UUID of the editable mesh asset (must already have a modifier stack —
    /// call `set_mesh_modifiers` first to give it a base).
    pub mesh: String,
    /// One strongly-typed modifier object (e.g. `{"twist":{"axis":"y","turns":2}}`).
    /// See the `awsm://docs/mesh-tools` resource for every modifier's shape.
    pub modifier: Flexible<awsm_renderer_editor_protocol::Modifier>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetModifierParams {
    /// UUID of the editable mesh asset (must already have a modifier stack).
    pub mesh: String,
    /// Zero-based index of the modifier to replace (must be in range).
    pub index: u32,
    /// The replacement modifier object.
    pub modifier: Flexible<awsm_renderer_editor_protocol::Modifier>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveModifierParams {
    /// UUID of the editable mesh asset (must already have a modifier stack).
    pub mesh: String,
    /// Zero-based index of the modifier to remove (must be in range).
    pub index: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetMeshModifiersParams {
    /// UUID of the mesh asset to read the modifier stack from.
    pub mesh: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PaintVertexColorsParams {
    /// UUID of the editable mesh asset.
    pub mesh: String,
    /// Vertex indices (into the resolved/baked topology) to paint (omit when using
    /// `selection`).
    #[serde(default)]
    pub indices: Vec<u32>,
    /// Linear RGBA color to set on each index.
    pub color: [f32; 4],
    /// §10: a selection HANDLE id supplying the target indices instead of `indices`.
    #[serde(default)]
    pub selection: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetVertexNormalsParams {
    /// UUID of the editable mesh asset.
    pub mesh: String,
    /// Vertex indices to override the normal of (omit when using `selection`).
    #[serde(default)]
    pub indices: Vec<u32>,
    /// The normal vector to set on each index (should be unit-length).
    pub normal: [f32; 3],
    /// §10: a selection HANDLE id supplying the target indices instead of `indices`.
    #[serde(default)]
    pub selection: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetVertexUvsParams {
    /// UUID of the editable mesh asset.
    pub mesh: String,
    /// Vertex indices to set UVs on (omit when using `selection`).
    #[serde(default)]
    pub indices: Vec<u32>,
    /// UV coordinates [u, v], aligned with `indices` (or with the `selection`
    /// handle's stored order). `uvs[k]` is written to vertex `indices[k]`.
    pub uvs: Vec<[f32; 2]>,
    /// §10: a selection HANDLE id supplying the target indices instead of `indices`.
    #[serde(default)]
    pub selection: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DisplaceFromTextureParams {
    /// UUID of the geometry node to displace.
    pub node: String,
    /// URL of a hosted PNG/JPEG heightmap. Fetched + decoded to RGBA; per-vertex
    /// height = perceptual luminance (black = flat, white = raised). Author ANY
    /// heightfield (eroded terrain, a logo, fbm) externally, host it, pass the URL.
    pub url: String,
    /// Displacement distance (world units) at full white. Negative carves inward.
    pub strength: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetVertexDataParams {
    /// UUID of the node whose resolved mesh to read.
    pub node: String,
    /// Vertex indices to read the final (post-eval + override) data of (omit when
    /// using `selection`).
    #[serde(default)]
    pub indices: Vec<u32>,
    /// §10: read a stored selection HANDLE's vertices instead of sending indices.
    #[serde(default)]
    pub selection: Option<u32>,
    /// Page the result (start index) — for a large selection.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Page the result (max returned).
    #[serde(default)]
    pub limit: Option<u32>,
    /// When true, add a per-vertex `source` block tagging each channel
    /// (position/normal/color/uv) as `"override"` or `"base"` — verify which
    /// channels an authoring op actually wrote.
    #[serde(default)]
    pub include_source: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetMeshLayersParams {
    /// UUID of the node whose mesh layer summary to read.
    pub node: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetMeshDataParams {
    /// UUID of the node whose resolved-mesh topology to read.
    pub node: String,
    /// Page the triangle list (start triangle) — a full index buffer overflows
    /// the token cap.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Page the triangle list (max triangles returned).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UvLayoutParams {
    /// UUID of the node whose UV layout to read.
    pub node: String,
    /// UV set (TEXCOORD_n), default 0.
    #[serde(default)]
    pub uv_set: Option<u32>,
    /// Page the UV-edge wireframe (start edge).
    #[serde(default)]
    pub offset: Option<u32>,
    /// Page the UV-edge wireframe (max edges returned).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StripParameterizeParams {
    /// UUID of the node whose resolved mesh to parameterize.
    pub node: String,
    /// §10: a selection HANDLE id naming the band (preferred for big bands).
    #[serde(default)]
    pub selection: Option<u32>,
    /// Explicit vertex indices of the band (used when no `selection`). Both empty
    /// ⇒ the whole mesh.
    #[serde(default)]
    pub indices: Vec<u32>,
    /// The axle [x, y, z] (normalized internally). Omit to auto-fit the band's
    /// least-variance PCA direction.
    #[serde(default)]
    pub axis: Option<[f32; 3]>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenameParams {
    pub node: String,
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetBoolParams {
    pub node: String,
    pub value: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InsertParams {
    /// Optional parent node UUID (root when omitted).
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetMeshShadowParams {
    /// Mesh / SkinnedMesh / InstancesAlongCurve node UUID.
    pub node: String,
    /// Whether the mesh appears in the shadow-generation pass (casts shadows).
    pub cast: bool,
    /// Whether the mesh's shaded pixels darken under shadow (receives shadows).
    pub receive: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetMeshLodParams {
    /// Mesh / SkinnedMesh / InstancesAlongCurve node UUID.
    pub node: String,
    /// Whether the export-time LOD bake generates simplified levels for this
    /// mesh. LOD is opt-out (default on); set false for hero/low-poly/UI meshes.
    pub enabled: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetInstanceColorsParams {
    /// InstancesAlongCurve node UUID.
    pub node: String,
    /// Linear RGBA `[r, g, b, a]` per instance, in placement order. Empty clears
    /// the per-instance tints (every instance renders with its material color).
    pub colors: Vec<[f32; 4]>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShapeArg {
    Plane,
    Box,
    Sphere,
    Cylinder,
    Cone,
    Torus,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InsertPrimitiveParams {
    pub shape: ShapeArg,
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LightArg {
    Directional,
    Point,
    Spot,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InsertLightParams {
    pub kind: LightArg,
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReparentParams {
    pub node: String,
    #[serde(default)]
    pub new_parent: Option<String>,
    #[serde(default)]
    pub index: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SelectionParams {
    /// Ordered node UUIDs (last = primary/anchor).
    pub ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct VertexSelectionParams {
    /// UUID of the geometry node whose vertices are highlighted.
    pub node: String,
    /// Vertex indices to highlight (empty = clear the highlight).
    pub indices: Vec<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UrlParams {
    pub url: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ImportNaniteParams {
    /// URL of a pre-baked cluster-LOD DAG file (`<id>.clusters.bin`) produced by the
    /// `awsm-renderer-lod-bake` CLI. The editor fetches + renders it as a view-only nanite mesh.
    pub clusters_url: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BaseUrlParams {
    pub base_url: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShadingArg {
    Pbr,
    Unlit,
    /// Cel shading (sensible default knobs; edit via dispatch_command
    /// UpdateBuiltinMaterial / the studio).
    Toon,
    /// Sprite-sheet animation. Defaults: 4×4 grid, 16 frames, 12 fps, loop.
    /// The atlas image is the material's BASE-COLOR texture slot; grid /
    /// playback knobs edit via dispatch_command. Mask alpha mode gives an
    /// animated CUTOUT (alpha-tested opaque, casts hole-shaped shadows).
    Flipbook,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ShadingParams {
    pub shading: ShadingArg,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateBuiltinParams {
    /// The built-in material's asset id.
    pub id: String,
    /// The FULL MaterialDef as a JSON **object** (read the current one from
    /// get_snapshot, modify, send back). See the tool description for field
    /// shapes. Typed as `object` in the schema (like `patch_kind.patch`) so
    /// clients don't stringify it — a bare `Value` schema made every client
    /// send a string, which the server then rejected.
    #[schemars(with = "serde_json::Map<String, serde_json::Value>")]
    pub def: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AssetArg {
    /// Asset UUID (material / texture / clip).
    pub asset: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SelectMaterialVariantParams {
    /// Mesh node UUID.
    pub node: String,
    /// Variant UUID from this mesh's palette (get_node_details →
    /// mesh.material_variants[].id), or omit/null to UNASSIGN the mesh
    /// (renders magenta). Selection is the ONLY way a mesh's rendered
    /// material changes, and it never mutates variant state — each variant
    /// keeps its own overrides across switches.
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddMaterialVariantParams {
    /// Mesh node UUID.
    pub node: String,
    /// LIBRARY material asset UUID to instantiate (a variant is always a
    /// library material + this mesh's own overrides). Add the same material
    /// twice for two independent tunings.
    pub material: String,
    /// Display name (defaults to the library material's name, counter-suffixed
    /// if taken on this mesh). Renameable later; the returned id is stable.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveMaterialVariantParams {
    /// Mesh node UUID.
    pub node: String,
    /// Variant UUID to remove. Removing the selected variant leaves the mesh
    /// unassigned (magenta).
    pub variant: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenameMaterialVariantParams {
    /// Mesh node UUID.
    pub node: String,
    /// Variant UUID to rename (display only — the id never changes).
    pub variant: String,
    /// New display name.
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetWgslParams {
    /// Custom (dynamic-WGSL) material asset UUID.
    pub material: String,
    /// The new WGSL shader source.
    pub wgsl: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlphaModeArg {
    Opaque,
    Mask,
    Blend,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ContractParams {
    /// Which fragment contract: false (default) = opaque/mask, true =
    /// transparent/blend. Ignored when `vertex` is true.
    #[serde(default)]
    pub transparent: bool,
    /// When true, return the VERTEX-displacement contract (the third, vertex
    /// WGSL window's ABI) instead of the fragment contract.
    #[serde(default)]
    pub vertex: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AlphaModeParams {
    /// Custom material asset UUID.
    pub material: String,
    pub mode: AlphaModeArg,
    /// Alpha cutoff for `mask` mode (default 0.5; ignored otherwise).
    #[serde(default)]
    pub cutoff: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MaterialBoolParams {
    pub material: String,
    pub value: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuiltinAlphaModeParams {
    /// Built-in library material UUID (see get_snapshot's materials list).
    pub material: String,
    pub mode: AlphaModeArg,
    /// Alpha cutoff for `mask` mode (default 0.5; ignored otherwise). The
    /// cutoff VALUE is a per-mesh uniform — tune it per node afterwards via
    /// set_builtin_param alpha_cutoff.
    #[serde(default)]
    pub cutoff: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MaterialHexParams {
    pub material: String,
    /// Hex color `#rrggbb`.
    pub hex: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SlotArg {
    /// Slot name (the WGSL field/binding name).
    pub name: String,
    /// WGSL type, e.g. `"f32"`, `"vec3<f32>"`, `"texture_2d<f32>"`, `"array<vec4<f32>>"`.
    pub ty: String,
    /// Default value for uniforms (comma-separated for vectors, e.g. `"0.6, 0.7, 1.0"`).
    #[serde(default)]
    pub val: String,
    /// Debug-preview source for textures/buffers (optional).
    #[serde(default)]
    pub debug: String,
    /// Texture slots only: the slot's semantic ROLE — decides the bound
    /// image's color space (sRGB decode for color data, verbatim for data
    /// maps) and mipmap kind, in the editor AND the player. One of: albedo
    /// (default, sRGB) | normal | metallic_roughness | occlusion | emissive
    /// (sRGB) | specular | specular_color (sRGB) | transmission |
    /// volume_thickness. Declare data maps or they shade wrong.
    #[serde(default)]
    pub color_kind: awsm_renderer_scene::TextureColorKind,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MaterialLayoutParams {
    pub material: String,
    #[serde(default)]
    pub uniforms: Vec<SlotArg>,
    #[serde(default)]
    pub textures: Vec<SlotArg>,
    #[serde(default)]
    pub buffers: Vec<SlotArg>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MaterialKeysParams {
    pub material: String,
    /// The declared keys (validated against the legal set; unknowns dropped).
    pub keys: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MaterialUniformParams {
    pub material: String,
    /// Declared uniform slot name.
    pub name: String,
    /// Value as the comma-separated form the layout uses (e.g. `"0.6, 0.7, 1.0"`).
    pub value: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NodeMaterialUniformParams {
    /// Mesh node UUID (must have a CUSTOM-WGSL material assigned).
    pub node: String,
    /// Declared uniform slot name (a `UniformField::name` on the material layout).
    pub name: String,
    /// Typed value: `{ "kind": "f32"|"vec2"|"vec3"|"vec4"|"u32"|…, "value": number|array }`.
    pub value: awsm_renderer_editor_protocol::dynamic_material::UniformValue,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinParamArg {
    BaseColor,
    Metallic,
    Roughness,
    Emissive,
    NormalScale,
    OcclusionStrength,
    EmissiveStrength,
    AlphaCutoff,
    ToonDiffuseBands,
    ToonSpecularSteps,
    ToonShininess,
    ToonRimStrength,
    ToonRimPower,
    FlipbookFps,
    FlipbookTimeOffset,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuiltinParamParams {
    /// Mesh node UUID.
    pub node: String,
    pub param: BuiltinParamArg,
    /// 1 element for metallic/roughness, 3 for base_color/emissive.
    pub value: Vec<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuiltinTextureParams {
    /// Mesh node UUID.
    pub node: String,
    /// Which built-in PBR slot to bind.
    pub slot: awsm_renderer_editor_protocol::BuiltinTextureSlot,
    /// Texture asset UUID to bind, or omit/null to clear the slot.
    #[serde(default)]
    pub texture: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ParticleEmitterParams {
    /// ParticleEmitter node UUID.
    pub node: String,
    /// Particles spawned per second.
    #[serde(default)]
    pub spawn_rate: Option<f32>,
    /// One-shot burst count (with `one_shot`).
    #[serde(default)]
    pub burst_count: Option<u32>,
    /// Max simultaneously-alive particles.
    #[serde(default)]
    pub max_alive: Option<u32>,
    /// Emit one burst then stop (vs continuous).
    #[serde(default)]
    pub one_shot: Option<bool>,
    /// Simulation space: `"world"` (particles persist) or `"local"` (follow the
    /// emitter transform).
    #[serde(default)]
    pub space: Option<awsm_renderer_editor_protocol::EmitterSpaceDef>,
    /// Spawn shape (externally-tagged): `{"point":{}}`, `{"sphere":{"radius":r}}`,
    /// or `{"cone":{"angle_radians":a,"direction":[x,y,z]}}` (direction in the
    /// emitter's LOCAL space).
    #[serde(default)]
    pub shape: Option<awsm_renderer_editor_protocol::SpawnShapeDef>,
    /// `[min, max]` initial speed (m/s).
    #[serde(default)]
    pub initial_speed: Option<[f32; 2]>,
    /// `[min, max]` lifetime (seconds).
    #[serde(default)]
    pub lifetime: Option<[f32; 2]>,
    /// `[min, max]` spawn size.
    #[serde(default)]
    pub size: Option<[f32; 2]>,
    /// Forces: list of `{"gravity":{"acceleration":[x,y,z]}}` /
    /// `{"linear_drag":{"coefficient_x1000":n}}` (replaces the whole list).
    #[serde(default)]
    pub forces: Option<Vec<awsm_renderer_editor_protocol::ForceDef>>,
    /// Color over life: `{"const":[r,g,b,a]}` or
    /// `{"linear":{"start":[r,g,b,a],"end":[r,g,b,a]}}` (alpha = transparency).
    #[serde(default)]
    pub color_over_life: Option<awsm_renderer_editor_protocol::ColorOverLifeDef>,
    /// Size over life: `{"const":s}` or `{"linear":{"start":s,"end":s}}`.
    #[serde(default)]
    pub size_over_life: Option<awsm_renderer_editor_protocol::SizeOverLifeDef>,
    /// Route through the transparent-blend pass (true alpha fades) vs the cheaper
    /// opaque-emissive path.
    #[serde(default)]
    pub blend: Option<bool>,
    /// Billboard SPRITE texture asset id the particles sample — e.g. a soft
    /// radial-alpha disc (import one with `import_texture_from_url`) for soft-edged
    /// particles instead of hard squares. Pair with `blend: true` so the sprite
    /// alpha fades the edges. Omit to leave unchanged.
    #[serde(default)]
    pub texture: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PatchKindParams {
    /// Node UUID to patch.
    pub node: String,
    /// RFC 7386 JSON merge-patch over the node's `NodeKind`, sent as a JSON
    /// **object** (not a stringified object): only the fields you include change;
    /// `null` removes a key; nested objects merge recursively; arrays replace
    /// wholesale. Read `get_node_details` first to see the exact shape + field
    /// names, then send just the delta.
    #[schemars(with = "serde_json::Map<String, serde_json::Value>")]
    pub patch: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct KindSchemaParams {
    /// Which schema: `"node"` (default) for the full `NodeKind` config schema, or
    /// `"modifier"` for the `ModifierStack` (mesh base + modifiers) schema.
    #[serde(default)]
    pub schema: Option<String>,
    /// Optional: a single NodeKind VARIANT name (snake_case, e.g. `"collider"`,
    /// `"light"`, `"particle_emitter"`) — returns just that variant's schema with
    /// only the `$defs` it references, instead of the full multi-hundred-KB
    /// `NodeKind` schema. Only applies to `schema: "node"`. An unknown name errors
    /// with the list of available variants.
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NodeTextureTransformParams {
    /// Mesh node UUID (the slot must already have a texture bound).
    pub node: String,
    /// Which built-in PBR slot to transform.
    pub slot: awsm_renderer_editor_protocol::BuiltinTextureSlot,
    /// UV offset `[u, v]` (the base the `flow` scroll accumulates onto).
    #[serde(default)]
    pub offset: Option<[f32; 2]>,
    /// UV scale `[u, v]` (>1 tiles the texture; default 1).
    #[serde(default)]
    pub scale: Option<[f32; 2]>,
    /// UV rotation in radians.
    #[serde(default)]
    pub rotation: Option<f32>,
    /// UV **flow** `[u, v]` auto-scroll velocity in UV-units/sec (set `[0,0]` to
    /// stop). Composes over `offset`; integrated from real time each frame.
    #[serde(default)]
    pub flow: Option<[f32; 2]>,
    /// Sampler wrap on U: `repeat` | `clamp_to_edge` | `mirrored_repeat`.
    #[serde(default)]
    pub wrap_u: Option<String>,
    /// Sampler wrap on V: `repeat` | `clamp_to_edge` | `mirrored_repeat`.
    #[serde(default)]
    pub wrap_v: Option<String>,
    /// Which TEXCOORD set this slot samples (glTF `texCoord` index).
    #[serde(default)]
    pub uv_set: Option<u32>,
    /// Sampler magnification filter: `nearest` | `linear`.
    #[serde(default)]
    pub mag_filter: Option<String>,
    /// Sampler minification filter: `nearest` | `linear`.
    #[serde(default)]
    pub min_filter: Option<String>,
    /// Sampler mipmap filter: `nearest` | `linear`.
    #[serde(default)]
    pub mipmap_filter: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProceduralArg {
    Checker,
    Gradient,
    Noise,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddTextureParams {
    /// Procedural generator: checker | gradient | noise.
    pub proc: ProceduralArg,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MaterialTextureParams {
    /// Mesh node UUID (must already have a custom material assigned).
    pub node: String,
    /// Declared texture slot name on the material.
    pub slot: String,
    /// Texture asset UUID to bind, or omit/null to clear the slot.
    #[serde(default)]
    pub texture: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MaterialBufferParams {
    /// Mesh node UUID (must already have a custom material assigned).
    pub node: String,
    /// Declared buffer slot name on the material.
    pub slot: String,
    /// The buffer's f32 words in declaration order (e.g. 4·N floats for an
    /// `array<vec4<f32>>` of N elements). Empty = clear the slot.
    #[serde(default)]
    pub values: Vec<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LightColorParams {
    /// Light node UUID.
    pub node: String,
    /// Linear RGB `[r, g, b]`.
    pub color: [f32; 3],
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LightScalarParams {
    /// Light node UUID.
    pub node: String,
    pub value: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SolveIkParams {
    /// Chain TIP joint node UUID (e.g. a foot); the chain is its parent (knee)
    /// and grandparent (upper leg) from the scene hierarchy.
    pub end_node: String,
    /// World-space target position for the tip.
    pub target: [f32; 3],
    /// Optional world-space pole hint — the chain bends toward it (e.g. put it
    /// in front of a knee). Omit to keep the chain's current bend plane.
    pub pole: Option<[f32; 3]>,
    /// Optional explicit chain ROOT joint UUID. When set, the 2-bone chain is
    /// `root_node → (its child toward end_node) → end_node`, so you choose which
    /// upper joint bends instead of the auto-pick (end → parent → grandparent),
    /// which can walk into the wrong bones (e.g. finger joints above a hand).
    /// Must be an ancestor of `end_node`. Discover chains via get_skin_data.
    #[serde(default)]
    pub root_node: Option<String>,
    /// Apply the solution (default true): one DispatchBatch of two
    /// SetTransforms = one undo step. False = solve-only (returns rotations).
    #[serde(default = "default_true_param")]
    pub apply: bool,
}

fn default_true_param() -> bool {
    true
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SkinWeightsGetParams {
    /// Skinned node UUID.
    pub node: String,
    /// ORIGINAL vertex indices to read; empty = every vertex.
    #[serde(default)]
    pub indices: Vec<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SkinWeightsSetParams {
    /// Skinned node UUID.
    pub node: String,
    /// Per-vertex rewrites: { vertex, joints:[u32;4], weights:[f32;4] }.
    pub entries: Vec<awsm_renderer_editor_protocol::SkinWeightEntry>,
    /// Rescale each entry's weights to sum to 1.
    #[serde(default)]
    pub normalize: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MorphWeightParams {
    /// Mesh node UUID (a node whose mesh has morph targets).
    pub node: String,
    /// Morph target index (0-based; `get_morph_data` reports the target count).
    pub index: u32,
    /// New weight (0.0 = off, 1.0 = full; out-of-[0,1] extrapolates, matching
    /// glTF semantics).
    pub value: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LightAnglesParams {
    /// Spot light node UUID.
    pub node: String,
    /// Inner cone angle (radians).
    pub inner: f32,
    /// Outer cone angle (radians).
    pub outer: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EnvironmentParams {
    /// Skybox (the background cubemap): omit to keep the current one, "builtin"
    /// for the built-in default sky, a KTX cubemap asset UUID, or a https:// URL
    /// to a .ktx2 cubemap.
    #[serde(default)]
    pub skybox: Option<String>,
    /// IBL specular (a.k.a. the prefiltered / roughness-mipped env map that
    /// drives reflections): omit to keep the current one, "builtin" for the
    /// built-in default, a KTX asset UUID, or a https:// .ktx2 URL. Independent
    /// of `irradiance` — you can override just this one.
    #[serde(default)]
    pub specular: Option<String>,
    /// IBL irradiance (the diffuse-convolved env map that drives ambient light):
    /// omit to keep the current one, "builtin" for the built-in default, a KTX
    /// asset UUID, or a https:// .ktx2 URL. Independent of `specular`.
    #[serde(default)]
    pub irradiance: Option<String>,
    /// Agent-authored SKY-GRADIENT environment (§18): linear-RGB `[r,g,b]` zenith
    /// (sky) color. When `zenith`+`nadir` are both given they set ALL THREE slots
    /// (skybox + specular + irradiance) to that two-color gradient (overriding the
    /// slot args above) — author dusk / overcast / night / studio from your own
    /// colors, no hosted `.ktx2` needed.
    #[serde(default)]
    pub zenith: Option<[f32; 3]>,
    /// Agent-authored sky-gradient nadir (ground) color, linear-RGB `[r,g,b]`.
    /// Pairs with `zenith`.
    #[serde(default)]
    pub nadir: Option<[f32; 3]>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SscsParams {
    /// Master enable for screen-space contact shadows (a short view-space
    /// ray-march that darkens contact gaps the shadow map misses). Off by
    /// default. Compile-time — toggling recompiles the shadow pipelines.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Ray-march step count (clamped >= 1). Compile-time loop bound — changing it
    /// recompiles. More steps = longer reach / more cost.
    #[serde(default)]
    pub step_count: Option<u32>,
    /// World-space length of each march step, in metres. Live uniform. Total
    /// reach = step_world * step_count.
    #[serde(default)]
    pub step_world: Option<f32>,
    /// Occluder-slab thickness in metres — a depth texel this far or less in
    /// front of the ray counts as an occluder. Live uniform. Larger admits
    /// thicker casters (a resting ball) at the cost of over-darkening thin geo.
    #[serde(default)]
    pub thickness: Option<f32>,
    /// Max darkening (0..1) applied to the DIRECTIONAL shadow term. Live uniform.
    #[serde(default)]
    pub directional_darkening: Option<f32>,
    /// Max darkening (0..1) for PUNCTUAL (point/spot) shadow terms — higher than
    /// directional since a cube map leaves a fully-lit contact gap to fill. Live.
    #[serde(default)]
    pub punctual_darkening: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PostProcessParams {
    /// Tonemapping operator: "none" (linear, HDR clips), "khronos_neutral_pbr"
    /// (default — color-preserving), or "aces" (filmic). Omit to leave unchanged.
    #[serde(default)]
    pub tonemapping: Option<String>,
    /// Bloom on/off. Recompiles the effects pipelines. Omit to leave unchanged.
    #[serde(default)]
    pub bloom: Option<bool>,
    /// Depth of field on/off (uses the active camera's focus distance /
    /// aperture). Recompiles the effects pipelines. Omit to leave unchanged.
    #[serde(default)]
    pub dof: Option<bool>,
    /// Pre-tonemap exposure in EV stops (0 unity, +1 = 2x, -1 = half). Live
    /// uniform. Omit to leave unchanged.
    #[serde(default)]
    pub exposure: Option<f32>,
    /// Bloom bright-pass threshold in pre-exposure HDR luminance — pixels
    /// brighter than this glow (default 1.0). Live uniform. Omit to leave
    /// unchanged.
    #[serde(default)]
    pub bloom_threshold: Option<f32>,
    /// Bloom soft-knee width below the threshold, for a smooth fade-in
    /// (default 0.5). Live uniform. Omit to leave unchanged.
    #[serde(default)]
    pub bloom_knee: Option<f32>,
    /// Bloom mix strength over the scene (default 1.0). Live uniform. Omit to
    /// leave unchanged.
    #[serde(default)]
    pub bloom_intensity: Option<f32>,
    /// Bloom scatter — higher biases the glow toward wider, softer mips
    /// (default 1.0). Live uniform. Omit to leave unchanged.
    #[serde(default)]
    pub bloom_scatter: Option<f32>,
    /// Screen-space reflections on/off. Off records + allocates nothing. Omit
    /// to leave unchanged.
    #[serde(default)]
    pub ssr_enabled: Option<bool>,
    /// SSR reflection strength (~0..2, default 1.0). Live uniform. Omit to leave
    /// unchanged.
    #[serde(default)]
    pub ssr_intensity: Option<f32>,
    /// SSR maximum ray length in world units (default 100). Live uniform. Omit
    /// to leave unchanged.
    #[serde(default)]
    pub ssr_max_distance: Option<f32>,
    /// SSR hit thickness in world units — the depth band a ray must cross to
    /// register a hit (default 1.0). Live uniform. Omit to leave unchanged.
    #[serde(default)]
    pub ssr_thickness: Option<f32>,
    /// SSR linear-march step budget (default 96). Live uniform. Omit to leave
    /// unchanged.
    #[serde(default)]
    pub ssr_max_steps: Option<u32>,
    /// SSR reflection-spread cutoff (0 mirror … 1 diffuse) above which SSR hands
    /// off to IBL (default 0.6). Live uniform. Omit to leave unchanged.
    #[serde(default)]
    pub ssr_spread_cutoff: Option<f32>,
    /// SSR screen-border fade width 0..1 (default 0.1). Live uniform. Omit to
    /// leave unchanged.
    #[serde(default)]
    pub ssr_edge_fade: Option<f32>,
    /// SSR temporal reprojection on/off (default off). STRUCTURAL — toggling
    /// recompiles the SSR pass. Omit to leave unchanged.
    #[serde(default)]
    pub ssr_temporal: Option<bool>,
    /// SSR resolution scale: 0.5 = half-res trace (default), 1.0 = full-res.
    /// STRUCTURAL — changing it recompiles. Omit to leave unchanged.
    #[serde(default)]
    pub ssr_resolution_scale: Option<f32>,
    /// SSR temporal history blend weight 0..1 (default 0.9) — fraction of the
    /// previous frame's accumulated reflection kept each frame (higher =
    /// smoother, more ghosting). Live uniform; only meaningful when
    /// ssr_temporal is on. Omit to leave unchanged.
    #[serde(default)]
    pub ssr_temporal_weight: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeArg {
    Scene,
    Material,
    Animation,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ModeParams {
    pub mode: ModeArg,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AxisArg {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AxisParams {
    pub axis: AxisArg,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CameraOrbitParams {
    /// Yaw (radians): 0 looks down -Z, π/2 down -X.
    pub yaw: f32,
    /// Pitch (radians): > 0 raises the camera (looks down).
    pub pitch: f32,
    /// Distance from the look-at point.
    pub radius: f32,
    /// Orbit center `[x, y, z]`.
    pub look_at: [f32; 3],
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CameraProjectionParams {
    /// true = perspective, false = orthographic.
    pub perspective: bool,
    /// Optional perspective vertical FOV (radians).
    #[serde(default)]
    pub fov_y: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CameraClipParams {
    /// true = manual (pin near/far); false = auto (planes track the orbit
    /// distance every move). Omit to leave the mode unchanged.
    #[serde(default)]
    pub manual: Option<bool>,
    /// Near clip plane in metres (applied when manual). Omit to leave unchanged.
    #[serde(default)]
    pub near: Option<f64>,
    /// Far clip plane in metres (applied when manual). Omit to leave unchanged.
    #[serde(default)]
    pub far: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FrameNodeParams {
    /// Node UUID to frame.
    pub node: String,
    /// Margin around the node (0 = tight, 0.2 = 20% padding). Default 0.1.
    #[serde(default)]
    pub padding: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClipOptParams {
    /// Clip asset UUID, or omit/null to clear.
    #[serde(default)]
    pub clip: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackTargetArg {
    /// Target kind: transform | morph | uniform | builtin_param | light | camera |
    /// texture_transform.
    pub kind: String,
    /// Node UUID (transform / morph / builtin_param / light / camera /
    /// texture_transform).
    #[serde(default)]
    pub node: Option<String>,
    /// Property: for `transform` translation | rotation | scale; for
    /// `texture_transform` offset (vec2) | scale (vec2) | rotation (scalar radians).
    #[serde(default)]
    pub prop: Option<String>,
    /// Built-in texture slot for `texture_transform`: base_color |
    /// metallic_roughness | normal | occlusion | emissive.
    #[serde(default)]
    pub slot: Option<String>,
    /// Morph target index.
    #[serde(default)]
    pub index: Option<u32>,
    /// Custom material UUID (uniform target).
    #[serde(default)]
    pub material: Option<String>,
    /// Uniform slot name (uniform target).
    #[serde(default)]
    pub name: Option<String>,
    /// Param name for builtin_param/light/camera (snake_case, e.g. base_color,
    /// intensity, color, range, inner_angle, outer_angle, fov_y).
    #[serde(default)]
    pub param: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackValueArg {
    /// Value kind: vec3 (3 floats) | quat (4, xyzw) | scalar (1 float).
    pub kind: String,
    pub value: Vec<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddTrackParams {
    pub clip: String,
    pub target: TrackTargetArg,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddSpinTrackParams {
    /// UUID of the clip to add the spin track to.
    pub clip: String,
    /// UUID of the node to spin (a rotation Transform track is created on it).
    pub node: String,
    /// Local rotation axis [x, y, z] (normalized internally; degenerate → +Y).
    pub axis: [f32; 3],
    /// Number of full revolutions over `duration` (fractional / negative allowed).
    pub turns: f32,
    /// Clip-time span of the spin, in seconds.
    pub duration: f64,
    /// Keyframes generated per revolution (default 4 = 90° steps).
    #[serde(default)]
    pub keys_per_turn: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddKeyframeParams {
    pub clip: String,
    pub track: u32,
    /// Time in seconds.
    pub t: f64,
    pub value: TrackValueArg,
    /// Interpolation for the new key: step | linear | cubic, optional. Omit to
    /// derive from the track's sampler (the default).
    #[serde(default)]
    pub interp: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetTrackKeysParams {
    pub clip: String,
    pub track: u32,
    /// Key times in seconds (paired index-wise with `values`; need not be sorted).
    pub times: Vec<f64>,
    /// One value per time.
    pub values: Vec<TrackValueArg>,
    /// Interpolation for every key: step | linear | cubic, optional. Omit to
    /// derive from the track's sampler.
    #[serde(default)]
    pub interp: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListCommandsParams {
    /// Exact command tag (e.g. "reparent") for that command's full JSON Schema;
    /// omit for the compact list of all commands.
    #[serde(default)]
    pub cmd: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddClipParams {
    /// Optional display name for the new clip (e.g. "robot_wave"); omit for the
    /// default "Clip N" numbering.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetKeyframeParams {
    pub clip: String,
    pub track: u32,
    pub index: u32,
    /// New time (seconds), optional.
    #[serde(default)]
    pub t: Option<f64>,
    /// New value, optional.
    #[serde(default)]
    pub value: Option<TrackValueArg>,
    /// New interpolation: step | linear | cubic, optional.
    #[serde(default)]
    pub interp: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteKeyframeParams {
    pub clip: String,
    pub track: u32,
    pub index: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackDataParams {
    pub clip: String,
    pub track: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackIndexParams {
    pub clip: String,
    pub track: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackMuteParams {
    pub clip: String,
    pub track: u32,
    pub mute: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackSoloParams {
    pub clip: String,
    pub track: u32,
    pub solo: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackSamplerParams {
    pub clip: String,
    pub track: u32,
    /// step | linear | cubic
    pub sampler: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StepPlayheadParams {
    /// home | prev | next | end
    pub kind: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClipNameParams {
    pub clip: String,
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClipScalarParams {
    pub clip: String,
    pub value: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClipLoopParams {
    pub clip: String,
    /// once | loop | ping_pong.
    pub loop_style: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CopyInstanceParams {
    /// Source mesh node UUID.
    pub from: String,
    /// Destination mesh node UUID (must reference the same material).
    pub to: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PlayheadParams {
    /// Playhead time in seconds.
    pub t: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PlayingParams {
    pub on: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RegionParams {
    /// Optional `[x, y, w, h]` region; whole canvas when omitted.
    #[serde(default)]
    pub region: Option<[u32; 4]>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LogLimitParams {
    /// Max entries to return (default 50).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams {
    /// Output width in px (optional; alone, preserves aspect).
    #[serde(default)]
    pub width: Option<u32>,
    /// Output height in px (optional; alone, preserves aspect).
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaitSettledParams {
    /// Max time to wait, in milliseconds (default 4000).
    #[serde(default)]
    pub max_ms: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CommandJsonParams {
    /// A raw `EditorCommand` as JSON, internally tagged by `"cmd"`. Example:
    /// `{"cmd":"set_keyframe","clip":"<uuid>","track":0,"index":0,"value":{"vec3":[0,1,0]}}`.
    /// Discover variants from docs/MCP.md or the editor command enum.
    pub command: Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchJsonParams {
    /// An ordered list of raw `EditorCommand`s (each internally tagged by
    /// `"cmd"`), applied atomically as one undo step. Example:
    /// `[{"cmd":"set_visible","id":"<uuid>","visible":false}, ...]`.
    pub commands: Vec<Value>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct QueryJsonParams {
    /// Strongly-typed `EditorQuery` (the schema lists every variant, tagged by
    /// `"query"`). Example: `{"query":"canvas_pixels","coords":[[100,100],[200,200]]}`.
    pub query: Flexible<EditorQuery>,
}

// ──────────────────────────────── the tools ─────────────────────────────────

#[tool_router]
impl EditorMcp {
    pub fn new(link: EditorLink) -> Self {
        let agent = link.register_agent();
        Self {
            link,
            agent,
            tool_router: Self::tool_router(),
        }
    }

    // ── discovery / read ────────────────────────────────────────────────────

    #[tool(
        annotations(read_only_hint = true),
        description = "Snapshot the editor state: scene tree (node ids/names/kinds), selection, mode, undo/redo depth, animation library, custom materials. Start here to discover ids."
    )]
    async fn get_snapshot(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Snapshot).await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Health check: confirms an editor is attached (fails fast with 'no editor attached' if not). Returns the current mode and the tab's visibility — `tab=HIDDEN` means the browser paused rendering (rAF), so frame-bound tools (screenshots, wait_render_settled, re-materializations) will fail fast until the tab is visible again; JS-only queries still work."
    )]
    async fn ping(&self) -> Result<CallToolResult, McpError> {
        match self.req(Request::Mode).await? {
            Response::Mode(m) => {
                let vis = match self.link.current_visibility() {
                    Some(true) => "HIDDEN — rendering paused; frame-bound tools will fail fast",
                    Some(false) => "visible",
                    None => "unknown",
                };
                Ok(text(format!(
                    "pong — editor attached (mode={m:?}, tab={vis})"
                )))
            }
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Report this server's editor connection state WITHOUT performing an editor operation: whether an editor tab is attached. This server is single-session — one server serves exactly one editor tab — so there is no pairing or pairing code. Call this first (or after a 'no editor tab is attached' error) to know whether to wait for the editor to connect, instead of issuing doomed editor calls. To run a second concurrent session, start another server on a different port and point a second editor at it."
    )]
    async fn pairing_status(&self) -> Result<CallToolResult, McpError> {
        let editors = self.link.connection_count();
        let connected = editors > 0;
        let origin = self.link.self_origin();
        let mut body = serde_json::json!({
            "status": if connected {
                "connected (an editor tab is attached)"
            } else {
                "waiting for an editor tab to connect"
            },
            "editor_connected": connected,
            "editors_connected": editors,
            "how_to_connect": format!(
                "open the awsm-renderer editor with `?mcp={origin}` appended to its URL, \
                 or enter this server's address in its MCP connect modal"
            ),
        });
        if let Some(hidden) = self.link.current_visibility() {
            body["tab_hidden"] = serde_json::json!(hidden);
            if hidden {
                body["tab_hidden_note"] = serde_json::json!(
                    "the tab reported itself HIDDEN: the browser paused rendering \
                     (requestAnimationFrame), so frame-bound tools (screenshots, \
                     wait_render_settled, re-materializations) fail fast until the \
                     tab is visible again; JS-only queries still work"
                );
            }
        }
        Ok(text(body.to_string()))
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "The last `limit` log entries: `logs` = editor toasts (info/warning/error notices), and `tracing` = raw `tracing` events (WARN/ERROR/etc. from the render loop, bridges, loader — the same lines you'd see in the browser devtools console, otherwise invisible over MCP). For material compile errors prefer get_material_diagnostics."
    )]
    async fn get_console_logs(
        &self,
        Parameters(p): Parameters<LogLimitParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::ConsoleLogs {
            limit: p.limit.unwrap_or(50),
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Live memory + renderer-object counts for leak detection and soak testing: Chrome JS-heap bytes (used/total/limit; zeros on other browsers) plus renderer counts (meshes, transforms, materials, lines, compiled render/compute pipelines). Sample repeatedly over minutes — flat-ish slopes are healthy; a steady climb on an idle scene is a leak. Pure read."
    )]
    async fn get_memory_stats(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::MemoryStats).await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Renderer-side animation runtime state — the 'why doesn't my clip pose the rig' probe. Returns the lowered clip_groups, RESOLVED channel count, per_clip channel counts, rest_entries, mixer_layers, plus the controller's current_clip / authored_tracks / playing / playhead. If resolved_channels < authored_tracks, some tracks failed to resolve (target node/material pending or deleted); resolved_channels == 0 with a live current_clip means every track targets a node that no longer exists (e.g. an orphaned clip left after its imported model was deleted). Pure read."
    )]
    async fn get_animation_runtime(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::AnimationRuntime).await
    }

    #[tool(
        description = "Replace a built-in library material's VARIANT definition wholesale (idempotent full MaterialDef as a JSON object, undoable; assigned meshes re-materialize). Key fields: shading (unit variants are PLAIN STRINGS: \"pbr\" | \"unlit\"; struct variants are single-key objects: {\"toon\":{...}} | {\"flip_book\":{\"cols\":2,\"rows\":2,\"frame_count\":4,\"fps\":2.0,\"time_offset\":0.0,\"mode\":\"loop\",\"flip_y\":false}}), alpha_mode (\"opaque\" | {\"mask\":{\"cutoff\":0.5}} | \"blend\"), double_sided, base_color (rgba), base_color_texture ({\"asset\":\"<texture-asset-id>\"} — for a FlipBook this is the ATLAS), extensions (e.g. {\"clearcoat\":{\"factor\":1.0,\"roughness_factor\":0.0}}), label. Read the current def from get_snapshot first and send it back modified. A Mask-mode FlipBook = an ANIMATED CUTOUT (alpha-tested opaque, hole-shaped shadows)."
    )]
    async fn update_builtin_material(
        &self,
        Parameters(p): Parameters<UpdateBuiltinParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_asset(&p.id)?;
        // `json_arg` (not a bare `from_value`) so a client that double-encodes
        // the object as a JSON string still parses.
        let def: awsm_renderer_editor_protocol::MaterialDef =
            json_arg(p.def.clone(), "def (a MaterialDef object)")?;
        self.dispatch(EditorCommand::UpdateBuiltinMaterial {
            id,
            def: Box::new(def),
        })
        .await
    }

    #[tool(description = "The current workspace mode (scene | material | animation).")]
    async fn get_mode(&self) -> Result<CallToolResult, McpError> {
        match self.req(Request::Mode).await? {
            Response::Mode(m) => Ok(text(format!("{m:?}"))),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Read a custom (dynamic-WGSL) material's shader source."
    )]
    async fn get_material_wgsl(
        &self,
        Parameters(p): Parameters<AssetArg>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::CustomMaterialWgsl {
            material: parse_asset(&p.asset)?,
        })
        .await
    }

    #[tool(
        description = "The dynamic-material WGSL authoring contract (the shader ABI): input.* fields, return type, time/camera access, legal shader_include + fragment_input keys. Pass transparent:true for the blend contract. Pass vertex:true for the VERTEX-displacement contract (the third, vertex WGSL window). Read this before authoring a custom material."
    )]
    async fn get_material_contract(
        &self,
        Parameters(p): Parameters<ContractParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.vertex {
            // The vertex hook owns its own (narrower) include set; the
            // fragment-only key list does not apply, so return the vertex
            // contract verbatim.
            return Ok(text(CONTRACT_VERTEX.to_string()));
        }
        let body = if p.transparent {
            CONTRACT_TRANSPARENT
        } else {
            CONTRACT_OPAQUE
        };
        Ok(text(format!("{body}\n\n{MATERIAL_KEYS_DOC}")))
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Compile diagnostics for a custom (dynamic-WGSL) material: { registered, ok, errors:[{line?,message}] }. A black mesh + ok:false means the WGSL failed to compile (the error is in `errors`); ok:true + black means a successful-but-dark shader (check lighting/inputs)."
    )]
    async fn get_material_diagnostics(
        &self,
        Parameters(p): Parameters<AssetArg>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::MaterialDiagnostics {
            material: parse_asset(&p.asset)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Local TRS + world matrix for each node (pass node UUIDs, or empty for all nodes). Reads the live scene — no animation-clip hack."
    )]
    async fn get_node_transforms(
        &self,
        Parameters(p): Parameters<NodesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::NodeTransforms {
            nodes: parse_nodes(&p.nodes)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Full per-kind config (primitive shape, light/camera config, assigned + inline material) for each node, as serialized NodeKind. Pass node UUIDs, or empty for all."
    )]
    async fn get_node_details(
        &self,
        Parameters(p): Parameters<NodesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::NodeKindDetails {
            nodes: parse_nodes(&p.nodes)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "World-space AABB { min, max } for each node (CPU-estimated; pass node UUIDs, or empty for all). Use to frame the camera or size objects. Collider nodes report bounds from their ColliderShape extents at the node's world translation+rotation (scale is not part of a collider) — so this matches what the collider gizmo and physics actually use."
    )]
    async fn get_node_bounds(
        &self,
        Parameters(p): Parameters<NodesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::NodeBounds {
            nodes: parse_nodes(&p.nodes)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Bake the whole scene to a binary glTF. Writes the .glb to a temp file and returns its path + byte length as JSON (the bytes are NOT inlined — read the file to consume it). Built-in PBR → glTF PBR; Unlit → KHR_materials_unlit; custom/Toon → AWSM_materials_none (no embedded material). Textures are referenced-only."
    )]
    async fn export_scene_glb(&self) -> Result<CallToolResult, McpError> {
        self.glb(Request::ExportGlb { node: None }).await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Bake one node (and its subtree) to a binary glTF. Writes the .glb to a temp file and returns its path + byte length as JSON (the bytes are NOT inlined — read the file to consume it). Same material mapping as export_scene_glb."
    )]
    async fn export_node_glb(
        &self,
        Parameters(p): Parameters<ExportNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.glb(Request::ExportGlb {
            node: Some(parse_node(&p.node)?),
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Geometry stats for a node's resolved mesh (Primitive/Mesh/Sweep): vertex+triangle counts, bbox, centroid, surface area, volume, watertight. The perceive half of a measure→adjust loop."
    )]
    async fn get_mesh_stats(
        &self,
        Parameters(p): Parameters<ExportNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::MeshStats {
            node: parse_node(&p.node)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Resolve the material a node actually RENDERS with — the direct answer to 'what material is on this node?' (otherwise only reachable by parsing the opaque NodeKind blob from get_node_details). Returns { assigned, kind: builtin|custom|unassigned|none, asset (material UUID), name, shading, base_color }. `unassigned` = a geometry node with no material (renders magenta); `none` = not a geometry node."
    )]
    async fn resolve_node_material(
        &self,
        Parameters(p): Parameters<ExportNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::ResolveNodeMaterial {
            node: parse_node(&p.node)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Silhouette radius profile of a node's mesh along an axis (0=X,1=Y,2=Z) in `samples` bins, as [[height,radius],…]. Pairs with a lathe (height,radius) profile — measure the tip radius, adjust, re-measure."
    )]
    async fn get_mesh_cross_section(
        &self,
        Parameters(p): Parameters<MeshCrossSectionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::MeshCrossSection {
            node: parse_node(&p.node)?,
            axis: p.axis,
            samples: p.samples,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Bake the whole project to a player runtime bundle DIRECTORY on disk: a `scene.toml` (the runtime scene — node hierarchy + transforms + material instances + lights/cameras + our animation clips + environment, meshes referenced by id) plus an `assets/` directory: one geometry-only `assets/<id>.glb` per non-primitive mesh (bare primitives stay procedural in scene.toml), custom-material wgsl folders, and referenced textures. Materials + animations are NOT in the glbs (they're ours, applied by the player from scene.toml + clips). A read; the files ride the `/bundle/<id>/<path>` side-channel and land in a temp directory — NEVER inlined in this result. Returns `{name, bundle_dir, files:[{path, byte_len}], total_bytes, url_base}`: read/copy the bundle from `bundle_dir`, or fetch files over HTTP at `<server>/<url_base>/<path>`."
    )]
    async fn export_player_bundle(
        &self,
        Parameters(p): Parameters<ExportBundleParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.req(Request::ExportPlayerBundle).await? {
            Response::Bundle(handle) => {
                let dir = crate::http::bundle_dir(&handle.id);
                // Confirm the uploads actually landed before reporting success.
                if !dir.exists() {
                    return Err(McpError::internal_error(
                        format!("bundle {} not found at {}", handle.id, dir.display()),
                        None,
                    ));
                }
                let total: usize = handle.files.iter().map(|f| f.byte_len).sum();
                let files: Vec<serde_json::Value> = handle
                    .files
                    .iter()
                    .map(|f| serde_json::json!({ "path": f.path, "byte_len": f.byte_len }))
                    .collect();
                Ok(text(
                    serde_json::json!({
                        "name": p.name,
                        "bundle_dir": dir.display().to_string(),
                        "files": files,
                        "total_bytes": total,
                        "url_base": format!("/bundle/{}", handle.id),
                    })
                    .to_string(),
                ))
            }
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Mean/min/max luma over a canvas region (or the whole canvas)."
    )]
    async fn canvas_stats(
        &self,
        Parameters(p): Parameters<RegionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::CanvasStats { region: p.region })
            .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Run a raw EditorQuery (escape hatch for queries without a dedicated tool, e.g. canvas_pixels, sample_clip_timeseries). `query` is internally tagged by \"query\"."
    )]
    async fn run_query(
        &self,
        Parameters(p): Parameters<QueryJsonParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(p.query.0).await
    }

    // ── screenshots ─────────────────────────────────────────────────────────

    #[tool(
        annotations(read_only_hint = true),
        description = "Block until the scene has settled — no material recompile pending, the renderer's pipeline scheduler drained, and a fresh frame presented — or max_ms elapses. Call between an edit and screenshot_scene so the image reflects the edit, not a mid-recompile frame. Returns { settled, waited_ms }."
    )]
    async fn wait_render_settled(
        &self,
        Parameters(p): Parameters<WaitSettledParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::WaitRenderSettled {
            max_ms: p.max_ms.unwrap_or(4000),
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "PNG screenshot of the scene viewport (through the active camera). Optional width/height scale the output (one given preserves aspect). Frame a subject first with frame_node / set_camera_orbit."
    )]
    async fn screenshot_scene(
        &self,
        Parameters(p): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        self.png(Request::ScenePng {
            width: p.width,
            height: p.height,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "PNG of the material-mode preview sphere. Optional width/height scale the output (e.g. 512 for a readable preview)."
    )]
    async fn screenshot_material(
        &self,
        Parameters(p): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        self.png(Request::MaterialPng {
            width: p.width,
            height: p.height,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "PNG thumbnail of a texture asset (by UUID)."
    )]
    async fn screenshot_texture(
        &self,
        Parameters(p): Parameters<AssetArg>,
    ) -> Result<CallToolResult, McpError> {
        self.png(Request::TexturePng(parse_asset(&p.asset)?)).await
    }

    // ── scene / nodes ───────────────────────────────────────────────────────

    #[tool(
        description = "Insert a primitive (plane/box/sphere/cylinder/cone/torus) under an optional parent."
    )]
    async fn insert_primitive(
        &self,
        Parameters(p): Parameters<InsertPrimitiveParams>,
    ) -> Result<CallToolResult, McpError> {
        let spec = InsertSpec::Primitive(default_shape(p.shape));
        self.insert(spec, p.parent).await
    }

    #[tool(description = "Insert an empty group node under an optional parent.")]
    async fn insert_empty(
        &self,
        Parameters(p): Parameters<InsertParams>,
    ) -> Result<CallToolResult, McpError> {
        self.insert(InsertSpec::Empty, p.parent).await
    }

    #[tool(description = "Insert a camera node under an optional parent.")]
    async fn insert_camera(
        &self,
        Parameters(p): Parameters<InsertParams>,
    ) -> Result<CallToolResult, McpError> {
        self.insert(InsertSpec::Camera, p.parent).await
    }

    #[tool(description = "Insert a light (directional/point/spot) under an optional parent.")]
    async fn insert_light(
        &self,
        Parameters(p): Parameters<InsertLightParams>,
    ) -> Result<CallToolResult, McpError> {
        let kind = match p.kind {
            LightArg::Directional => LightKind::Directional,
            LightArg::Point => LightKind::Point,
            LightArg::Spot => LightKind::Spot,
        };
        self.insert(InsertSpec::Light(kind), p.parent).await
    }

    #[tool(
        description = "Insert a CPU particle emitter node under an optional parent. Spawns short-lived sprites with configurable spawn rate, lifetime, initial velocity / forces, texture, and blend mode. Tune its full config (patch-style) with set_particle_emitter; bind a sprite with set_node_texture."
    )]
    async fn insert_particle(
        &self,
        Parameters(p): Parameters<InsertParams>,
    ) -> Result<CallToolResult, McpError> {
        self.insert(InsertSpec::Particle, p.parent).await
    }

    #[tool(
        description = "Insert a projection decal node under an optional parent. The node's transform is the oriented unit-cube volume; the renderer projects the decal's texture down the local -Z axis onto opaque geometry inside that volume. Its texture/config is edited via the kind (dispatch_command SetKind) for now."
    )]
    async fn insert_decal(
        &self,
        Parameters(p): Parameters<InsertParams>,
    ) -> Result<CallToolResult, McpError> {
        self.insert(InsertSpec::Decal, p.parent).await
    }

    #[tool(description = "Delete a node and its subtree.")]
    async fn delete_node(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::Delete {
            id: parse_node(&p.node)?,
        })
        .await
    }

    #[tool(
        description = "Bake a SKINNED mesh node to a static, EDITABLE mesh: discards the skin (joints/weights + skeleton) and captures the bind-pose geometry into a new captured Mesh asset, swapping the node from SkinnedMesh → Mesh. This is the TERMINAL, explicit bridge that makes an imported rigged mesh editable — a hard prerequisite for ANY geometry op (set_mesh_modifiers, vertex tools, get_mesh_layers, select_vertices_where) on it, which otherwise error 'node <id> is skinned; call drop_skinning first'. The mesh stops animating after this. Errors if the node isn't a SkinnedMesh. Undoable (restores the prior SkinnedMesh kind)."
    )]
    async fn drop_skinning(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::DropSkinning {
            node: parse_node(&p.node)?,
        })
        .await
    }

    #[tool(
        description = "Set a mesh node's shadow casting / receiving flags. Works on Mesh, SkinnedMesh, and InstancesAlongCurve nodes (the shadow-bearing kinds); errors on any other kind. `cast` = appears in the shadow-generation pass; `receive` = its shaded pixels darken under shadow. Reads the node's current kind, updates only its `shadow` config, and re-sends it (one SetKind = one undo step)."
    )]
    async fn set_mesh_shadow(
        &self,
        Parameters(p): Parameters<SetMeshShadowParams>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        let mut kind = self.current_kind(node).await?;
        let label = kind.label();
        let shadow = kind.mesh_shadow_mut().ok_or_else(|| {
            McpError::invalid_params(
                format!(
                    "node {node} is a {label} — not a shadow-bearing kind (Mesh / SkinnedMesh / \
                     InstancesAlongCurve)"
                ),
                None,
            )
        })?;
        *shadow = MeshShadowConfig {
            cast: p.cast,
            receive: p.receive,
        };
        self.dispatch(EditorCommand::SetKind {
            id: node,
            kind: Box::new(kind),
        })
        .await
    }

    #[tool(
        description = "Set a mesh node's LOD opt-out flag. Works on Mesh, SkinnedMesh, and InstancesAlongCurve nodes; errors on any other kind. `enabled` = the export-time LOD bake generates simplified detail levels for this mesh (opt-out, default on — set false for hero assets, already-low-poly meshes, or HUD/UI meshes). Authored in the editable project and consumed by the player-bundle export bake. Reads the node's current kind, updates only its `lod` config, and re-sends it (one SetKind = one undo step)."
    )]
    async fn set_mesh_lod(
        &self,
        Parameters(p): Parameters<SetMeshLodParams>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        let mut kind = self.current_kind(node).await?;
        let label = kind.label();
        let lod = kind.mesh_lod_mut().ok_or_else(|| {
            McpError::invalid_params(
                format!(
                    "node {node} is a {label} — not a LOD-bearing kind (Mesh / SkinnedMesh / \
                     InstancesAlongCurve)"
                ),
                None,
            )
        })?;
        *lod = MeshLodConfig { enabled: p.enabled };
        self.dispatch(EditorCommand::SetKind {
            id: node,
            kind: Box::new(kind),
        })
        .await
    }

    #[tool(
        description = "Set the per-instance tint colors of an InstancesAlongCurve node — one linear RGBA per placed instance, in placement order. Pass an empty list to clear the tints (every instance renders with its material color). Errors if the node isn't an InstancesAlongCurve. Reads the node's current kind, replaces `per_instance_colors`, and re-sends it (one SetKind = one undo step)."
    )]
    async fn set_instance_colors(
        &self,
        Parameters(p): Parameters<SetInstanceColorsParams>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        let kind = self.current_kind(node).await?;
        let NodeKind::InstancesAlongCurve(mut def) = kind else {
            return Err(McpError::invalid_params(
                format!(
                    "node {node} is a {} — not an InstancesAlongCurve",
                    kind.label()
                ),
                None,
            ));
        };
        def.per_instance_colors = p.colors;
        self.dispatch(EditorCommand::SetKind {
            id: node,
            kind: Box::new(NodeKind::InstancesAlongCurve(def)),
        })
        .await
    }

    #[tool(
        description = "Duplicate a node (deep clone, fresh ids) as a following sibling. Returns the new clone's root node id (descendants get fresh ids — use get_children/get_subtree on the returned id to find them)."
    )]
    async fn duplicate_node(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        let new_id = NodeId::new();
        match self
            .req(Request::Dispatch(EditorCommand::Duplicate {
                id: parse_node(&p.node)?,
                new_id: Some(new_id),
            }))
            .await?
        {
            Response::Ok => Ok(text(new_id.to_string())),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Direct children of a node as a lightweight [{ id, name, kind }] list — find a node you just created (e.g. a duplicate_node clone's descendants) without the heavy whole-scene get_snapshot."
    )]
    async fn get_children(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::GetChildren {
            node: parse_node(&p.node)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "The id/name/kind subtree rooted at `node` (or EVERY scene root when `node` is omitted), with nested `children` — the lightweight alternative to get_snapshot for navigating the hierarchy without the per-node config blobs."
    )]
    async fn get_subtree(
        &self,
        Parameters(p): Parameters<OptionalNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::GetSubtree {
            root: parse_node_opt(&p.node)?,
        })
        .await
    }

    #[tool(
        description = "Reparent a node under new_parent (root when omitted) at an optional index."
    )]
    async fn reparent_node(
        &self,
        Parameters(p): Parameters<ReparentParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::Reparent {
            id: parse_node(&p.node)?,
            new_parent: parse_node_opt(&p.new_parent)?,
            index: p.index.map(|i| i as usize),
        })
        .await
    }

    #[tool(
        description = "Set a node's local transform: translation [x,y,z], rotation quaternion [x,y,z,w], scale [x,y,z]. NOTE: collider nodes have no scale — `scale` is forced to [1,1,1] for them (a Rapier collider's size is its ColliderShape extents, and only translation+rotation export). To resize a collider, edit its shape (set_kind/patch_kind), not this scale."
    )]
    async fn node_set_transform(
        &self,
        Parameters(p): Parameters<SetTransformParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetTransform {
            id: parse_node(&p.node)?,
            transform: Trs {
                translation: p.translation,
                rotation: p.rotation,
                scale: p.scale,
            },
        })
        .await
    }

    #[tool(
        description = "Aim a node at a world-space target: computes the roll-free rotation that points the node's local -Z (the forward axis of lights and cameras) at `target`, converts it into the node's parent space, and applies it — translation and scale are untouched. Params: { node, target: [x,y,z], up?: [x,y,z] }. Replaces hand-computing quaternions for spot/camera aiming."
    )]
    async fn node_look_at(
        &self,
        Parameters(p): Parameters<LookAtParams>,
    ) -> Result<CallToolResult, McpError> {
        use glam::{Mat3, Mat4, Quat, Vec3};
        let node = parse_node(&p.node)?;
        let (trs, world) = self.current_trs_and_world(node).await?;

        let world_mat = Mat4::from_cols_array(&world);
        let world_pos = world_mat.w_axis.truncate();
        let target = Vec3::from_array(p.target);
        let dir = target - world_pos;
        if dir.length_squared() < 1e-12 {
            return Err(McpError::invalid_params(
                "look_at target coincides with the node's position",
                None,
            ));
        }
        let dir = dir.normalize();

        // Roll-free frame: -Z = dir; X ⊥ up-hint; fall back when the aim
        // direction is (anti)parallel to the hint.
        let mut up = Vec3::from_array(p.up.unwrap_or([0.0, 1.0, 0.0]));
        if up.cross(dir).length_squared() < 1e-8 {
            up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
        }
        let z = -dir;
        let x = up.cross(z).normalize();
        let y = z.cross(x);
        let world_rot = Quat::from_mat3(&Mat3::from_cols(x, y, z));

        // Local = parent⁻¹ * world. Parent rotation from the node's world
        // basis with scale stripped, divided by its current local rotation.
        let local_rot_cur = Quat::from_array(trs.rotation);
        let basis = Mat3::from_cols(
            world_mat.x_axis.truncate().normalize(),
            world_mat.y_axis.truncate().normalize(),
            world_mat.z_axis.truncate().normalize(),
        );
        let world_rot_cur = Quat::from_mat3(&basis).normalize();
        let parent_rot = (world_rot_cur * local_rot_cur.inverse()).normalize();
        let local_rot = (parent_rot.inverse() * world_rot).normalize();

        self.dispatch(EditorCommand::SetTransform {
            id: node,
            transform: Trs {
                translation: trs.translation,
                rotation: local_rot.to_array(),
                scale: trs.scale,
            },
        })
        .await
    }

    #[tool(
        description = "Set a node's local translation [x,y,z], keeping its current rotation + scale."
    )]
    async fn set_translation(
        &self,
        Parameters(p): Parameters<Vec3Params>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        let mut trs = self.current_trs(node).await?;
        trs.translation = p.value;
        self.dispatch(EditorCommand::SetTransform {
            id: node,
            transform: trs,
        })
        .await
    }

    #[tool(
        description = "Translate a node by a local delta [dx,dy,dz] (added to its current translation)."
    )]
    async fn translate_by(
        &self,
        Parameters(p): Parameters<Vec3Params>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        let mut trs = self.current_trs(node).await?;
        trs.translation = [
            trs.translation[0] + p.value[0],
            trs.translation[1] + p.value[1],
            trs.translation[2] + p.value[2],
        ];
        self.dispatch(EditorCommand::SetTransform {
            id: node,
            transform: trs,
        })
        .await
    }

    #[tool(
        description = "Set a node's per-axis scale [x,y,z], keeping its translation + rotation."
    )]
    async fn set_scale(
        &self,
        Parameters(p): Parameters<Vec3Params>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        let mut trs = self.current_trs(node).await?;
        trs.scale = p.value;
        self.dispatch(EditorCommand::SetTransform {
            id: node,
            transform: trs,
        })
        .await
    }

    #[tool(
        description = "Set a node's local rotation from Euler angles (radians) + order (default xyz), keeping translation + scale. Avoids hand-computing quaternions."
    )]
    async fn set_rotation_euler(
        &self,
        Parameters(p): Parameters<EulerParams>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        let order = euler_order(p.order.as_deref())?;
        let q = glam::Quat::from_euler(order, p.euler[0], p.euler[1], p.euler[2]);
        let mut trs = self.current_trs(node).await?;
        trs.rotation = [q.x, q.y, q.z, q.w];
        self.dispatch(EditorCommand::SetTransform {
            id: node,
            transform: trs,
        })
        .await
    }

    #[tool(description = "Rename a node.")]
    async fn rename_node(
        &self,
        Parameters(p): Parameters<RenameParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::Rename {
            id: parse_node(&p.node)?,
            name: p.name,
        })
        .await
    }

    #[tool(description = "Set a node's visibility (eye toggle).")]
    async fn set_node_visible(
        &self,
        Parameters(p): Parameters<SetBoolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetVisible {
            id: parse_node(&p.node)?,
            visible: p.value,
        })
        .await
    }

    #[tool(description = "Set a node's locked flag.")]
    async fn set_node_locked(
        &self,
        Parameters(p): Parameters<SetBoolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetLocked {
            id: parse_node(&p.node)?,
            locked: p.value,
        })
        .await
    }

    #[tool(description = "Mark a node as a prefab root (or clear the flag).")]
    async fn set_prefab(
        &self,
        Parameters(p): Parameters<SetBoolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetPrefab {
            id: parse_node(&p.node)?,
            prefab: p.value,
        })
        .await
    }

    #[tool(description = "Set the selection (ordered node UUIDs; last = primary).")]
    async fn set_selection(
        &self,
        Parameters(p): Parameters<SelectionParams>,
    ) -> Result<CallToolResult, McpError> {
        let ids = p
            .ids
            .iter()
            .map(|s| parse_node(s))
            .collect::<Result<Vec<_>, _>>()?;
        self.dispatch(EditorCommand::SetSelection { ids }).await
    }

    #[tool(
        description = "Highlight a node mesh's vertices in the viewport (read-only overlay; no geometry change). Pairs with select_vertices_where: run that query to get matching indices, then call this so the human can SEE which vertices matched (a small amber cross marks each). Pass an empty `indices` to clear the highlight."
    )]
    async fn set_vertex_selection(
        &self,
        Parameters(p): Parameters<VertexSelectionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetVertexSelection {
            node: parse_node(&p.node)?,
            indices: p.indices,
        })
        .await
    }

    // ── project / import / history ──────────────────────────────────────────

    #[tool(
        description = "Start a fresh project (clears undo history). Re-seeds the default environment + IBL and a single key Directional light — NOT a fully empty scene. For an IBL-only / punctual-light-free baseline (e.g. to test custom-material IBL), delete the seeded Directional light first (get_snapshot to find it, then delete_node)."
    )]
    async fn new_project(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::NewProject).await
    }

    #[tool(
        description = "Round-trip self-test: bake the CURRENT project to an in-memory player bundle (scene.toml + assets/), reset to empty, then reload it through populate_awsm_scene (the runtime/player path). DESTRUCTIVE: the viewport ends up showing the reload and the scene tree is left empty (reload your project to keep editing). Workflow: screenshot_scene (authored) → load_player_bundle → wait_render_settled → screenshot_scene (runtime reload), and compare. Geometry/built-in-materials/lights load today; textures, custom-WGSL, glb-mesh materials, cameras + clips are follow-ons."
    )]
    async fn load_player_bundle(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::LoadPlayerBundle).await
    }

    #[tool(description = "Load a project from a base URL (fetches <base>/project.toml).")]
    async fn load_project_from_url(
        &self,
        Parameters(p): Parameters<BaseUrlParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::LoadProjectFromUrl {
            base_url: p.base_url,
        })
        .await
    }

    #[tool(
        description = "Import a glTF/glb model from a URL and return the import report (created root node ids/names, node/material/skin-joint/clip counts, source asset id). Fails with the load error when the fetch/parse fails — note the URL's server must send CORS headers (`Access-Control-Allow-Origin`): the editor is a browser app, and e.g. plain `python3 -m http.server` will fail."
    )]
    async fn import_model_from_url(
        &self,
        Parameters(p): Parameters<UrlParams>,
    ) -> Result<CallToolResult, McpError> {
        // Dispatch first (errors — e.g. CORS fetch failures — propagate here),
        // then return WHAT the import created instead of a bare "ok".
        self.dispatch(EditorCommand::ImportModelFromUrl { url: p.url })
            .await?;
        self.query(EditorQuery::LastImportReport).await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Report of the most recent model import this session: created root node ids/names, node/material/skin-joint/clip counts, and the source asset id. `report: null` when nothing has been imported. The import tools also return this inline; use this to re-read it later without re-importing."
    )]
    async fn get_last_import_report(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::LastImportReport).await
    }

    #[tool(
        description = "Import a PRE-BAKED nanite / cluster-LOD asset as a VIEW-ONLY mesh. \
        `clusters_url` points at a `<id>.clusters.bin` produced offline by the `awsm-renderer-lod-bake` \
        CLI (which converts a glTF/GLB). The editor renders it through the bounded cluster \
        pipeline — the same path the player uses — so a multi-million-triangle mesh views as \
        nanite (bounded draw + VRAM) without the dense explode that would otherwise crash the \
        editor. The node is non-editable (no geometry stack / modifiers — it IS the LOD); \
        move/scale it and assign a material like any node. Use this instead of \
        `import_model_from_url` for heavy static meshes."
    )]
    async fn import_nanite_asset(
        &self,
        Parameters(p): Parameters<ImportNaniteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::ImportNaniteAsset {
            clusters_url: p.clusters_url,
        })
        .await
    }

    #[tool(description = "Undo the last recorded command.")]
    async fn undo(&self) -> Result<CallToolResult, McpError> {
        match self.req(Request::Undo).await? {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(description = "Redo the last undone command.")]
    async fn redo(&self) -> Result<CallToolResult, McpError> {
        match self.req(Request::Redo).await? {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    // ── materials ───────────────────────────────────────────────────────────

    #[tool(
        description = "Create a fresh custom WGSL (dynamic) material and make it current. Returns the new material id."
    )]
    async fn add_custom_material(&self) -> Result<CallToolResult, McpError> {
        let id = AssetId::new();
        self.dispatch_echo_asset(EditorCommand::AddCustomMaterial { id }, id)
            .await
    }

    #[tool(
        description = "Create a fresh built-in material (pbr | unlit | toon | flipbook). Returns the new material id. Flipbook: the atlas is the base-color texture; 4×4/16-frame/12fps/loop defaults — knob edits + Mask (animated cutout) via dispatch_command."
    )]
    async fn add_builtin_material(
        &self,
        Parameters(p): Parameters<ShadingParams>,
    ) -> Result<CallToolResult, McpError> {
        let shading = match p.shading {
            ShadingArg::Pbr => MaterialShading::Pbr,
            ShadingArg::Unlit => MaterialShading::Unlit,
            ShadingArg::Toon => MaterialShading::Toon {
                diffuse_bands: 3,
                rim_strength: 0.4,
                specular_steps: 2,
                shininess: 32.0,
                rim_power: 2.0,
            },
            ShadingArg::Flipbook => MaterialShading::FlipBook {
                cols: 4,
                rows: 4,
                frame_count: 16,
                fps: 12.0,
                time_offset: 0.0,
                mode: awsm_renderer_editor_protocol::FlipBookPlayMode::Loop,
                flip_y: false,
            },
        };
        let id = AssetId::new();
        self.dispatch_echo_asset(EditorCommand::AddBuiltinMaterial { id, shading }, id)
            .await
    }

    #[tool(
        description = "Retired/no-op: every procedural node is already an editable Mesh backed by a ModifierStack (MeshDef), so there is nothing to convert. Echoes the node's EXISTING mesh asset id (use it with set_mesh_modifiers / vertex tools). Errors if the node isn't a Mesh."
    )]
    async fn convert_to_editable_mesh(
        &self,
        Parameters(p): Parameters<ExportNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        // Resolve the node's existing mesh asset id from its serialized kind.
        let resp = self
            .req(Request::Query(EditorQuery::NodeKindDetails {
                nodes: vec![node],
            }))
            .await?;
        let Response::Query(qr) = resp else {
            return Err(unexpected(resp));
        };
        let QueryResult::Map(m) = *qr else {
            return Err(McpError::internal_error("unexpected kind result", None));
        };
        let entry = m.entries.get(&node.to_string());
        // A SkinnedMesh isn't editable — steer the agent to drop_skinning, which
        // bakes its bind pose into an editable Mesh.
        if entry.and_then(|v| v.get("skinned_mesh")).is_some() {
            return Err(McpError::invalid_params(
                format!("node {node} is skinned; call drop_skinning first"),
                None,
            ));
        }
        let mesh = entry
            .and_then(|v| v.get("mesh"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_params(format!("node {node} is not a Mesh"), None))?;
        Ok(text(mesh.to_string()))
    }

    #[tool(
        description = "Replace an editable mesh's procedural recipe (modifier stack). `mesh` is the mesh asset UUID; `stack` is a ModifierStack JSON { base, modifiers }. base = primitive/lathe/superquadric/sweep/captured/sdf; modifiers = ordered taper/twist/bend/inflate/spherify/roughen/subdivide/smooth/mirror/array/displace. **Read the `awsm://docs/mesh-tools` resource for the full JSON shapes + copy-paste examples (twist, lathe bat, SDF mug).** Re-bakes geometry; the recipe lives in the project, the .mesh.bin is a regenerable cache."
    )]
    async fn set_mesh_modifiers(
        &self,
        Parameters(p): Parameters<SetMeshModifiersParams>,
    ) -> Result<CallToolResult, McpError> {
        let mesh = AssetId(uuid::Uuid::parse_str(&p.mesh).map_err(|e| {
            McpError::invalid_params(format!("invalid mesh id {:?}: {e}", p.mesh), None)
        })?);
        self.dispatch(EditorCommand::SetMeshModifiers {
            mesh,
            stack: p.stack.0,
        })
        .await
    }

    #[tool(
        description = "Append one modifier to the END of a mesh's modifier stack — the convenience alternative to resending the whole stack via set_mesh_modifiers. `mesh` is the mesh asset UUID; `modifier` is a single Modifier object (e.g. {\"twist\":{\"axis\":\"y\",\"turns\":2}}, {\"taper\":{\"axis\":\"y\",\"factor\":0.3}}, {\"subdivide\":{\"iterations\":2}}). Every Mesh node already carries a stack (its base shape), so this works on any mesh asset id; errors only if `mesh` isn't a mesh asset. Re-bakes geometry; each call is one discrete undo step. Read get_mesh_modifiers to see the current stack + indices. Full modifier shapes: the `awsm://docs/mesh-tools` resource."
    )]
    async fn add_modifier(
        &self,
        Parameters(p): Parameters<AddModifierParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AddModifier {
            mesh: parse_asset(&p.mesh)?,
            modifier: p.modifier.0,
        })
        .await
    }

    #[tool(
        description = "Replace the modifier at `index` (zero-based) in a mesh's existing modifier stack with `modifier`. `mesh` is the mesh asset UUID. PRECONDITION: the mesh must already have a modifier stack and `index` must be in range — both error otherwise. Re-bakes geometry; one discrete undo step. Use get_mesh_modifiers to read the current stack + valid indices first. Modifier shapes: the `awsm://docs/mesh-tools` resource."
    )]
    async fn set_modifier(
        &self,
        Parameters(p): Parameters<SetModifierParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetModifier {
            mesh: parse_asset(&p.mesh)?,
            index: p.index,
            modifier: p.modifier.0,
        })
        .await
    }

    #[tool(
        description = "Remove the modifier at `index` (zero-based) from a mesh's existing modifier stack. `mesh` is the mesh asset UUID. PRECONDITION: the mesh must already have a modifier stack and `index` must be in range — both error otherwise. Re-bakes geometry; one discrete undo step. Use get_mesh_modifiers to read the current stack + valid indices first."
    )]
    async fn remove_modifier(
        &self,
        Parameters(p): Parameters<RemoveModifierParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::RemoveModifier {
            mesh: parse_asset(&p.mesh)?,
            index: p.index,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Read a mesh's current modifier-stack recipe ({ base, modifiers }) as JSON. `mesh` is the mesh asset UUID. Returns `null` when the mesh has no recipe yet (a raw captured/converted mesh — call set_mesh_modifiers to give it a base before add_/set_/remove_modifier). The read half of incremental modifier editing: read the stack, find the index you want, then add_/set_/remove_modifier."
    )]
    async fn get_mesh_modifiers(
        &self,
        Parameters(p): Parameters<GetMeshModifiersParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::MeshModifiers {
            mesh: parse_asset(&p.mesh)?,
        })
        .await
    }

    #[tool(
        description = "Replace specific vertices' positions on an editable mesh (raw editing). `indices[k]` ↦ `positions[k]`; normals are recomputed. Undo restores the prior positions (sparse)."
    )]
    async fn set_vertex_positions(
        &self,
        Parameters(p): Parameters<SetVertexPositionsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetVertexPositions {
            mesh: parse_asset(&p.mesh)?,
            indices: p.indices,
            positions: p.positions,
            selection: p.selection,
        })
        .await
    }

    #[tool(
        description = "Translate a vertex selection with a smooth radial falloff (server computes the per-vertex weights). `falloff` 0 = hard move of exactly the selection. Pairs with select-by-predicate + get_mesh_stats for cursor-free editing."
    )]
    async fn soft_transform_vertices(
        &self,
        Parameters(p): Parameters<SoftTransformParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SoftTransformVertices {
            mesh: parse_asset(&p.mesh)?,
            indices: p.indices,
            translation: p.translation,
            falloff: p.falloff,
            selection: p.selection,
        })
        .await
    }

    #[tool(
        description = "Bake an editable mesh's modifier stack into raw triangles and clear the recipe (the deliberate heavy snapshot, undoable). After this the mesh is raw-vertex-edited via set_vertex_positions / soft_transform_vertices."
    )]
    async fn collapse_mesh_stack(
        &self,
        Parameters(p): Parameters<MeshIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::CollapseMeshStack {
            mesh: parse_asset(&p.mesh)?,
        })
        .await
    }

    #[tool(
        description = "Detach the faces fully covered by a vertex selection into a NEW sibling Mesh node — region isolation, e.g. to give one region (a belt, a panel, a bolt) its own material. A triangle moves when ALL 3 of its vertices are selected; pick the region with select_vertices_where (the {\"kind\":\"connected_to_seed\"} predicate grabs a whole connected piece). `node` is the source node UUID; pass `selection` (a stored handle) or `indices`. By default the source is left intact (the new node is an extracted COPY); pass `keep_remainder:true` to also REMOVE those faces from the source (no overlap / z-fighting). The new node inherits the source's transform + material palette — add_material_variant + select_material_variant to give it a different look next. Undoable. Returns ok."
    )]
    async fn separate_mesh(
        &self,
        Parameters(p): Parameters<SeparateMeshParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SeparateMesh {
            node: parse_node(&p.node)?,
            indices: p.indices,
            selection: p.selection,
            new_node: None,
            keep_remainder: p.keep_remainder,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Select a node mesh's vertices by predicate (no cursor), returning their indices to feed into set_vertex_positions / soft_transform_vertices. `predicate` is a VertexPredicate JSON. For \"the top of the mesh\" pick the right notion: {\"kind\":\"top_count\",\"axis\":1,\"count\":40} = the 40 HIGHEST verts by count (use get_mesh_stats for the total to turn a fraction into a count); {\"kind\":\"top_percent\",\"axis\":1,\"percent\":0.2} = every vert in the top 20% of the axis EXTENT (a height band — the count it returns depends on tessellation, not 0.2). Others: {\"kind\":\"normal_dir\",\"dir\":[0,1,0],\"threshold\":0.7} / axis_greater / axis_less / within_radius / within_aabb (box: {\"kind\":\"within_aabb\",\"min\":[x,y,z],\"max\":[x,y,z]} — local space; pair with get_node_bounds for region selection). TOPOLOGY (island) selection: {\"kind\":\"connected_to_seed\",\"seed\":[i,...]} selects the whole connected PIECE(s) containing the seed vertices — position-welded so a UV/normal seam doesn't fragment a solid piece (grab \"this whole bolt/belt/panel\" from one seed; companion to separate_mesh). §10: pass `store:true` to keep the indices SERVER-SIDE and get back a reusable `{id,count}` HANDLE — then paint_vertex_colors / soft_transform_vertices / set_vertex_positions / set_vertex_normals / get_vertex_data accept `selection:<id>` so ONE selection drives many ops and a full-res band never crosses the token cap. `count_only:true` returns just the count; `offset`/`limit` page the raw indices. (The fused paint_where/transform_where are the one-shot alternative.)"
    )]
    async fn select_vertices_where(
        &self,
        Parameters(p): Parameters<SelectVerticesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::SelectVerticesWhere {
            node: parse_node(&p.node)?,
            predicate: p.predicate.0,
            store: p.store,
            count_only: p.count_only,
            offset: p.offset,
            limit: p.limit,
        })
        .await
    }

    #[tool(
        description = "Paint per-vertex COLORS on an editable mesh. `mesh` is the mesh asset UUID; `indices` are vertex indices (into the resolved/baked topology — get them from select_vertices_where); `color` is a linear RGBA [r,g,b,a]. FOOTGUN: UNPAINTED vertices default to (1,1,1,1) WHITE, not 0 — a splat shader mix(base, snow, vColor.r) reads full weight everywhere until painted (whole mesh = snow). Clear-to-0 first: paint the whole mesh to [0,0,0,1] (use paint_where with a giant within_aabb predicate), THEN paint the band. TERMINAL/COLLAPSE: the first per-vertex authoring op freezes the procedural stack to a Captured base (topology locks; modifier params bake in) — after this only the sparse override layer is editable. NOTE: painted colors only DISPLAY under a material that reads vertex colors — built-in PBR with `vertex_colors_enabled`, or a custom material that samples them (see the texture-splatting recipe in `awsm://docs/mesh-tools`). Re-bakes geometry; coalesces consecutive strokes on one mesh into one undo step. Verify with get_vertex_data."
    )]
    async fn paint_vertex_colors(
        &self,
        Parameters(p): Parameters<PaintVertexColorsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::PaintVertexColors {
            mesh: parse_asset(&p.mesh)?,
            indices: p.indices,
            color: p.color,
            selection: p.selection,
        })
        .await
    }

    #[tool(
        description = "FUSED select-and-paint: pick the vertices of `node`'s resolved mesh matching `predicate` (same shapes as select_vertices_where) and paint them `color` (linear RGBA) in ONE call. Use this instead of select_vertices_where→paint_vertex_colors for full-resolution selections — a height-band/slope match on a real terrain can be tens of thousands of indices that overflow the tool-result token cap when round-tripped; here the index array stays server-side. TIP: clear a splat mask to 0 in one call with predicate {\"kind\":\"within_aabb\",\"min\":[-1e9,-1e9,-1e9],\"max\":[1e9,1e9,1e9]} + color [0,0,0,1] BEFORE painting the band (unpainted verts default to (1,1,1,1) WHITE = full weight). Same collapse/re-bake/undo semantics + display caveat as paint_vertex_colors (needs a vertex-color-reading material). Verify with get_vertex_data or a screenshot."
    )]
    async fn paint_where(
        &self,
        Parameters(p): Parameters<PaintWhereParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::PaintVerticesWhere {
            node: parse_node(&p.node)?,
            predicate: p.predicate.0,
            color: p.color,
        })
        .await
    }

    #[tool(
        description = "FUSED select-and-soft-transform: pick the vertices of `node`'s resolved mesh matching `predicate` and translate them by `translation` with a smooth radial `falloff` (0 = move exactly the selection), in ONE call (indices stay server-side — see paint_where). Same collapse/re-bake/undo semantics as soft_transform_vertices. Verify with get_mesh_stats / get_vertex_data / a screenshot."
    )]
    async fn transform_where(
        &self,
        Parameters(p): Parameters<TransformWhereParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::TransformVerticesWhere {
            node: parse_node(&p.node)?,
            predicate: p.predicate.0,
            translation: p.translation,
            falloff: p.falloff,
        })
        .await
    }

    #[tool(
        description = "Override per-vertex NORMALS on an editable mesh (hand-author shading — e.g. flatten a face, fake a crease). `mesh` is the mesh asset UUID; `indices` are vertex indices; `normal` is the vector [x,y,z] (unit-length) set on each. An explicit normal override always wins over the auto-recompute that follows position sculpting. TERMINAL/COLLAPSE: freezes the procedural stack on first authoring op (see paint_vertex_colors). Re-bakes geometry; coalesces on one mesh. Verify with get_vertex_data."
    )]
    async fn set_vertex_normals(
        &self,
        Parameters(p): Parameters<SetVertexNormalsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetVertexNormals {
            mesh: parse_asset(&p.mesh)?,
            indices: p.indices,
            normal: p.normal,
            selection: p.selection,
        })
        .await
    }

    #[tool(
        description = "Author per-vertex UVs (TEXCOORD_0) on an editable mesh — the write verb that completes the per-vertex authoring family (positions/colors/normals already had one). `mesh` is the mesh asset UUID; `indices` are vertex indices (into the resolved/baked topology — get them from select_vertices_where or get_mesh_data); `uvs[k]` is the [u,v] written to `indices[k]` (per-vertex parallel arrays, so a whole continuous strip parameterization lands in one call). Use this to lay a continuous strip UV (travel along one axis) for conveyor/tread/road scrolling — see the 'Geometry-locked scroll' recipe in `awsm://docs/material-recipes`, and pair with strip_parameterize to compute (along, across) coords. TERMINAL/COLLAPSE: the first per-vertex authoring op freezes the procedural stack to a Captured base (topology locks). The bake creates the UV channel if the mesh had none. Single UV set (0). Re-bakes; verify with get_vertex_data."
    )]
    async fn set_vertex_uvs(
        &self,
        Parameters(p): Parameters<SetVertexUvsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetVertexUvs {
            mesh: parse_asset(&p.mesh)?,
            indices: p.indices,
            uvs: p.uvs,
            selection: p.selection,
        })
        .await
    }

    #[tool(
        description = "Displace a node's mesh by an agent-authored HEIGHTMAP image (§16) — the generic 'supply your own heightfield' hook. `url` is a hosted PNG/JPEG heightmap (author ANY terrain externally — eroded ridges, a stamped logo, scanned relief, multi-octave fbm baked to a PNG — host it, pass the URL); each vertex moves along its normal by luminance(heightmap @ its UV) * `strength` (black = flat, white = raised; negative strength carves in). Needs a UV-mapped, sufficiently TESSELLATED mesh (subdivide a plane via set_mesh_modifiers first — displacement only moves existing verts). Collapses to a frozen-topology override layer (like the sculpt verbs) and re-bakes; undoable. Verify with get_mesh_stats (bbox grows) or a screenshot."
    )]
    async fn displace_from_texture(
        &self,
        Parameters(p): Parameters<DisplaceFromTextureParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::DisplaceFromTexture {
            node: parse_node(&p.node)?,
            url: p.url,
            strength: p.strength,
        })
        .await
    }

    #[tool(
        description = "Project-wide FINALIZE: collapse every Mesh asset's modifier stack to a frozen-topology Captured base (bakes all procedural params + override layers into the geometry cache). The deliberate whole-project bake before export/handoff. Undoable (restores every mesh's prior stack as one step). No params."
    )]
    async fn bake_all(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::BakeAll {}).await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Read the FINAL (post-eval + override) per-vertex data for specific indices of a node's resolved mesh: returns `{ vertex_count, vertices: [{ index, position, normal, color, uv }] }` (color/uv null when the mesh has no such channel). The read counterpart to paint_vertex_colors / set_vertex_normals / set_vertex_positions / set_vertex_uvs — confirm what your last authoring op actually produced. `node` is the node UUID; `indices` the verts to read. Pass `include_source:true` to also get a per-vertex `source:{position,normal,color,uv}` block tagging each channel `\"override\"` (authored) or `\"base\"` (rides the evaluated geometry)."
    )]
    async fn get_vertex_data(
        &self,
        Parameters(p): Parameters<GetVertexDataParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::GetVertexData {
            node: parse_node(&p.node)?,
            indices: p.indices,
            selection: p.selection,
            offset: p.offset,
            limit: p.limit,
            include_source: p.include_source,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Read a node mesh's LAYER SUMMARY — what's live (still procedural) vs locked (frozen-topology authoring): `{ base, modifiers, modifier_count, frozen_topology, has_overrides, override_counts:{positions,colors,normals,uvs} }`. `base` is primitive/lathe/superquadric/sweep/sdf/captured; `frozen_topology` true means per-vertex authoring already collapsed the stack (terminal). The perceive for deciding whether to edit modifiers (still procedural) or author per-vertex (terminal). `node` is the node UUID."
    )]
    async fn get_mesh_layers(
        &self,
        Parameters(p): Parameters<GetMeshLayersParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::GetMeshLayers {
            node: parse_node(&p.node)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Read a node's resolved-mesh TOPOLOGY: `{ vertex_count, triangle_count, triangles:[[a,b,c],…], offset, returned, bbox:{min,max} }` — the triangle index buffer (paged by triangle via `offset`/`limit`, since a full index buffer overflows the token cap) plus counts and the local-space bounding box. The read counterpart to set_mesh_data and the connectivity source for loop-ordering / edge-adjacency / arc-length (e.g. ordering a conveyor belt loop before set_vertex_uvs). Per-vertex attributes (position/normal/uv/color) come from get_vertex_data — this returns only indices + metadata to stay compact. `node` is the node UUID."
    )]
    async fn get_mesh_data(
        &self,
        Parameters(p): Parameters<GetMeshDataParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::GetMeshData {
            node: parse_node(&p.node)?,
            offset: p.offset,
            limit: p.limit,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "HEURISTIC strip/loop parameterization of a vertex band → normalized (along, across) UVs to feed straight into set_vertex_uvs for a conveyor / tread / road. Returns `{ axis, count, vertices:[{index, along, across}], heuristic:true, note }`: `along` ∈ [0,1) = angle about the axle (monotonic travel around the loop), `across` ∈ [0,1] = lateral position along the axle. `axis` is the axle [x,y,z] (normalized); omit to auto-fit it as the band's least-variance PCA direction — but auto-fit is BEST-EFFORT and unreliable on near-isotropic bands (e.g. a tube whose height ≈ diameter, where the axle and a radial direction have comparable variance), so PREFER passing an explicit `axis` for treads/belts (you usually know the axle, e.g. split L/R belts by x and spin about the wheel axle). Target band: a `selection` HANDLE (from select_vertices_where {store:true}), an explicit `indices` list, or — both omitted — the whole mesh. It's a heuristic (assumes a surface of revolution about the axle, not a true geodesic unwrap); the winding direction / polarity may come out flipped — pass an explicit `axis`, or use `1-along`/`1-across`, to correct. Pairs with set_vertex_uvs (write the coords) + a texture_transform V-scroll."
    )]
    async fn strip_parameterize(
        &self,
        Parameters(p): Parameters<StripParameterizeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::StripParameterize {
            node: parse_node(&p.node)?,
            selection: p.selection,
            indices: p.indices,
            axis: p.axis,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "UV-layout overlay of a node's resolved mesh (UV set `uv_set`, default 0): `{ has_uv, uv_set, island_count, bounds:{min,max}, islands:[{count,min,max}], edge_count, edges:[[[u,v],[u,v]],…] }`. Diagnoses 'atlas vs strip' in ONE read — a continuous strip UV is ONE island spanning ~[0,1] (good for scrolling/tiling); a baked atlas is MANY small islands (scrolling slides samples onto unrelated content). `edges` is the UV wireframe for drawing the overlay, paged by `offset`/`limit` (DEFAULT 1000 edges/page when `limit` is omitted, so a naive call stays small — `edge_count` is the full total and `returned` the page length; page via `offset` for the rest); the island summaries are always full. `has_uv:false` means the mesh carries no such UV set."
    )]
    async fn get_uv_layout(
        &self,
        Parameters(p): Parameters<UvLayoutParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::UvLayout {
            node: parse_node(&p.node)?,
            uv_set: p.uv_set,
            offset: p.offset,
            limit: p.limit,
        })
        .await
    }

    #[tool(
        description = "Delete a custom (dynamic/built-in) material by id. Params: { asset: <material asset UUID> } (NOT {material})."
    )]
    async fn delete_custom_material(
        &self,
        Parameters(p): Parameters<AssetArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::DeleteCustomMaterial {
            id: parse_asset(&p.asset)?,
        })
        .await
    }

    #[tool(description = "Delete a project asset (material / texture) from the asset table by id.")]
    async fn delete_asset(
        &self,
        Parameters(p): Parameters<AssetArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::DeleteAsset {
            id: parse_asset(&p.asset)?,
        })
        .await
    }

    #[tool(
        description = "Purge ALL unused project assets in one undoable step — deletes every texture / material / mesh / buffer NOT referenced by the live scene. The reachable set is walked from node material/mesh/texture/buffer bindings, the environment cubemaps, and animation targets (transitively), so an asset still in use is NEVER removed. Use after importing/replacing assets to drop orphans left behind. Verify the result with get_snapshot."
    )]
    async fn purge_unused(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::PurgeUnusedAssets).await
    }

    #[tool(
        description = "Copy a mesh's per-mesh material instance (its inline uniform values) onto another mesh that references the same assigned material."
    )]
    async fn copy_material_instance(
        &self,
        Parameters(p): Parameters<CopyInstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::CopyMaterialInstance {
            from: parse_node(&p.from)?,
            to: parse_node(&p.to)?,
        })
        .await
    }

    #[tool(description = "Register (compile to a renderer bucket) a custom material by id.")]
    async fn register_material(
        &self,
        Parameters(p): Parameters<AssetArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::RegisterMaterial {
            id: parse_asset(&p.asset)?,
        })
        .await
    }

    #[tool(
        description = "Point a mesh node at one of its material VARIANTS — a mesh renders only entries of its own palette (mesh.material_variants; each = a library material + this mesh's independent overrides), and selection is the ONLY way its rendered material changes. variant omitted = unassign (magenta). Switching never mutates variant state: every material tool (set_builtin_param, set_node_texture, set_node_material_uniform, …) edits the SELECTED variant, and that tuning persists when you switch away and back. Single-step undoable. There is NO assign_material — add a variant (add_material_variant), then select it."
    )]
    async fn select_material_variant(
        &self,
        Parameters(p): Parameters<SelectMaterialVariantParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SelectMaterialVariant {
            node: parse_node(&p.node)?,
            variant: parse_variant_opt(&p.variant)?,
        })
        .await
    }

    #[tool(
        description = "Add a material VARIANT to a mesh node's palette: a fresh instance of the given LIBRARY material (seeded from its defaults) with a stable id, returned by this call. NEVER changes what renders — follow with select_material_variant to render it. Add the same library material twice for two independent tunings (each variant keeps its own overrides). Read the palette back via get_node_details (mesh.material_variants: id/name/instance). Single-step undoable."
    )]
    async fn add_material_variant(
        &self,
        Parameters(p): Parameters<AddMaterialVariantParams>,
    ) -> Result<CallToolResult, McpError> {
        // Mint the id HERE so the tool can report it (the command treats a
        // provided id as authoritative).
        let id = awsm_renderer_scene::VariantId::new();
        let res = self
            .dispatch(EditorCommand::AddMaterialVariant {
                node: parse_node(&p.node)?,
                material: parse_asset(&p.material)?,
                id: Some(id),
                name: p.name.clone(),
            })
            .await?;
        // On success answer with the new variant's id instead of a bare "ok".
        if res.is_error != Some(true) {
            return Ok(CallToolResult::success(vec![Content::text(id.to_string())]));
        }
        Ok(res)
    }

    #[tool(
        description = "Remove a variant from a mesh node's palette by its UUID. Removing the SELECTED variant leaves the mesh unassigned (magenta). Single-step undoable."
    )]
    async fn remove_material_variant(
        &self,
        Parameters(p): Parameters<RemoveMaterialVariantParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::RemoveMaterialVariant {
            node: parse_node(&p.node)?,
            variant: parse_variant(&p.variant)?,
        })
        .await
    }

    #[tool(
        description = "Rename a mesh node's material variant (display name only — the UUID is the stable identity and never changes). Single-step undoable."
    )]
    async fn rename_material_variant(
        &self,
        Parameters(p): Parameters<RenameMaterialVariantParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::RenameMaterialVariant {
            node: parse_node(&p.node)?,
            variant: parse_variant(&p.variant)?,
            name: p.name.clone(),
        })
        .await
    }

    #[tool(
        description = "Replace a custom material's WGSL source and synchronously recompile. Answers truthfully: returns ok only if the shader compiled, else an error carrying the compiler diagnostics (no more silent `ok` on a black mesh). Inspect later with get_material_diagnostics."
    )]
    async fn set_material_wgsl(
        &self,
        Parameters(p): Parameters<SetWgslParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_asset(&p.material)?;
        // 1. Set the source (the debounced auto-register also fires, but we don't
        //    wait on it — step 2 forces a deterministic compile of THIS edit).
        if let Response::Err(e) = self
            .req(Request::Dispatch(EditorCommand::SetCustomMaterialWgsl {
                id,
                wgsl: p.wgsl,
            }))
            .await?
        {
            return Err(McpError::internal_error(e, None));
        }
        // 2. Synchronous compile/register — records diagnostics on the material.
        if let Response::Err(e) = self
            .req(Request::Dispatch(EditorCommand::RegisterMaterial { id }))
            .await?
        {
            return Err(McpError::internal_error(e, None));
        }
        // 3. Report the compile outcome truthfully.
        match self
            .req(Request::Query(EditorQuery::MaterialDiagnostics {
                material: id,
            }))
            .await?
        {
            Response::Query(qr) => match *qr {
                QueryResult::Diagnostics(d) if d.ok => Ok(text("ok")),
                QueryResult::Diagnostics(d) => Err(McpError::internal_error(
                    format!("WGSL compile failed:\n{}", fmt_diag_errors(&d.errors)),
                    None,
                )),
                QueryResult::Error { error } => Err(McpError::internal_error(error, None)),
                other => Ok(text(
                    serde_json::to_string_pretty(&other).unwrap_or_default(),
                )),
            },
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Set a custom MASK material's SECOND, alpha-only WGSL window — a cheap `f32`-returning fragment compiled into the masked visibility raster so the cutout is alpha-tested (holes see-through + hole-shaped shadows + transmission-through-holes). Body must `return` an f32 alpha in [0,1]; the raster discards below the material's cutoff. Inputs arrive on `input` (e.g. input.uv, input.barycentric, input.material.<field>, material_sample_<tex>(input.material, input.uv)). Only meaningful when alpha mode = mask (set via set_material_alpha_mode). Empty clears it. Recompiles + reports diagnostics like set_material_wgsl."
    )]
    async fn set_material_alpha_wgsl(
        &self,
        Parameters(p): Parameters<SetWgslParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_asset(&p.material)?;
        if let Response::Err(e) = self
            .req(Request::Dispatch(
                EditorCommand::SetCustomMaterialAlphaWgsl { id, wgsl: p.wgsl },
            ))
            .await?
        {
            return Err(McpError::internal_error(e, None));
        }
        // Synchronous re-register so the masked variant recompiles + diagnostics
        // are recorded on the material.
        if let Response::Err(e) = self
            .req(Request::Dispatch(EditorCommand::RegisterMaterial { id }))
            .await?
        {
            return Err(McpError::internal_error(e, None));
        }
        match self
            .req(Request::Query(EditorQuery::MaterialDiagnostics {
                material: id,
            }))
            .await?
        {
            Response::Query(qr) => match *qr {
                QueryResult::Diagnostics(d) if d.ok => Ok(text("ok")),
                QueryResult::Diagnostics(d) => Err(McpError::internal_error(
                    format!("alpha WGSL compile failed:\n{}", fmt_diag_errors(&d.errors)),
                    None,
                )),
                QueryResult::Error { error } => Err(McpError::internal_error(error, None)),
                other => Ok(text(
                    serde_json::to_string_pretty(&other).unwrap_or_default(),
                )),
            },
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Set a custom material's THIRD, vertex-displacement WGSL window — the body is wrapped into `fn custom_displace_vertex(input: VertexDisplaceInput) -> VertexDisplaceOutput` and compiled into the geometry/shadow raster so the material moves its own vertices. Return VertexDisplaceOutput(position, normal, tangent) in LOCAL space (post-morph, pre-skin). Inputs: input.position/normal/tangent (local), input.uv (array<vec2<f32>,4> — ALL of the mesh's real per-vertex UV sets on every alpha mode: input.uv[0] = TEXCOORD_0, input.uv[1] = TEXCOORD_1, …; input.uv_count = number of valid sets), input.vertex_index, input.instance_id (u32::MAX non-instanced), input.material.<field>, input.globals.time. Sample a declared texture via material_sample_<name>(input.material, input.uv[i]). A shared `recompute_normal_from_height(n, t, h_center, h_du, h_dv, eps, strength)` helper is in scope for heightmap normals. The hook OWNS the normal (the renderer does NOT recompute it — pass input.normal through if unchanged). Read get_material_contract { vertex: true } first. Gentle sine-ripple starter: `var o: VertexDisplaceOutput; let off = sin(input.position.x * 6.0 + input.globals.time * 2.0) * 0.05; o.position = input.position + input.normal * off; o.normal = input.normal; o.tangent = input.tangent; return o;`. Empty clears it (→ shared fast pipeline, zero cost). Recompiles + reports diagnostics like set_material_wgsl."
    )]
    async fn set_material_vertex_wgsl(
        &self,
        Parameters(p): Parameters<SetWgslParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_asset(&p.material)?;
        if let Response::Err(e) = self
            .req(Request::Dispatch(
                EditorCommand::SetCustomMaterialVertexWgsl { id, wgsl: p.wgsl },
            ))
            .await?
        {
            return Err(McpError::internal_error(e, None));
        }
        // Synchronous re-register so the custom-vertex pipeline recompiles +
        // diagnostics are recorded on the material.
        if let Response::Err(e) = self
            .req(Request::Dispatch(EditorCommand::RegisterMaterial { id }))
            .await?
        {
            return Err(McpError::internal_error(e, None));
        }
        match self
            .req(Request::Query(EditorQuery::MaterialDiagnostics {
                material: id,
            }))
            .await?
        {
            Response::Query(qr) => match *qr {
                QueryResult::Diagnostics(d) if d.ok => Ok(text("ok")),
                QueryResult::Diagnostics(d) => Err(McpError::internal_error(
                    format!(
                        "vertex WGSL compile failed:\n{}",
                        fmt_diag_errors(&d.errors)
                    ),
                    None,
                )),
                QueryResult::Error { error } => Err(McpError::internal_error(error, None)),
                other => Ok(text(
                    serde_json::to_string_pretty(&other).unwrap_or_default(),
                )),
            },
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Set a custom material's alpha mode: opaque | mask (with `cutoff`) | blend."
    )]
    async fn set_material_alpha_mode(
        &self,
        Parameters(p): Parameters<AlphaModeParams>,
    ) -> Result<CallToolResult, McpError> {
        let mode = match p.mode {
            AlphaModeArg::Opaque => CustomAlphaMode::Opaque,
            AlphaModeArg::Mask => CustomAlphaMode::Mask {
                cutoff: p.cutoff.unwrap_or(0.5),
            },
            AlphaModeArg::Blend => CustomAlphaMode::Blend,
        };
        self.dispatch(EditorCommand::SetCustomMaterialAlphaMode {
            id: parse_asset(&p.material)?,
            mode,
        })
        .await
    }

    #[tool(description = "Set a custom material's double-sided flag.")]
    async fn set_material_double_sided(
        &self,
        Parameters(p): Parameters<MaterialBoolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCustomMaterialDoubleSided {
            id: parse_asset(&p.material)?,
            double_sided: p.value,
        })
        .await
    }

    #[tool(description = "Set a custom material's debug base color (#rrggbb, preview-only).")]
    async fn set_material_debug_color(
        &self,
        Parameters(p): Parameters<MaterialHexParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCustomMaterialDebugColor {
            id: parse_asset(&p.material)?,
            hex: p.hex,
        })
        .await
    }

    #[tool(
        description = "Replace a custom material's declared slot layout (uniforms / textures / buffers). Send the FULL lists. Each slot is { name, ty, val?, debug? }. Re-registers the material."
    )]
    async fn set_material_layout(
        &self,
        Parameters(p): Parameters<MaterialLayoutParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCustomMaterialLayout {
            id: parse_asset(&p.material)?,
            uniforms: p.uniforms.into_iter().map(slot_arg).collect(),
            textures: p.textures.into_iter().map(slot_arg).collect(),
            buffers: p.buffers.into_iter().map(slot_arg).collect(),
        })
        .await
    }

    #[tool(
        description = "Set the ShaderIncludes (generic helper modules) a custom material's WGSL needs (`keys`). Legal (Tier-A generic): math, camera, color_space, textures, vertex_color, light_access, shadows, skybox, extras, ibl. `ibl` exposes sample_ibl(albedo, normal, surface_to_camera, roughness, metallic) (+ sample_ibl_diffuse/_specular) — the SAME environment ambient + reflection first-party PBR gets, so a custom material matches the scene IBL instead of hand-faking a sky gradient (fixes black custom materials in IBL-only scenes; pair with normals + view_dir fragment_inputs). The PBR-internal modules (apply_lighting, brdf, material_color_calc) are NOT available to custom materials — they're welded to the built-in PbrMaterial types and are ignored on the custom path; write your own shading (you can build on light_access + ibl). Unknown keys are dropped. Call material_helper_catalog for descriptions."
    )]
    async fn set_material_includes(
        &self,
        Parameters(p): Parameters<MaterialKeysParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCustomMaterialShaderIncludes {
            id: parse_asset(&p.material)?,
            includes: p.keys,
        })
        .await
    }

    #[tool(
        description = "Set the FragmentInputs (interpolants) a custom material's WGSL reads (`keys`). Legal: normals, tangents, uv, lights, view_dir, vertex_color. Unknown keys are dropped. NOTE: the per-vertex ACCESSOR functions are gated on SHADER-INCLUDES, not these inputs — `material_uv(input, n)` needs `set_material_includes [\"textures\"]` and `material_vertex_color(input, n)` needs `[\"vertex_color\"]`; declaring fragment_inputs:[\"uv\"] does NOT bring `material_uv` into scope (it only sets the vertex-attribute layout)."
    )]
    async fn set_material_fragment_inputs(
        &self,
        Parameters(p): Parameters<MaterialKeysParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCustomMaterialFragmentInputs {
            id: parse_asset(&p.material)?,
            inputs: p.keys,
        })
        .await
    }

    #[tool(
        description = "Set the default value of a custom material's declared uniform slot (by name). `value` here is comma-separated text (e.g. \"0.6, 0.7, 1.0\"); the raw dispatch_command form also accepts the tagged encoding {\"kind\":\"vec3\",\"value\":[..]} — the SAME shape set_node_material_uniform takes. Applies live to every mesh using the material AND updates the registration default (fresh assigns pick it up). The writable counterpart of reading a uniform back. A uniform (e.g. a scroll `speed` / time multiplier) is the usual handle a custom-WGSL scroll animates — see the 'Geometry-locked scroll (conveyor / tread / road)' recipe in awsm://docs/material-recipes for the geometry-locked vs normal-derived distinction."
    )]
    async fn set_material_uniform(
        &self,
        Parameters(p): Parameters<MaterialUniformParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetMaterialUniform {
            material: parse_asset(&p.material)?,
            name: p.name,
            value: p.value.into(),
        })
        .await
    }

    #[tool(
        description = "Set a PER-MESH uniform override on a node assigned a CUSTOM-WGSL material — writes MaterialInstance.uniform_overrides[name], distinct from set_material_uniform (which sets the material's SHARED default for every mesh using it). `name` = a declared uniform slot; `value` = the typed UniformValue { kind: f32|vec2|vec3|vec4|u32|ivec2|ivec3|ivec4|mat3|mat4|color3|color4|bool, value: number OR array } (e.g. {\"kind\":\"f32\",\"value\":0.5} or {\"kind\":\"vec3\",\"value\":[1,0,0]}). Renders immediately; undoable."
    )]
    async fn set_node_material_uniform(
        &self,
        Parameters(p): Parameters<NodeMaterialUniformParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetNodeMaterialUniform {
            node: parse_node(&p.node)?,
            name: p.name,
            value: p.value,
        })
        .await
    }

    #[tool(
        description = "Bind a texture asset into a mesh node's custom-material texture slot (by slot name), or clear it (omit `texture`). The node needs a custom material assigned with a matching declared texture slot."
    )]
    async fn set_material_texture(
        &self,
        Parameters(p): Parameters<MaterialTextureParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetMaterialTexture {
            node: parse_node(&p.node)?,
            slot: p.slot,
            texture: parse_asset_opt(&p.texture)?,
        })
        .await
    }

    #[tool(
        description = "Bind raw buffer DATA into a mesh node's custom-material buffer slot (by slot name), or clear it (empty `values`). `values` are f32 words in declaration order — e.g. for an `array<vec4<f32>>` slot pass 4·N floats (the shader reads them via `extras_load_vec4_f32(<slot>_offset + i*4)`). The node needs a custom material assigned with a matching declared buffer slot. The bundle bake emits the bytes as `assets/<id>.bin`."
    )]
    async fn set_material_buffer(
        &self,
        Parameters(p): Parameters<MaterialBufferParams>,
    ) -> Result<CallToolResult, McpError> {
        // f32 values → little-endian u32 bit patterns (the extras pool stores
        // u32 words; the shader bitcasts back via `extras_load_f32`).
        let data = if p.values.is_empty() {
            None
        } else {
            Some(p.values.iter().map(|f| f.to_bits()).collect())
        };
        self.dispatch(EditorCommand::SetMaterialBuffer {
            node: parse_node(&p.node)?,
            slot: p.slot,
            data,
        })
        .await
    }

    #[tool(
        description = "Import a raster texture (PNG/JPEG/WebP) from a URL: fetch + decode + upload to the GPU + add the asset. Returns the new texture id. Cross-origin URLs need CORS headers. Bind with set_material_texture."
    )]
    async fn import_texture_from_url(
        &self,
        Parameters(p): Parameters<UrlParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = AssetId::new();
        self.dispatch_echo_asset(EditorCommand::ImportTextureFromUrl { id, url: p.url }, id)
            .await
    }

    #[tool(
        description = "Create a procedural texture asset (checker | gradient | noise). Returns the new texture id. Discover textures via get_snapshot's `textures`; bind with set_material_texture."
    )]
    async fn add_texture_asset(
        &self,
        Parameters(p): Parameters<AddTextureParams>,
    ) -> Result<CallToolResult, McpError> {
        let proc = match p.proc {
            ProceduralArg::Checker => ProceduralKind::Checker,
            ProceduralArg::Gradient => ProceduralKind::Gradient,
            ProceduralArg::Noise => ProceduralKind::Noise,
        };
        let id = AssetId::new();
        self.dispatch_echo_asset(EditorCommand::AddTextureAsset { id, proc }, id)
            .await
    }

    #[tool(
        description = "Set a built-in material factor on a mesh node's inline material. param: base_color (value = 3 floats RGB, OR 4 floats RGBA where the 4th is the base-color ALPHA — pair with set_builtin_alpha_mode blend for glass) | emissive (3 floats) | metallic | roughness | normal_scale | occlusion_strength (1 float). For KHR extension PARAMS (clearcoat, sheen, transmission, ior, ...) use patch_kind on mesh.material.inline.extensions — e.g. {\"mesh\":{\"material\":{\"inline\":{\"extensions\":{\"clearcoat\":{\"factor\":1.0,\"roughness_factor\":0.0}}}}}}. Extension ENABLES are owned by the library material (update_builtin_material) — inline params only take effect when the material enables the extension; an inline-only extension is dropped."
    )]
    async fn set_builtin_param(
        &self,
        Parameters(p): Parameters<BuiltinParamParams>,
    ) -> Result<CallToolResult, McpError> {
        let param = match p.param {
            BuiltinParamArg::BaseColor => BuiltinParamKind::BaseColor,
            BuiltinParamArg::Metallic => BuiltinParamKind::Metallic,
            BuiltinParamArg::Roughness => BuiltinParamKind::Roughness,
            BuiltinParamArg::Emissive => BuiltinParamKind::Emissive,
            BuiltinParamArg::NormalScale => BuiltinParamKind::NormalScale,
            BuiltinParamArg::OcclusionStrength => BuiltinParamKind::OcclusionStrength,
            BuiltinParamArg::EmissiveStrength => BuiltinParamKind::EmissiveStrength,
            BuiltinParamArg::AlphaCutoff => BuiltinParamKind::AlphaCutoff,
            BuiltinParamArg::ToonDiffuseBands => BuiltinParamKind::ToonDiffuseBands,
            BuiltinParamArg::ToonSpecularSteps => BuiltinParamKind::ToonSpecularSteps,
            BuiltinParamArg::ToonShininess => BuiltinParamKind::ToonShininess,
            BuiltinParamArg::ToonRimStrength => BuiltinParamKind::ToonRimStrength,
            BuiltinParamArg::ToonRimPower => BuiltinParamKind::ToonRimPower,
            BuiltinParamArg::FlipbookFps => BuiltinParamKind::FlipbookFps,
            BuiltinParamArg::FlipbookTimeOffset => BuiltinParamKind::FlipbookTimeOffset,
        };
        self.dispatch(EditorCommand::SetBuiltinParam {
            node: parse_node(&p.node)?,
            param,
            value: p.value,
        })
        .await
    }

    #[tool(
        description = "Set a BUILT-IN library MATERIAL's alpha mode: opaque | mask (with `cutoff`) | blend. Alpha mode is pipeline routing owned by the material asset — it applies to every node using the material (their variants re-materialize). Per glTF, opaque IGNORES base-color alpha; for glass set the material to `blend`, then set_builtin_param base_color with a 4th alpha float < 1 per node. Mask cutoff VALUE stays per-node tunable (set_builtin_param alpha_cutoff)."
    )]
    async fn set_builtin_alpha_mode(
        &self,
        Parameters(p): Parameters<BuiltinAlphaModeParams>,
    ) -> Result<CallToolResult, McpError> {
        let mode = match p.mode {
            AlphaModeArg::Opaque => awsm_renderer_editor_protocol::MaterialAlphaMode::Opaque,
            AlphaModeArg::Mask => awsm_renderer_editor_protocol::MaterialAlphaMode::Mask {
                cutoff: p.cutoff.unwrap_or(0.5) as f32,
            },
            AlphaModeArg::Blend => awsm_renderer_editor_protocol::MaterialAlphaMode::Blend,
        };
        self.dispatch(EditorCommand::SetBuiltinAlphaMode {
            material: parse_asset(&p.material)?,
            mode,
        })
        .await
    }

    #[tool(
        description = "Bind a texture asset onto a mesh node's BUILT-IN (inline PBR) material slot: base_color | metallic_roughness | normal | occlusion | emissive. Binds are pure data — every core slot's sampling code is always compiled (unbound slots sample a shared 1x1 neutral), so binding any slot on any node never recompiles anything. Omit `texture` to clear. Create textures with import_texture_from_url (raster) or add_texture_asset (procedural). (set_material_texture is the custom-WGSL-material counterpart.)"
    )]
    async fn set_node_texture(
        &self,
        Parameters(p): Parameters<BuiltinTextureParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetBuiltinTexture {
            node: parse_node(&p.node)?,
            slot: p.slot,
            texture: parse_asset_opt(&p.texture)?,
        })
        .await
    }

    #[tool(
        description = "Patch a node's kind with a JSON merge-patch (RFC 7386) — edit only the fields you name instead of resending the whole NodeKind via dispatch_command SetKind. `node` is the node UUID; `patch` is a partial JSON **object** (send it as an object, not a stringified object) merged over the node's current kind (fields present overwrite; null removes a key; nested objects merge recursively; arrays replace wholesale). Read get_node_details to see the exact shape + field names, then send just the delta. The result must still be a valid NodeKind (rejected loudly with the deserialize error otherwise). Ideal for escape-hatch edits with no typed tool: particle-emitter config, decal, sprite, collider, and per-mesh PBR extension params (mesh.material.inline.extensions.clearcoat = {\"factor\":1.0,\"roughness_factor\":0.0} enables + parameterizes clearcoat on JUST this mesh; null disables)."
    )]
    async fn patch_kind(
        &self,
        Parameters(p): Parameters<PatchKindParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::PatchKind {
            id: parse_node(&p.node)?,
            patch: p.patch,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "§3: the machine-readable JSON Schema for a node's `kind` config — the EXACT field shape + enum options of every NodeKind variant, so you can author a fresh kind via set_kind / patch_kind without guessing (the complement to get_node_details, which only shows an existing instance). `schema: \"node\"` (default) returns the full `NodeKind` schema: every variant under `oneOf`, and every referenced sub-type (LightConfig, CameraConfig, MaterialInstance, ParticleEmitterDef, the KHR PBR extensions, …) expanded under `$defs` — this is LARGE (hundreds of KB); prefer `variant` to scope it. `variant: \"<snake_case>\"` (e.g. `\"collider\"`, `\"light\"`, `\"particle_emitter\"`) returns JUST that one variant's schema plus only the `$defs` it references — small and targeted. `schema: \"modifier\"` returns the `ModifierStack` schema (mesh base + every modifier) for set_mesh_modifiers. Static metadata — no scene state. Returns a JSON Schema (draft 2020-12)."
    )]
    async fn get_kind_schema(
        &self,
        Parameters(p): Parameters<KindSchemaParams>,
    ) -> Result<CallToolResult, McpError> {
        match p.schema.as_deref() {
            Some("modifier") | Some("modifier_stack") | Some("modifiers") => {
                let json = serde_json::to_string_pretty(&schemars::schema_for!(
                    awsm_renderer_editor_protocol::ModifierStack
                ))
                .map_err(|e| McpError::internal_error(format!("serialize schema: {e}"), None))?;
                Ok(text(json))
            }
            None | Some("node") => {
                let full = serde_json::to_value(schemars::schema_for!(NodeKind)).map_err(|e| {
                    McpError::internal_error(format!("serialize schema: {e}"), None)
                })?;
                match p.variant.as_deref() {
                    // Full NodeKind schema (large — the historical behaviour).
                    None => Ok(text(
                        serde_json::to_string_pretty(&full).unwrap_or_default(),
                    )),
                    // One variant + only its transitively-referenced $defs.
                    Some(v) => {
                        let scoped = filter_node_variant_schema(&full, v).ok_or_else(|| {
                            McpError::invalid_params(
                                format!(
                                    "unknown NodeKind variant \"{v}\"; available: {}",
                                    node_variant_names(&full).join(", ")
                                ),
                                None,
                            )
                        })?;
                        Ok(text(
                            serde_json::to_string_pretty(&scoped).unwrap_or_default(),
                        ))
                    }
                }
            }
            // Previously an unrecognized `schema` (e.g. "environment") silently
            // fell through to the full NodeKind schema — a confusing hundreds-of-KB
            // dump for a value that isn't a valid selector. Reject it with guidance.
            Some(other) => {
                let full = serde_json::to_value(schemars::schema_for!(NodeKind)).ok();
                let variants = full
                    .as_ref()
                    .map(node_variant_names)
                    .unwrap_or_default()
                    .join(", ");
                Err(McpError::invalid_params(
                    format!(
                        "unknown schema \"{other}\"; valid: \"node\" | \"modifier\". For a SINGLE \
                         node kind's (small) schema pass variant:<name>. NodeKind variants: {variants}"
                    ),
                    None,
                ))
            }
        }
    }

    #[tool(
        description = "Configure a ParticleEmitter node — the typed, patch-style companion to insert_particle. Every field is optional; send any subset and only those change. Knobs: spawn_rate, burst_count, max_alive, one_shot, space (world|local), shape ({point}|{sphere:{radius}}|{cone:{angle_radians,direction}} — cone direction is LOCAL space), initial_speed/lifetime/size ([min,max]), forces ([{gravity:{acceleration:[x,y,z]}} | {linear_drag:{coefficient_x1000}}]), color_over_life ({const:[rgba]}|{linear:{start,end}}), size_over_life ({const}|{linear:{start,end}}), blend (transparent-blend pass for true alpha fades vs cheap opaque-emissive), texture (billboard SPRITE asset id — a soft radial-alpha disc imported via import_texture_from_url gives soft-edged particles; pair with blend:true so the alpha fades the edges). Errors if the node isn't a particle emitter."
    )]
    async fn set_particle_emitter(
        &self,
        Parameters(p): Parameters<ParticleEmitterParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetParticleEmitter {
            node: parse_node(&p.node)?,
            spawn_rate: p.spawn_rate,
            burst_count: p.burst_count,
            max_alive: p.max_alive,
            one_shot: p.one_shot,
            space: p.space,
            shape: p.shape,
            initial_speed: p.initial_speed,
            lifetime: p.lifetime,
            size: p.size,
            forces: p.forces,
            color_over_life: p.color_over_life,
            size_over_life: p.size_over_life,
            blend: p.blend,
            texture: p.texture.as_deref().map(parse_asset).transpose()?.map(Some),
        })
        .await
    }

    #[tool(
        description = "Set the UV transform / flow / wrap of a mesh node's BUILT-IN (inline PBR) texture slot (base_color | metallic_roughness | normal | occlusion | emissive). Patch-style: only the fields you pass change. offset/scale/rotation set the KHR_texture_transform (scale>1 tiles); flow=[u,v] auto-scrolls the texture (UV-units/sec — conveyors/water/lava — set [0,0] to stop); wrap_u/wrap_v = repeat|clamp_to_edge|mirrored_repeat; mag_filter/min_filter/mipmap_filter = nearest|linear; uv_set picks the TEXCOORD set. The slot must already have a texture bound (set_node_texture first) — an empty slot is rejected, not silently ignored. Renders immediately. For a directional/keyframed scroll use a texture_transform animation track instead. NOTE: scrolling only reads as travel on a GEOMETRY-LOCKED strip UV (one axis = travel) + a tileable texture — on a baked atlas UV it slides samples onto unrelated content. See the 'Geometry-locked scroll (conveyor / tread / road)' recipe in awsm://docs/material-recipes (author the strip UV with set_vertex_uvs + strip_parameterize first)."
    )]
    async fn set_node_texture_transform(
        &self,
        Parameters(p): Parameters<NodeTextureTransformParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetNodeTextureTransform {
            node: parse_node(&p.node)?,
            slot: p.slot,
            offset: p.offset,
            scale: p.scale,
            rotation: p.rotation,
            flow: p.flow,
            wrap_u: p.wrap_u.as_deref().map(parse_wrap).transpose()?,
            wrap_v: p.wrap_v.as_deref().map(parse_wrap).transpose()?,
            uv_set: p.uv_set,
            mag_filter: p.mag_filter.as_deref().map(parse_filter).transpose()?,
            min_filter: p.min_filter.as_deref().map(parse_filter).transpose()?,
            mipmap_filter: p.mipmap_filter.as_deref().map(parse_filter).transpose()?,
        })
        .await
    }

    // ── lighting / environment ───────────────────────────────────────────────

    #[tool(description = "Set a light node's color (linear RGB).")]
    async fn set_light_color(
        &self,
        Parameters(p): Parameters<LightColorParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetLightParam {
            node: parse_node(&p.node)?,
            param: LightParamKind::Color,
            value: p.color.to_vec(),
        })
        .await
    }

    #[tool(
        description = "Set a light node's intensity. Params: { node: <light node UUID>, value: <number> } — the same {node, value} shape as set_light_range/set_translation (NOT {light, intensity})."
    )]
    async fn set_light_intensity(
        &self,
        Parameters(p): Parameters<LightScalarParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetLightParam {
            node: parse_node(&p.node)?,
            param: LightParamKind::Intensity,
            value: vec![p.value],
        })
        .await
    }

    #[tool(
        description = "Set a point/spot light node's range. Params: { node: <light node UUID>, value: <number> }."
    )]
    async fn set_light_range(
        &self,
        Parameters(p): Parameters<LightScalarParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetLightParam {
            node: parse_node(&p.node)?,
            param: LightParamKind::Range,
            value: vec![p.value],
        })
        .await
    }

    #[tool(description = "Set a spot light node's inner + outer cone angles (radians).")]
    async fn set_light_angles(
        &self,
        Parameters(p): Parameters<LightAnglesParams>,
    ) -> Result<CallToolResult, McpError> {
        let node = parse_node(&p.node)?;
        if let Response::Err(e) = self
            .req(Request::Dispatch(EditorCommand::SetLightParam {
                node,
                param: LightParamKind::InnerAngle,
                value: vec![p.inner],
            }))
            .await?
        {
            return Err(McpError::internal_error(e, None));
        }
        self.dispatch(EditorCommand::SetLightParam {
            node,
            param: LightParamKind::OuterAngle,
            value: vec![p.outer],
        })
        .await
    }

    // ── morphs (live preview; persistent poses are animation tracks) ─────────

    #[tool(
        description = "Set one morph-target weight on a node's mesh, LIVE in the renderer (transient preview — it does not persist in the scene and a playing/scrubbing morph animation track will overwrite it). 0-based index; out-of-range or a morph-less node is a no-op. Read back with get_morph_data; author persistent poses as animation tracks (add_track morph)."
    )]
    async fn set_morph_weight(
        &self,
        Parameters(p): Parameters<MorphWeightParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetMorphWeight {
            node: parse_node(&p.node)?,
            index: p.index,
            value: p.value,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Live morph data per node: { target_count, weights, names } from the renderer's morph buffer (names from the glTF mesh.extras.targetNames convention; empty when the source had none) (what set_morph_weight writes and morph animation tracks drive). Pass node UUIDs, or empty for all. Nodes without MATERIALIZED morphs are omitted — empty on a morph-bearing scene means not-yet-materialized, not no-morphs."
    )]
    async fn get_morph_data(
        &self,
        Parameters(p): Parameters<NodesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::MorphData {
            nodes: parse_nodes(&p.nodes)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Rig discovery, grouped PER SKIN (keyed by source asset id): { source, meshes:[{node,name,primitive_index}], joints:[{node,index,name,live,translation,rotation,scale}] }. One joint table per rig — the skinned-mesh nodes sharing it are listed in `meshes` (a multi-material rig is one entry, not one copy per primitive). Joints ARE ordinary scene nodes — POSE one with set_node_transform on its `node` id (the skin deforms live), ANIMATE one with add_track targeting it (transform), or restore the whole rig with reset_to_bind_pose. Pass skinned-mesh node UUIDs, or empty for all."
    )]
    async fn get_skin_data(
        &self,
        Parameters(p): Parameters<NodesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::SkinData {
            nodes: parse_nodes(&p.nodes)?,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Per-vertex skin weights (set 0) for a skinned node: { vertex_count, set_count, weights: { \"<vertex>\": { joints:[4], weights:[4] } } }. `joints` index the skin's joint ARRAY (the order get_skin_data lists joints), not scene nodes. Empty indices = all vertices (fox ≈ 1.7k — fine). Pairs with set_skin_weights."
    )]
    async fn get_skin_weights(
        &self,
        Parameters(p): Parameters<SkinWeightsGetParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::GetSkinWeights {
            node: parse_node(&p.node)?,
            indices: p.indices,
        })
        .await
    }

    #[tool(
        description = "Rewrite per-vertex skin weights (set 0) on a skinned node's LIVE skin — the mesh re-deforms immediately, undoable. entries = [{ vertex, joints:[u32;4], weights:[f32;4] }]; joints index the skin's joint ARRAY (get_skin_data order); normalize rescales each entry to sum 1. Verify by posing the newly-weighted joint."
    )]
    async fn set_skin_weights(
        &self,
        Parameters(p): Parameters<SkinWeightsSetParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetSkinWeights {
            node: parse_node(&p.node)?,
            entries: p.entries,
            normalize: p.normalize,
        })
        .await
    }

    #[tool(
        description = "Two-bone IK: bring a chain TIP (end_node, e.g. a foot joint) to a world-space target, bending at its parent (knee) under its grandparent (upper leg). Solves analytically and (by default) APPLIES the two joint rotations as one undoable batch — auto-key compatible. `pole` biases the bend direction. `root_node` (optional) pins the chain ROOT explicitly (root_node → its child toward end → end_node) when the auto-pick (end→parent→grandparent) would walk into the wrong bones (e.g. finger joints above a hand); must be an ancestor of end_node. Returns { root_node, mid_node, root_rotation, mid_rotation, reach } (reach < 1 ⇒ target beyond the chain's span, clamped). Discover chains via get_skin_data; clips OWN bones while active (delete/pause first)."
    )]
    async fn solve_ik(
        &self,
        Parameters(p): Parameters<SolveIkParams>,
    ) -> Result<CallToolResult, McpError> {
        let end_node = parse_node(&p.end_node)?;
        let root_node = p.root_node.as_deref().map(parse_node).transpose()?;
        // 1. Solve (read-only).
        let sol = match self
            .req(Request::Query(EditorQuery::SolveIk {
                end_node,
                target: p.target,
                pole: p.pole,
                root_node,
            }))
            .await?
        {
            Response::Query(q) => *q,
            Response::Err(e) => return Err(McpError::internal_error(e, None)),
            other => {
                return Err(McpError::internal_error(
                    format!("unexpected response: {other:?}"),
                    None,
                ))
            }
        };
        let entries = match &sol {
            awsm_renderer_editor_protocol::QueryResult::Map(m) if m.kind == "ik_solution" => {
                &m.entries
            }
            awsm_renderer_editor_protocol::QueryResult::Error { error } => {
                return Err(McpError::internal_error(error.clone(), None))
            }
            other => {
                return Err(McpError::internal_error(
                    format!("unexpected solve result: {other:?}"),
                    None,
                ))
            }
        };
        if !p.apply {
            return Ok(text(serde_json::to_string(entries).unwrap_or_default()));
        }
        // 2. Apply: current locals (translation/scale preserved) + solved
        // rotations, as ONE batch (one undo step).
        let get = |k: &str| entries.get(k).cloned().unwrap_or(serde_json::Value::Null);
        let root = get("root_node")
            .as_str()
            .map(parse_node)
            .transpose()?
            .ok_or_else(|| McpError::internal_error("bad root_node", None))?;
        let mid = get("mid_node")
            .as_str()
            .map(parse_node)
            .transpose()?
            .ok_or_else(|| McpError::internal_error("bad mid_node", None))?;
        let quat = |k: &str| -> Result<[f32; 4], McpError> {
            serde_json::from_value(get(k))
                .map_err(|e| McpError::internal_error(format!("bad {k}: {e}"), None))
        };
        let (rq, mq) = (quat("root_rotation")?, quat("mid_rotation")?);
        // Current locals for translation/scale.
        let tr = match self
            .req(Request::Query(EditorQuery::NodeTransforms {
                nodes: vec![root, mid],
            }))
            .await?
        {
            Response::Query(q) => *q,
            other => {
                return Err(McpError::internal_error(
                    format!("unexpected transforms response: {other:?}"),
                    None,
                ))
            }
        };
        let tmap = match &tr {
            awsm_renderer_editor_protocol::QueryResult::Map(m) => &m.entries,
            _ => return Err(McpError::internal_error("bad transforms result", None)),
        };
        let trs_of =
            |id: NodeId, rot: [f32; 4]| -> Result<awsm_renderer_editor_protocol::Trs, McpError> {
                let e = tmap
                    .get(&id.to_string())
                    .ok_or_else(|| McpError::internal_error("joint transform missing", None))?;
                let v3 = |k: &str| -> [f32; 3] {
                    serde_json::from_value(e.get(k).cloned().unwrap_or_default())
                        .unwrap_or([0.0, 0.0, 0.0])
                };
                let mut scale = v3("scale");
                if scale == [0.0, 0.0, 0.0] {
                    scale = [1.0, 1.0, 1.0];
                }
                Ok(awsm_renderer_editor_protocol::Trs {
                    translation: v3("translation"),
                    rotation: rot,
                    scale,
                })
            };
        let cmds = vec![
            EditorCommand::SetTransform {
                id: root,
                transform: trs_of(root, rq)?,
            },
            EditorCommand::SetTransform {
                id: mid,
                transform: trs_of(mid, mq)?,
            },
        ];
        if let Response::Err(e) = self.req(Request::DispatchBatch(cmds)).await? {
            return Err(McpError::internal_error(e, None));
        }
        Ok(text(serde_json::to_string(entries).unwrap_or_default()))
    }

    #[tool(
        description = "Set the scene environment. THREE INDEPENDENT slots — skybox (background), specular (the prefiltered/roughness-mipped IBL map that drives reflections), and irradiance (the diffuse-convolved IBL map that drives ambient light). Two ways: (1) `zenith` + `nadir` ([r,g,b] linear) sets ALL THREE to a two-color SKY GRADIENT — author dusk / overcast / night / studio from your own colors (no hosting needed). (2) Otherwise each of skybox / specular / irradiance accepts: 'builtin' for the built-in default sky, an existing KTX cubemap asset UUID, OR a https:// URL to a .ktx2 cubemap. PARTIAL UPDATE: an OMITTED slot keeps its current config (pass 'builtin' to explicitly reset one) — so e.g. keeping default-sky irradiance while overriding just specular is one call, and slots never silently reset each other across sequential calls. Slots are fully decoupled (unlike before, specular and irradiance are set separately). URL cubemaps are fetched AND parse-validated here — a non-cubemap/bad .ktx2 fails this call instead of silently keeping the previous environment. Precedence: zenith/nadir > per-slot args. A fresh scene already seeds the built-in environment. Use get_snapshot (project.environment) to read what is currently set."
    )]
    async fn set_environment(
        &self,
        Parameters(p): Parameters<EnvironmentParams>,
    ) -> Result<CallToolResult, McpError> {
        // §18: agent-authored sky-gradient short-circuit — zenith+nadir drive all
        // three slots from the same two colors (no KTX2 needed).
        if let (Some(zenith), Some(nadir)) = (p.zenith, p.nadir) {
            let grad = EnvSlot::SkyGradient { zenith, nadir };
            return self
                .dispatch(EditorCommand::SetEnvironment {
                    env: EnvironmentConfig {
                        skybox: grad,
                        specular: grad,
                        irradiance: grad,
                    },
                })
                .await;
        }
        let is_url = |s: &str| s.starts_with("http://") || s.starts_with("https://");
        // Resolve a cubemap arg → an existing KTX asset id, registering a
        // URL-sourced asset first when given a URL (the cubemap analogue of
        // import_texture_from_url; the env-sync fetches the bytes on apply).
        // The import fetches + parse-validates the KTX2 NOW, so a bad URL
        // fails THIS call loudly instead of a silent apply-time toast.
        macro_rules! resolve_ktx {
            ($v:expr) => {{
                let v: &str = $v;
                if is_url(v) {
                    let id = AssetId::new();
                    self.dispatch(EditorCommand::ImportKtxEnvFromUrl {
                        id,
                        url: v.to_string(),
                    })
                    .await?;
                    id
                } else {
                    parse_asset(v)?
                }
            }};
        }
        // PARTIAL semantics: an omitted slot is `None` → the editor PRESERVES its
        // current config; 'builtin' explicitly resets that slot. Each slot
        // resolves identically and independently.
        macro_rules! resolve_slot {
            ($arg:expr) => {
                match $arg.as_deref() {
                    None => None,
                    // Accept every spelling of the built-in-default reset: the
                    // canonical "builtin", plus "builtin_default" and the
                    // "built_in_default" string the SCENE EXPORTER serializes into
                    // `[environment] skybox/specular/irradiance` — so a value the
                    // tool writes on export is a value it accepts back on input.
                    Some("builtin") | Some("builtin_default") | Some("built_in_default") => {
                        Some(EnvSlot::BuiltInDefault)
                    }
                    Some(v) => Some(EnvSlot::Ktx {
                        asset_id: resolve_ktx!(v),
                    }),
                }
            };
        }
        let skybox = resolve_slot!(p.skybox);
        let specular = resolve_slot!(p.specular);
        let irradiance = resolve_slot!(p.irradiance);
        self.dispatch(EditorCommand::PatchEnvironment {
            skybox,
            specular,
            irradiance,
        })
        .await
    }

    #[tool(
        description = "Set the global SSCS (screen-space contact shadows) settings — a short view-space ray-march that darkens contact gaps the shadow map leaves lit (e.g. the 'Peter-Pan' hole under a resting ball). Persisted on scene.shadows + carried in the player bundle; applied to the live renderer immediately. Every field is optional (patch semantics — only the ones you pass change). `enabled` + `step_count` are compile-time and recompile the shadow pipelines; the scalars are live uniforms. Off by default."
    )]
    async fn set_sscs(
        &self,
        Parameters(p): Parameters<SscsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetShadowsSscs {
            enabled: p.enabled,
            step_count: p.step_count,
            step_world: p.step_world,
            thickness: p.thickness,
            directional_darkening: p.directional_darkening,
            punctual_darkening: p.punctual_darkening,
        })
        .await
    }

    #[tool(
        description = "Set the global post-processing settings: tonemapping ('none' | 'khronos_neutral_pbr' | 'aces'), bloom (bool), dof (bool — depth of field, uses the active camera's focus/aperture), exposure (f32, EV stops pre-tonemap: 0 unity, +1 twice as bright), and the bloom tuning knobs bloom_threshold / bloom_knee / bloom_intensity / bloom_scatter (all f32). Bloom is a COD/Jimenez-style mip-pyramid glow: bloom_threshold (default 1.0) is the HDR luminance above which pixels glow, bloom_knee (0.5) softens the fade-in, bloom_intensity (1.0) is the mix strength, bloom_scatter (1.0) widens the halo toward coarser mips. Also SCREEN-SPACE REFLECTIONS via the ssr_* fields: ssr_enabled (bool, default off — zero cost when off), ssr_intensity / ssr_max_distance / ssr_thickness / ssr_max_steps / ssr_spread_cutoff / ssr_edge_fade / ssr_temporal_weight (LIVE uniforms), ssr_temporal + ssr_resolution_scale (0.5 half-res default / 1.0 full — STRUCTURAL, recompile the SSR pass). Glossy/metallic PBR surfaces reflect on-screen content; roughness beyond ssr_spread_cutoff falls back to IBL. Persisted on scene.post_process + carried in the player bundle; applied to the live renderer immediately. Every field is optional (patch semantics — only the ones you pass change). tonemapping/bloom/dof/ssr_enabled/ssr_temporal/ssr_resolution_scale recompile pipelines (wait_render_settled after); everything else is a LIVE uniform (no recompile). Defaults: khronos_neutral_pbr, bloom off, dof off, exposure 0, threshold 1.0, knee 0.5, intensity 1.0, scatter 1.0. Read the current values back with get_post_process."
    )]
    async fn set_post_process(
        &self,
        Parameters(p): Parameters<PostProcessParams>,
    ) -> Result<CallToolResult, McpError> {
        let tonemapping = match p.tonemapping.as_deref() {
            None => None,
            Some("none") => Some(ToneMappingConfig::None),
            Some("khronos_neutral_pbr") | Some("khronos") => {
                Some(ToneMappingConfig::KhronosNeutralPbr)
            }
            Some("aces") => Some(ToneMappingConfig::Aces),
            Some(other) => {
                return Err(McpError::invalid_params(
                    format!(
                        "unknown tonemapping '{other}' — expected 'none', \
                         'khronos_neutral_pbr', or 'aces'"
                    ),
                    None,
                ))
            }
        };
        self.dispatch(EditorCommand::SetPostProcess {
            tonemapping,
            bloom: p.bloom,
            dof: p.dof,
            exposure: p.exposure,
            bloom_threshold: p.bloom_threshold,
            bloom_knee: p.bloom_knee,
            bloom_intensity: p.bloom_intensity,
            bloom_scatter: p.bloom_scatter,
            ssr_enabled: p.ssr_enabled,
            ssr_intensity: p.ssr_intensity,
            ssr_max_distance: p.ssr_max_distance,
            ssr_thickness: p.ssr_thickness,
            ssr_max_steps: p.ssr_max_steps,
            ssr_spread_cutoff: p.ssr_spread_cutoff,
            ssr_edge_fade: p.ssr_edge_fade,
            ssr_temporal: p.ssr_temporal,
            ssr_resolution_scale: p.ssr_resolution_scale,
            ssr_temporal_weight: p.ssr_temporal_weight,
        })
        .await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Current global post-processing settings as JSON (the read half of set_post_process): tonemapping, bloom, dof, exposure, bloom_threshold/knee/intensity/scatter, and the full ssr block (enabled, intensity, max_distance, thickness, max_steps, spread_cutoff, edge_fade, resolution_scale, temporal, temporal_weight). Pure read."
    )]
    async fn get_post_process(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::PostProcess).await
    }

    // ── view / camera ───────────────────────────────────────────────────────

    #[tool(description = "Switch the workspace mode (scene | material | animation).")]
    async fn switch_mode(
        &self,
        Parameters(p): Parameters<ModeParams>,
    ) -> Result<CallToolResult, McpError> {
        let mode = match p.mode {
            ModeArg::Scene => EditorMode::Scene,
            ModeArg::Material => EditorMode::Material,
            ModeArg::Animation => EditorMode::Animation,
        };
        self.dispatch(EditorCommand::SwitchMode { mode }).await
    }

    #[tool(description = "Snap the viewport camera to a world axis (pos_x, neg_x, pos_y, …).")]
    async fn snap_camera_to_axis(
        &self,
        Parameters(p): Parameters<AxisParams>,
    ) -> Result<CallToolResult, McpError> {
        let axis = match p.axis {
            AxisArg::PosX => CameraAxis::PosX,
            AxisArg::NegX => CameraAxis::NegX,
            AxisArg::PosY => CameraAxis::PosY,
            AxisArg::NegY => CameraAxis::NegY,
            AxisArg::PosZ => CameraAxis::PosZ,
            AxisArg::NegZ => CameraAxis::NegZ,
        };
        self.dispatch(EditorCommand::SnapCameraToAxis { axis })
            .await
    }

    #[tool(description = "Reset the viewport camera to its default framing.")]
    async fn reset_camera(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::ResetCamera).await
    }

    #[tool(
        description = "Set the orbit camera's full pose: yaw/pitch (radians), radius (distance), look_at [x,y,z]. Compose any view (e.g. 3/4 front)."
    )]
    async fn set_camera_orbit(
        &self,
        Parameters(p): Parameters<CameraOrbitParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCameraOrbit {
            yaw: p.yaw,
            pitch: p.pitch,
            radius: p.radius,
            look_at: p.look_at,
        })
        .await
    }

    #[tool(
        description = "Switch the viewport projection (perspective vs orthographic), with optional perspective FOV (radians)."
    )]
    async fn set_camera_projection(
        &self,
        Parameters(p): Parameters<CameraProjectionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCameraProjection {
            perspective: p.perspective,
            fov_y: p.fov_y,
        })
        .await
    }

    #[tool(
        description = "Set the viewport camera near/far clip planes. `manual=true` pins the planes to `near`/`far` (metres); `manual=false` restores auto (planes track the orbit distance). Any omitted field is left unchanged. Editor default is AUTO (manual=false); manual-mode defaults are near 1.0, far 5000."
    )]
    async fn set_camera_clip(
        &self,
        Parameters(p): Parameters<CameraClipParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCameraClip {
            manual: p.manual,
            near: p.near,
            far: p.far,
        })
        .await
    }

    #[tool(
        description = "Frame a node in the viewport — fit its world bounds with `padding` (0 = tight, default 0.1). Then screenshot_scene to capture the framed subject."
    )]
    async fn frame_node(
        &self,
        Parameters(p): Parameters<FrameNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::FrameNode {
            node: parse_node(&p.node)?,
            padding: p.padding.unwrap_or(0.1),
        })
        .await
    }

    #[tool(
        description = "Restore a node + all its descendants to their scene base transforms — reverts a clip's last-previewed pose (clearing the current clip with set_current_clip leaves the last pose baked in the viewport). Pass a rig ROOT to reset a whole skeleton. Transient (re-syncs the renderer from the scene; no scene edit, not undoable)."
    )]
    async fn reset_pose(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::ResetPose {
            node: parse_node(&p.node)?,
        })
        .await
    }

    #[tool(
        description = "Restore every SKIN-JOINT node under `node` (pass a rig root) to its import-time transform — the glTF bind/rest pose (T-pose). This is the way back after posing joints with set_node_transform / solve_ik, which EDIT the scene base transforms and are therefore untouched by reset_pose (that only reverts clip-preview poses). A real scene edit: single-step undoable. No-op for nodes without recorded rest data (non-joints)."
    )]
    async fn reset_to_bind_pose(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::ResetToBindPose {
            id: parse_node(&p.node)?,
        })
        .await
    }

    // ── animation (lifecycle + transport) ───────────────────────────────────

    #[tool(
        description = "Create a fresh empty animation clip (optionally named) and make it current. Returns the new clip id."
    )]
    async fn add_clip(
        &self,
        Parameters(p): Parameters<AddClipParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = AssetId::new();
        match self
            .req(Request::Dispatch(EditorCommand::AddClip {
                id,
                name: p.name,
            }))
            .await?
        {
            Response::Ok => Ok(text(id.to_string())),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "REPLACE a track's entire key list in one call — the bulk authoring path (one call per track instead of one add_keyframe per key). `times` pairs index-wise with `values`; unsorted input is sorted by time. Single-step undoable (undo restores the prior keys exactly)."
    )]
    async fn set_track_keys(
        &self,
        Parameters(p): Parameters<SetTrackKeysParams>,
    ) -> Result<CallToolResult, McpError> {
        let values = p
            .values
            .iter()
            .map(build_track_value)
            .collect::<Result<Vec<_>, _>>()?;
        let interp = match p.interp.as_deref() {
            Some(s) => Some(parse_enum(s, "interp")?),
            None => None,
        };
        self.dispatch(EditorCommand::SetTrackKeys {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
            times: p.times,
            values,
            interp,
            keys: Vec::new(),
        })
        .await
    }

    #[tool(description = "Delete an animation clip by id.")]
    async fn delete_clip(
        &self,
        Parameters(p): Parameters<AssetArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::DeleteClip {
            id: parse_asset(&p.asset)?,
        })
        .await
    }

    #[tool(description = "Duplicate an animation clip (deep copy, fresh id) and select it.")]
    async fn duplicate_clip(
        &self,
        Parameters(p): Parameters<AssetArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::DuplicateClip {
            id: parse_asset(&p.asset)?,
        })
        .await
    }

    #[tool(description = "Rename an animation clip.")]
    async fn rename_clip(
        &self,
        Parameters(p): Parameters<ClipNameParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::RenameClip {
            id: parse_asset(&p.clip)?,
            name: p.name,
        })
        .await
    }

    #[tool(description = "Set an animation clip's duration (seconds).")]
    async fn set_clip_duration(
        &self,
        Parameters(p): Parameters<ClipScalarParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetClipDuration {
            id: parse_asset(&p.clip)?,
            duration: p.value,
        })
        .await
    }

    #[tool(description = "Set an animation clip's speed multiplier.")]
    async fn set_clip_speed(
        &self,
        Parameters(p): Parameters<ClipScalarParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetClipSpeed {
            id: parse_asset(&p.clip)?,
            speed: p.value,
        })
        .await
    }

    #[tool(description = "Set an animation clip's loop style: once | loop | ping_pong.")]
    async fn set_clip_loop(
        &self,
        Parameters(p): Parameters<ClipLoopParams>,
    ) -> Result<CallToolResult, McpError> {
        let loop_style: ClipLoop = parse_enum(&p.loop_style, "loop style")?;
        self.dispatch(EditorCommand::SetClipLoop {
            id: parse_asset(&p.clip)?,
            loop_style,
        })
        .await
    }

    #[tool(description = "Set the clip Animation mode is editing (or clear).")]
    async fn set_current_clip(
        &self,
        Parameters(p): Parameters<ClipOptParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCurrentClip {
            id: parse_asset_opt(&p.clip)?,
        })
        .await
    }

    #[tool(
        description = "Pin the renderer's frame_globals.time to `seconds` so a temporal material (sin(time*f)) screenshots the same phase every call. Separate from the animation playhead. Clear with clear_frame_time."
    )]
    async fn set_frame_time(
        &self,
        Parameters(p): Parameters<PlayheadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetFrameTime {
            seconds: p.t as f32,
        })
        .await
    }

    #[tool(description = "Clear the pinned frame time — back to the wall-clock source.")]
    async fn clear_frame_time(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::ClearFrameTime).await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Read the renderer's current frame globals: time, delta_time, frame_count, resolution. Reflects a set_frame_time pin."
    )]
    async fn get_frame_globals(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::FrameGlobals).await
    }

    #[tool(description = "Set the animation playhead (seconds).")]
    async fn set_playhead(
        &self,
        Parameters(p): Parameters<PlayheadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetPlayhead { t: p.t }).await
    }

    #[tool(description = "Set animation play/pause.")]
    async fn set_playing(
        &self,
        Parameters(p): Parameters<PlayingParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetPlaying { on: p.on }).await
    }

    #[tool(
        description = "Add an animation track to a clip, bound to a target. target.kind = transform (node+prop) | morph (node+index) | uniform (material+name) | builtin_param/light/camera (node+param) | texture_transform (node + slot[base_color|metallic_roughness|normal|occlusion|emissive] + prop[offset(vec2)|scale(vec2)|rotation(scalar)] — keyframe a built-in texture's UV offset/scale/rotation, e.g. a directional/reversible conveyor scroll). Tracks append; the new index is the prior track count."
    )]
    async fn add_track(
        &self,
        Parameters(p): Parameters<AddTrackParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AddTrack {
            clip: parse_asset(&p.clip)?,
            target: build_track_target(&p.target)?,
        })
        .await
    }

    #[tool(
        description = "Add a one-line SPIN: a rotation Transform track on `node` that turns `turns` full revolutions about local `axis` [x,y,z] over `duration` seconds, expanded to evenly-spaced quaternion keyframes (`keys_per_turn` per revolution, default 4; linear). Collapses the verbose hand-author-N-quarter-turn-quats workflow for wheels / rotors / fans. `turns` may be fractional (0.25 = a quarter turn) or negative (reverse). Plays/reverses further via set_clip_speed / set_clip_direction. Appends one track (its index = prior track count); undo removes it."
    )]
    async fn add_spin_track(
        &self,
        Parameters(p): Parameters<AddSpinTrackParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AddSpinTrack {
            clip: parse_asset(&p.clip)?,
            node: parse_node(&p.node)?,
            axis: p.axis,
            turns: p.turns,
            duration: p.duration,
            keys_per_turn: p.keys_per_turn,
        })
        .await
    }

    #[tool(
        description = "Insert a keyframe at time `t` (seconds) with `value` on a track. value.kind = vec2 | vec3 | vec4 | quat (xyzw) | scalar. Optional `interp` = step | linear | cubic (omit to use the track's sampler). Replaces any existing key at `t`."
    )]
    async fn add_keyframe(
        &self,
        Parameters(p): Parameters<AddKeyframeParams>,
    ) -> Result<CallToolResult, McpError> {
        let interp = match p.interp.as_deref() {
            None => None,
            Some(s) => Some(parse_interp(s)?),
        };
        self.dispatch(EditorCommand::AddKeyframe {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
            t: p.t,
            value: build_track_value(&p.value)?,
            interp,
        })
        .await
    }

    #[tool(
        description = "Patch a keyframe by index (any subset of t / value / interp). interp = step | linear | cubic."
    )]
    async fn set_keyframe(
        &self,
        Parameters(p): Parameters<SetKeyframeParams>,
    ) -> Result<CallToolResult, McpError> {
        let value = match &p.value {
            Some(v) => Some(build_track_value(v)?),
            None => None,
        };
        let interp = match p.interp.as_deref() {
            None => None,
            Some(s) => Some(parse_interp(s)?),
        };
        self.dispatch(EditorCommand::SetKeyframe {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
            index: p.index as usize,
            t: p.t,
            value,
            interp,
            in_tangent: None,
            out_tangent: None,
        })
        .await
    }

    #[tool(description = "Delete a keyframe by index from a track.")]
    async fn delete_keyframe(
        &self,
        Parameters(p): Parameters<DeleteKeyframeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::DeleteKeyframe {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
            index: p.index as usize,
        })
        .await
    }

    #[tool(
        description = "Delete a track (by index) from a clip. Undoable. Index is the track's position in the clip's tracks (see get_snapshot / get_track_data)."
    )]
    async fn delete_track(
        &self,
        Parameters(p): Parameters<TrackIndexParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::DeleteTrack {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
        })
        .await
    }

    #[tool(
        description = "Mute / unmute a track (muted tracks don't contribute to the pose). Undoable."
    )]
    async fn set_track_mute(
        &self,
        Parameters(p): Parameters<TrackMuteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetTrackMute {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
            mute: p.mute,
        })
        .await
    }

    #[tool(
        description = "Solo / unsolo a track (when any track is soloed, only soloed tracks contribute). Undoable."
    )]
    async fn set_track_solo(
        &self,
        Parameters(p): Parameters<TrackSoloParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetTrackSolo {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
            solo: p.solo,
        })
        .await
    }

    #[tool(description = "Set a track's interpolation sampler: step | linear | cubic. Undoable.")]
    async fn set_track_sampler(
        &self,
        Parameters(p): Parameters<TrackSamplerParams>,
    ) -> Result<CallToolResult, McpError> {
        let sampler: SamplerKind = parse_enum(&p.sampler, "sampler")?;
        self.dispatch(EditorCommand::SetTrackSampler {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
            sampler,
        })
        .await
    }

    #[tool(
        description = "Step the playhead: home (t=0) | prev (previous keyframe) | next | end (clip duration). Transport — not undoable."
    )]
    async fn step_playhead(
        &self,
        Parameters(p): Parameters<StepPlayheadParams>,
    ) -> Result<CallToolResult, McpError> {
        let kind: StepKind = parse_enum(&p.kind, "step kind")?;
        self.dispatch(EditorCommand::StepPlayhead { kind }).await
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Read a track's full stored data (target, sampler, mute/solo, times, keyframes incl. interp/tangents) — to verify what you authored."
    )]
    async fn get_track_data(
        &self,
        Parameters(p): Parameters<TrackDataParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::GetTrackData {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
        })
        .await
    }

    // ── generic escape hatch ────────────────────────────────────────────────

    #[tool(
        description = "Dispatch a raw EditorCommand (escape hatch for any command without a dedicated tool: keyframes, tracks, mixer, environment…). `command` is internally tagged by \"cmd\"."
    )]
    async fn dispatch_command(
        &self,
        Parameters(p): Parameters<CommandJsonParams>,
    ) -> Result<CallToolResult, McpError> {
        let cmd: EditorCommand = json_arg(p.command, "command")?;
        self.dispatch(cmd).await
    }

    #[tool(
        description = "Dispatch a list of raw EditorCommands as ONE atomic step (applied in order, collapsed into a single undo entry, one round-trip). Cuts latency for multi-step edits (e.g. building a rig). Each command is internally tagged by \"cmd\" — discover tags + payload shapes with list_commands."
    )]
    async fn dispatch_batch(
        &self,
        Parameters(p): Parameters<BatchJsonParams>,
    ) -> Result<CallToolResult, McpError> {
        let cmds: Vec<EditorCommand> = p
            .commands
            .into_iter()
            .map(|c| json_arg(c, "command in batch"))
            .collect::<Result<_, _>>()?;
        match self.req(Request::DispatchBatch(cmds)).await? {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        annotations(read_only_hint = true),
        description = "Discover the raw EditorCommand vocabulary for dispatch_command / dispatch_batch (handoff #5/#12). No args: a compact list of every command tag with its field names ('*' marks required). With `cmd` (e.g. \"reparent\"): that command's FULL JSON Schema (exact payload shape + referenced $defs). Local — no editor round-trip."
    )]
    async fn list_commands(
        &self,
        Parameters(p): Parameters<ListCommandsParams>,
    ) -> Result<CallToolResult, McpError> {
        let root = serde_json::to_value(schemars::schema_for!(EditorCommand))
            .map_err(|e| McpError::internal_error(format!("schema: {e}"), None))?;
        let variants = root
            .get("oneOf")
            .or_else(|| root.get("anyOf"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        // The variant's tag: properties.cmd is `{"const": "..."}` (or a 1-enum).
        fn tag_of(variant: &serde_json::Value) -> Option<String> {
            let c = variant.get("properties")?.get("cmd")?;
            c.get("const")
                .and_then(|v| v.as_str())
                .or_else(|| c.get("enum")?.get(0)?.as_str())
                .map(str::to_string)
        }
        if let Some(want) = p.cmd.as_deref() {
            for v in &variants {
                if tag_of(v).as_deref() == Some(want) {
                    let body = serde_json::json!({
                        "cmd": want,
                        "schema": v,
                        "$defs": root.get("$defs").cloned().unwrap_or(serde_json::json!({})),
                    });
                    return Ok(text(body.to_string()));
                }
            }
            return Err(McpError::invalid_params(
                format!("unknown command tag `{want}` — call list_commands with no args for the full list"),
                None,
            ));
        }
        let mut list = Vec::new();
        for v in &variants {
            let Some(tag) = tag_of(v) else { continue };
            let required: std::collections::HashSet<&str> = v
                .get("required")
                .and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            let fields: Vec<String> = v
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|props| {
                    props
                        .keys()
                        .filter(|k| k.as_str() != "cmd")
                        .map(|k| {
                            if required.contains(k.as_str()) {
                                format!("{k}*")
                            } else {
                                k.clone()
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            list.push(serde_json::json!({ "cmd": tag, "fields": fields }));
        }
        list.sort_by(|a, b| a["cmd"].as_str().cmp(&b["cmd"].as_str()));
        Ok(text(
            serde_json::json!({
                "count": list.len(),
                "note": "fields marked * are required; call list_commands{cmd} for a command's full JSON Schema",
                "commands": list,
            })
            .to_string(),
        ))
    }
}

// ──────────────────────────────── helpers ───────────────────────────────────

impl EditorMcp {
    async fn req(&self, r: Request) -> Result<Response, McpError> {
        self.link
            .request(&self.agent, &r)
            .await
            .map_err(|e| match e {
                LinkError::Transport(msg) => McpError::internal_error(msg, None),
            })
    }

    async fn dispatch(&self, cmd: EditorCommand) -> Result<CallToolResult, McpError> {
        match self.req(Request::Dispatch(cmd)).await? {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    /// Dispatch a creation command that carries a caller-minted `AssetId` and, on
    /// success, echo the id back as the tool result (the `add_clip` pattern — no
    /// snapshot round-trip needed to discover what was just made).
    async fn dispatch_echo_asset(
        &self,
        cmd: EditorCommand,
        id: AssetId,
    ) -> Result<CallToolResult, McpError> {
        match self.req(Request::Dispatch(cmd)).await? {
            Response::Ok => Ok(text(id.to_string())),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    async fn insert(
        &self,
        spec: InsertSpec,
        parent: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        let id = NodeId::new();
        match self
            .req(Request::Dispatch(EditorCommand::Insert {
                id,
                spec,
                parent: parse_node_opt(&parent)?,
            }))
            .await?
        {
            Response::Ok => Ok(text(id.to_string())),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    /// Read a node's current `NodeKind` (for the read-modify-write kind setters)
    /// via the `NodeKindDetails` query — each entry is the node's `NodeKind`
    /// serialized with `serde_json::to_value`, so it round-trips straight back.
    async fn current_kind(&self, node: NodeId) -> Result<NodeKind, McpError> {
        let resp = self
            .req(Request::Query(EditorQuery::NodeKindDetails {
                nodes: vec![node],
            }))
            .await?;
        let Response::Query(qr) = resp else {
            return Err(unexpected(resp));
        };
        let QueryResult::Map(m) = *qr else {
            return Err(McpError::internal_error("unexpected kind result", None));
        };
        let v = m
            .entries
            .get(&node.to_string())
            .ok_or_else(|| McpError::invalid_params(format!("no such node {node}"), None))?;
        serde_json::from_value(v.clone()).map_err(|e| {
            McpError::internal_error(format!("node {node} kind did not parse: {e}"), None)
        })
    }

    /// Read a node's current local TRS + world matrix (column-major 16)
    /// via the `NodeTransforms` query — for tools that need world-space
    /// context (`node_look_at`).
    async fn current_trs_and_world(&self, node: NodeId) -> Result<(Trs, [f32; 16]), McpError> {
        let trs = self.current_trs(node).await?;
        let resp = self
            .req(Request::Query(EditorQuery::NodeTransforms {
                nodes: vec![node],
            }))
            .await?;
        let Response::Query(qr) = resp else {
            return Err(unexpected(resp));
        };
        let QueryResult::Map(m) = *qr else {
            return Err(McpError::internal_error(
                "unexpected transforms result",
                None,
            ));
        };
        let mut world = [0.0f32; 16];
        world[0] = 1.0;
        world[5] = 1.0;
        world[10] = 1.0;
        world[15] = 1.0;
        if let Some(a) = m
            .entries
            .get(&node.to_string())
            .and_then(|v| v.get("world"))
            .and_then(|a| a.as_array())
        {
            for (i, x) in a.iter().take(16).enumerate() {
                world[i] = x.as_f64().unwrap_or(world[i] as f64) as f32;
            }
        }
        Ok((trs, world))
    }

    /// Read a node's current local TRS (for the partial-transform convenience
    /// tools) via the `NodeTransforms` query.
    async fn current_trs(&self, node: NodeId) -> Result<Trs, McpError> {
        let resp = self
            .req(Request::Query(EditorQuery::NodeTransforms {
                nodes: vec![node],
            }))
            .await?;
        let Response::Query(qr) = resp else {
            return Err(unexpected(resp));
        };
        let QueryResult::Map(m) = *qr else {
            return Err(McpError::internal_error(
                "unexpected transforms result",
                None,
            ));
        };
        let v = m
            .entries
            .get(&node.to_string())
            .ok_or_else(|| McpError::invalid_params(format!("no such node {node}"), None))?;
        let arr3 = |key: &str, fallback: [f32; 3]| -> [f32; 3] {
            v.get(key)
                .and_then(|a| a.as_array())
                .map(|a| {
                    [
                        a.first()
                            .and_then(|x| x.as_f64())
                            .unwrap_or(fallback[0] as f64) as f32,
                        a.get(1)
                            .and_then(|x| x.as_f64())
                            .unwrap_or(fallback[1] as f64) as f32,
                        a.get(2)
                            .and_then(|x| x.as_f64())
                            .unwrap_or(fallback[2] as f64) as f32,
                    ]
                })
                .unwrap_or(fallback)
        };
        let rotation = v
            .get("rotation")
            .and_then(|a| a.as_array())
            .map(|a| {
                [
                    a.first().and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                    a.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                    a.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                    a.get(3).and_then(|x| x.as_f64()).unwrap_or(1.0) as f32,
                ]
            })
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
        Ok(Trs {
            translation: arr3("translation", [0.0; 3]),
            rotation,
            scale: arr3("scale", [1.0; 3]),
        })
    }

    async fn query(&self, q: EditorQuery) -> Result<CallToolResult, McpError> {
        match self.req(Request::Query(q)).await? {
            Response::Query(qr) => match *qr {
                // Text results (e.g. WGSL source) return raw, not JSON-quoted.
                QueryResult::Text(s) => Ok(text(s)),
                other => Ok(text(
                    serde_json::to_string_pretty(&other)
                        .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
                )),
            },
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    async fn png(&self, r: Request) -> Result<CallToolResult, McpError> {
        match self.req(r).await? {
            // The bytes rode the `/png/<id>` side-channel, not the link — read
            // them back from the temp file the editor uploaded them to.
            Response::Png(handle) => {
                let bytes = std::fs::read(crate::http::png_path(&handle.id)).map_err(|e| {
                    McpError::internal_error(format!("read png {}: {e}", handle.id), None)
                })?;
                Ok(CallToolResult::success(vec![Content::image(
                    STANDARD.encode(bytes),
                    "image/png".to_string(),
                )]))
            }
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    /// Run a `.glb` export request. The bytes rode the `/glb/<id>` side-channel
    /// (not the control link, which a multi-MiB export would blow), and we return
    /// the temp-file **path** rather than inlining the base64 — keeping the payload
    /// off both the link and the token stream. The caller reads the file to use it.
    async fn glb(&self, r: Request) -> Result<CallToolResult, McpError> {
        match self.req(r).await? {
            Response::Glb(handle) => {
                let path = crate::http::glb_path(&handle.id);
                // Confirm the upload actually landed before reporting success.
                if !path.exists() {
                    return Err(McpError::internal_error(
                        format!("glb {} not found at {}", handle.id, path.display()),
                        None,
                    ));
                }
                Ok(text(
                    serde_json::json!({
                        "glb_path": path.display().to_string(),
                        "byte_len": handle.byte_len,
                        "url": format!("/glb/{}", handle.id),
                    })
                    .to_string(),
                ))
            }
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }
}

#[tool_handler]
impl ServerHandler for EditorMcp {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]` in rmcp 1.x — build from Default and
        // set the public fields rather than a struct literal.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        info.instructions = Some(
            "Drive the awsm-renderer editor. Call get_snapshot to discover node/asset ids, \
             mutate with the scene/material/animation tools (or dispatch_command/dispatch_batch \
             for anything without a dedicated tool), then wait_render_settled + screenshot_scene \
             to see the result. For custom WGSL materials read get_material_contract first and \
             check get_material_diagnostics after editing. Assets (textures, environments, \
             heightmaps) come from URLs — generate + host them, then import/reference by URL; \
             there is NO inline-base64 texture or equirect tool. For the environment / texture / \
             material / displacement / purge workflows (incl. baking a .ktx2 cubemap offline via \
             cmgen/ktx), read the awsm://docs/asset-workflows resource. Docs + workflow templates \
             are exposed as MCP resources + prompts."
                .to_string(),
        );
        info
    }

    // ── push channel: forward this session's editor events as MCP logging ────
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let mut rx = self.link.subscribe_events();
        let peer = context.peer;
        let link = self.link.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok((conn_id, ev)) => {
                        // Only forward events from the live (most-recent) tab — a
                        // just-evicted stale tab must not leak events.
                        if link.current_conn_id() != Some(conn_id) {
                            continue;
                        }
                        let level = match ev.level.as_deref() {
                            Some("error") => LoggingLevel::Error,
                            Some("warning") => LoggingLevel::Warning,
                            _ => LoggingLevel::Info,
                        };
                        let param = LoggingMessageNotificationParam {
                            level,
                            logger: Some("awsm-renderer-editor".to_string()),
                            data: serde_json::to_value(&ev).unwrap_or(Value::Null),
                        };
                        // Stops the forwarder once this MCP session drops.
                        if peer.notify_logging_message(param).await.is_err() {
                            break;
                        }
                    }
                    // Slow consumer dropped some events — keep going.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // ── resources: the published docs (read-only) ───────────────────────────
    async fn list_resources(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let res = |uri: &str, name: &str, desc: &str| {
            let mut r = RawResource::new(uri, name);
            r.description = Some(desc.to_string());
            r.mime_type = Some("text/markdown".to_string());
            r.no_annotation()
        };
        Ok(ListResourcesResult::with_all_items(vec![
            res(
                "awsm://docs/mcp",
                "MCP guide",
                "How to drive the editor over MCP (docs/MCP.md).",
            ),
            res(
                "awsm://docs/asset-workflows",
                "Asset workflows",
                "Environment / texture / material / displacement / purge workflows: bake a .ktx2 \
                 cubemap offline (cmgen/ktx) + set_environment by URL, import_texture_from_url → \
                 PBR slots, custom WGSL, displace_from_texture, purge_unused, and the patch_kind \
                 escape hatch for long-tail material fields. Assets come from URLs (no inline base64).",
            ),
            res(
                "awsm://docs/agent-guide",
                "Agent guide",
                "The agent loop, end-to-end scene walkthrough, lighting, batching, troubleshooting.",
            ),
            res(
                "awsm://docs/material-recipes",
                "Material recipes",
                "Copy-paste custom-material WGSL recipes (textured, emissive, fresnel, scrolling, glass).",
            ),
            res(
                "awsm://docs/animation",
                "Animation authoring",
                "Clips, tracks, keyframes + worked examples (spin, pulse).",
            ),
            res(
                "awsm://docs/mesh-tools",
                "Mesh tools",
                "Authoring/editing geometry: set_mesh_modifiers (modifier stack + SDF) \
                 JSON shapes, vertex selection/edit predicates, per-vertex authoring \
                 (paint_vertex_colors / set_vertex_normals / sculpt + bake_all), \
                 introspection (get_vertex_data / get_mesh_layers), export — with \
                 copy-paste examples (twist, lathe bat, SDF mug, texture splatting).",
            ),
            res(
                "awsm://docs/material-contract-opaque",
                "Opaque material contract",
                "The WGSL ABI for opaque/mask dynamic materials.",
            ),
            res(
                "awsm://docs/material-contract-transparent",
                "Transparent material contract",
                "The WGSL ABI for blend (transparent) dynamic materials.",
            ),
            res(
                "awsm://docs/material-contract-vertex",
                "Vertex-displacement material contract",
                "The WGSL ABI for the vertex-displacement hook (the third, \
                 vertex WGSL window) — input.*, return type, normal ownership.",
            ),
        ]))
    }

    async fn read_resource(
        &self,
        req: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let body = match req.uri.as_str() {
            "awsm://docs/mcp" => MCP_DOC,
            "awsm://docs/asset-workflows" => ASSET_WORKFLOWS,
            "awsm://docs/agent-guide" => AGENT_GUIDE,
            "awsm://docs/material-recipes" => MATERIAL_RECIPES,
            "awsm://docs/animation" => ANIMATION_DOC,
            "awsm://docs/mesh-tools" => MESH_TOOLS_DOC,
            "awsm://docs/material-contract-opaque" => CONTRACT_OPAQUE,
            "awsm://docs/material-contract-transparent" => CONTRACT_TRANSPARENT,
            "awsm://docs/material-contract-vertex" => CONTRACT_VERTEX,
            other => {
                return Err(McpError::resource_not_found(
                    format!("unknown resource {other}"),
                    None,
                ))
            }
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            body, req.uri,
        )]))
    }

    // ── prompts: workflow templates (the correct create→diagnose→settle loop) ─
    async fn list_prompts(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(vec![
            Prompt::new(
                "author_lit_material",
                Some("Author a lit custom WGSL material end-to-end (the no-black-screen loop)."),
                None,
            ),
            Prompt::new(
                "setup_rotation_clip",
                Some("Create an animation clip with a rotation track + keyframes and play it."),
                None,
            ),
            Prompt::new(
                "import_and_frame_model",
                Some("Import a glTF model from a URL and frame it for a screenshot."),
                None,
            ),
            Prompt::new(
                "setup_environment",
                Some("Set the environment: a two-color sky gradient, or a baked .ktx2 cubemap by URL."),
                None,
            ),
        ]))
    }

    async fn get_prompt(
        &self,
        req: GetPromptRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let (description, body) = match req.name.as_str() {
            "author_lit_material" => (
                "Author a lit custom WGSL material end-to-end.",
                PROMPT_AUTHOR_MATERIAL,
            ),
            "setup_rotation_clip" => (
                "Create + play a rotation animation clip.",
                PROMPT_ROTATION_CLIP,
            ),
            "import_and_frame_model" => ("Import a glTF model and frame it.", PROMPT_IMPORT_FRAME),
            "setup_environment" => (
                "Set the scene environment (sky gradient, or a baked .ktx2 by URL).",
                PROMPT_SETUP_ENVIRONMENT,
            ),
            other => {
                return Err(McpError::invalid_params(
                    format!("unknown prompt {other}"),
                    None,
                ))
            }
        };
        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, body)])
                .with_description(description),
        )
    }
}

fn text(s: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s.into())])
}

/// The externally-tagged variant name a `oneOf` schema entry represents — for a
/// serde-external enum, either a `const`/single-`enum` string (unit variant) or
/// the single `required`/`properties` key (data variant). `None` if the entry
/// doesn't match that shape.
fn oneof_variant_name(entry: &Value) -> Option<String> {
    if let Some(c) = entry.get("const").and_then(Value::as_str) {
        return Some(c.to_string());
    }
    if let Some(arr) = entry.get("enum").and_then(Value::as_array) {
        if let [Value::String(s)] = arr.as_slice() {
            return Some(s.clone());
        }
    }
    if let Some(req) = entry.get("required").and_then(Value::as_array) {
        if let [Value::String(s)] = req.as_slice() {
            return Some(s.clone());
        }
    }
    if let Some(props) = entry.get("properties").and_then(Value::as_object) {
        if props.len() == 1 {
            return props.keys().next().cloned();
        }
    }
    None
}

/// Every NodeKind variant name (snake_case) from a schema's `oneOf`.
fn node_variant_names(full: &Value) -> Vec<String> {
    full.get("oneOf")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(oneof_variant_name).collect())
        .unwrap_or_default()
}

/// Collect every `#/$defs/<Name>` referenced anywhere in `v`.
fn collect_defs_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                if k == "$ref" {
                    if let Some(name) = val.as_str().and_then(|s| s.strip_prefix("#/$defs/")) {
                        out.push(name.to_string());
                    }
                } else {
                    collect_defs_refs(val, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_defs_refs(x, out)),
        _ => {}
    }
}

/// Scope a full `NodeKind` schema down to a single `variant`: the matching `oneOf`
/// entry plus ONLY the `$defs` it references (transitively). `None` if no variant
/// matches. Shrinks a hundreds-of-KB schema to just the relevant slice.
fn filter_node_variant_schema(full: &Value, variant: &str) -> Option<Value> {
    let one_of = full.get("oneOf").and_then(Value::as_array)?;
    let entry = one_of
        .iter()
        .find(|e| oneof_variant_name(e).as_deref() == Some(variant))?
        .clone();

    // Transitive $defs closure reachable from the chosen variant.
    let mut kept = serde_json::Map::new();
    if let Some(defs) = full.get("$defs").and_then(Value::as_object) {
        let mut stack = Vec::new();
        collect_defs_refs(&entry, &mut stack);
        let mut seen = std::collections::HashSet::new();
        while let Some(name) = stack.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some(def) = defs.get(&name) {
                kept.insert(name.clone(), def.clone());
                collect_defs_refs(def, &mut stack);
            }
        }
    }

    let mut out = serde_json::Map::new();
    if let Some(s) = full.get("$schema") {
        out.insert("$schema".to_string(), s.clone());
    }
    out.insert(
        "title".to_string(),
        Value::String(format!("NodeKind::{variant}")),
    );
    if let Some(obj) = entry.as_object() {
        for (k, v) in obj {
            out.insert(k.clone(), v.clone());
        }
    }
    if !kept.is_empty() {
        out.insert("$defs".to_string(), Value::Object(kept));
    }
    Some(Value::Object(out))
}

fn parse_node(s: &str) -> Result<NodeId, McpError> {
    uuid::Uuid::parse_str(s)
        .map(NodeId)
        .map_err(|e| McpError::invalid_params(format!("invalid node id {s:?}: {e}"), None))
}

fn parse_variant(s: &str) -> Result<awsm_renderer_scene::VariantId, McpError> {
    uuid::Uuid::parse_str(s)
        .map(awsm_renderer_scene::VariantId)
        .map_err(|e| McpError::invalid_params(format!("invalid variant id {s:?}: {e}"), None))
}

fn parse_variant_opt(
    s: &Option<String>,
) -> Result<Option<awsm_renderer_scene::VariantId>, McpError> {
    s.as_ref().map(|s| parse_variant(s)).transpose()
}

fn parse_node_opt(s: &Option<String>) -> Result<Option<NodeId>, McpError> {
    s.as_deref().map(parse_node).transpose()
}

fn parse_wrap(s: &str) -> Result<awsm_renderer_editor_protocol::TextureWrap, McpError> {
    use awsm_renderer_editor_protocol::TextureWrap as W;
    match s.trim().to_ascii_lowercase().as_str() {
        "repeat" => Ok(W::Repeat),
        "clamp" | "clamp_to_edge" | "clamptoedge" => Ok(W::ClampToEdge),
        "mirror" | "mirrored_repeat" | "mirroredrepeat" => Ok(W::MirroredRepeat),
        other => Err(McpError::invalid_params(
            format!("invalid wrap {other:?} (use repeat | clamp_to_edge | mirrored_repeat)"),
            None,
        )),
    }
}

fn parse_filter(s: &str) -> Result<awsm_renderer_editor_protocol::TextureFilter, McpError> {
    use awsm_renderer_editor_protocol::TextureFilter as F;
    match s.trim().to_ascii_lowercase().as_str() {
        "nearest" | "point" => Ok(F::Nearest),
        "linear" | "smooth" => Ok(F::Linear),
        other => Err(McpError::invalid_params(
            format!("invalid filter {other:?} (use nearest | linear)"),
            None,
        )),
    }
}

fn parse_nodes(ids: &[String]) -> Result<Vec<NodeId>, McpError> {
    ids.iter().map(|s| parse_node(s)).collect()
}

fn parse_asset(s: &str) -> Result<AssetId, McpError> {
    uuid::Uuid::parse_str(s)
        .map(AssetId)
        .map_err(|e| McpError::invalid_params(format!("invalid asset id {s:?}: {e}"), None))
}

fn parse_asset_opt(s: &Option<String>) -> Result<Option<AssetId>, McpError> {
    s.as_deref().map(parse_asset).transpose()
}

/// A tool argument that is **strongly typed** (its JSON Schema is `T`'s, so
/// clients see the exact shape) yet tolerant of clients that deliver a nested
/// object as a JSON *string* — it deserializes from either form. The best of
/// both: typed/self-documenting AND robust.
#[derive(Debug, Clone)]
pub struct Flexible<T>(pub T);

impl<'de, T: serde::de::DeserializeOwned> serde::Deserialize<'de> for Flexible<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let inner = match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) => serde_json::from_str(&s).map_err(Error::custom)?,
            other => serde_json::from_value(other).map_err(Error::custom)?,
        };
        Ok(Flexible(inner))
    }
}

// Schema is exactly `T`'s — clients that respect schemas send a structured object.
impl<T: schemars::JsonSchema> schemars::JsonSchema for Flexible<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn schema_id() -> std::borrow::Cow<'static, str> {
        T::schema_id()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        T::json_schema(generator)
    }
}

/// Deserialize a free-form JSON tool argument into `T`. Some MCP clients deliver
/// an untyped (schema-less) object argument as a JSON *string* rather than a
/// nested object; accept both by re-parsing a string first. (For typed args
/// prefer [`Flexible<T>`], which also publishes the schema.)
fn json_arg<T: serde::de::DeserializeOwned>(
    v: serde_json::Value,
    what: &str,
) -> Result<T, McpError> {
    // A bare JSON string arg is itself JSON (some clients double-encode); parse it
    // to a Value first, then deserialize. Integer-keyed maps inside tagged-enum
    // commands (e.g. `set_vertex_overrides {overrides:{uvs:{"0":[u,v]}}}`) are
    // handled at the field level in `VertexOverrides` (a string-or-int key
    // deserializer that survives serde's internally-tagged `Content` buffering) —
    // `from_str` alone does NOT fix them, because the `#[serde(tag="cmd")]` enum
    // buffers the variant into `Content`, which can't coerce a string key to u32.
    let v = match v {
        serde_json::Value::String(s) => serde_json::from_str(&s)
            .map_err(|e| McpError::invalid_params(format!("bad {what}: {e}"), None))?,
        other => other,
    };
    serde_json::from_value(v)
        .map_err(|e| McpError::invalid_params(format!("bad {what}: {e}"), None))
}

/// Parse a snake_case string into a scene-schema enum via serde (e.g. param /
/// interp names). Keeps the MCP layer from re-enumerating every variant.
fn parse_enum<T: serde::de::DeserializeOwned>(s: &str, what: &str) -> Result<T, McpError> {
    serde_json::from_value(Value::String(s.to_string()))
        .map_err(|_| McpError::invalid_params(format!("unknown {what}: {s:?}"), None))
}

fn build_track_target(a: &TrackTargetArg) -> Result<TrackTarget, McpError> {
    let need_node = || -> Result<NodeId, McpError> {
        a.node
            .as_deref()
            .ok_or_else(|| McpError::invalid_params("target requires `node`", None))
            .and_then(parse_node)
    };
    Ok(match a.kind.as_str() {
        "transform" => {
            let prop_s = a.prop.as_deref().ok_or_else(|| {
                McpError::invalid_params("transform target requires `prop`", None)
            })?;
            TrackTarget::Transform {
                node: need_node()?,
                prop: parse_enum::<TransformProp>(prop_s, "transform prop")?,
            }
        }
        "morph" => TrackTarget::Morph {
            node: need_node()?,
            index: a.index.unwrap_or(0) as usize,
        },
        "uniform" => TrackTarget::Uniform {
            material: parse_asset(a.material.as_deref().ok_or_else(|| {
                McpError::invalid_params("uniform target requires `material`", None)
            })?)?,
            name: a
                .name
                .clone()
                .ok_or_else(|| McpError::invalid_params("uniform target requires `name`", None))?,
        },
        "builtin_param" => TrackTarget::BuiltinParam {
            node: need_node()?,
            param: parse_enum(param_str(a)?, "builtin param")?,
        },
        "light" => TrackTarget::Light {
            node: need_node()?,
            param: parse_enum(param_str(a)?, "light param")?,
        },
        "camera" => TrackTarget::Camera {
            node: need_node()?,
            param: parse_enum(param_str(a)?, "camera param")?,
        },
        "texture_transform" => {
            let slot_s = a.slot.as_deref().ok_or_else(|| {
                McpError::invalid_params(
                    "texture_transform target requires `slot` (base_color | metallic_roughness | normal | occlusion | emissive)",
                    None,
                )
            })?;
            let prop_s = a.prop.as_deref().ok_or_else(|| {
                McpError::invalid_params(
                    "texture_transform target requires `prop` (offset | scale | rotation)",
                    None,
                )
            })?;
            TrackTarget::TextureTransform {
                node: need_node()?,
                slot: parse_enum::<TexSlot>(slot_s, "texture slot")?,
                prop: parse_enum::<TexTransformProp>(prop_s, "texture transform prop")?,
            }
        }
        other => {
            return Err(McpError::invalid_params(
                format!("unknown target kind {other:?}"),
                None,
            ))
        }
    })
}

fn param_str(a: &TrackTargetArg) -> Result<&str, McpError> {
    a.param
        .as_deref()
        .ok_or_else(|| McpError::invalid_params("target requires `param`", None))
}

fn build_track_value(a: &TrackValueArg) -> Result<TrackValue, McpError> {
    let bad =
        |n: usize| McpError::invalid_params(format!("{} value needs {n} number(s)", a.kind), None);
    Ok(match a.kind.as_str() {
        "vec2" => {
            if a.value.len() < 2 {
                return Err(bad(2));
            }
            TrackValue::Vec2([a.value[0], a.value[1]])
        }
        "vec3" => {
            if a.value.len() < 3 {
                return Err(bad(3));
            }
            TrackValue::Vec3([a.value[0], a.value[1], a.value[2]])
        }
        "vec4" => {
            if a.value.len() < 4 {
                return Err(bad(4));
            }
            TrackValue::Vec4([a.value[0], a.value[1], a.value[2], a.value[3]])
        }
        "quat" => {
            if a.value.len() < 4 {
                return Err(bad(4));
            }
            TrackValue::Quat([a.value[0], a.value[1], a.value[2], a.value[3]])
        }
        "scalar" => {
            if a.value.is_empty() {
                return Err(bad(1));
            }
            TrackValue::Scalar(a.value[0])
        }
        other => {
            return Err(McpError::invalid_params(
                format!("unknown value kind {other:?} (use vec3|quat|scalar)"),
                None,
            ))
        }
    })
}

fn parse_interp(s: &str) -> Result<Interp, McpError> {
    parse_enum(s, "interp")
}

/// The published dynamic-material contracts, embedded at build time (served
/// verbatim by `get_material_contract` + the `awsm://docs/...` MCP resources).
const CONTRACT_OPAQUE: &str = include_str!("../../../docs/dynamic-materials/contract-opaque.md");
const CONTRACT_TRANSPARENT: &str =
    include_str!("../../../docs/dynamic-materials/contract-transparent.md");
const CONTRACT_VERTEX: &str = include_str!("../../../docs/dynamic-materials/contract-vertex.md");
const MCP_DOC: &str = include_str!("../../../docs/MCP.md");
const ASSET_WORKFLOWS: &str = include_str!("../../../docs/ASSET_WORKFLOWS.md");
const AGENT_GUIDE: &str = include_str!("../../../docs/AGENT_GUIDE.md");
const MATERIAL_RECIPES: &str = include_str!("../../../docs/dynamic-materials/recipes.md");
const ANIMATION_DOC: &str = include_str!("../../../docs/ANIMATION_AUTHORING.md");
const MESH_TOOLS_DOC: &str = include_str!("../../../docs/MESH_TOOLS.md");

const PROMPT_AUTHOR_MATERIAL: &str = "\
Author a lit custom WGSL material so it renders (never a silent black mesh):
1. get_material_contract — read the input ABI + legal include/input keys.
2. add_custom_material — returns the new material id.
3. set_material_fragment_inputs { keys: [\"normals\",\"view_dir\"] } and, if you \
   call apply_lighting/brdf, set_material_includes the matching keys.
4. set_material_layout — declare any uniforms (e.g. a Color3 tint), if needed.
5. set_material_wgsl — write the body using input.world_normal / \
   input.surface_to_camera etc. This recompiles synchronously and FAILS LOUDLY \
   with the compiler error if the WGSL is invalid (no silent ok).
6. If it errored, get_material_diagnostics for details; fix and retry.
7. add_material_variant { node, material } + select_material_variant { node, variant } \
   on a mesh node, wait_render_settled, then screenshot_scene.
8. A fresh scene already has a key light + IBL; if the mesh is still dark, check \
   set_light_intensity or set_environment.";

const PROMPT_ROTATION_CLIP: &str = "\
Create and play a rotation animation clip:
1. add_clip — returns the clip id.
2. add_track { clip, target: { kind: \"transform\", node: \"<id>\", prop: \"rotation\" } }.
   Tracks append; the new index is the prior track count (read get_snapshot).
3. add_keyframe { clip, track, t: 0.0, value: { kind: \"quat\", value: [0,0,0,1] } }.
4. add_keyframe { clip, track, t: 1.0, value: { kind: \"quat\", value: [0,0,0.707,0.707] } }.
5. get_track_data to verify the keyframes you authored.
6. set_current_clip { clip }, set_playing { on: true } (or set_playhead to scrub).";

const PROMPT_IMPORT_FRAME: &str = "\
Import a glTF/glb model and frame it:
1. import_model_from_url { url }.
2. wait_render_settled (the import compiles pipelines).
3. get_snapshot to find the imported node id.
4. frame_node { node } (optionally set_camera_orbit for a 3/4 view).
5. wait_render_settled, then screenshot_scene.";

const PROMPT_SETUP_ENVIRONMENT: &str = "\
Set the scene environment. Assets come from URLs — there is NO inline-base64 / \
equirect tool. Two routes:

The environment has THREE independent slots — skybox, specular (the prefiltered/\
roughness-mipped IBL map that drives reflections), irradiance (the diffuse-\
convolved IBL map that drives ambient light). Each is set separately; omit a slot \
to leave it unchanged. Two routes:

A) Quick two-color sky (no hosting):
   1. set_environment { zenith: [r,g,b], nadir: [r,g,b] }  (linear RGB; sets all three slots).
   2. wait_render_settled, then screenshot_scene.

B) Baked HDRI / studio KTX2 cubemaps by URL (read awsm://docs/asset-workflows for full flags):
   1. Make an .hdr/.exr (a real HDRI, or generate an equirect panorama procedurally — numpy → flat-RGBE .hdr).
   2. cmgen → skybox faces (-x skybox), prefiltered spec (--ibl-ld), irradiance (--ibl-irradiance).
      The equirect→cubemap projection happens HERE, offline (there is no runtime equirect).
   3. ktx create --cubemap --format B10G11R11_UFLOAT_PACK32 … → skybox.ktx2, env.ktx2, irradiance.ktx2.
   4. Serve them (a local CORS static server is fine), then:
      set_environment { skybox: \"<url>/skybox.ktx2\", specular: \"<url>/env.ktx2\", irradiance: \"<url>/irradiance.ktx2\" }.
      (Slots are independent — set only the ones you want. Mix freely: e.g. skybox: \"builtin\" for a \
clean default sky + a studio specular/irradiance for chrome, or keep default-sky irradiance and \
override just specular.)
   5. wait_render_settled, then screenshot_scene. Save embeds the .ktx2 bytes so it survives reload with no server.
   To read what's currently set: get_snapshot → project.environment (per-slot kind + asset).";

/// The legal Pass-Dependency keys, appended to the contract output.
const MATERIAL_KEYS_DOC: &str = "\
## Legal Pass-Dependency keys

shader_includes (set_material_includes): math, camera, color_space, textures, \
vertex_color, light_access, apply_lighting, brdf, material_color_calc, shadows, \
skybox, extras, ibl.

`ibl` (image-based lighting): declare it + call `sample_ibl(albedo, normal, \
surface_to_camera, roughness, metallic) -> vec3<f32>` (or `sample_ibl_diffuse(n)` \
/ `sample_ibl_specular(reflect_dir, roughness)`) to get the SAME environment \
ambient + reflection first-party PBR gets — so a custom material matches the \
scene's IBL instead of hand-faking a sky gradient (the fix for custom materials \
rendering black in an IBL-only, no-punctual-light scene). Pair with the `normals` \
+ `view_dir` fragment_inputs for `input.world_normal` + `input.surface_to_camera`.

fragment_inputs (set_material_fragment_inputs): normals, tangents, uv, lights, \
view_dir, vertex_color.

Declare exactly the inputs/includes your WGSL references — an under-declaration \
fails to resolve a referenced symbol (→ compile error / black), an \
over-declaration just compiles a heavier bucket.";

fn euler_order(order: Option<&str>) -> Result<glam::EulerRot, McpError> {
    Ok(match order.unwrap_or("xyz").to_ascii_lowercase().as_str() {
        "xyz" => glam::EulerRot::XYZ,
        "xzy" => glam::EulerRot::XZY,
        "yxz" => glam::EulerRot::YXZ,
        "yzx" => glam::EulerRot::YZX,
        "zxy" => glam::EulerRot::ZXY,
        "zyx" => glam::EulerRot::ZYX,
        other => {
            return Err(McpError::invalid_params(
                format!("unknown euler order {other:?} (use xyz|xzy|yxz|yzx|zxy|zyx)"),
                None,
            ))
        }
    })
}

fn slot_arg(s: SlotArg) -> SlotSpec {
    SlotSpec {
        name: s.name,
        ty: s.ty,
        val: s.val,
        debug: s.debug,
        color_kind: s.color_kind,
    }
}

fn default_shape(shape: ShapeArg) -> PrimitiveShape {
    match shape {
        ShapeArg::Plane => PrimitiveShape::Plane {
            width: 1.0,
            depth: 1.0,
            segments_x: 1,
            segments_z: 1,
        },
        ShapeArg::Box => PrimitiveShape::Box {
            dims: [1.0, 1.0, 1.0],
        },
        ShapeArg::Sphere => PrimitiveShape::Sphere {
            radius: 0.5,
            segments_long: 32,
            segments_lat: 16,
        },
        ShapeArg::Cylinder => PrimitiveShape::Cylinder {
            radius: 0.5,
            height: 1.0,
            radial_segments: 32,
        },
        ShapeArg::Cone => PrimitiveShape::Cone {
            radius: 0.5,
            height: 1.0,
            radial_segments: 32,
        },
        ShapeArg::Torus => PrimitiveShape::Torus {
            radius: 0.5,
            thickness: 0.2,
            segments_major: 32,
            segments_minor: 16,
        },
    }
}

fn unexpected(resp: Response) -> McpError {
    McpError::internal_error(format!("unexpected editor response: {resp:?}"), None)
}

/// Render compile diagnostics as a human-readable multi-line string (line-tagged
/// when the line is known).
fn fmt_diag_errors(errors: &[CompileError]) -> String {
    errors
        .iter()
        .map(|e| match e.line {
            Some(l) => format!("  line {l}: {}", e.message),
            None => format!("  {}", e.message),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `json_arg` must deserialize an `EditorCommand` carrying an
    /// integer-keyed map (`VertexOverrides.uvs: HashMap<u32,…>`) whose JSON keys
    /// are strings. The old `from_value` path rejected this with
    /// "invalid type: string \"0\", expected u32", making every integer-keyed-map
    /// command un-drivable over dispatch_command. `from_str` parses it.
    #[test]
    fn json_arg_parses_integer_keyed_map_command() {
        let v = serde_json::json!({
            "cmd": "set_vertex_overrides",
            "mesh": "00000000-0000-0000-0000-000000000001",
            "overrides": { "uvs": { "0": [0.1, 0.2], "7": [0.3, 0.4] } }
        });
        let cmd: EditorCommand = json_arg(v, "command").expect("integer-keyed map should parse");
        match cmd {
            EditorCommand::SetVertexOverrides { overrides, .. } => {
                assert_eq!(overrides.uvs.get(&0), Some(&[0.1, 0.2]));
                assert_eq!(overrides.uvs.get(&7), Some(&[0.3, 0.4]));
            }
            other => panic!("expected SetVertexOverrides, got {other:?}"),
        }
    }

    /// Regression (handoff #11): `set_current_clip` called with NO `clip`
    /// argument must deserialize to `clip: None` (⇒ `SetCurrentClip { id: None }`
    /// = CLEAR the current clip). A deployed build once left the previous clip
    /// active on the no-arg form, so later animation re-lowers silently re-posed
    /// the rig mid-scene-edit.
    #[test]
    fn clip_opt_params_omitted_clip_is_clear() {
        let p: ClipOptParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(p.clip.is_none());
        assert!(parse_asset_opt(&p.clip).unwrap().is_none());

        let p: ClipOptParams = serde_json::from_value(serde_json::json!({ "clip": null })).unwrap();
        assert!(p.clip.is_none());
    }

    /// The String-wrapped form (a JSON-encoded string arg) must still work.
    #[test]
    fn json_arg_parses_string_wrapped_command() {
        let inner = r#"{"cmd":"set_vertex_overrides","mesh":"00000000-0000-0000-0000-000000000001","overrides":{"uvs":{"3":[0.5,0.6]}}}"#;
        let v = serde_json::Value::String(inner.to_string());
        let cmd: EditorCommand = json_arg(v, "command").expect("string-wrapped should parse");
        match cmd {
            EditorCommand::SetVertexOverrides { overrides, .. } => {
                assert_eq!(overrides.uvs.get(&3), Some(&[0.5, 0.6]));
            }
            other => panic!("expected SetVertexOverrides, got {other:?}"),
        }
    }
}
