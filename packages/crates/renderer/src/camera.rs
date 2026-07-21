//! Camera buffers and matrices.

use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
use awsm_renderer_core::error::AwsmCoreError;
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use glam::{Mat4, Vec2, Vec3, Vec4};
use thiserror::Error;

use crate::bind_groups::BindGroups;
use crate::render_textures::RenderTextures;
use crate::{AwsmRenderer, AwsmRendererLogging};

const APPLY_JITTER: bool = false;

impl AwsmRenderer {
    /// Updates the camera buffer with new matrices.
    pub fn update_camera(&mut self, camera_matrices: CameraMatrices) -> Result<()> {
        // Render resolution (scaled), not swap-chain: the camera uniform's
        // viewport feeds shader screen-space math, which runs at render res.
        let (surface_w, surface_h) = self.gpu.current_context_texture_size()?;
        let current_width = crate::size::scale_extent(surface_w, self.render_scale);
        let current_height = crate::size::scale_extent(surface_h, self.render_scale);

        self.camera.update(
            camera_matrices,
            &self.render_textures,
            current_width as f32,
            current_height as f32,
            self.features.depth(),
        )?;

        Ok(())
    }
}

/// GPU camera buffer and cached state.
pub struct CameraBuffer {
    pub(crate) raw_data: [u8; Self::BYTE_SIZE],
    pub gpu_buffer: web_sys::GpuBuffer,
    pub last_matrices: Option<CameraMatrices>,
    camera_moved: bool,
    gpu_dirty: bool,
    /// One-shot latch for the depth-convention mismatch error — a static
    /// misconfiguration, so it must not spam once per frame.
    warned_convention: bool,
    uploader: crate::buffer::mapped_uploader::MappedUploader,
}

/// Camera matrices and parameters.
#[derive(Clone, Debug)]
pub struct CameraMatrices {
    pub view: Mat4,
    pub projection: Mat4,
    pub position_world: Vec3,
    /// Focus distance for depth of field (world units). Default: 10.0
    pub focus_distance: f32,
    /// Aperture f-stop for depth of field. Lower = more blur. Default: 5.6
    pub aperture: f32,
    /// Depth convention the `projection` was built under (003). Consumers that
    /// derive convention-dependent data from these matrices (frustum-plane
    /// extraction, near/far recovery) read this instead of guessing from the
    /// matrix. MUST match the renderer's `features.reverse_z`.
    pub reverse_z: bool,
    /// Near clip plane in world units — carried EXPLICITLY (003 stage 5) so
    /// froxel z-slicing / cascade fitting never recover it from the matrix
    /// (that algebra breaks under reverse-Z and outright fails under
    /// infinite-far, where `proj[2][2] == 0`).
    pub near: f32,
    /// Far clip plane in world units. May be `f32::INFINITY` under the
    /// stage-8 infinite-far projection — consumers that need a finite bound
    /// (froxel slicing, cascade fitting) clamp it themselves.
    pub far: f32,
}

impl CameraMatrices {
    /// Right-handed perspective camera from eye/target/up + frustum params — the
    /// common case, so a consumer doesn't hand-roll glam matrices. `fov_y` is in
    /// radians; `aspect` = width / height. Depth-of-field defaults to focusing on
    /// `target` at f/16 (tweak `focus_distance` / `aperture` afterward if needed).
    /// `convention` MUST match the renderer's `features.depth()` — a forward-Z
    /// projection on a reverse-Z renderer inverts every depth test.
    pub fn perspective(
        convention: crate::depth_convention::DepthConvention,
        eye: Vec3,
        target: Vec3,
        up: Vec3,
        fov_y: f32,
        aspect: f32,
        near: f32,
        far: f32,
    ) -> Self {
        Self {
            view: Mat4::look_at_rh(eye, target, up),
            projection: convention.perspective(fov_y, aspect, near, far),
            reverse_z: convention.reverse_z,
            near,
            far,
            position_world: eye,
            focus_distance: (target - eye).length().max(0.001),
            aperture: 16.0,
        }
    }

    /// Right-handed ORTHOGRAPHIC camera from eye/target/up + box extents — the
    /// counterpart to [`Self::perspective`], so an ortho consumer never has to
    /// hand-roll a glam matrix and remember to set `reverse_z` to match.
    ///
    /// `convention` MUST match the renderer's `features.depth()`; it is the
    /// single source for both the projection and the `reverse_z` flag here, so
    /// they cannot drift. (`DepthConvention::orthographic` builds reverse-Z by
    /// SWAPPING near/far, which is exactly the kind of detail a hand-rolled
    /// literal gets wrong — a mismatch inverts every depth test.)
    ///
    /// Depth-of-field fields are carried for uniformity; ortho has no
    /// perspective divide, so they only matter if a DoF pass reads them.
    #[allow(clippy::too_many_arguments)]
    pub fn orthographic(
        convention: crate::depth_convention::DepthConvention,
        eye: Vec3,
        target: Vec3,
        up: Vec3,
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        near: f32,
        far: f32,
    ) -> Self {
        Self {
            view: Mat4::look_at_rh(eye, target, up),
            projection: convention.orthographic(left, right, bottom, top, near, far),
            reverse_z: convention.reverse_z,
            near,
            far,
            position_world: eye,
            focus_distance: (target - eye).length().max(0.001),
            aperture: 16.0,
        }
    }

