use std::sync::Arc;

use anyhow::{Context, Result};
use awsm_renderer::{
    camera::CameraMatrices,
    meshes::MeshKey,
    transforms::{Transform, TransformKey},
    AwsmRenderer,
};
use awsm_renderer_gltf::GltfKeyLookups;
use glam::{Mat4, Quat, Vec3, Vec4};

#[derive(Clone, Debug)]
pub struct TransformController {
    pub selected_object: Option<TransformObject>,
    mesh_keys: TransformControllerMeshKeys,
    transform_keys: TransformControllerTransformKeys,
    _gizmo_space: GizmoSpace,
    current_gizmo_kind: Option<GizmoKind>,
    drag_state: Option<DragState>,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub struct TransformObject {
    pub key: TransformKey,
    pub instance: Option<usize>,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub enum TransformTarget {
    GizmoHit(GizmoKind),
    ObjectHit(TransformObject),
}

/// State tracked during a gizmo drag operation
#[derive(Clone, Debug)]
struct DragState {
    /// Current accumulated screen position (starts at initial click, updated by deltas)
    screen_pos: (f32, f32),
    /// Initial object local translation when drag started
    initial_translation: Vec3,
    /// Initial object local scale when drag started
    initial_scale: Vec3,
    /// Initial object local rotation when drag started
    initial_rotation: Quat,
    /// Initial object world position when drag started
    initial_world_position: Vec3,
    /// The constraint axis in world space (may be rotated for local mode)
    world_axis: Vec3,
    /// Inverse of parent's world rotation (to convert world deltas to parent space)
    parent_inverse_rotation: Quat,
    /// The plane used for ray intersection (normal vector)
    plane_normal: Vec3,
    /// A point on the plane (the initial world position)
    plane_point: Vec3,
    /// Initial intersection point on the plane
    initial_intersection: Vec3,
    /// For rotation: the initial angle from center to intersection (in the rotation plane)
    initial_angle: f32,
    /// For scale in Global mode: which local axis to actually scale (0=X, 1=Y, 2=Z)
    /// In Global mode, this maps the world axis to the closest local axis.
    /// In Local mode, this matches the gizmo kind directly.
    scale_target_axis: u8,
}

#[derive(Clone, Debug)]
pub struct TransformControllerMeshKeys {
    pub cube_x: MeshKey,
    pub cube_y: MeshKey,
    pub cube_z: MeshKey,

    pub ring_x: MeshKey,
    pub ring_y: MeshKey,
    pub ring_z: MeshKey,

    pub arrow_x: MeshKey,
    pub arrow_y: MeshKey,
    pub arrow_z: MeshKey,
}

#[derive(Clone, Debug)]
pub struct TransformControllerTransformKeys {
    pub root: TransformKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GizmoKind {
    TranslationX,
    TranslationY,
    TranslationZ,
    RotationX,
    RotationY,
    RotationZ,
    ScaleX,
    ScaleY,
    ScaleZ,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GizmoSpace {
    Local,
    #[default]
    Global,
}

impl TransformController {
    pub fn new(
        lookups: Arc<std::sync::Mutex<GltfKeyLookups>>,
        gizmo_space: GizmoSpace,
    ) -> Result<Self> {
        let (mesh_keys, transform_keys) = {
            let lookups = lookups.lock().unwrap();
            let mesh_keys = TransformControllerMeshKeys::new(&lookups)?;
            let transform_keys = TransformControllerTransformKeys::new(&lookups)?;

            (mesh_keys, transform_keys)
        };
        Ok(Self {
            mesh_keys,
            transform_keys,
            selected_object: None,
            current_gizmo_kind: None,
            drag_state: None,
            _gizmo_space: gizmo_space,
        })
    }

    pub fn is_gizmo_mesh_key(&self, mesh_key: MeshKey) -> bool {
        self.get_gizmo_mesh_kind(mesh_key).is_some()
    }

    /// Debug accessor for the gizmo mesh-key table. Returns a JSON
    /// object string keyed by gizmo kind. Used by the
    /// `measurement::debug_gizmo_mesh_keys` dev wasm-bindgen export
    /// so a JS-side test harness can verify the keys the controller
    /// is comparing against vs the keys the picker is returning.
    pub fn gizmo_mesh_keys_debug(&self) -> String {
        format!(
            "{{\"arrow_x\":\"{:?}\",\"arrow_y\":\"{:?}\",\"arrow_z\":\"{:?}\",\
             \"ring_x\":\"{:?}\",\"ring_y\":\"{:?}\",\"ring_z\":\"{:?}\",\
             \"cube_x\":\"{:?}\",\"cube_y\":\"{:?}\",\"cube_z\":\"{:?}\"}}",
            self.mesh_keys.arrow_x,
            self.mesh_keys.arrow_y,
            self.mesh_keys.arrow_z,
            self.mesh_keys.ring_x,
            self.mesh_keys.ring_y,
            self.mesh_keys.ring_z,
            self.mesh_keys.cube_x,
            self.mesh_keys.cube_y,
            self.mesh_keys.cube_z,
        )
    }

    pub fn zoom_gizmo_transforms(
        &self,
        renderer: &mut AwsmRenderer,
        camera_matrices: &CameraMatrices,
    ) -> awsm_renderer::error::Result<()> {
        let mut transform = renderer
            .transforms
            .get_local(self.transform_keys.root)?
            .clone();

        // Re-anchor the gizmo to the selected object every frame. The
        // selection can change via paths that don't go through
        // `start_pick` (tree-driven selection in a host editor, etc.),
        // and the selected object's transform can be edited externally
        // (a properties panel writing to the local TRS) — without this
        // the gizmo would otherwise stay where it was last positioned.
        if let Some(selected_object) = self.selected_object {
            if let Some(world_matrix) = get_world_matrix(renderer, selected_object) {
                let (_, world_rotation, world_position) =
                    world_matrix.to_scale_rotation_translation();
                transform.translation = world_position;
                transform.rotation = match self._gizmo_space {
                    GizmoSpace::Global => Quat::IDENTITY,
                    GizmoSpace::Local => world_rotation,
                };
            }
        }

        const DESIRED_PIXEL_SIZE: f32 = 100.0; // Desired size in pixels
        const REFERENCE_SIZE: f32 = 1.0; // Reference size of the gizmo in world

        let (_, viewport_y) = renderer.gpu.canvas_size(false);

        let desired_ndc = 2.0 * DESIRED_PIXEL_SIZE / viewport_y as f32;
        let proj11 = camera_matrices.projection.y_axis.y;

        let depth = if camera_matrices.is_orthographic() {
            1.0
        } else {
            let cam_pos = camera_matrices.position_world;
            let gizmo_pos = transform.translation;
            (gizmo_pos - cam_pos).length()
        };

        let scale = (desired_ndc * depth / proj11) / REFERENCE_SIZE;

        transform.scale = Vec3::new(scale, scale, scale);

        renderer
            .transforms
            .set_local(self.transform_keys.root, transform)?;

        Ok(())
    }

    fn get_gizmo_mesh_kind(&self, mesh_key: MeshKey) -> Option<GizmoKind> {
        if mesh_key == self.mesh_keys.arrow_x {
            Some(GizmoKind::TranslationX)
        } else if mesh_key == self.mesh_keys.arrow_y {
            Some(GizmoKind::TranslationY)
        } else if mesh_key == self.mesh_keys.arrow_z {
            Some(GizmoKind::TranslationZ)
        } else if mesh_key == self.mesh_keys.ring_x {
            Some(GizmoKind::RotationX)
        } else if mesh_key == self.mesh_keys.ring_y {
            Some(GizmoKind::RotationY)
        } else if mesh_key == self.mesh_keys.ring_z {
            Some(GizmoKind::RotationZ)
        } else if mesh_key == self.mesh_keys.cube_x {
            Some(GizmoKind::ScaleX)
        } else if mesh_key == self.mesh_keys.cube_y {
            Some(GizmoKind::ScaleY)
        } else if mesh_key == self.mesh_keys.cube_z {
            Some(GizmoKind::ScaleZ)
        } else {
            None
        }
    }

    // returns whether or not we hit a gizmo mesh
    pub fn start_pick(
        &mut self,
        renderer: &mut AwsmRenderer,
        mesh_key: MeshKey,
        x: i32,
        y: i32,
    ) -> Option<TransformTarget> {
        match self.get_gizmo_mesh_kind(mesh_key) {
            Some(gizmo_kind) => {
                self.current_gizmo_kind = Some(gizmo_kind);

                // Initialize drag state for gizmo manipulation
                if let Some(selected_object) = self.selected_object {
                    if let (Some(selected_transform), Some(world_matrix), Some(camera_matrices)) = (
                        get_local_transform(renderer, selected_object),
                        get_world_matrix(renderer, selected_object),
                        renderer.camera.last_matrices.as_ref(),
                    ) {
                        // Extract world position and rotation from world matrix
                        let (_world_scale, world_rotation, world_position) =
                            world_matrix.to_scale_rotation_translation();
                        let camera_pos = camera_matrices.position_world;

                        // Get parent's world rotation to convert deltas back to parent space
                        let parent_inverse_rotation = if let Some(parent_key) =
                            get_parent_key(renderer, selected_object)
                        {
                            if let Ok(parent_world) = renderer.transforms.get_world(parent_key) {
                                let (_, parent_rot, _) =
                                    parent_world.to_scale_rotation_translation();
                                parent_rot.inverse()
                            } else {
                                Quat::IDENTITY
                            }
                        } else {
                            Quat::IDENTITY
                        };

                        let gizmo_space = self._gizmo_space;

                        // Get the local axis for this gizmo kind
                        let local_axis = match gizmo_kind {
                            GizmoKind::TranslationX | GizmoKind::ScaleX | GizmoKind::RotationX => {
                                Vec3::X
                            }
                            GizmoKind::TranslationY | GizmoKind::ScaleY | GizmoKind::RotationY => {
                                Vec3::Y
                            }
                            GizmoKind::TranslationZ | GizmoKind::ScaleZ | GizmoKind::RotationZ => {
                                Vec3::Z
                            }
                        };

                        // Compute world-space axis based on gizmo space mode
                        let world_axis = match gizmo_space {
                            GizmoSpace::Global => local_axis,
                            GizmoSpace::Local => world_rotation * local_axis,
                        };

                        // For scale in Global mode, find which local axis is closest to the world axis
                        // This allows scaling to visually match the gizmo direction
                        let scale_target_axis = match gizmo_kind {
                            GizmoKind::ScaleX | GizmoKind::ScaleY | GizmoKind::ScaleZ => {
                                match gizmo_space {
                                    GizmoSpace::Global => {
                                        // Find which object local axis is closest to the gizmo's world axis
                                        let local_x_world = world_rotation * Vec3::X;
                                        let local_y_world = world_rotation * Vec3::Y;
                                        let local_z_world = world_rotation * Vec3::Z;

                                        let dot_x = world_axis.dot(local_x_world).abs();
                                        let dot_y = world_axis.dot(local_y_world).abs();
                                        let dot_z = world_axis.dot(local_z_world).abs();

                                        if dot_x >= dot_y && dot_x >= dot_z {
                                            0 // Scale X
                                        } else if dot_y >= dot_x && dot_y >= dot_z {
                                            1 // Scale Y
                                        } else {
                                            2 // Scale Z
                                        }
                                    }
                                    GizmoSpace::Local => {
                                        // In Local mode, use the axis matching the gizmo kind directly
                                        match gizmo_kind {
                                            GizmoKind::ScaleX => 0,
                                            GizmoKind::ScaleY => 1,
                                            GizmoKind::ScaleZ => 2,
                                            _ => 0,
                                        }
                                    }
                                }
                            }
                            _ => 0, // Not used for non-scale operations
                        };

                        // Determine the plane normal based on gizmo kind
                        let plane_normal = match gizmo_kind {
                            // Translation/Scale: plane contains the axis and faces the camera
                            GizmoKind::TranslationX
                            | GizmoKind::TranslationY
                            | GizmoKind::TranslationZ
                            | GizmoKind::ScaleX
                            | GizmoKind::ScaleY
                            | GizmoKind::ScaleZ => {
                                let to_camera = (camera_pos - world_position).normalize();
                                let normal = (to_camera - world_axis * to_camera.dot(world_axis))
                                    .normalize();
                                // Handle edge case when camera looks along axis
                                if normal.length_squared() < 0.001 {
                                    if world_axis.dot(Vec3::Y).abs() < 0.9 {
                                        (Vec3::Y - world_axis * Vec3::Y.dot(world_axis)).normalize()
                                    } else {
                                        (Vec3::X - world_axis * Vec3::X.dot(world_axis)).normalize()
                                    }
                                } else {
                                    normal
                                }
                            }
                            // Rotation: plane is perpendicular to the rotation axis
                            GizmoKind::RotationX | GizmoKind::RotationY | GizmoKind::RotationZ => {
                                world_axis
                            }
                        };

                        // Get viewport size for screen-to-NDC conversion
                        let (width, height) = renderer.gpu.canvas_size(false);

                        // Compute initial ray-plane intersection
                        if let Some(intersection) = ray_plane_intersection(
                            x as f32,
                            y as f32,
                            width as f32,
                            height as f32,
                            camera_matrices,
                            world_position,
                            plane_normal,
                        ) {
                            // For rotation, compute the initial angle in the rotation plane
                            let initial_angle = match gizmo_kind {
                                GizmoKind::RotationX
                                | GizmoKind::RotationY
                                | GizmoKind::RotationZ => {
                                    let from_center = intersection - world_position;
                                    // Get two basis vectors in the rotation plane
                                    let (basis_u, basis_v) = get_rotation_plane_basis(world_axis);
                                    let u = from_center.dot(basis_u);
                                    let v = from_center.dot(basis_v);
                                    v.atan2(u)
                                }
                                _ => 0.0,
                            };

                            self.drag_state = Some(DragState {
                                screen_pos: (x as f32, y as f32),
                                initial_translation: selected_transform.translation,
                                initial_scale: selected_transform.scale,
                                initial_rotation: selected_transform.rotation,
                                initial_world_position: world_position,
                                world_axis,
                                parent_inverse_rotation,
                                plane_normal,
                                plane_point: world_position,
                                initial_intersection: intersection,
                                initial_angle,
                                scale_target_axis,
                            });
                        }
                    }
                }

                Some(TransformTarget::GizmoHit(gizmo_kind))
            }
            None => {
                self.current_gizmo_kind = None;
                self.drag_state = None;

                if let Ok(mesh) = renderer.meshes.get(mesh_key) {
                    let instance = if mesh.instanced {
                        // TODO - check aabb vs. ray intersection for each index?
                        Some(0)
                    } else {
                        None
                    };

                    let obj = TransformObject {
                        key: mesh.transform_key,
                        instance,
                    };

                    self.selected_object = Some(obj);
                    self.update_gizmo_transform(renderer);

                    Some(TransformTarget::ObjectHit(obj))
                } else {
                    None
                }
            }
        }
    }
    pub fn update_transform(&mut self, renderer: &mut AwsmRenderer, x_delta: i32, y_delta: i32) {
        let Some(drag_state) = self.drag_state.as_mut() else {
            return;
        };

        let Some(selected_object) = self.selected_object else {
            return;
        };

        let Some(gizmo_kind) = self.current_gizmo_kind else {
            return;
        };

        let Some(camera_matrices) = renderer.camera.last_matrices.as_ref() else {
            return;
        };

        // Accumulate screen position from deltas
        drag_state.screen_pos.0 += x_delta as f32;
        drag_state.screen_pos.1 += y_delta as f32;

        let (width, height) = renderer.gpu.canvas_size(false);

        // Compute new ray-plane intersection at current screen position
        let Some(current_intersection) = ray_plane_intersection(
            drag_state.screen_pos.0,
            drag_state.screen_pos.1,
            width as f32,
            height as f32,
            camera_matrices,
            drag_state.plane_point,
            drag_state.plane_normal,
        ) else {
            return;
        };

        // Apply the transform based on gizmo kind
        let Some(mut selected_transform) = get_local_transform(renderer, selected_object) else {
            return;
        };

        // Use the stored world-space axis
        let world_axis = drag_state.world_axis;

        match gizmo_kind {
            GizmoKind::TranslationX | GizmoKind::TranslationY | GizmoKind::TranslationZ => {
                // Calculate the movement in world space and project onto the world axis
                let world_delta = current_intersection - drag_state.initial_intersection;
                let movement_along_axis = world_delta.dot(world_axis);

                // World-space translation delta along the constraint axis
                let world_translation_delta = world_axis * movement_along_axis;

                // Convert world delta to parent space
                let parent_space_delta =
                    drag_state.parent_inverse_rotation * world_translation_delta;

                // Translation: add the parent-space movement to the initial local translation
                selected_transform.translation =
                    drag_state.initial_translation + parent_space_delta;
            }
            GizmoKind::ScaleX | GizmoKind::ScaleY | GizmoKind::ScaleZ => {
                // Scale: use ratio of distances from object center along the world axis
                // This ensures dragging "outward" always increases scale
                let initial_offset =
                    drag_state.initial_intersection - drag_state.initial_world_position;
                let current_offset = current_intersection - drag_state.initial_world_position;

                let initial_dist = initial_offset.dot(world_axis);
                let current_dist = current_offset.dot(world_axis);

                // Compute scale factor based on ratio of distances
                let scale_factor = if initial_dist.abs() > 0.001 {
                    // Ratio of current to initial distance
                    (current_dist / initial_dist).max(0.01)
                } else {
                    // Initial click was at center, use linear fallback
                    let camera_distance = (camera_matrices.position_world
                        - drag_state.initial_world_position)
                        .length();
                    let sensitivity = camera_distance * 0.5;
                    (1.0 + current_dist / sensitivity).max(0.01)
                };

                // Use the pre-computed target axis (handles Global mode remapping)
                let mut new_scale = drag_state.initial_scale;
                match drag_state.scale_target_axis {
                    0 => new_scale.x = drag_state.initial_scale.x * scale_factor,
                    1 => new_scale.y = drag_state.initial_scale.y * scale_factor,
                    _ => new_scale.z = drag_state.initial_scale.z * scale_factor,
                }
                selected_transform.scale = new_scale;
            }
            GizmoKind::RotationX | GizmoKind::RotationY | GizmoKind::RotationZ => {
                // Compute current angle in the rotation plane using the world axis
                let from_center = current_intersection - drag_state.plane_point;
                let (basis_u, basis_v) = get_rotation_plane_basis(world_axis);
                let u = from_center.dot(basis_u);
                let v = from_center.dot(basis_v);
                let current_angle = v.atan2(u);

                // Calculate the angle delta
                let angle_delta = current_angle - drag_state.initial_angle;

                // Create rotation quaternion in parent space
                // Convert the world-space rotation axis to parent space
                let parent_space_axis = drag_state.parent_inverse_rotation * world_axis;
                let rotation_delta = Quat::from_axis_angle(parent_space_axis, angle_delta);
                selected_transform.rotation =
                    (rotation_delta * drag_state.initial_rotation).normalize();
            }
        }

        let _ = set_local_transform(renderer, selected_object, selected_transform.clone());

        // Force update of world matrices so we can get the new world position
        renderer.update_transforms();

        // Also update the gizmo position to follow the object's world position
        if let (Some(world_matrix), Ok(mut gizmo_transform)) = (
            get_world_matrix(renderer, selected_object),
            renderer
                .transforms
                .get_local(self.transform_keys.root)
                .cloned(),
        ) {
            let (_, world_rotation, world_position) = world_matrix.to_scale_rotation_translation();

            gizmo_transform.translation = world_position;

            // For local mode, also update gizmo rotation to follow object
            match self._gizmo_space {
                GizmoSpace::Global => gizmo_transform.rotation = Quat::IDENTITY,
                GizmoSpace::Local => gizmo_transform.rotation = world_rotation,
            }

            let _ = renderer
                .transforms
                .set_local(self.transform_keys.root, gizmo_transform);
        }
    }

    pub fn set_hidden(
        &self,
        renderer: &mut AwsmRenderer,
        translation_hidden: bool,
        rotation_hidden: bool,
        scale_hidden: bool,
    ) -> Result<()> {
        for mesh_key in self.translation_keys() {
            renderer.set_mesh_hidden(*mesh_key, translation_hidden)?;
        }

        for mesh_key in self.rotation_keys() {
            renderer.set_mesh_hidden(*mesh_key, rotation_hidden)?;
        }

        for mesh_key in self.scale_keys() {
            renderer.set_mesh_hidden(*mesh_key, scale_hidden)?;
        }

        Ok(())
    }

    fn translation_keys(&self) -> impl Iterator<Item = &MeshKey> {
        [
            &self.mesh_keys.arrow_x,
            &self.mesh_keys.arrow_y,
            &self.mesh_keys.arrow_z,
        ]
        .into_iter()
    }

    fn rotation_keys(&self) -> impl Iterator<Item = &MeshKey> {
        [
            &self.mesh_keys.ring_x,
            &self.mesh_keys.ring_y,
            &self.mesh_keys.ring_z,
        ]
        .into_iter()
    }

    fn scale_keys(&self) -> impl Iterator<Item = &MeshKey> {
        [
            &self.mesh_keys.cube_x,
            &self.mesh_keys.cube_y,
            &self.mesh_keys.cube_z,
        ]
        .into_iter()
    }

    /// Updates the gizmo's position and rotation to match the selected object.
    /// Call this when the selected object changes or when the gizmo space changes.
    fn update_gizmo_transform(&self, renderer: &mut AwsmRenderer) {
        let Some(selected_object) = self.selected_object else {
            return;
        };

        let (Some(world_matrix), Ok(gizmo_transform)) = (
            get_world_matrix(renderer, selected_object),
            renderer.transforms.get_local(self.transform_keys.root),
        ) else {
            return;
        };

        let (_, world_rotation, world_position) = world_matrix.to_scale_rotation_translation();

        let gizmo_rotation = match self._gizmo_space {
            GizmoSpace::Global => Quat::IDENTITY,
            GizmoSpace::Local => world_rotation,
        };

        let gizmo_transform = gizmo_transform
            .clone()
            .with_translation(world_position)
            .with_rotation(gizmo_rotation);

        let _ = renderer
            .transforms
            .set_local(self.transform_keys.root, gizmo_transform);
    }

    /// Call this when the gizmo space (local/global) changes in the UI.
    /// Updates the gizmo rotation to reflect the new space immediately.
    pub fn set_space(&mut self, renderer: &mut AwsmRenderer, space: GizmoSpace) {
        self._gizmo_space = space;
        self.update_gizmo_transform(renderer);
    }
}

impl TransformControllerMeshKeys {
    pub fn new(lookups: &GltfKeyLookups) -> Result<Self> {
        let get_mesh_key = |node_name: &str| -> Result<MeshKey> {
            lookups
                .meshes_for_node_iter(node_name)
                .next()
                .cloned()
                .context(format!("No mesh for node '{}'", node_name))
        };

        Ok(Self {
            cube_x: get_mesh_key("Cube_X")?,
            cube_y: get_mesh_key("Cube_Y")?,
            cube_z: get_mesh_key("Cube_Z")?,
            ring_x: get_mesh_key("Ring_X")?,
            ring_y: get_mesh_key("Ring_Y")?,
            ring_z: get_mesh_key("Ring_Z")?,
            arrow_x: get_mesh_key("Arrow_X")?,
            arrow_y: get_mesh_key("Arrow_Y")?,
            arrow_z: get_mesh_key("Arrow_Z")?,
        })
    }
}

impl TransformControllerTransformKeys {
    pub fn new(lookups: &GltfKeyLookups) -> Result<Self> {
        let get_transform_key = |node_name: &str| -> Result<TransformKey> {
            lookups
                .node_transforms
                .get(node_name)
                .cloned()
                .context(format!("No transform for node '{}'", node_name))
        };

        Ok(Self {
            root: get_transform_key("GizmoRoot")?,
        })
    }
}

/// Get two orthonormal basis vectors in the plane perpendicular to the given axis.
/// Used for computing angles in the rotation plane.
fn get_rotation_plane_basis(axis: Vec3) -> (Vec3, Vec3) {
    // Choose a vector not parallel to the axis
    let not_parallel = if axis.dot(Vec3::Y).abs() < 0.9 {
        Vec3::Y
    } else {
        Vec3::X
    };

    // First basis vector: perpendicular to axis
    let basis_u = axis.cross(not_parallel).normalize();
    // Second basis vector: perpendicular to both axis and basis_u
    let basis_v = axis.cross(basis_u).normalize();

    (basis_u, basis_v)
}

/// Cast a ray from the camera through a screen point and find intersection with a plane.
///
/// Returns the world-space intersection point, or None if the ray is parallel to the plane
/// or pointing away from it.
pub fn ray_plane_intersection(
    screen_x: f32,
    screen_y: f32,
    viewport_width: f32,
    viewport_height: f32,
    camera_matrices: &CameraMatrices,
    plane_point: Vec3,
    plane_normal: Vec3,
) -> Option<Vec3> {
    // Convert screen coordinates to NDC (Normalized Device Coordinates)
    // Screen Y is typically top-down, NDC Y is bottom-up
    let ndc_x = (2.0 * screen_x / viewport_width) - 1.0;
    let ndc_y = 1.0 - (2.0 * screen_y / viewport_height);

    // Create points on the near and far planes in clip space
    // WebGPU uses depth range [0, 1], so near plane is z=0, far plane is z=1
    let near_clip = Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
    let far_clip = Vec4::new(ndc_x, ndc_y, 1.0, 1.0);

    // Transform to world space using inverse view-projection matrix
    let inv_view_proj = camera_matrices.inv_view_projection();

    let near_world = inv_view_proj * near_clip;
    let far_world = inv_view_proj * far_clip;

    // Perspective divide
    let near_world = near_world.truncate() / near_world.w;
    let far_world = far_world.truncate() / far_world.w;

    // Construct ray
    let ray_origin = near_world;
    let ray_direction = (far_world - near_world).normalize();

    // Ray-plane intersection
    // Plane equation: dot(P - plane_point, plane_normal) = 0
    // Ray equation: P = ray_origin + t * ray_direction
    // Solving for t: t = dot(plane_point - ray_origin, plane_normal) / dot(ray_direction, plane_normal)
    let denom = ray_direction.dot(plane_normal);

    // If denom is close to zero, ray is parallel to plane
    if denom.abs() < 1e-6 {
        tracing::info!("Ray is parallel to plane");
        return None;
    }

    let t = (plane_point - ray_origin).dot(plane_normal) / denom;

    // If t is negative, intersection is behind the camera
    if t < 0.0 {
        tracing::info!("Intersection is behind the camera");
        return None;
    }

    Some(ray_origin + ray_direction * t)
}

fn get_local_transform(renderer: &AwsmRenderer, object: TransformObject) -> Option<Transform> {
    let local = match renderer.transforms.get_local(object.key) {
        Ok(local) => local.clone(),
        Err(_) => {
            return None;
        }
    };

    match object.instance {
        Some(index) => {
            if let Some(t) = renderer.instances.get_transform(object.key, index) {
                Some(t)
                // Some(Transform::from_matrix(
                //     local.to_matrix().mul_mat4(&t.to_matrix()),
                // ))
            } else {
                Some(local)
            }
        }
        None => Some(local),
    }
}

fn get_world_matrix(renderer: &AwsmRenderer, object: TransformObject) -> Option<Mat4> {
    let world = match renderer.transforms.get_world(object.key) {
        Ok(world) => *world,
        Err(_) => {
            return None;
        }
    };
    match object.instance {
        Some(index) => {
            if let Some(t) = renderer.instances.get_transform(object.key, index) {
                Some(world.mul_mat4(&t.to_matrix()))
            } else {
                Some(world)
            }
        }
        None => {
            let mut world = world;
            if let Some(mesh_keys) = renderer.meshes.keys_by_transform_key(object.key) {
                let mut center_sum = Vec3::ZERO;
                let mut center_count = 0u32;
                for mesh_key in mesh_keys {
                    if let Ok(mesh) = renderer.meshes.get(*mesh_key) {
                        if let Some(aabb) = mesh.world_aabb.as_ref() {
                            center_sum += aabb.center();
                            center_count += 1;
                        }
                    }
                }
                if center_count > 0 {
                    let center = center_sum / center_count as f32;
                    world.w_axis = Vec4::new(center.x, center.y, center.z, 1.0);
                }
            }

            Some(world)
        }
    }
}

fn get_parent_key(renderer: &AwsmRenderer, object: TransformObject) -> Option<TransformKey> {
    renderer.transforms.get_parent(object.key).ok()
}

fn set_local_transform(
    renderer: &mut AwsmRenderer,
    object: TransformObject,
    transform: Transform,
) -> Result<()> {
    match object.instance {
        None => {
            renderer.transforms.set_local(object.key, transform)?;
        }
        Some(index) => {
            renderer
                .instances
                .transform_update(object.key, index, &transform);
        }
    }

    Ok(())
}
