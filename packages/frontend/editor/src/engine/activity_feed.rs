//! "Watch-it-work" agent-activity feed — a **read-only, informational** layer
//! that lets a human watch the MCP agent build the scene, narrated + spotlit.
//!
//! It rides the *same* inbound `EditorCommand` stream the editor already
//! receives over [`remote`](crate::remote) (which today only drives the 🤖
//! idle/working presence pulse). When a command is dispatched on that stream,
//! [`narrate`] maps it to a short human phrase + a UI *focus target*, pushes a
//! feed entry, and arms a ~1s panel highlight.
//!
//! Strictly additive + silent when no agent is connected (nothing calls
//! [`narrate`], so the feed stays empty and the app shell hides it). It never
//! mutates editor state — narration is derived from the command alone.

use std::cell::Cell;

use awsm_editor_protocol::{EditorCommand, InsertSpec};
use awsm_editor_protocol::{LightKind, Modifier, PrimitiveShape};
use awsm_web_shared::prelude::{Mutable, MutableVec};
use wasm_bindgen_futures::spawn_local;

/// Cap on the retained feed — older entries fall off the front. Keeps the strip
/// compact + memory bounded under a long agent session.
const MAX_ENTRIES: usize = 50;

/// How long a panel highlight lingers after a command lands (the §2 "~1s"
/// transient spotlight). Re-armed on each new command, so a burst keeps the
/// active panel lit rather than flickering.
const HIGHLIGHT_MS: u32 = 1000;

/// Which top-level region of the editor chrome a command "happens in". The app
/// shell pulses the matching panel for [`HIGHLIGHT_MS`] when a command lands.
///
/// Kept deliberately coarse (one accent per top-level panel) — wiring finer,
/// per-widget highlight anchors is a follow-on (see the module-level deferral
/// note in `app.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    /// The scene Outliner (left rail) — node add / delete / reparent / rename.
    Outliner,
    /// The viewport (center) — mesh geometry, vertex paint/move, camera, import.
    Viewport,
    /// The right inspector rail — modifiers, material + light params, props.
    Inspector,
    /// No specific panel (mode switch, project-level, transport) — feed only.
    None,
}

/// One narration entry shown in the feed strip.
#[derive(Debug, Clone)]
pub struct FeedEntry {
    /// The human phrase, e.g. `"added a box"`. Rendered as "🤖 {phrase}".
    pub phrase: String,
}

thread_local! {
    /// The reactive feed (oldest → newest), capped at [`MAX_ENTRIES`]. The app
    /// shell renders this as the auto-scrolling narration strip.
    static FEED: MutableVec<FeedEntry> = MutableVec::new();
    /// The currently-spotlit panel (or `None`). The app shell pulses it. Cleared
    /// after [`HIGHLIGHT_MS`] unless a newer command re-arms it.
    static FOCUS: Mutable<Option<FocusTarget>> = Mutable::new(None);
    /// Bumped on each highlight; lets a queued clear cancel itself when a newer
    /// command re-arms the spotlight (so it stays lit through a burst).
    static FOCUS_GEN: Cell<u64> = const { Cell::new(0) };
    /// User toggle: when `false`, [`narrate`] is a no-op (no new entries, no
    /// spotlight) so the feed never crowds the screen. Bound to a Settings
    /// toggle; defaults on. Session-only (editor chrome, not project state).
    static ENABLED: Mutable<bool> = Mutable::new(true);
}

/// The reactive feed for the app shell to render.
pub fn feed() -> MutableVec<FeedEntry> {
    FEED.with(|f| f.clone())
}

/// The reactive spotlight target for the app shell to pulse.
pub fn focus() -> Mutable<Option<FocusTarget>> {
    FOCUS.with(|f| f.clone())
}