    /// Returns the combined view-projection matrix.
    pub fn view_projection(&self) -> Mat4 {
        self.projection * self.view
    }

    /// Returns the inverse view-projection matrix.
    pub fn inv_view_projection(&self) -> Mat4 {
        self.view_projection().inverse()
    }

    /// Returns true if the projection is orthographic.
    pub fn is_orthographic(&self) -> bool {
        // Orthographic projections have m[3][3] = 1.0 (no perspective divide)
        // Perspective projections have m[3][3] = 0.0 (w' = -z for perspective divide)
        // This is the definitive check for standard projection matrices.
        self.projection.w_axis.w.abs() > 0.5
    }
}

impl CameraBuffer {
    // Layout (tightly packed, no implicit padding):
    //  view                (mat4)  64 bytes
    //  projection          (mat4)  64 bytes
    //  view_projection     (mat4)  64 bytes
    //  inv_view_projection (mat4)  64 bytes
    //  inv_projection      (mat4)  64 bytes
    //  inv_view            (mat4)  64 bytes
    //  position (vec4, w=unused) 16 bytes
    //  frustum corner rays (4 * vec4) 64 bytes
    //  viewport (vec4) 16 bytes
    //  dof_params (vec4: focus_distance, aperture, unused, unused) 16 bytes
    //  prev_view_projection (mat4)  64 bytes  (M3 SSR temporal reprojection)
    // Total = 560 bytes (all members 16-byte aligned, no implicit gaps)
    //
    // `prev_view_projection` is END-APPENDED (M3): it carries the PRIOR frame's
    // unjittered view-projection so the SSR temporal variant can depth-reproject
    // this frame's world positions into last frame's screen space. Appending at
    // the end preserves every existing field offset — the 17 shaders that share
    // `CameraRaw` (`shared_wgsl/camera.wgsl`) ignore the trailing field unless
    // they opt in, and none hardcode the struct size (a bound uniform buffer may
    // be larger than the WGSL struct that reads it).
    //
    // A `vec4<u32>` slot used to live between `position` and
    // `frustum_rays` carrying `render_textures.frame_count()` as
    // `frame_count_and_padding.x`. No WGSL ever read it; the monotonic
    // frame counter now lives on the `frame_globals` uniform (see
    // `crates/renderer/src/frame_globals`). The slot was removed —
    // Camera is 16 bytes slimmer.
    /// Byte size of the camera uniform buffer.
    pub const BYTE_SIZE: usize = 560;

