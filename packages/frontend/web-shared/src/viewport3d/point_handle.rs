//! Editor point-handle gizmo — a translation-only handle per control point.
//!
//! Each handle is a small unit-sphere mesh rendered through the HUD pass
//! so it stays visible regardless of occluding world geometry. Picking
//! uses the renderer's visibility-buffer picker (the handle meshes are
//! addressable by `MeshKey`). Drag math projects the mouse delta onto a
//! camera-facing plane through the handle's anchor; the world-space
//! intersection delta becomes the control-point offset.
//!
//! This is the simplified "translate" arm of the TRS gizmo
//! (`transform_controller`) — no axis constraints, no rotation, no scale.
//! Authors grab the handle and drag in the camera plane.

use awsm_meshgen::sphere_mesh;
use awsm_renderer::{
    camera::CameraMatrices,
    materials::{unlit::UnlitMaterial, Material, MaterialAlphaMode, MaterialKey},
    meshes::MeshKey,
    raw_mesh::RawMeshData,
    transforms::{Transform, TransformKey},
    AwsmRenderer,
};
use glam::{Quat, Vec3};

use crate::viewport3d::transform_controller::ray_plane_intersection;

/// Visual radius of each handle in world units (scaled per-frame to track
/// a fixed pixel size — see [`PointHandleSet::zoom_handles`]).
const HANDLE_RADIUS: f32 = 1.0;

/// Desired on-screen radius in CSS pixels.
const HANDLE_DESIRED_PIXEL_RADIUS: f32 = 12.0;

/// Per-handle GPU state.
struct PointHandle {
    mesh_key: MeshKey,
    transform_key: TransformKey,
    world_pos: Vec3,
}

/// Drag state for one active handle.
struct PointDragState {
    handle_index: usize,
    initial_world_pos: Vec3,
    plane_normal: Vec3,
    plane_point: Vec3,
    initial_intersection: Vec3,
    screen_pos: (f32, f32),
}

/// Set of N translation-only point handles. Used by the editor to expose
/// control-point editing for `NodeKind::Curve` and `NodeKind::Line` nodes
/// directly in the viewport.
#[derive(Default)]
pub struct PointHandleSet {
    handles: Vec<PointHandle>,
    material_key: Option<MaterialKey>,
    drag_state: Option<PointDragState>,
    visible: bool,
}

impl PointHandleSet {
    /// Empty set — call [`Self::set_points`] to populate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the world-space positions of the handles. Allocates additional
    /// handles or frees stale ones so the set matches `world_positions`.
    pub fn set_points(
        &mut self,
        renderer: &mut AwsmRenderer,
        world_positions: &[Vec3],
    ) -> awsm_renderer::error::Result<()> {
        // Ensure we have a shared handle material registered.
        if self.material_key.is_none() {
            // Bright emissive cyan so handles stay readable against a
            // wide range of scene materials.
            // Unlit so the handle reads as a flat, bright marker regardless of
            // scene lighting / a blown-out IBL. A PBR sphere lit + tonemapped
            // against a bright background washed out to a faint tint; Unlit
            // emits the base color directly. HDR-bright cyan so it survives
            // tonemapping as a strong, saturated dot.
            let mut mat = UnlitMaterial::new(MaterialAlphaMode::Opaque, false);
            mat.base_color_factor = [0.5, 6.0, 8.0, 1.0];
            let key = renderer.materials.insert(
                Material::Unlit(mat),
                &renderer.textures,
                &renderer.dynamic_materials,
                &renderer.extras_pool,
            );
            self.material_key = Some(key);
        }
        let material_key = self.material_key.unwrap();

        // Grow.
        while self.handles.len() < world_positions.len() {
            let idx = self.handles.len();
            let transform_key = renderer.transforms.insert(
                Transform {
                    translation: world_positions[idx],
                    rotation: Quat::IDENTITY,
                    scale: Vec3::splat(HANDLE_RADIUS),
                },
                None,
            );
            // Tiny unit sphere — vertex count kept low; gets scaled per
            // frame to maintain a fixed pixel size.
            let mesh = sphere_mesh(1.0, 16, 8);
            let raw = RawMeshData {
                positions: mesh.positions,
                normals: mesh.normals,
                uvs: mesh.uvs,
                colors: mesh.colors,
                indices: mesh.indices,
            };
            let mesh_key = renderer.add_raw_mesh(raw, transform_key, material_key)?;
            renderer.set_mesh_hud(mesh_key, true)?;
            renderer.set_mesh_hidden(mesh_key, !self.visible)?;
            self.handles.push(PointHandle {
                mesh_key,
                transform_key,
                world_pos: world_positions[idx],
            });
        }

        // Shrink.
        while self.handles.len() > world_positions.len() {
            let handle = self.handles.pop().expect("shrink while populated");
            renderer.remove_mesh(handle.mesh_key);
            renderer.transforms.remove(handle.transform_key);
        }

        // Re-anchor existing handles.
        for (i, pos) in world_positions.iter().enumerate() {
            let handle = &mut self.handles[i];
            handle.world_pos = *pos;
            let _ = renderer.transforms.set_local(
                handle.transform_key,
                Transform {
                    translation: *pos,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::splat(HANDLE_RADIUS),
                },
            );
        }
        Ok(())
    }

