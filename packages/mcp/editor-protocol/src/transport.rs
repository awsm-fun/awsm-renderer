//! The request/response vocabulary exchanged over the editor link between the
//! native MCP server and the in-browser editor, plus the [`WsServerMsg`] /
//! [`WsClientMsg`] frames that carry it over the WebSocket.
//!
//! The link is one ordered WebSocket channel: the server tags each [`Request`]
//! with an `id` and the editor replies with a [`Response`] carrying the same id
//! (so ids correlate request↔response — there is no per-stream identity). Frames
//! are JSON text. Large PNG bytes never ride the link — the editor POSTs them to
//! the server's `/png/<id>` side-channel and returns a small [`PngHandle`] here.

use serde::{Deserialize, Serialize};

use awsm_scene::AssetId;

use crate::{EditorCommand, EditorMode, EditorQuery, QueryResult};

/// Server → editor. What the editor should do / report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Apply a mutation through `EditorController::dispatch`.
    Dispatch(EditorCommand),
    /// Apply a list of mutations in order as one atomic undo step
    /// (`EditorController::dispatch_batch`). One round-trip, one undo entry.
    DispatchBatch(Vec<EditorCommand>),
    /// Run a read-only `EditorQuery`.
    Query(EditorQuery),
    /// Undo the last recorded command (a controller method, not a command).
    Undo,
    /// Redo the last undone command.
    Redo,
    /// PNG of the scene viewport (raw bytes, not a data: URL). Optional
    /// `width`/`height` scale the output (the source is the live viewport;
    /// scaling normalizes size / trims tokens, it doesn't add detail).
    ScenePng {
        width: Option<u32>,
        height: Option<u32>,
    },
    /// PNG of the material-mode preview sphere (raw bytes). Optional output size.
    MaterialPng {
        width: Option<u32>,
        height: Option<u32>,
    },
    /// PNG of a texture asset thumbnail (raw bytes).
    TexturePng(AssetId),
    /// The current workspace mode.
    Mode,
}

/// Editor → server **push** event (the unsolicited channel, distinct from the
/// request/response path). The editor opens a unidirectional stream per event;
/// the server relays it to the connected agent as an MCP logging notification.
/// Carries compile/runtime notices (toasts) and selection changes so an agent
/// can react to what a human (or async work) did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorEvent {
    /// Event kind: `"toast"` | `"selection"`.
    pub kind: String,
    /// Toast severity (`"info"` | `"warning"` | `"error"`) for `kind == "toast"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Human-readable message (toast text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Selected node ids for `kind == "selection"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<String>>,
}

/// Server → browser WebSocket frame.
// `Request` is the dominant variant (one per editor request); boxing it to shrink
// the rarely-used unit variants would just add an allocation to the hot path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WsServerMsg {
    /// Serve this request and reply with [`WsClientMsg::Response`] carrying the
    /// same `id`.
    Request { id: u64, req: Request },
    /// The agent that wants this editor is ambiguous and supplied no pairing
    /// code — the editor should prompt for one and send [`WsClientMsg::Pair`].
    PairingRequired,
    /// This socket's binding was taken over (another tab/agent paired) — the
    /// editor should show itself disconnected.
    Detached,
}

/// Browser → server WebSocket frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WsClientMsg {
    /// Claim a binding to the agent holding this pairing code. Optional first
    /// frame; unnecessary in the unambiguous 1:1 auto-bind case.
    Pair { code: String },
    /// Reply to a [`WsServerMsg::Request`] with the matching `id`.
    Response { id: u64, resp: Response },
    /// An unsolicited editor push event.
    Event(EditorEvent),
}

/// A reference to a PNG the editor uploaded out-of-band. The image bytes do
/// **not** ride the control link — the editor POSTs them to the server's
/// `/png/<id>` HTTP route and returns this small handle here instead. Keeps the
/// link byte-light (a multi-MiB render never blocks small frames).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PngHandle {
    /// Opaque id (uuid v4) the editor minted and POSTed the bytes under; the
    /// server stores them at a temp path keyed by this id.
    pub id: String,
    /// Size of the uploaded PNG in bytes.
    pub byte_len: usize,
    /// Pixel dimensions of the encoded image.
    pub width: u32,
    pub height: u32,
}