    /// Creates a camera buffer on the GPU.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Camera"),
                Self::BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        Ok(Self {
            raw_data: [0; Self::BYTE_SIZE],
            gpu_dirty: true,
            last_matrices: None,
            camera_moved: false,
            warned_convention: false,
            gpu_buffer,
            uploader: crate::buffer::mapped_uploader::MappedUploader::new("Camera"),
        })
    }

    /// Mapped-ring upload telemetry for the camera buffer.
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.uploader.stats()
    }

    // this is fast/cheap to call, so we can call it multiple times a frame
    // it will only update the data in the buffer once per frame, at render time
    pub(crate) fn update(
        &mut self,
        camera_matrices_orig: CameraMatrices,
        render_textures: &RenderTextures,
        screen_width: f32,
        screen_height: f32,
        convention: crate::depth_convention::DepthConvention,
    ) -> Result<()> {
        // The caller builds the PROJECTION; the renderer owns the CONVENTION.
        // If they disagree every depth test is inverted — geometry still draws,
        // just occluded backwards, which is miserable to diagnose from the
        // symptom. Both values are right here, so say so instead of rendering
        // garbage silently. Logged once per renderer: it is a static
        // misconfiguration, not a per-frame event.
        if camera_matrices_orig.reverse_z != convention.reverse_z && !self.warned_convention {
            self.warned_convention = true;
            tracing::error!(
                "camera/renderer depth-convention MISMATCH: the projection was built with \
                 reverse_z={}, the renderer runs reverse_z={}. Every depth test is inverted. \
                 Build the camera with `CameraMatrices::perspective(renderer.features.depth(), ..)` \
                 (or pass `features.depth()` to `DepthConvention::perspective/orthographic`) so the \
                 two cannot drift.",
                camera_matrices_orig.reverse_z,
                convention.reverse_z
            );
        }
        let mut camera_matrices = camera_matrices_orig.clone();

        self.camera_moved = match &self.last_matrices {
            Some(last_matrices) => {
                fn matrices_equal(a: Mat4, b: Mat4, epsilon: f32) -> bool {
                    for i in 0..16 {
                        if (a.to_cols_array()[i] - b.to_cols_array()[i]).abs() > epsilon {
                            return false;
                        }
                    }
                    true
                }
                // Check if matrices changed (with small epsilon for floating point comparison)
                !matrices_equal(last_matrices.view, camera_matrices.view, 1e-6)
                    || !matrices_equal(last_matrices.projection, camera_matrices.projection, 1e-6)
            }
            _ => true, // First frame, assume movement
        };

        if APPLY_JITTER {
            let jitter_strength = if self.camera_moved { 0.2 } else { 0.8 };
            // TAA jitter
            let jitter = get_halton_jitter(render_textures.frame_count());
            let jitter_ndc_x = (jitter.x / screen_width) * jitter_strength;
            let jitter_ndc_y = (jitter.y / screen_height) * jitter_strength;

            // Create jitter translation matrix
            let jitter_matrix = Mat4::from_translation(Vec3::new(jitter_ndc_x, jitter_ndc_y, 0.0));

            // Apply to your projection matrix
            camera_matrices.projection = jitter_matrix * camera_matrices.projection;
        }

        // Layout written below (mirrors `CameraUniform` in WGSL). The additional inverse
        // projection and frustum rays let compute passes reconstruct per-pixel view/world
        // positions directly from the depth buffer.
        //
        // IMPORTANT: frustum_rays are for SCREEN-SPACE RECONSTRUCTION, NOT frustum culling!
        // They are 4 normalized view-space ray directions at the near plane corners,
        // used for unprojecting screen pixels to world space (deferred rendering, grids, etc.).
        // For frustum culling, you need 6 frustum planes extracted from the view-proj matrix.

        let inv_projection = camera_matrices.projection.inverse();
        let inv_view_projection = camera_matrices.inv_view_projection();
        let inv_view = camera_matrices.view.inverse();
        let frustum_rays = compute_view_frustum_rays(inv_projection, convention.near_ndc_z());

        // let s = format!("CameraBuffer Update, inv_projection: {inv_projection:?} inv_view_projection: {inv_view_projection:?} inv_view: {inv_view:?} frustum rays: {frustum_rays:?}");

        // debug_unique_string(1, &s, || tracing::info!("{s}"));

        let mut offset = 0;

        let view = camera_matrices.view.to_cols_array();
        write_f32_slice(&mut self.raw_data, &mut offset, &view);
        let projection = camera_matrices.projection.to_cols_array();
        write_f32_slice(&mut self.raw_data, &mut offset, &projection);
        let view_projection = camera_matrices.view_projection().to_cols_array();
        write_f32_slice(&mut self.raw_data, &mut offset, &view_projection);
        let inv_view_projection_cols = inv_view_projection.to_cols_array();
        write_f32_slice(&mut self.raw_data, &mut offset, &inv_view_projection_cols);
        let inv_projection_cols = inv_projection.to_cols_array();
        write_f32_slice(&mut self.raw_data, &mut offset, &inv_projection_cols);
        let inv_view_cols = inv_view.to_cols_array();
        write_f32_slice(&mut self.raw_data, &mut offset, &inv_view_cols);
        // Write position as vec4 (xyz + unused w component)
        let position = camera_matrices.position_world.extend(0.0).to_array();
        write_f32_slice(&mut self.raw_data, &mut offset, &position);
        // The 16-byte `frame_count_and_padding` slot that used to sit
        // here is removed — see `BYTE_SIZE`'s rationale. frustum_rays
        // follows directly.

        for ray in frustum_rays.iter() {
            let ray_values = ray.to_array();
            write_f32_slice(&mut self.raw_data, &mut offset, &ray_values);
        }
        //viewport
        write_f32_slice(
            &mut self.raw_data,
            &mut offset,
            &[0.0, 0.0, screen_width, screen_height],
        );

        // DoF parameters: focus_distance, aperture, and 2 unused floats
        write_f32_slice(
            &mut self.raw_data,
            &mut offset,
            &[
                camera_matrices.focus_distance,
                camera_matrices.aperture,
                0.0,
                0.0,
            ],
        );

        // M3 SSR temporal: the PRIOR frame's unjittered view-projection, for
        // depth reprojection in the temporal SSR variant. `self.last_matrices`
        // still holds the previous frame's matrices here — it is overwritten
        // with THIS frame's below. Frame 0 (no history) → the current
        // view-projection, i.e. zero camera motion so the first temporal frame
        // reprojects onto itself. Non-temporal shaders ignore this trailing
        // field (see `BYTE_SIZE`'s rationale).
        let prev_view_projection = self
            .last_matrices
            .as_ref()
            .map(|m| m.view_projection())
            .unwrap_or_else(|| camera_matrices_orig.view_projection());
        write_f32_slice(
            &mut self.raw_data,
            &mut offset,
            &prev_view_projection.to_cols_array(),
        );

        debug_assert_eq!(offset, Self::BYTE_SIZE, "Buffer layout mismatch!");

        self.gpu_dirty = true;

        // Store for next frame (unjittered versions)
        self.last_matrices = Some(camera_matrices_orig);

        Ok(())
    }

    /// Returns true if the camera was moved since the last update.
    pub fn moved(&self) -> bool {
        self.camera_moved
    }

    // writes to the GPU
    /// Writes the camera buffer to the GPU when dirty.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        _bind_groups: &BindGroups,
    ) -> Result<()> {
        if self.gpu_dirty {
            let _maybe_span_guard = if logging.cpu.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Camera GPU write").entered())
            } else {
                None
            };

            self.uploader.write_dirty_ranges(
                gpu,
                &self.gpu_buffer,
                Self::BYTE_SIZE,
                self.raw_data.as_slice(),
                &[(0, Self::BYTE_SIZE)],
            )?;

            self.gpu_dirty = false;
        }

        Ok(())
    }
}
fn get_halton_jitter(frame_count: u32) -> Vec2 {
    let x = halton(frame_count, 2) - 0.5;
    let y = halton(frame_count, 3) - 0.5;
    Vec2::new(x, y)
}

