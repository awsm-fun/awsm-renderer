//! The rmcp tool layer. Each tool is a thin typed wrapper that builds a protocol
//! [`Request`] and relays it to the attached editor over the WebTransport link,
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

use awsm_editor_protocol::{
    CameraAxis, CompileError, CustomAlphaMode, EditorCommand, EditorMode, EditorQuery, InsertSpec,
    ProceduralKind, QueryResult, Request, Response, SlotSpec,
};
use awsm_scene::animation::{
    BuiltinParamKind, ClipLoop, Interp, LightParamKind, TrackTarget, TrackValue, TransformProp,
};
use awsm_scene::{
    AssetId, EnvironmentConfig, IblConfig, LightKind, MaterialShading, NodeId, PrimitiveShape,
    SkyboxConfig, Trs,
};

use crate::link::EditorLink;

/// The MCP tool provider. Cheap to clone (the link is an `Arc` handle).
#[derive(Clone)]
pub struct EditorMcp {
    link: EditorLink,
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
    pub predicate: Flexible<awsm_editor_protocol::VertexPredicate>,
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
pub struct SetVertexPositionsParams {
    pub mesh: String,
    /// Vertex indices to move.
    pub indices: Vec<u32>,
    /// New positions, aligned with `indices`.
    pub positions: Vec<[f32; 3]>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SoftTransformParams {
    pub mesh: String,
    /// The selected vertex indices (the move's full-weight center).
    pub indices: Vec<u32>,
    /// Translation applied at the selection, fading over the falloff radius.
    pub translation: [f32; 3],
    /// Falloff radius (world units); 0 = hard move of exactly the selection.
    pub falloff: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetMeshModifiersParams {
    /// UUID of the editable mesh asset.
    pub mesh: String,
    /// Strongly-typed modifier stack (the schema lists every base + modifier).
    /// See the `awsm://docs/mesh-tools` resource for worked examples.
    pub stack: Flexible<awsm_editor_protocol::ModifierStack>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddModifierParams {
    /// UUID of the editable mesh asset (must already have a modifier stack —
    /// call `set_mesh_modifiers` first to give it a base).
    pub mesh: String,
    /// One strongly-typed modifier object (e.g. `{"twist":{"axis":"y","turns":2}}`).
    /// See the `awsm://docs/mesh-tools` resource for every modifier's shape.
    pub modifier: Flexible<awsm_editor_protocol::Modifier>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetModifierParams {
    /// UUID of the editable mesh asset (must already have a modifier stack).
    pub mesh: String,
    /// Zero-based index of the modifier to replace (must be in range).
    pub index: u32,
    /// The replacement modifier object.
    pub modifier: Flexible<awsm_editor_protocol::Modifier>,
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
    /// Vertex indices (into the resolved/baked topology) to paint.
    pub indices: Vec<u32>,
    /// Linear RGBA color to set on each index.
    pub color: [f32; 4],
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetVertexNormalsParams {
    /// UUID of the editable mesh asset.
    pub mesh: String,
    /// Vertex indices to override the normal of.
    pub indices: Vec<u32>,
    /// The normal vector to set on each index (should be unit-length).
    pub normal: [f32; 3],
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetVertexDataParams {
    /// UUID of the node whose resolved mesh to read.
    pub node: String,
    /// Vertex indices to read the final (post-eval + override) data of.
    pub indices: Vec<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetMeshLayersParams {
    /// UUID of the node whose mesh layer summary to read.
    pub node: String,
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
pub struct BaseUrlParams {
    pub base_url: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShadingArg {
    Pbr,
    Unlit,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ShadingParams {
    pub shading: ShadingArg,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AssetArg {
    /// Asset UUID (material / texture / clip).
    pub asset: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AssignMaterialParams {
    /// Mesh node UUID.
    pub node: String,
    /// Material asset UUID, or omit/null to clear (→ magenta unassigned).
    #[serde(default)]
    pub material: Option<String>,
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
    /// Which contract: false (default) = opaque/mask, true = transparent/blend.
    #[serde(default)]
    pub transparent: bool,
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
#[serde(rename_all = "snake_case")]
pub enum BuiltinParamArg {
    BaseColor,
    Metallic,
    Roughness,
    Emissive,
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
    pub slot: awsm_editor_protocol::BuiltinTextureSlot,
    /// Texture asset UUID to bind, or omit/null to clear the slot.
    #[serde(default)]
    pub texture: Option<String>,
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
    pub entries: Vec<awsm_editor_protocol::SkinWeightEntry>,
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
    /// Skybox: omit/"builtin" for the built-in default, or a KTX texture asset
    /// UUID for a custom cubemap.
    #[serde(default)]
    pub skybox: Option<String>,
    /// IBL prefiltered specular: omit/"builtin" for the built-in default, or a
    /// KTX asset UUID (then `ibl_irradiance` is required too).
    #[serde(default)]
    pub ibl_prefiltered: Option<String>,
    /// IBL irradiance KTX asset UUID (required when `ibl_prefiltered` is a UUID).
    #[serde(default)]
    pub ibl_irradiance: Option<String>,
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
    /// Target kind: transform | morph | uniform | builtin_param | light | camera.
    pub kind: String,
    /// Node UUID (transform / morph / builtin_param / light / camera).
    #[serde(default)]
    pub node: Option<String>,
    /// Transform property: translation | rotation | scale.
    #[serde(default)]
    pub prop: Option<String>,
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
pub struct AddKeyframeParams {
    pub clip: String,
    pub track: u32,
    /// Time in seconds.
    pub t: f64,
    pub value: TrackValueArg,
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
        Self {
            link,
            tool_router: Self::tool_router(),
        }
    }

    // ── discovery / read ────────────────────────────────────────────────────

    #[tool(
        description = "Snapshot the editor state: scene tree (node ids/names/kinds), selection, mode, undo/redo depth, animation library, custom materials. Start here to discover ids."
    )]
    async fn get_snapshot(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Snapshot).await
    }

    #[tool(
        description = "Health check: confirms an editor is attached (fails fast with 'no editor attached' if not). Returns the current mode."
    )]
    async fn ping(&self) -> Result<CallToolResult, McpError> {
        match self.req(Request::Mode).await? {
            Response::Mode(m) => Ok(text(format!("pong — editor attached (mode={m:?})"))),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
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

    #[tool(description = "The current workspace mode (scene | material | animation).")]
    async fn get_mode(&self) -> Result<CallToolResult, McpError> {
        match self.req(Request::Mode).await? {
            Response::Mode(m) => Ok(text(format!("{m:?}"))),
            other => Err(unexpected(other)),
        }
    }

    #[tool(description = "Read a custom (dynamic-WGSL) material's shader source.")]
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
        description = "The dynamic-material WGSL authoring contract (the shader ABI): input.* fields, return type, time/camera access, legal shader_include + fragment_input keys. Pass transparent:true for the blend contract. Read this before authoring a custom material."
    )]
    async fn get_material_contract(
        &self,
        Parameters(p): Parameters<ContractParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = if p.transparent {
            CONTRACT_TRANSPARENT
        } else {
            CONTRACT_OPAQUE
        };
        Ok(text(format!("{body}\n\n{MATERIAL_KEYS_DOC}")))
    }

    #[tool(
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
        description = "World-space AABB { min, max } for each node (CPU-estimated; pass node UUIDs, or empty for all). Use to frame the camera or size objects."
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
        description = "Bake the whole scene to a binary glTF and return the .glb bytes base64-encoded. Built-in PBR → glTF PBR; Unlit → KHR_materials_unlit; custom/Toon → AWSM_materials_none (no embedded material). Textures are referenced-only."
    )]
    async fn export_scene_glb(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::ExportGlb { node: None }).await
    }

    #[tool(
        description = "Bake one node (and its subtree) to a binary glTF and return the .glb bytes base64-encoded. Same material mapping as export_scene_glb."
    )]
    async fn export_node_glb(
        &self,
        Parameters(p): Parameters<ExportNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::ExportGlb {
            node: Some(parse_node(&p.node)?),
        })
        .await
    }

    #[tool(
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
        description = "Bake the whole project to a player runtime bundle DIRECTORY: a `scene.toml` (the runtime scene — node hierarchy + transforms + material instances + lights/cameras + our animation clips + environment, meshes referenced by id) plus an `assets/` directory: one geometry-only `assets/<id>.glb` per non-primitive mesh (bare primitives stay procedural in scene.toml), custom-material wgsl folders, and referenced textures. Materials + animations are NOT in the glbs (they're ours, applied by the player from scene.toml + clips). A read; returns the file set `{name, files:[{path, base64 bytes}]}` (result kind `player_bundle`). Skinned/morph meshes' glb re-export from source is a follow-on (static geometry for now)."
    )]
    async fn export_player_bundle(
        &self,
        Parameters(p): Parameters<ExportBundleParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::ExportPlayerBundle { name: p.name })
            .await
    }

