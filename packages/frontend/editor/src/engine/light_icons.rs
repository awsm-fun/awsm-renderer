//! Viewport **light icons** — a pickable bulb glyph at every light node, plus a
//! direction ray for directional / spot lights.
//!
//! Lights carry no renderable geometry, so without a proxy they can't be clicked
//! in the viewport at all (you'd have to find them in the outliner). This renders
//! one HUD **bulb** (a small glass sphere + screw base) per light — always on top,
//! like the transform gizmo — and, for lights that have a direction (directional
//! and spot), a HUD **ray** (shaft + arrowhead) pointing the way the light shines.
//! A pick on either resolves to a selection of that light; once selected the
//! transform gizmo appears and the light moves / rotates normally.
//!
//! Mirrors `gizmo.rs` / `curve_handles.rs`: the icon set lives in a thread-local
//! so the render loop (per-frame re-anchor + screen-constant zoom) and the canvas
//! pick handler can both reach it. The glyph geometry is built once per icon (only
//! rebuilt when the live light set / kinds change); every frame just re-anchors
//! the transforms and rescales them to a constant pixel size.
//!
//! The per-frame CPU gather buffers (the light-id list, the pose snapshot, and
//! the signature) are REUSED across frames via a thread-local `Scratch` (cleared,
//! capacity retained) so an on-screen light-icon set allocates nothing at steady
//! state — the same zero-alloc rationale as `skeleton_viz`'s `Scratch`.

use std::cell::RefCell;

use awsm_meshgen::{cone_mesh, cylinder_mesh, sphere_mesh, MeshData};
use awsm_renderer::camera::CameraMatrices;
use awsm_renderer::materials::{unlit::UnlitMaterial, Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use glam::{Quat, Vec3};

use crate::engine::bridge::bridge;
use crate::engine::scene::{LightConfig, NodeId, NodeKind};

thread_local! {
    static ICONS: RefCell<Option<LightIcons>> = const { RefCell::new(None) };
    /// Per-frame gather buffers, reused across frames (cleared, capacity
    /// retained) so an on-screen light-icon set does zero heap allocation at
    /// steady state. Mirrors `skeleton_viz`'s `Scratch`.
    static SCRATCH: RefCell<Scratch> = const { RefCell::new(Scratch::new()) };
}

/// Reused per-frame working set for the light-icon rebuild/re-anchor.
struct Scratch {
    /// Live light node ids (collected so the `light_node_ids` lock releases
    /// before the `nodes` lock is taken — avoids holding both at once).
    light_ids: Vec<NodeId>,
    /// One snapshot per light, sorted by node id for a stable icon order.
    lights: Vec<LightSnapshot>,
    /// `(node_id, has_ray)` per light — compared against the icon set's
    /// signature to decide rebuild-vs-reanchor.
    sig: Vec<(NodeId, bool)>,
}

impl Scratch {
    const fn new() -> Self {
        Self {
            light_ids: Vec::new(),
            lights: Vec::new(),
            sig: Vec::new(),
        }
    }
    fn clear(&mut self) {
        self.light_ids.clear();
        self.lights.clear();
        self.sig.clear();
    }
}

/// Desired on-screen radius of the bulb glass in CSS pixels (the whole glyph,
/// base + ray, scales with it).
const DESIRED_PIXEL_RADIUS: f32 = 17.0;

/// Which directional cue a light's icon draws.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LightShape {
    Point,
    Directional,
    Spot,
}

impl LightShape {
    /// Directional + spot get a ray; a point light shines everywhere.
    fn has_ray(self) -> bool {
        !matches!(self, LightShape::Point)
    }
}

/// One light's live pose + kind, snapshotted per frame.
struct LightSnapshot {
    node_id: NodeId,
    pos: Vec3,
    /// World-space rotation — orients the direction ray (light shines along
    /// local -Z, the glTF punctual-light convention).
    rot: Quat,
    shape: LightShape,
}

/// GPU state for one light's icon.
struct LightIcon {
    node_id: NodeId,
    bulb_mesh: MeshKey,
    bulb_tf: TransformKey,
    ray: Option<(MeshKey, TransformKey)>,
    world_pos: Vec3,
}

struct LightIcons {
    icons: Vec<LightIcon>,
    /// Stable identity of the current icon set — `(node_id, has_ray)` per light,
    /// sorted. A change means geometry must be rebuilt; otherwise we only
    /// re-anchor transforms.
    signature: Vec<(NodeId, bool)>,
    bulb_material: Option<MaterialKey>,
    ray_material: Option<MaterialKey>,
    visible: bool,
}

/// Initialise the icon set. Call once after the renderer + bridge are ready.
pub fn init() {
    ICONS.with(|c| {
        *c.borrow_mut() = Some(LightIcons {
            icons: Vec::new(),
            signature: Vec::new(),
            bulb_material: None,
            ray_material: None,
            visible: true,
        });
    });
}