/// The reactive "narrate agent activity" toggle, for a Settings checkbox. When
/// flipped off it also clears any existing entries + spotlight so the screen
/// frees up immediately.
pub fn enabled() -> Mutable<bool> {
    let m = ENABLED.with(|e| e.clone());
    // Lazily wire the side effect once: turning the toggle off clears the strip.
    thread_local!(static WIRED: Cell<bool> = const { Cell::new(false) });
    if !WIRED.with(|w| w.replace(true)) {
        let sig = m.clone();
        spawn_local(async move {
            use futures_signals::signal::SignalExt;
            sig.signal()
                .for_each(|on| {
                    if !on {
                        clear();
                    }
                    async {}
                })
                .await;
        });
    }
    m
}

/// Empty the feed + drop the spotlight (the "clear" button). Also the immediate
/// effect of toggling the feed off.
pub fn clear() {
    FEED.with(|f| f.lock_mut().clear());
    FOCUS.with(|f| f.set(None));
}

/// Narrate one inbound agent command: push a feed entry + arm the panel
/// spotlight. Called from the place inbound commands are dispatched in
/// `remote.rs`. A [`Batch`](EditorCommand::Batch) narrates its first meaningful
/// child (the agent's typical "do one logical thing" batch) so the feed reads
/// as one action rather than spamming sub-steps.
pub fn narrate(cmd: &EditorCommand) {
    let cmd = match cmd {
        EditorCommand::Batch(cmds) => pick_batch_child(cmds).unwrap_or(cmd),
        other => other,
    };
    emit(cmd);
}

/// Narrate an MCP `dispatch_batch` (an explicit list, not wrapped in a `Batch`
/// command): surfaces the same single "meaningful child" phrase. Avoids cloning
/// the whole command list just to wrap it.
pub fn narrate_batch(cmds: &[EditorCommand]) {
    if let Some(cmd) = pick_batch_child(cmds) {
        emit(cmd);
    }
}

fn emit(cmd: &EditorCommand) {
    if !ENABLED.with(|e| e.get()) {
        return; // feed muted by the user
    }
    let Some((target, phrase)) = describe(cmd) else {
        return; // purely-transient/no-op chatter we don't surface
    };
    push_entry(phrase);
    if target != FocusTarget::None {
        arm_focus(target);
    }
}

/// Pick the child of a batch worth narrating (skip leading transient
/// selection/mode noise the agent brackets its real edit with). Falls back to
/// the batch's first command.
fn pick_batch_child(cmds: &[EditorCommand]) -> Option<&EditorCommand> {
    cmds.iter()
        .find(|c| !matches!(c, EditorCommand::Batch(_)) && describe_non_transient(c))
        .or_else(|| cmds.first())
}

/// Whether a command resolves to a feed entry that isn't a low-signal transient
/// (used to pick the meaningful child of a batch).
fn describe_non_transient(cmd: &EditorCommand) -> bool {
    !matches!(
        cmd,
        EditorCommand::SetSelection { .. }
            | EditorCommand::SetAssetSelection { .. }
            | EditorCommand::SetCurrentMaterial { .. }
            | EditorCommand::SetCurrentClip { .. }
            | EditorCommand::SetAnimSelection { .. }
            | EditorCommand::SetVertexSelection { .. }
            | EditorCommand::SwitchMode { .. }
    )
}

fn push_entry(phrase: String) {
    FEED.with(|f| {
        let mut v = f.lock_mut();
        if v.len() >= MAX_ENTRIES {
            v.remove(0);
        }
        v.push_cloned(FeedEntry { phrase });
    });
}

/// Light the spotlight on `target` and schedule it to clear after
/// [`HIGHLIGHT_MS`], unless a newer command re-arms it first.
fn arm_focus(target: FocusTarget) {
    focus().set(Some(target));
    let generation = FOCUS_GEN.with(|g| {
        let n = g.get().wrapping_add(1);
        g.set(n);
        n
    });
    spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(HIGHLIGHT_MS).await;
        // Only clear if no newer command re-armed the spotlight since.
        if FOCUS_GEN.with(|g| g.get()) == generation {
            focus().set(None);
        }
    });
}