/// Editor → server. The reply to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// A mutation / control op succeeded with no payload.
    Ok,
    /// A query result (boxed — `QueryResult::Snapshot` is large, and serde boxes
    /// transparently so the JSON wire form is unchanged).
    Query(Box<QueryResult>),
    /// A rendered PNG, uploaded out-of-band (see [`PngHandle`]); only this handle
    /// crosses the control link.
    Png(PngHandle),
    /// The current workspace mode.
    Mode(EditorMode),
    /// The request failed; the string is a human-readable reason.
    Err(String),
}

#[cfg(test)]
mod wire_roundtrip_tests {
    //! The MCP / `/debug` wire is serde_json: the native server serializes a
    //! `Request`, the editor deserializes it (and vice-versa for `Response`).
    //! A serde rename / tag drift would silently break every agent tool with no
    //! compile error. These round-trip a representative slice of the actively-
    //! used command + query surface and assert ser→de→ser is idempotent (which
    //! also proves deserialize accepts what serialize produced — the failure
    //! mode that bites over the wire). Complex-payload variants (MaterialDef /
    //! Trs) are exercised continuously by the live MCP path; this guards the
    //! envelope + the simple high-traffic verbs against rename drift.

    use super::*;
    use awsm_scene::{MaterialShading, NodeId};

    /// ser → de → ser must be byte-stable, and the round-tripped value must
    /// re-serialize to the SAME JSON (catches asymmetric/lossy serde + a
    /// deserialize that silently drops or renames a field).
    fn assert_roundtrips(req: &Request, label: &str) {
        let j1 =
            serde_json::to_string(req).unwrap_or_else(|e| panic!("{label}: serialize failed: {e}"));
        let back: Request = serde_json::from_str(&j1).unwrap_or_else(|e| {
            panic!("{label}: deserialize of own output failed: {e}\njson={j1}")
        });
        let j2 = serde_json::to_string(&back)
            .unwrap_or_else(|e| panic!("{label}: re-serialize failed: {e}"));
        assert_eq!(j1, j2, "{label}: round-trip not idempotent");
    }