// ── Glyph geometry ───────────────────────────────────────────────────────────

/// Append `other` into `into`, rotating + translating its positions / normals
/// and offsetting its indices. Lets a glyph be composed from primitives that are
/// each authored around the origin on their own axis.
fn append(into: &mut MeshData, other: &MeshData, rot: Quat, translate: Vec3) {
    let base = into.positions.len() as u32;
    let count = other.positions.len();
    for p in &other.positions {
        let v = rot * Vec3::from_array(*p) + translate;
        into.positions.push(v.to_array());
    }
    // Keep every per-vertex stream the same length as `positions` (a mismatch
    // trips the mesh uploader). Missing source attributes fill with a default.
    {
        let dst = into.normals.get_or_insert_with(Vec::new);
        match &other.normals {
            Some(src) => dst.extend(
                src.iter()
                    .map(|n| (rot * Vec3::from_array(*n)).normalize_or_zero().to_array()),
            ),
            None => dst.extend(std::iter::repeat_n([0.0, 1.0, 0.0], count)),
        }
    }
    {
        // Procedural icon meshes are single-UV — merge into set 0.
        if into.uvs.is_empty() {
            into.uvs.push(Vec::new());
        }
        let dst = &mut into.uvs[0];
        match other.uvs.first() {
            Some(src) => dst.extend_from_slice(src),
            None => dst.extend(std::iter::repeat_n([0.0, 0.0], count)),
        }
    }
    into.indices.extend(other.indices.iter().map(|i| i + base));
}

/// A lightbulb: a glass sphere centred at the origin (the light position) with a
/// short cylindrical screw base hanging below it.
fn bulb_mesh_data() -> MeshData {
    let mut bulb = sphere_mesh(0.5, 16, 10);
    bulb.normals.get_or_insert_with(Vec::new);
    let base = cylinder_mesh(0.22, 0.42, 12);
    append(&mut bulb, &base, Quat::IDENTITY, Vec3::new(0.0, -0.62, 0.0));
    bulb
}

/// A direction ray pointing along local -Z (the light's forward): a thin shaft
/// starting just outside the bulb, capped with a cone arrowhead.
fn ray_mesh_data() -> MeshData {
    // Primitives are authored along +Y; rotate that onto -Z.
    let y_to_neg_z = Quat::from_rotation_arc(Vec3::Y, Vec3::NEG_Z);
    let mut ray = MeshData {
        positions: Vec::new(),
        normals: Some(Vec::new()),
        uvs: vec![Vec::new()],
        colors: None,
        indices: Vec::new(),
    };
    // Shaft: 1.4 long, centred → spans local z [-0.7, 0.7]; push it out to start
    // beyond the bulb sphere (r=0.5) at z=-0.65 and run to z=-2.05.
    let shaft = cylinder_mesh(0.06, 1.4, 8);
    append(&mut ray, &shaft, y_to_neg_z, Vec3::new(0.0, 0.0, -1.35));
    // Arrowhead at the far end, apex pointing -Z.
    let head = cone_mesh(0.17, 0.45, 12);
    append(&mut ray, &head, y_to_neg_z, Vec3::new(0.0, 0.0, -2.28));
    ray
}

fn to_raw(mesh: MeshData) -> RawMeshData {
    RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uvs: mesh.uvs.into_iter().next(),
        uvs1: None,
        colors: mesh.colors,
        indices: mesh.indices,
        ..Default::default()
    }
}

impl LightIcons {
    /// Ensure both glyph materials exist (bright Unlit so they read as flat
    /// markers regardless of scene lighting / a blown-out IBL — the same
    /// rationale as the point-handle gizmo).
    fn ensure_materials(&mut self, renderer: &mut AwsmRenderer) {
        if self.bulb_material.is_none() {
            // Warm bulb-white, HDR-bright so it survives tonemapping.
            let mut mat = UnlitMaterial::new(MaterialAlphaMode::Opaque, false);
            mat.base_color_factor = [9.0, 7.5, 3.0, 1.0];
            self.bulb_material = Some(renderer.materials.insert(
                Material::Unlit(mat),
                &renderer.textures,
                &renderer.dynamic_materials,
                &renderer.extras_pool,
            ));
        }
        if self.ray_material.is_none() {
            // Dimmer warm amber for the direction ray so the bulb stays the
            // brightest, primary mark.
            let mut mat = UnlitMaterial::new(MaterialAlphaMode::Opaque, false);
            mat.base_color_factor = [5.0, 3.6, 1.2, 1.0];
            self.ray_material = Some(renderer.materials.insert(
                Material::Unlit(mat),
                &renderer.textures,
                &renderer.dynamic_materials,
                &renderer.extras_pool,
            ));
        }
    }