/// The command → (focus target, human phrase) table. Returns `None` for purely
/// transient view chatter we deliberately don't narrate (selection, camera
/// orbit, playhead scrubbing) so the feed stays a readable *story* of edits.
fn describe(cmd: &EditorCommand) -> Option<(FocusTarget, String)> {
    use FocusTarget as F;
    let pair = match cmd {
        // ── Scene graph (Outliner) ────────────────────────────────────────
        EditorCommand::Insert { spec, .. } => {
            (F::Outliner, format!("added a {}", insert_label(spec)))
        }
        EditorCommand::InsertTree { .. } => (F::Outliner, "restored a node".to_string()),
        EditorCommand::Delete { .. } => (F::Outliner, "deleted a node".to_string()),
        EditorCommand::Duplicate { .. } => (F::Outliner, "duplicated a node".to_string()),
        EditorCommand::Reparent { .. } => (F::Outliner, "reparented a node".to_string()),
        EditorCommand::Rename { name, .. } => (
            F::Outliner,
            format!("renamed a node to \u{201c}{name}\u{201d}"),
        ),
        EditorCommand::SetVisible { visible, .. } => (
            F::Outliner,
            if *visible {
                "showed a node"
            } else {
                "hid a node"
            }
            .to_string(),
        ),
        EditorCommand::SetLocked { locked, .. } => (
            F::Outliner,
            if *locked {
                "locked a node"
            } else {
                "unlocked a node"
            }
            .to_string(),
        ),
        EditorCommand::SetPrefab { .. } => (F::Outliner, "set a prefab root".to_string()),
        EditorCommand::NewProject => (F::Outliner, "started a new project".to_string()),

        // ── Transforms + geometry (Viewport) ──────────────────────────────
        EditorCommand::SetTransform { .. } => (F::Viewport, "moved a node".to_string()),
        EditorCommand::SetMeshData { .. } => (F::Viewport, "edited a mesh".to_string()),
        EditorCommand::SetVertexPositions { indices, .. } => {
            (F::Viewport, format!("moved {}", verts(indices.len())))
        }
        EditorCommand::SoftTransformVertices { indices, .. } => {
            (F::Viewport, format!("soft-moved {}", verts(indices.len())))
        }
        EditorCommand::PaintVertexColors { indices, .. } => {
            (F::Viewport, format!("painted {}", verts(indices.len())))
        }
        EditorCommand::SetVertexNormals { indices, .. } => (
            F::Viewport,
            format!("set normals on {}", verts(indices.len())),
        ),
        EditorCommand::SetVertexOverrides { .. } => (F::Viewport, "edited vertex data".to_string()),
        EditorCommand::CollapseMeshStack { .. } => (F::Viewport, "baked a mesh".to_string()),
        EditorCommand::BakeAll {} => (F::Viewport, "baked all meshes".to_string()),
        EditorCommand::DropSkinning { .. } => (F::Viewport, "dropped skinning".to_string()),
        EditorCommand::ConvertToEditableMesh { .. } => {
            (F::Viewport, "made a mesh editable".to_string())
        }
        EditorCommand::ImportModelFromUrl { .. } | EditorCommand::ImportModelFromFile { .. } => {
            (F::Viewport, "imported a model".to_string())
        }
        EditorCommand::LoadProjectFromUrl { .. } => (F::Viewport, "loaded a project".to_string()),

        // ── Modifiers + materials + light params (Inspector) ──────────────
        EditorCommand::AddModifier { modifier, .. } => (
            F::Inspector,
            format!("added a {} modifier", modifier_label(modifier)),
        ),
        EditorCommand::SetModifier { .. } => (F::Inspector, "tweaked a modifier".to_string()),
        EditorCommand::RemoveModifier { .. } => (F::Inspector, "removed a modifier".to_string()),
        EditorCommand::SetMeshModifiers { stack, .. } => (
            F::Inspector,
            match stack.modifiers.last() {
                Some(m) => format!("added a {} modifier", modifier_label(m)),
                None => "set modifiers".to_string(),
            },
        ),
        EditorCommand::SetKind { .. } => (F::Inspector, "edited a node".to_string()),
        EditorCommand::AssignMaterial { material, .. } => (
            F::Inspector,
            if material.is_some() {
                "assigned a material"
            } else {
                "cleared a material"
            }
            .to_string(),
        ),
        EditorCommand::CopyMaterialInstance { .. } => {
            (F::Inspector, "copied material settings".to_string())
        }
        EditorCommand::AddMaterialAsset { .. }
        | EditorCommand::AddBuiltinMaterial { .. }
        | EditorCommand::AddCustomMaterial { .. } => {
            (F::Inspector, "created a material".to_string())
        }
        EditorCommand::DeleteCustomMaterial { .. } => {
            (F::Inspector, "deleted a material".to_string())
        }
        EditorCommand::RegisterMaterial { .. } => (F::Inspector, "compiled a material".to_string()),
        EditorCommand::SetCustomMaterialWgsl { .. } => {
            (F::Inspector, "edited a shader".to_string())
        }
        EditorCommand::SetCustomMaterialAlphaMode { .. } => {
            (F::Inspector, "set a material's alpha mode".to_string())
        }
        EditorCommand::SetCustomMaterialDoubleSided { .. } => {
            (F::Inspector, "set a material double-sided".to_string())
        }
        EditorCommand::SetCustomMaterialDebugColor { .. } => {
            (F::Inspector, "set a material color".to_string())
        }
        EditorCommand::SetCustomMaterialLayout { .. } => {
            (F::Inspector, "edited a material layout".to_string())
        }
        EditorCommand::SetCustomMaterialShaderIncludes { .. }
        | EditorCommand::SetCustomMaterialFragmentInputs { .. } => {
            (F::Inspector, "edited material inputs".to_string())
        }
        EditorCommand::SetMaterialUniform { .. } | EditorCommand::SetBuiltinParam { .. } => {
            (F::Inspector, "tuned a material".to_string())
        }
        EditorCommand::SetLightParam { .. } => (F::Inspector, "adjusted a light".to_string()),
        EditorCommand::SetBuiltinTexture { .. } | EditorCommand::SetMaterialTexture { .. } => {
            (F::Inspector, "bound a texture".to_string())
        }
        EditorCommand::ImportTextureFromUrl { .. } | EditorCommand::AddTextureAsset { .. } => {
            (F::Inspector, "added a texture".to_string())
        }
        EditorCommand::DeleteAsset { .. } | EditorCommand::RestoreAsset { .. } => {
            (F::Inspector, "removed an asset".to_string())
        }
        EditorCommand::SetEnvironment { .. } => (F::Inspector, "set the environment".to_string()),

        // ── Animation ─────────────────────────────────────────────────────
        EditorCommand::AddClip { .. } => (F::None, "added a clip".to_string()),
        EditorCommand::DuplicateClip { .. } => (F::None, "duplicated a clip".to_string()),
        EditorCommand::DeleteClip { .. } => (F::None, "deleted a clip".to_string()),
        EditorCommand::RenameClip { name, .. } => {
            (F::None, format!("renamed a clip to \u{201c}{name}\u{201d}"))
        }
        EditorCommand::AddTrack { .. } => (F::None, "added a track".to_string()),
        EditorCommand::DeleteTrack { .. } | EditorCommand::RestoreTrack { .. } => {
            (F::None, "removed a track".to_string())
        }
        EditorCommand::AddKeyframe { .. } => (F::None, "added a keyframe".to_string()),
        EditorCommand::DeleteKeyframe { .. } | EditorCommand::InsertKeyframe { .. } => {
            (F::None, "removed a keyframe".to_string())
        }
        EditorCommand::SetKeyframe { .. } => (F::None, "edited a keyframe".to_string()),
        EditorCommand::AddLayer => (F::None, "added an animation layer".to_string()),
        EditorCommand::AddStrip { .. } => (F::None, "added an animation strip".to_string()),

        // ── Transient view/camera/transport chatter — not narrated ────────
        EditorCommand::SwitchMode { .. }
        | EditorCommand::SetSelection { .. }
        | EditorCommand::SetVertexSelection { .. }
        | EditorCommand::SetAssetSelection { .. }
        | EditorCommand::SetCurrentMaterial { .. }
        | EditorCommand::SnapCameraToAxis { .. }
        | EditorCommand::ResetCamera
        | EditorCommand::SetCameraOrbit { .. }
        | EditorCommand::SetCameraProjection { .. }
        | EditorCommand::FrameNode { .. }
        | EditorCommand::SetFrameTime { .. }
        | EditorCommand::ClearFrameTime
        | EditorCommand::SetCurrentClip { .. }
        | EditorCommand::SetPlayhead { .. }
        | EditorCommand::SetPlaying { .. }
        | EditorCommand::StepPlayhead { .. }
        | EditorCommand::SetAnimFps { .. }
        | EditorCommand::SetSoloRoot { .. }
        | EditorCommand::SetAnimSelection { .. }
        | EditorCommand::SetAnimView { .. } => return None,

        // ── Sensible default for any unmapped / future command ─────────────
        other => (F::None, format!("ran {}", other.label().to_lowercase())),
    };
    Some(pair)
}