    #[test]
    fn representative_commands_roundtrip() {
        let cmds = [
            (
                "switch_mode",
                EditorCommand::SwitchMode {
                    mode: EditorMode::Animation,
                },
            ),
            (
                "set_selection",
                EditorCommand::SetSelection {
                    ids: vec![NodeId::new()],
                },
            ),
            ("add_clip", EditorCommand::AddClip { id: AssetId::new() }),
            (
                "delete_clip",
                EditorCommand::DeleteClip { id: AssetId::new() },
            ),
            ("set_playhead", EditorCommand::SetPlayhead { t: 0.35 }),
            ("set_playing", EditorCommand::SetPlaying { on: true }),
            ("set_anim_fps", EditorCommand::SetAnimFps { fps: 30 }),
            (
                "set_morph_weight",
                EditorCommand::SetMorphWeight {
                    node: NodeId::new(),
                    index: 2,
                    value: 0.5,
                },
            ),
            (
                "add_builtin_material",
                EditorCommand::AddBuiltinMaterial {
                    id: AssetId::new(),
                    shading: MaterialShading::Pbr,
                },
            ),
            (
                "create_texture",
                EditorCommand::CreateTexture {
                    id: AssetId::new(),
                    data: "AAAA".to_string(),
                    width: Some(1),
                    height: Some(1),
                    format: Some("rgba8".to_string()),
                    linear: true,
                },
            ),
            (
                "patch_kind",
                EditorCommand::PatchKind {
                    id: NodeId::new(),
                    patch: serde_json::json!({"mesh": {"shadow": {"cast": false}}}),
                },
            ),
            (
                "duplicate",
                EditorCommand::Duplicate {
                    id: NodeId::new(),
                    new_id: Some(NodeId::new()),
                },
            ),
            (
                "reset_pose",
                EditorCommand::ResetPose {
                    node: NodeId::new(),
                },
            ),
            (
                "paint_vertex_colors",
                EditorCommand::PaintVertexColors {
                    mesh: AssetId::new(),
                    indices: Vec::new(),
                    color: [1.0, 0.0, 0.0, 1.0],
                    selection: Some(7),
                },
            ),
            (
                "set_builtin_alpha_mode",
                EditorCommand::SetBuiltinAlphaMode {
                    node: NodeId::new(),
                    mode: crate::MaterialAlphaMode::Mask { cutoff: 0.4 },
                },
            ),
            (
                "paint_vertices_where",
                EditorCommand::PaintVerticesWhere {
                    node: NodeId::new(),
                    predicate: crate::query::VertexPredicate::TopPercent {
                        axis: 1,
                        percent: 0.2,
                    },
                    color: [1.0, 0.0, 0.0, 1.0],
                },
            ),
            (
                "transform_vertices_where",
                EditorCommand::TransformVerticesWhere {
                    node: NodeId::new(),
                    predicate: crate::query::VertexPredicate::WithinRadius {
                        center: [0.0, 0.0, 0.0],
                        radius: 1.0,
                    },
                    translation: [0.0, 1.0, 0.0],
                    falloff: 0.5,
                },
            ),
            (
                "add_track",
                EditorCommand::AddTrack {
                    clip: AssetId::new(),
                    target: awsm_scene::animation::TrackTarget::TextureTransform {
                        node: NodeId::new(),
                        slot: awsm_scene::animation::TexSlot::BaseColor,
                        prop: awsm_scene::animation::TexTransformProp::Offset,
                    },
                },
            ),
            (
                "set_particle_emitter",
                EditorCommand::SetParticleEmitter {
                    node: NodeId::new(),
                    spawn_rate: Some(120.0),
                    burst_count: None,
                    max_alive: Some(512),
                    one_shot: Some(false),
                    space: Some(awsm_scene::particle::EmitterSpaceDef::World),
                    shape: Some(awsm_scene::particle::SpawnShapeDef::Cone {
                        angle_radians: 0.5,
                        direction: [0.0, 1.0, 0.0],
                    }),
                    initial_speed: Some([1.0, 3.0]),
                    lifetime: None,
                    size: None,
                    forces: Some(vec![awsm_scene::particle::ForceDef::Gravity {
                        acceleration: [0.0, -9.8, 0.0],
                    }]),
                    color_over_life: None,
                    size_over_life: None,
                    blend: Some(true),
                    texture: Some(Some(AssetId::new())),
                },
            ),
            (
                "set_node_texture_transform",
                EditorCommand::SetNodeTextureTransform {
                    node: NodeId::new(),
                    slot: crate::BuiltinTextureSlot::BaseColor,
                    offset: Some([0.25, 0.0]),
                    scale: Some([2.0, 2.0]),
                    rotation: Some(0.5),
                    flow: Some([0.4, 0.0]),
                    wrap_u: Some(awsm_scene::primitive::TextureWrap::MirroredRepeat),
                    wrap_v: None,
                    uv_set: Some(1),
                },
            ),
            // Track flags + transport (newly typed MCP tools — must round-trip).
            (
                "delete_track",
                EditorCommand::DeleteTrack {
                    clip: AssetId::new(),
                    track: 0,
                },
            ),
            (
                "set_track_mute",
                EditorCommand::SetTrackMute {
                    clip: AssetId::new(),
                    track: 1,
                    mute: true,
                },
            ),
            (
                "set_track_solo",
                EditorCommand::SetTrackSolo {
                    clip: AssetId::new(),
                    track: 1,
                    solo: true,
                },
            ),
            (
                "set_track_sampler",
                EditorCommand::SetTrackSampler {
                    clip: AssetId::new(),
                    track: 0,
                    sampler: awsm_scene::animation::SamplerKind::Cubic,
                },
            ),
            (
                "step_playhead",
                EditorCommand::StepPlayhead {
                    kind: crate::StepKind::Next,
                },
            ),
            // NLA mixer (dispatch_command surface, documented in ANIMATION_AUTHORING).
            ("add_layer", EditorCommand::AddLayer),
            ("delete_layer", EditorCommand::DeleteLayer { layer: 0 }),
            (
                "set_layer_weight",
                EditorCommand::SetLayerWeight {
                    layer: 0,
                    weight: 0.5,
                },
            ),
            (
                "add_strip",
                EditorCommand::AddStrip {
                    layer: 0,
                    clip: AssetId::new(),
                    start: 0.0,
                    len: 2.0,
                },
            ),
        ];
        for (label, cmd) in cmds {
            // The serde tag must be the snake_case `cmd` discriminator.
            let j = serde_json::to_string(&cmd).unwrap();
            assert!(
                j.contains(&format!("\"cmd\":\"{label}\"")),
                "command tag drift: expected cmd=\"{label}\" in {j}",
            );
            assert_roundtrips(&Request::Dispatch(cmd), label);
        }
    }