    /// Tear down every icon mesh + transform.
    fn clear(&mut self, renderer: &mut AwsmRenderer) {
        for icon in self.icons.drain(..) {
            renderer.remove_mesh(icon.bulb_mesh);
            renderer.transforms.remove(icon.bulb_tf);
            if let Some((mesh, tf)) = icon.ray {
                renderer.remove_mesh(mesh);
                renderer.transforms.remove(tf);
            }
        }
        self.signature.clear();
    }

    /// Rebuild the icon set to match `lights` (geometry per light — only called
    /// when the light set / kinds change).
    fn rebuild(&mut self, renderer: &mut AwsmRenderer, lights: &[LightSnapshot]) {
        self.clear(renderer);
        self.ensure_materials(renderer);
        let bulb_mat = self.bulb_material.unwrap();
        let ray_mat = self.ray_material.unwrap();

        for l in lights {
            let bulb_tf = renderer.transforms.insert(
                Transform {
                    translation: l.pos,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::ONE,
                },
                None,
            );
            let Ok(bulb_mesh) = renderer.add_raw_mesh(to_raw(bulb_mesh_data()), bulb_tf, bulb_mat)
            else {
                renderer.transforms.remove(bulb_tf);
                continue;
            };
            let _ = renderer.set_mesh_hud(bulb_mesh, true);
            let _ = renderer.set_mesh_hidden(bulb_mesh, !self.visible);

            let ray = if l.shape.has_ray() {
                let ray_tf = renderer.transforms.insert(
                    Transform {
                        translation: l.pos,
                        rotation: l.rot,
                        scale: Vec3::ONE,
                    },
                    None,
                );
                match renderer.add_raw_mesh(to_raw(ray_mesh_data()), ray_tf, ray_mat) {
                    Ok(ray_mesh) => {
                        let _ = renderer.set_mesh_hud(ray_mesh, true);
                        let _ = renderer.set_mesh_hidden(ray_mesh, !self.visible);
                        Some((ray_mesh, ray_tf))
                    }
                    Err(_) => {
                        renderer.transforms.remove(ray_tf);
                        None
                    }
                }
            } else {
                None
            };

            self.icons.push(LightIcon {
                node_id: l.node_id,
                bulb_mesh,
                bulb_tf,
                ray,
                world_pos: l.pos,
            });
        }
        self.signature = lights
            .iter()
            .map(|l| (l.node_id, l.shape.has_ray()))
            .collect();
    }

    /// Per-frame: re-anchor each icon to its light's pose and rescale the whole
    /// glyph to a constant pixel size. Assumes `lights` matches `self.icons` by
    /// index (the caller rebuilds first when the signature changes).
    fn reanchor(
        &mut self,
        renderer: &mut AwsmRenderer,
        lights: &[LightSnapshot],
        camera: &CameraMatrices,
    ) {
        let (_, viewport_y) = renderer.gpu.canvas_size(false);
        if viewport_y <= 0.0 {
            return;
        }
        let desired_ndc = 2.0 * DESIRED_PIXEL_RADIUS / viewport_y as f32;
        let proj11 = camera.projection.y_axis.y;
        let is_ortho = camera.is_orthographic();

        for (icon, l) in self.icons.iter_mut().zip(lights) {
            icon.world_pos = l.pos;
            let depth = if is_ortho {
                1.0
            } else {
                (l.pos - camera.position_world).length()
            };
            let scale = (desired_ndc * depth / proj11).max(0.001);
            let _ = renderer.transforms.set_local(
                icon.bulb_tf,
                Transform {
                    translation: l.pos,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::splat(scale),
                },
            );
            if let Some((_, ray_tf)) = icon.ray {
                let _ = renderer.transforms.set_local(
                    ray_tf,
                    Transform {
                        translation: l.pos,
                        rotation: l.rot,
                        scale: Vec3::splat(scale),
                    },
                );
            }
        }
    }

    fn show(&mut self, renderer: &mut AwsmRenderer, visible: bool) {
        self.visible = visible;
        for icon in &self.icons {
            let _ = renderer.set_mesh_hidden(icon.bulb_mesh, !visible);
            if let Some((mesh, _)) = icon.ray {
                let _ = renderer.set_mesh_hidden(mesh, !visible);
            }
        }
    }
}