    /// Toggle handle visibility (the meshes stay registered so the
    /// `MeshKey`s remain stable across show/hide cycles).
    pub fn show(&mut self, renderer: &mut AwsmRenderer, visible: bool) {
        self.visible = visible;
        for h in &self.handles {
            let _ = renderer.set_mesh_hidden(h.mesh_key, !visible);
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Tear every handle down (e.g. on selection change to a non-curve kind).
    pub fn clear(&mut self, renderer: &mut AwsmRenderer) {
        for handle in self.handles.drain(..) {
            renderer.remove_mesh(handle.mesh_key);
            renderer.transforms.remove(handle.transform_key);
        }
        self.drag_state = None;
    }

    /// Returns the handle index if `mesh_key` belongs to this set.
    pub fn is_handle_mesh(&self, mesh_key: MeshKey) -> Option<usize> {
        self.handles.iter().position(|h| h.mesh_key == mesh_key)
    }

    /// CPU-side picking fallback for when two handles project close
    /// together in screen space and the per-pixel GPU pick lands on
    /// the wrong one (or just misses, e.g. clicking the screen between
    /// two adjacent handles).
    ///
    /// Projects every visible handle to screen space using the camera
    /// matrices captured last frame, then returns the index of the
    /// closest one whose 2D distance from `(screen_x, screen_y)` is at
    /// most `tolerance_px`. Returns `None` if no handle qualifies.
    ///
    /// Cheap: O(N) over the handle set (max ~30 handles for any
    /// authored Curve / Line), no GPU work, sub-pixel-precise.
    pub fn pick_with_tolerance(
        &self,
        renderer: &AwsmRenderer,
        screen_x: i32,
        screen_y: i32,
        tolerance_px: f32,
    ) -> Option<usize> {
        if !self.visible || self.handles.is_empty() {
            return None;
        }
        let matrices = renderer.camera.last_matrices.as_ref()?;
        let (width, height) = renderer.gpu.canvas_size(false);
        let viewport_w = width as f32;
        let viewport_h = height as f32;
        if viewport_w <= 0.0 || viewport_h <= 0.0 {
            return None;
        }
        let view_proj = matrices.projection * matrices.view;
        let cursor = glam::Vec2::new(screen_x as f32, screen_y as f32);

        let mut best: Option<(usize, f32)> = None;
        for (i, handle) in self.handles.iter().enumerate() {
            let clip = view_proj * handle.world_pos.extend(1.0);
            if clip.w <= 0.0 {
                // Behind the camera (or on the near plane) — skip.
                continue;
            }
            let ndc_x = clip.x / clip.w;
            let ndc_y = clip.y / clip.w;
            // NDC → backing-store pixel. Y is flipped because NDC is
            // bottom-up but pixel space is top-down.
            let px = (ndc_x * 0.5 + 0.5) * viewport_w;
            let py = (1.0 - (ndc_y * 0.5 + 0.5)) * viewport_h;
            let dist = (glam::Vec2::new(px, py) - cursor).length();
            if dist <= tolerance_px {
                match best {
                    Some((_, bd)) if bd <= dist => {}
                    _ => best = Some((i, dist)),
                }
            }
        }
        best.map(|(i, _)| i)
    }

    pub fn handle_count(&self) -> usize {
        self.handles.len()
    }

    /// Begin a drag on `idx`. The pointer position is in canvas backing-store
    /// pixels. Sets up the camera-facing plane that subsequent
    /// `update_drag` calls intersect.
    pub fn start_drag(
        &mut self,
        renderer: &AwsmRenderer,
        idx: usize,
        screen_x: i32,
        screen_y: i32,
    ) {
        let Some(handle) = self.handles.get(idx) else {
            return;
        };
        let Some(matrices) = renderer.camera.last_matrices.as_ref() else {
            return;
        };
        let camera_pos = matrices.position_world;
        let world_pos = handle.world_pos;
        // Plane through the handle's anchor, facing the camera.
        let to_camera = camera_pos - world_pos;
        let plane_normal = if to_camera.length_squared() > 1e-6 {
            to_camera.normalize()
        } else {
            Vec3::Z
        };
        let (width, height) = renderer.gpu.canvas_size(false);
        let Some(initial_intersection) = ray_plane_intersection(
            screen_x as f32,
            screen_y as f32,
            width as f32,
            height as f32,
            matrices,
            world_pos,
            plane_normal,
        ) else {
            return;
        };
        self.drag_state = Some(PointDragState {
            handle_index: idx,
            initial_world_pos: world_pos,
            plane_normal,
            plane_point: world_pos,
            initial_intersection,
            screen_pos: (screen_x as f32, screen_y as f32),
        });
    }

    /// Accumulate a mouse delta. Returns the new `(handle_index, world_pos)`
    /// if the drag math succeeded — the editor wires this back into the
    /// authored `CurveDef` / `LineDef` control points.
    pub fn update_drag(
        &mut self,
        renderer: &mut AwsmRenderer,
        x_delta: i32,
        y_delta: i32,
    ) -> Option<(usize, Vec3)> {
        let drag = self.drag_state.as_mut()?;
        drag.screen_pos.0 += x_delta as f32;
        drag.screen_pos.1 += y_delta as f32;

        let matrices = renderer.camera.last_matrices.as_ref()?;
        let (width, height) = renderer.gpu.canvas_size(false);
        let current = ray_plane_intersection(
            drag.screen_pos.0,
            drag.screen_pos.1,
            width as f32,
            height as f32,
            matrices,
            drag.plane_point,
            drag.plane_normal,
        )?;
        let delta = current - drag.initial_intersection;
        let new_world_pos = drag.initial_world_pos + delta;

        // Move the visual handle.
        let idx = drag.handle_index;
        if let Some(handle) = self.handles.get_mut(idx) {
            handle.world_pos = new_world_pos;
            let _ = renderer.transforms.set_local(
                handle.transform_key,
                Transform {
                    translation: new_world_pos,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::splat(HANDLE_RADIUS),
                },
            );
        }

        Some((idx, new_world_pos))
    }

    pub fn end_drag(&mut self) {
        self.drag_state = None;
    }

    pub fn is_dragging(&self) -> bool {
        self.drag_state.is_some()
    }

    /// Per-frame: scale each handle so it stays a fixed pixel size on
    /// screen. Mirrors `TransformController::zoom_gizmo_transforms`.
    pub fn zoom_handles(
        &self,
        renderer: &mut AwsmRenderer,
        camera_matrices: &CameraMatrices,
    ) -> awsm_renderer::error::Result<()> {
        if !self.visible || self.handles.is_empty() {
            return Ok(());
        }
        let (_, viewport_y) = renderer.gpu.canvas_size(false);
        let desired_ndc = 2.0 * HANDLE_DESIRED_PIXEL_RADIUS / viewport_y as f32;
        let proj11 = camera_matrices.projection.y_axis.y;
        let is_ortho = camera_matrices.is_orthographic();
        for handle in &self.handles {
            let depth = if is_ortho {
                1.0
            } else {
                (handle.world_pos - camera_matrices.position_world).length()
            };
            let scale = desired_ndc * depth / proj11;
            renderer.transforms.set_local(
                handle.transform_key,
                Transform {
                    translation: handle.world_pos,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::splat(scale.max(0.001)),
                },
            )?;
        }
        Ok(())
    }
}