fn halton(mut index: u32, base: u32) -> f32 {
    let mut result = 0.0;
    let mut f = 1.0;

    while index > 0 {
        f /= base as f32;
        result += f * (index % base) as f32;
        index /= base;
    }

    result
}

/// Compute 4 normalized view-space ray directions for the near plane corners.
///
/// These rays are used for screen-space reconstruction (unprojecting screen pixels to world space).
/// Shaders bilinearly interpolate these corner rays to get the ray direction for any pixel,
/// providing better numerical precision than doing full unprojection per-pixel.
///
/// **NOT for frustum culling** - culling needs 6 frustum planes extracted from view-proj matrix.
///
/// Order: [0]=bottom-left, [1]=bottom-right, [2]=top-left, [3]=top-right
fn compute_view_frustum_rays(inv_projection: Mat4, near_ndc_z: f32) -> [Vec4; 4] {
    // Reproject the clip-space corners of the NEAR plane back into view space. These serve as
    // canonical ray directions that the compute shader can bilinearly interpolate per pixel.
    // The near plane's NDC z is convention-dependent (forward-Z 0, reverse-Z 1) — the FAR
    // plane must never be used: it sits at infinity under infinite-far reverse-Z, where
    // unprojection yields w=0 → NaN rays.
    let ndc_corners = [
        Vec4::new(-1.0, -1.0, near_ndc_z, 1.0),
        Vec4::new(1.0, -1.0, near_ndc_z, 1.0),
        Vec4::new(-1.0, 1.0, near_ndc_z, 1.0),
        Vec4::new(1.0, 1.0, near_ndc_z, 1.0),
    ];

    let mut rays = [Vec4::ZERO; 4];
    for (i, corner) in ndc_corners.iter().enumerate() {
        let view_space = inv_projection * *corner;
        let view_space = view_space / view_space.w;
        // Normalize to get ray direction (not position)
        let ray_dir = Vec3::new(view_space.x, view_space.y, view_space.z).normalize();
        rays[i] = Vec4::new(ray_dir.x, ray_dir.y, ray_dir.z, 0.0);
    }

    rays
}

fn write_f32_slice(buffer: &mut [u8], offset: &mut usize, values: &[f32]) {
    // All matrices/vectors in the camera buffer are tightly packed f32 arrays. Writing them this
    // way keeps the CPU-side layout authoritative and avoids duplicating offset math.
    let byte_len = std::mem::size_of_val(values);

    // crate::debug::debug_unique_string(*offset as u32, &format!("{:?}", values), || {
    //     tracing::info!("[{}]: {:?}", offset, values);
    // });

    let bytes = unsafe { std::slice::from_raw_parts(values.as_ptr() as *const u8, byte_len) };
    buffer[*offset..*offset + byte_len].copy_from_slice(bytes);
    *offset += byte_len;
}

/// Result type for camera operations.
type Result<T> = std::result::Result<T, AwsmCameraError>;

/// Camera-related errors.
#[derive(Error, Debug)]
pub enum AwsmCameraError {
    #[error("[camera] {0:?}")]
    Core(#[from] AwsmCoreError),
}
