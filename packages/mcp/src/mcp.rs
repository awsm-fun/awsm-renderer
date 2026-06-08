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
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde_json::Value;

use awsm_editor_protocol::{
    CameraAxis, EditorCommand, EditorMode, EditorQuery, InsertSpec, QueryResult, Request, Response,
};
use awsm_scene_schema::{AssetId, LightKind, MaterialShading, NodeId, PrimitiveShape, Trs};

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
    /// World translation `[x, y, z]` (meters, right-handed Y-up).
    pub translation: [f32; 3],
    /// Rotation quaternion `[x, y, z, w]`.
    pub rotation: [f32; 4],
    /// Per-axis scale `[x, y, z]`.
    pub scale: [f32; 3],
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
pub struct ClipOptParams {
    /// Clip asset UUID, or omit/null to clear.
    #[serde(default)]
    pub clip: Option<String>,
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
pub struct CommandJsonParams {
    /// A raw `EditorCommand` as JSON, internally tagged by `"cmd"`. Example:
    /// `{"cmd":"set_keyframe","clip":"<uuid>","track":0,"index":0,"value":{"vec3":[0,1,0]}}`.
    /// Discover variants from docs/MCP.md or the editor command enum.
    pub command: Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct QueryJsonParams {
    /// A raw `EditorQuery` as JSON, internally tagged by `"query"`. Example:
    /// `{"query":"canvas_pixels","coords":[[100,100],[200,200]]}`.
    pub query: Value,
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
        let q: EditorQuery = serde_json::from_value(p.query)
            .map_err(|e| McpError::invalid_params(format!("bad query: {e}"), None))?;
        self.query(q).await
    }

    // ── screenshots ─────────────────────────────────────────────────────────

    #[tool(description = "PNG screenshot of the scene viewport (through the active camera).")]
    async fn screenshot_scene(&self) -> Result<CallToolResult, McpError> {
        self.png(Request::ScenePng).await
    }

    #[tool(description = "PNG of the material-mode preview sphere.")]
    async fn screenshot_material(&self) -> Result<CallToolResult, McpError> {
        self.png(Request::MaterialPng).await
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

    // ── project / import / history ──────────────────────────────────────────

    #[tool(description = "Start a fresh, empty project (clears undo history).")]
    async fn new_project(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::NewProject).await
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

    #[tool(description = "Create a fresh custom WGSL (dynamic) material and make it current.")]
    async fn add_custom_material(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AddCustomMaterial).await
    }

    #[tool(description = "Create a fresh built-in material (pbr | unlit).")]
    async fn add_builtin_material(
        &self,
        Parameters(p): Parameters<ShadingParams>,
    ) -> Result<CallToolResult, McpError> {
        let shading = match p.shading {
            ShadingArg::Pbr => MaterialShading::Pbr,
            ShadingArg::Unlit => MaterialShading::Unlit,
        };
        self.dispatch(EditorCommand::AddBuiltinMaterial { shading })
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

    #[tool(description = "Replace a custom material's WGSL source (auto-recompiles).")]
    async fn set_material_wgsl(
        &self,
        Parameters(p): Parameters<SetWgslParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetCustomMaterialWgsl {
            id: parse_asset(&p.material)?,
            wgsl: p.wgsl,
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

    // ── generic escape hatch ────────────────────────────────────────────────

    #[tool(
        description = "Dispatch a raw EditorCommand (escape hatch for any command without a dedicated tool: keyframes, tracks, mixer, environment…). `command` is internally tagged by \"cmd\"."
    )]
    async fn dispatch_command(
        &self,
        Parameters(p): Parameters<CommandJsonParams>,
    ) -> Result<CallToolResult, McpError> {
        let cmd: EditorCommand = serde_json::from_value(p.command)
            .map_err(|e| McpError::invalid_params(format!("bad command: {e}"), None))?;
        self.dispatch(cmd).await
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

    async fn insert(
        &self,
        spec: InsertSpec,
        parent: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::Insert {
            spec,
            parent: parse_node_opt(&parent)?,
        })
        .await
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
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Drive the awsm-renderer editor. Call get_snapshot to discover node/asset ids, \
             mutate with the scene/material/animation tools (or dispatch_command for anything \
             without a dedicated tool), and screenshot_scene to see the result."
                .to_string(),
        );
        info
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

fn parse_asset(s: &str) -> Result<AssetId, McpError> {
    uuid::Uuid::parse_str(s)
        .map(AssetId)
        .map_err(|e| McpError::invalid_params(format!("invalid asset id {s:?}: {e}"), None))
}

fn parse_asset_opt(s: &Option<String>) -> Result<Option<AssetId>, McpError> {
    s.as_deref().map(parse_asset).transpose()
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