    #[tool(description = "Mean/min/max luma over a canvas region (or the whole canvas).")]
    async fn canvas_stats(
        &self,
        Parameters(p): Parameters<RegionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::CanvasStats { region: p.region })
            .await
    }

    #[tool(
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

    #[tool(description = "PNG thumbnail of a texture asset (by UUID).")]
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

    #[tool(description = "Duplicate a node (deep clone, fresh ids) as a following sibling.")]
    async fn duplicate_node(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::Duplicate {
            id: parse_node(&p.node)?,
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
        description = "Set a node's local transform: translation [x,y,z], rotation quaternion [x,y,z,w], scale [x,y,z]."
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

    #[tool(description = "Start a fresh, empty project (clears undo history).")]
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

    #[tool(description = "Import a glTF/glb model from a URL.")]
    async fn import_model_from_url(
        &self,
        Parameters(p): Parameters<UrlParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::ImportModelFromUrl { url: p.url })
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
        description = "Create a fresh built-in material (pbr | unlit). Returns the new material id."
    )]
    async fn add_builtin_material(
        &self,
        Parameters(p): Parameters<ShadingParams>,
    ) -> Result<CallToolResult, McpError> {
        let shading = match p.shading {
            ShadingArg::Pbr => MaterialShading::Pbr,
            ShadingArg::Unlit => MaterialShading::Unlit,
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
        description = "Select a node mesh's vertices by predicate (no cursor), returning their indices to feed into set_vertex_positions / soft_transform_vertices. `predicate` is a VertexPredicate JSON, e.g. {\"kind\":\"top_percent\",\"axis\":1,\"percent\":0.2} (percent is a 0..1 FRACTION of the axis extent — 0.2 = top 20%; values >1 are clamped to 1.0 = everything) or {\"kind\":\"normal_dir\",\"dir\":[0,1,0],\"threshold\":0.7} / axis_greater / axis_less / within_radius / within_aabb (box: {\"kind\":\"within_aabb\",\"min\":[x,y,z],\"max\":[x,y,z]} — local space; pair with get_node_bounds for region selection)."
    )]
    async fn select_vertices_where(
        &self,
        Parameters(p): Parameters<SelectVerticesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::SelectVerticesWhere {
            node: parse_node(&p.node)?,
            predicate: p.predicate.0,
        })
        .await
    }

    #[tool(
        description = "Paint per-vertex COLORS on an editable mesh. `mesh` is the mesh asset UUID; `indices` are vertex indices (into the resolved/baked topology — get them from select_vertices_where); `color` is a linear RGBA [r,g,b,a]. TERMINAL/COLLAPSE: the first per-vertex authoring op freezes the procedural stack to a Captured base (topology locks; modifier params bake in) — after this only the sparse override layer is editable. NOTE: painted colors only DISPLAY under a material that reads vertex colors — built-in PBR with `vertex_colors_enabled`, or a custom material that samples them (see the texture-splatting recipe in `awsm://docs/mesh-tools`). Re-bakes geometry; coalesces consecutive strokes on one mesh into one undo step. Verify with get_vertex_data."
    )]
    async fn paint_vertex_colors(
        &self,
        Parameters(p): Parameters<PaintVertexColorsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::PaintVertexColors {
            mesh: parse_asset(&p.mesh)?,
            indices: p.indices,
            color: p.color,
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
        description = "Read the FINAL (post-eval + override) per-vertex data for specific indices of a node's resolved mesh: returns `{ vertex_count, vertices: [{ index, position, normal, color, uv }] }` (color/uv null when the mesh has no such channel). The read counterpart to paint_vertex_colors / set_vertex_normals / set_vertex_positions — confirm what your last authoring op actually produced. `node` is the node UUID; `indices` the verts to read."
    )]
    async fn get_vertex_data(
        &self,
        Parameters(p): Parameters<GetVertexDataParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::GetVertexData {
            node: parse_node(&p.node)?,
            indices: p.indices,
        })
        .await
    }

    #[tool(
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

    #[tool(description = "Delete a custom (dynamic/built-in) material by id.")]
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

    #[tool(description = "Assign a material to a mesh node (or clear it with material omitted).")]
    async fn assign_material(
        &self,
        Parameters(p): Parameters<AssignMaterialParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AssignMaterial {
            node: parse_node(&p.node)?,
            material: parse_asset_opt(&p.material)?,
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
        description = "Set the ShaderIncludes a custom material's WGSL needs (`keys`). Legal: math, camera, color_space, textures, vertex_color, light_access, apply_lighting, brdf, material_color_calc, shadows, skybox, extras. Unknown keys are dropped."
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
        description = "Set the FragmentInputs (interpolants) a custom material's WGSL reads (`keys`). Legal: normals, tangents, uv, lights, view_dir, vertex_color. Unknown keys are dropped."
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
        description = "Set the default value of a custom material's declared uniform slot (by name). `value` is comma-separated (e.g. \"0.6, 0.7, 1.0\"). The writable counterpart of reading a uniform back."
    )]
    async fn set_material_uniform(
        &self,
        Parameters(p): Parameters<MaterialUniformParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetMaterialUniform {
            material: parse_asset(&p.material)?,
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
        description = "Set a built-in material factor on a mesh node's inline material. param: base_color | emissive (value = 3 floats) | metallic | roughness (value = 1 float)."
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
        };
        self.dispatch(EditorCommand::SetBuiltinParam {
            node: parse_node(&p.node)?,
            param,
            value: p.value,
        })
        .await
    }

    #[tool(
        description = "Bind a texture asset onto a mesh node's BUILT-IN (inline PBR) material slot: base_color | metallic_roughness | normal | occlusion | emissive. Omit `texture` to clear. Create textures with import_texture_from_url (raster) or add_texture_asset (procedural). (set_material_texture is the custom-WGSL-material counterpart.)"
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

    #[tool(description = "Set a light node's intensity.")]
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

    #[tool(description = "Set a point/spot light node's range.")]
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
        description = "Rig discovery per skinned node: { source, primitive_index, joints:[{node,index,name,translation,rotation,scale}] }. Joints ARE ordinary scene nodes — POSE one with set_node_transform on its `node` id (the skin deforms live), ANIMATE one with add_track targeting it (transform). Pass node UUIDs, or empty for all skinned nodes."
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
        description = "Two-bone IK: bring a chain TIP (end_node, e.g. a foot joint) to a world-space target, bending at its parent (knee) under its grandparent (upper leg). Solves analytically and (by default) APPLIES the two joint rotations as one undoable batch — auto-key compatible. `pole` biases the bend direction. Returns { root_node, mid_node, root_rotation, mid_rotation, reach } (reach < 1 ⇒ target beyond the chain's span, clamped). Discover chains via get_skin_data; clips OWN bones while active (delete/pause first)."
    )]
    async fn solve_ik(
        &self,
        Parameters(p): Parameters<SolveIkParams>,
    ) -> Result<CallToolResult, McpError> {
        let end_node = parse_node(&p.end_node)?;
        // 1. Solve (read-only).
        let sol = match self
            .req(Request::Query(EditorQuery::SolveIk {
                end_node,
                target: p.target,
                pole: p.pole,
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
            awsm_editor_protocol::QueryResult::Map(m) if m.kind == "ik_solution" => &m.entries,
            awsm_editor_protocol::QueryResult::Error { error } => {
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
            awsm_editor_protocol::QueryResult::Map(m) => &m.entries,
            _ => return Err(McpError::internal_error("bad transforms result", None)),
        };
        let trs_of = |id: NodeId, rot: [f32; 4]| -> Result<awsm_editor_protocol::Trs, McpError> {
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
            Ok(awsm_editor_protocol::Trs {
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
        description = "Set the scene environment (skybox + IBL). Each of skybox / ibl_prefiltered / ibl_irradiance accepts: 'builtin' (or omit) for the built-in default cubemap/lighting, an existing KTX texture asset UUID, OR a https:// URL to a .ktx2 cubemap (fetched + registered on the fly, like import_texture_from_url). IBL needs both ibl_prefiltered + ibl_irradiance. A fresh scene already seeds the built-in environment."
    )]
    async fn set_environment(
        &self,
        Parameters(p): Parameters<EnvironmentParams>,
    ) -> Result<CallToolResult, McpError> {
        let is_url = |s: &str| s.starts_with("http://") || s.starts_with("https://");
        // Resolve a cubemap arg → an existing KTX asset id, registering a
        // URL-sourced asset first when given a URL (the cubemap analogue of
        // import_texture_from_url; the env-sync fetches the bytes on apply).
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
        let skybox = match p.skybox.as_deref() {
            None | Some("builtin") | Some("builtin_default") => SkyboxConfig::BuiltInDefault,
            Some(v) => SkyboxConfig::Ktx {
                asset_id: resolve_ktx!(v),
            },
        };
        let ibl = match p.ibl_prefiltered.as_deref() {
            None | Some("builtin") | Some("builtin_default") => IblConfig::BuiltInDefault,
            Some(prefiltered) => {
                let irradiance = p.ibl_irradiance.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "ibl_irradiance is required when ibl_prefiltered is set (KTX asset UUID or .ktx2 URL)",
                        None,
                    )
                })?;
                IblConfig::Ktx {
                    prefiltered_asset_id: resolve_ktx!(prefiltered),
                    irradiance_asset_id: resolve_ktx!(irradiance),
                }
            }
        };
        self.dispatch(EditorCommand::SetEnvironment {
            env: EnvironmentConfig { skybox, ibl },
        })
        .await
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

    // ── animation (lifecycle + transport) ───────────────────────────────────

    #[tool(
        description = "Create a fresh empty animation clip and make it current. Returns the new clip id."
    )]
    async fn add_clip(&self) -> Result<CallToolResult, McpError> {
        let id = AssetId::new();
        match self
            .req(Request::Dispatch(EditorCommand::AddClip { id }))
            .await?
        {
            Response::Ok => Ok(text(id.to_string())),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
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
        description = "Add an animation track to a clip, bound to a target. target.kind = transform (node+prop) | morph (node+index) | uniform (material+name) | builtin_param/light/camera (node+param). Tracks append; the new index is the prior track count."
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
        description = "Insert a keyframe at time `t` (seconds) with `value` on a track. value.kind = vec3 | quat (xyzw) | scalar. Replaces any existing key at `t`."
    )]
    async fn add_keyframe(
        &self,
        Parameters(p): Parameters<AddKeyframeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AddKeyframe {
            clip: parse_asset(&p.clip)?,
            track: p.track as usize,
            t: p.t,
            value: build_track_value(&p.value)?,
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
        description = "Dispatch a list of raw EditorCommands as ONE atomic step (applied in order, collapsed into a single undo entry, one round-trip). Cuts latency for multi-step edits (e.g. building a rig). Each command is internally tagged by \"cmd\"."
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
}

// ──────────────────────────────── helpers ───────────────────────────────────

impl EditorMcp {
    async fn req(&self, r: Request) -> Result<Response, McpError> {
        self.link
            .request(&r)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))
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
            Response::Png(bytes) => Ok(CallToolResult::success(vec![Content::image(
                STANDARD.encode(bytes),
                "image/png".to_string(),
            )])),
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
             check get_material_diagnostics after editing. Docs + workflow templates are exposed \
             as MCP resources + prompts."
                .to_string(),
        );
        info
    }

    // ── push channel: forward editor events as MCP logging notifications ─────
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let mut rx = self.link.subscribe_events();
        let peer = context.peer;
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let level = match ev.level.as_deref() {
                            Some("error") => LoggingLevel::Error,
                            Some("warning") => LoggingLevel::Warning,
                            _ => LoggingLevel::Info,
                        };
                        let param = LoggingMessageNotificationParam {
                            level,
                            logger: Some("awsm-editor".to_string()),
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
        ]))
    }

    async fn read_resource(
        &self,
        req: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let body = match req.uri.as_str() {
            "awsm://docs/mcp" => MCP_DOC,
            "awsm://docs/agent-guide" => AGENT_GUIDE,
            "awsm://docs/material-recipes" => MATERIAL_RECIPES,
            "awsm://docs/animation" => ANIMATION_DOC,
            "awsm://docs/mesh-tools" => MESH_TOOLS_DOC,
            "awsm://docs/material-contract-opaque" => CONTRACT_OPAQUE,
            "awsm://docs/material-contract-transparent" => CONTRACT_TRANSPARENT,
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

fn parse_node(s: &str) -> Result<NodeId, McpError> {
    uuid::Uuid::parse_str(s)
        .map(NodeId)
        .map_err(|e| McpError::invalid_params(format!("invalid node id {s:?}: {e}"), None))
}

fn parse_node_opt(s: &Option<String>) -> Result<Option<NodeId>, McpError> {
    s.as_deref().map(parse_node).transpose()
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
        "vec3" => {
            if a.value.len() < 3 {
                return Err(bad(3));
            }
            TrackValue::Vec3([a.value[0], a.value[1], a.value[2]])
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
const MCP_DOC: &str = include_str!("../../../docs/MCP.md");
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
7. assign_material to a mesh node, wait_render_settled, then screenshot_scene.
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

/// The legal Pass-Dependency keys, appended to the contract output.
const MATERIAL_KEYS_DOC: &str = "\
## Legal Pass-Dependency keys

shader_includes (set_material_includes): math, camera, color_space, textures, \
vertex_color, light_access, apply_lighting, brdf, material_color_calc, shadows, \
skybox, extras.

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