/// Per-frame: re-anchor one bulb (+ ray) on every light node + keep them a fixed
/// pixel size. Called from the render loop after world transforms are derived.
pub fn per_frame_update(renderer: &mut AwsmRenderer, camera_matrices: &CameraMatrices) {
    // Honor the Settings → "Light gizmos" toggle: hide the markers when off.
    if !crate::controller::controller().settings.light_gizmos.get() {
        ICONS.with(|c| {
            if let Some(icons) = c.borrow_mut().as_mut() {
                if icons.visible {
                    icons.show(renderer, false);
                }
            }
        });
        return;
    }

    // Snapshot the live light nodes into the reused scratch: world pose
    // (position + rotation) + kind. Sorted by node id so the icon set has a
    // stable order (the bridge tracks ids in a HashSet, whose iteration order
    // is not stable frame-to-frame).
    SCRATCH.with(|s| {
        let s = &mut *s.borrow_mut();
        s.clear();
        // Disjoint field borrows so the gather loop can read `light_ids` while
        // writing `lights`.
        let Scratch {
            light_ids,
            lights,
            sig,
        } = s;

        {
            let b = bridge();
            // Collect ids first so the `light_node_ids` lock releases before the
            // `nodes` lock is taken (never hold both at once).
            light_ids.extend(b.light_node_ids.lock().unwrap().iter().copied());
            let nodes = b.nodes.lock().unwrap();
            for id in light_ids.iter().copied() {
                if let Some(entry) = nodes.get(&id) {
                    let Ok(world) = renderer.transforms.get_world(entry.transform_key) else {
                        continue;
                    };
                    let shape = match entry.node.kind.get_cloned() {
                        NodeKind::Light(LightConfig::Directional { .. }) => LightShape::Directional,
                        NodeKind::Light(LightConfig::Spot { .. }) => LightShape::Spot,
                        NodeKind::Light(LightConfig::Point { .. }) => LightShape::Point,
                        _ => continue,
                    };
                    let (_s, rot, trans) = world.to_scale_rotation_translation();
                    lights.push(LightSnapshot {
                        node_id: id,
                        pos: trans,
                        rot,
                        shape,
                    });
                }
            }
        }
        lights.sort_by_key(|l| l.node_id.0);

        ICONS.with(|c| {
            let mut guard = c.borrow_mut();
            let Some(icons) = guard.as_mut() else {
                return;
            };
            if lights.is_empty() {
                if !icons.icons.is_empty() {
                    icons.clear(renderer);
                }
                return;
            }
            sig.extend(lights.iter().map(|l| (l.node_id, l.shape.has_ray())));
            if *sig != icons.signature {
                icons.rebuild(renderer, lights);
            }
            icons.reanchor(renderer, lights, camera_matrices);
            if !icons.visible {
                icons.show(renderer, true);
            }
        });
    });
}

/// If `mesh_key` (or a near-miss at the cursor) is a light icon — the bulb OR its
/// direction ray — return the light node it represents. Checked by the canvas
/// pick handler before the regular scene-mesh → node lookup, so clicking a
/// light's glyph selects it.
pub fn try_pick(renderer: &AwsmRenderer, mesh_key: MeshKey, x: i32, y: i32) -> Option<NodeId> {
    ICONS.with(|c| {
        let guard = c.borrow();
        let icons = guard.as_ref()?;
        // Exact mesh hit (bulb or ray).
        for icon in &icons.icons {
            if icon.bulb_mesh == mesh_key || icon.ray.map(|(m, _)| m == mesh_key).unwrap_or(false) {
                return Some(icon.node_id);
            }
        }
        // Cursor-tolerance fallback: nearest bulb whose screen projection is
        // within a few pixels (handles between-pixel clicks / overlapping
        // glyphs), mirroring the point-handle picker.
        nearest_bulb(renderer, icons, x, y)
    })
}

/// Nearest bulb to the cursor within a small pixel tolerance.
fn nearest_bulb(renderer: &AwsmRenderer, icons: &LightIcons, x: i32, y: i32) -> Option<NodeId> {
    if !icons.visible || icons.icons.is_empty() {
        return None;
    }
    let matrices = renderer.camera.last_matrices.as_ref()?;
    let (width, height) = renderer.gpu.canvas_size(false);
    let (vw, vh) = (width as f32, height as f32);
    if vw <= 0.0 || vh <= 0.0 {
        return None;
    }
    let view_proj = matrices.projection * matrices.view;
    let cursor = glam::Vec2::new(x as f32, y as f32);
    const TOL: f32 = 12.0;
    let mut best: Option<(NodeId, f32)> = None;
    for icon in &icons.icons {
        let clip = view_proj * icon.world_pos.extend(1.0);
        if clip.w <= 0.0 {
            continue;
        }
        let px = (clip.x / clip.w * 0.5 + 0.5) * vw;
        let py = (1.0 - (clip.y / clip.w * 0.5 + 0.5)) * vh;
        let dist = (glam::Vec2::new(px, py) - cursor).length();
        if dist <= TOL && best.map(|(_, bd)| dist < bd).unwrap_or(true) {
            best = Some((icon.node_id, dist));
        }
    }
    best.map(|(id, _)| id)
}