/// `"1 vert"` / `"240 verts"`.
fn verts(n: usize) -> String {
    if n == 1 {
        "1 vert".to_string()
    } else {
        format!("{n} verts")
    }
}

/// A short shape/kind word for an `Insert` ("box", "sphere", "directional
/// light", "camera", …) — best-effort from the spec alone.
fn insert_label(spec: &InsertSpec) -> &'static str {
    match spec {
        InsertSpec::Empty => "group",
        InsertSpec::Light(LightKind::Directional) => "directional light",
        InsertSpec::Light(LightKind::Point) => "point light",
        InsertSpec::Light(LightKind::Spot) => "spot light",
        InsertSpec::Camera => "camera",
        InsertSpec::CollisionBox => "box collider",
        InsertSpec::CollisionSphere => "sphere collider",
        InsertSpec::CollisionCapsule => "capsule collider",
        InsertSpec::CollisionCylinder => "cylinder collider",
        InsertSpec::CollisionCone => "cone collider",
        InsertSpec::CollisionEllipsoid => "ellipsoid collider",
        InsertSpec::Primitive(shape) => primitive_label(shape),
        InsertSpec::Curve => "curve",
        InsertSpec::Line => "line",
        InsertSpec::Sprite => "sprite",
        InsertSpec::Particle => "particle emitter",
        InsertSpec::Decal => "decal",
        InsertSpec::Sweep => "sweep",
        InsertSpec::Instances => "instances",
        InsertSpec::Mesh => "mesh",
    }
}

fn primitive_label(shape: &PrimitiveShape) -> &'static str {
    match shape {
        PrimitiveShape::Plane { .. } => "plane",
        PrimitiveShape::Box { .. } => "box",
        PrimitiveShape::Sphere { .. } => "sphere",
        PrimitiveShape::Cylinder { .. } => "cylinder",
        PrimitiveShape::Cone { .. } => "cone",
        PrimitiveShape::Torus { .. } => "torus",
    }
}

fn modifier_label(m: &Modifier) -> &'static str {
    match m {
        Modifier::Taper { .. } => "taper",
        Modifier::Twist { .. } => "twist",
        Modifier::Bend { .. } => "bend",
        Modifier::Inflate { .. } => "inflate",
        Modifier::Spherify { .. } => "spherify",
        Modifier::Roughen { .. } => "roughen",
        Modifier::Subdivide { .. } => "subdivide",
        Modifier::Smooth { .. } => "smooth",
        Modifier::Mirror { .. } => "mirror",
        Modifier::Array { .. } => "array",
        Modifier::Displace { .. } => "displace",
    }
}