    #[test]
    fn representative_queries_roundtrip() {
        let queries = [
            ("snapshot", EditorQuery::Snapshot),
            ("frame_globals", EditorQuery::FrameGlobals),
            ("memory_stats", EditorQuery::MemoryStats),
            ("console_logs", EditorQuery::ConsoleLogs { limit: 25 }),
            (
                "get_skin_weights",
                EditorQuery::GetSkinWeights {
                    node: NodeId::new(),
                    indices: vec![0, 1, 2],
                },
            ),
            (
                "get_children",
                EditorQuery::GetChildren {
                    node: NodeId::new(),
                },
            ),
            (
                "get_subtree",
                EditorQuery::GetSubtree {
                    root: Some(NodeId::new()),
                },
            ),
        ];
        for (label, q) in queries {
            assert_roundtrips(&Request::Query(q), label);
        }
    }

    #[test]
    fn envelope_variants_roundtrip() {
        assert_roundtrips(&Request::Undo, "undo");
        assert_roundtrips(&Request::Redo, "redo");
        assert_roundtrips(
            &Request::ScenePng {
                width: Some(900),
                height: Some(600),
            },
            "scene_png",
        );
        assert_roundtrips(&Request::Mode, "mode");
    }

    /// The WebSocket envelope + the out-of-band PNG handle must round-trip too:
    /// the server serializes `WsServerMsg`, the editor deserializes it (and
    /// vice-versa for `WsClientMsg`/`Response`). A drift here breaks the link
    /// itself, not just one tool.
    #[test]
    fn ws_envelope_roundtrips() {
        let server = WsServerMsg::Request {
            id: 7,
            req: Request::ScenePng {
                width: Some(800),
                height: None,
            },
        };
        let j = serde_json::to_string(&server).unwrap();
        let back: WsServerMsg = serde_json::from_str(&j).unwrap();
        assert_eq!(j, serde_json::to_string(&back).unwrap());

        let client = WsClientMsg::Response {
            id: 7,
            resp: Response::Png(PngHandle {
                id: "abc".to_string(),
                byte_len: 1234,
                width: 800,
                height: 600,
            }),
        };
        let j = serde_json::to_string(&client).unwrap();
        let back: WsClientMsg = serde_json::from_str(&j).unwrap();
        assert_eq!(j, serde_json::to_string(&back).unwrap());

        let event = WsClientMsg::Event(EditorEvent {
            kind: "toast".to_string(),
            level: Some("info".to_string()),
            message: Some("hi".to_string()),
            nodes: None,
        });
        let j = serde_json::to_string(&event).unwrap();
        let back: WsClientMsg = serde_json::from_str(&j).unwrap();
        assert_eq!(j, serde_json::to_string(&back).unwrap());

        // Pairing frames (Phase 2 isolation).
        let pair = WsClientMsg::Pair {
            code: "3K9J".to_string(),
        };
        let j = serde_json::to_string(&pair).unwrap();
        let back: WsClientMsg = serde_json::from_str(&j).unwrap();
        assert_eq!(j, serde_json::to_string(&back).unwrap());

        for msg in [WsServerMsg::PairingRequired, WsServerMsg::Detached] {
            let j = serde_json::to_string(&msg).unwrap();
            let back: WsServerMsg = serde_json::from_str(&j).unwrap();
            assert_eq!(j, serde_json::to_string(&back).unwrap());
        }
    }
}
