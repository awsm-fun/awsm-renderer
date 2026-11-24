//! The GPU camera uniform buffer — upload plumbing behind
//! [`crate::AwsmRenderer::set_camera`].

use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use glam::{Mat4, Vec2, Vec3, Vec4};

use super::{CameraMatrices, Result};
use crate::bind_groups::BindGroups;
use crate::AwsmRendererLogging;

/// What the last RENDERED frame used: the previous-frame side of every
/// temporal reprojection.
#[derive(Clone, Copy, Debug)]
struct RenderedCamera {
    /// Unjittered view-projection.
    view_projection: Mat4,
    /// Jitter in render pixels (zero when the frame was not jittered).
    jitter_px: Vec2,
    /// Camera world position.
    position: Vec3,
    /// World-space view direction.
    forward: Vec3,
    /// Whether the projection was orthographic (`projection[3][3] == 1`).
    orthographic: bool,
}

/// GPU camera buffer and cached state.
pub struct CameraBuffer {
    pub(crate) raw_data: [u8; Self::BYTE_SIZE],
    pub gpu_buffer: web_sys::GpuBuffer,
    pub last_matrices: Option<CameraMatrices>,
    camera_moved: bool,
    /// The camera of the last frame that was actually rendered (latched by
    /// [`Self::prepare_frame`]), as opposed to `last_matrices`, which is the
    /// last camera that was SET. They differ when the app sets the camera
    /// more than once per frame, or not every frame.
    rendered: Option<RenderedCamera>,
    /// Whether the camera jumped between the last two rendered frames (see
    /// [`Self::jumped`]).
    jumped: bool,
    /// Whether `raw_data` currently holds a jittered projection.
    raw_is_jittered: bool,
    /// TAA reactive factors packed into `dof_params.zw`: `(k_transparent,
    /// k_material)` (03 §5). `[0, 0]` without TAA, so the uniform stays what
    /// it always was.
    reactive_factors: [f32; 2],
    /// TAA texture LOD bias (03 §7), packed into `prev_position.w`:
    /// `log2(render / post) + mip_bias_extra`. `0` without TAA, so the
    /// uniform stays what it always was.
    mip_bias: f32,
    /// TAA with a HUD (03 §5.4): the same uniform with the UNJITTERED
    /// projection, bound by the HUD geometry pass so the HUD rasterises
    /// without jitter (the resolve passes HUD pixels through). `None` unless
    /// requested ([`Self::sync_hud_buffer`]); nothing changes for the main
    /// buffer.
    pub hud_gpu_buffer: Option<web_sys::GpuBuffer>,
    hud_raw: Option<Box<[u8; Self::BYTE_SIZE]>>,
    hud_dirty: bool,
    hud_uploader: Option<crate::buffer::mapped_uploader::MappedUploader>,
    gpu_dirty: bool,
    /// One-shot latch for the depth-convention mismatch error — a static
    /// misconfiguration, so it must not spam once per frame.
    warned_convention: bool,
    uploader: crate::buffer::mapped_uploader::MappedUploader,
}

/// What TAA adds to the camera uniform (see [`CameraBuffer::prepare_frame`]).
/// `Default` (all zero) is the uniform without TAA.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TaaCameraFactors {
    /// `TaaTuning::reactive_transparent` (`dof_params.z`).
    pub reactive_transparent: f32,
    /// `TaaTuning::reactive_material` (`dof_params.w`).
    pub reactive_material: f32,
    /// Texture LOD bias (`prev_position.w`): `log2(render / post) +
    /// mip_bias_extra`.
    pub mip_bias: f32,
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
    //  dof_params (vec4: focus_distance, aperture, TAA k_transparent,
    //              TAA k_material; zw zero without TAA) 16 bytes
    //  prev_view_projection (mat4)  64 bytes  (M3 SSR temporal reprojection)
    //  view_projection_unjittered (mat4) 64 bytes  (TAA motion vectors)
    //  jitter (vec4: xy = this frame, zw = previous frame; render pixels) 16 bytes
    //  prev_position (vec4, w = TAA texture LOD bias, 0 without TAA) 16 bytes
    //                (TAA: billboards' previous facing)
    // Total = 656 bytes (all members 16-byte aligned, no implicit gaps)
    //
    // The last three are END-APPENDED for TAA, the same way
    // `prev_view_projection` was. `view`, `projection`, `view_projection` and
    // every inverse are the JITTERED matrices when a jitter is applied: they
    // read or write the depth buffer, which was rasterised jittered, so they
    // must match it. Motion vectors and history reprojection use
    // `view_projection_unjittered` and `prev_view_projection` (also
    // unjittered), so a vector never contains jitter. Without jitter the two
    // view-projections are identical and `jitter` is zero.
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
    pub const BYTE_SIZE: usize = 656;

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
            rendered: None,
            jumped: false,
            raw_is_jittered: false,
            reactive_factors: [0.0; 2],
            mip_bias: 0.0,
            hud_gpu_buffer: None,
            hud_raw: None,
            hud_dirty: false,
            hud_uploader: None,
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
        screen_width: f32,
        screen_height: f32,
        convention: crate::depth_convention::DepthConvention,
    ) -> Result<()> {
        // Since `set_camera` became the only public entry point, the projection
        // and the convention are built from the same `features.depth()` value
        // and cannot disagree from the OUTSIDE. This tripwire guards in-crate
        // misuse (an internal caller hand-building `CameraMatrices` against the
        // wrong convention): a mismatch inverts every depth test — geometry
        // still draws, just occluded backwards, which is miserable to diagnose
        // from the symptom. Logged once per renderer: it is a static
        // misconfiguration, not a per-frame event.
        if camera_matrices_orig.reverse_z != convention.reverse_z && !self.warned_convention {
            self.warned_convention = true;
            tracing::error!(
                "camera/renderer depth-convention MISMATCH: the projection was built with \
                 reverse_z={}, the renderer runs reverse_z={}. Every depth test is inverted. \
                 Build matrices with `CameraMatrices::new(features.depth(), ..)` so the two \
                 cannot drift.",
                camera_matrices_orig.reverse_z,
                convention.reverse_z
            );
        }
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
                !matrices_equal(last_matrices.view, camera_matrices_orig.view, 1e-6)
                    || !matrices_equal(
                        last_matrices.projection,
                        camera_matrices_orig.projection,
                        1e-6,
                    )
            }
            _ => true, // First frame, assume movement
        };

        // The PRIOR frame's unjittered view-projection, for depth
        // reprojection in the temporal passes. `self.last_matrices` still
        // holds the previously set matrices here: it is overwritten with THIS
        // call's below. Frame 0 (no history) → the current view-projection,
        // i.e. zero camera motion, so the first temporal frame reprojects
        // onto itself.
        //
        // This is the unjittered upload. When a jitter applies (TAA),
        // `prepare_frame` re-packs once per rendered frame with the jitter
        // and with the last RENDERED frame's matrix as the previous one.
        let prev_view_projection = self
            .last_matrices
            .as_ref()
            .map(|m| m.view_projection())
            .unwrap_or_else(|| camera_matrices_orig.view_projection());
        let prev_position = self
            .last_matrices
            .as_ref()
            .map(|m| m.position_world)
            .unwrap_or(camera_matrices_orig.position_world);
        self.pack(
            &camera_matrices_orig,
            None,
            screen_width,
            screen_height,
            convention,
            prev_view_projection,
            Vec2::ZERO,
            prev_position,
        );

        // Store for next frame (unjittered versions)
        self.last_matrices = Some(camera_matrices_orig);

        Ok(())
    }

    /// Once per rendered frame, before [`Self::write_gpu`]: applies this
    /// frame's jitter (`Some` when TAA is on or a jitter override is set) and
    /// latches the camera this frame renders with as the next frame's
    /// "previous".
    ///
    /// `temporal` is "TAA is on". Then the uniform is re-packed every frame
    /// with the last RENDERED frame as the previous one, jittered or not.
    /// With `temporal == false`, `jitter == None` and no jittered data left
    /// in the buffer this only latches: the uniform is exactly what `update`
    /// packed, so a renderer without TAA uploads what it always did.
    ///
    /// `render_width` / `render_height` are the render size `R`: the jitter
    /// is in render pixels.
    ///
    /// `taa_factors`: TAA's reactive factors (packed into `dof_params.zw`)
    /// and texture LOD bias (packed into `prev_position.w`); `None` = TAA
    /// off = zeros.
    ///
    /// `hud`: also pack the unjittered HUD twin (TAA with a HUD; see
    /// [`Self::hud_gpu_buffer`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_frame(
        &mut self,
        jitter: Option<Vec2>,
        temporal: bool,
        render_width: f32,
        render_height: f32,
        convention: crate::depth_convention::DepthConvention,
        taa_factors: Option<TaaCameraFactors>,
        hud: bool,
    ) {
        let factors = taa_factors.unwrap_or_default();
        let reactive_factors = [factors.reactive_transparent, factors.reactive_material];
        let factors_changed =
            reactive_factors != self.reactive_factors || factors.mip_bias != self.mip_bias;
        self.reactive_factors = reactive_factors;
        self.mip_bias = factors.mip_bias;
        let Some(matrices) = self.last_matrices.clone() else {
            return;
        };
        let view_projection = matrices.view_projection();
        if temporal || jitter.is_some() || self.raw_is_jittered || factors_changed {
            // Previous = the last frame that was RENDERED. Not the last
            // camera that was set: an app that sets the camera twice in one
            // frame, or only when it changes, must still get the true
            // frame-to-frame motion. First frame: zero motion.
            let (prev_view_projection, prev_jitter, prev_position) = match self.rendered {
                Some(r) => (r.view_projection, r.jitter_px, r.position),
                None => (
                    view_projection,
                    jitter.unwrap_or(Vec2::ZERO),
                    matrices.position_world,
                ),
            };
            if hud && self.hud_gpu_buffer.is_some() {
                // The HUD twin: identical but for the jitter (none).
                self.pack(
                    &matrices,
                    None,
                    render_width,
                    render_height,
                    convention,
                    prev_view_projection,
                    prev_jitter,
                    prev_position,
                );
                self.hud_raw = Some(Box::new(self.raw_data));
                self.hud_dirty = true;
            }
            self.pack(
                &matrices,
                jitter,
                render_width,
                render_height,
                convention,
                prev_view_projection,
                prev_jitter,
                prev_position,
            );
        }
        // World-space view direction: the view matrix's third row, negated.
        let forward = -matrices.view.row(2).truncate().normalize_or_zero();
        self.rendered = Some(RenderedCamera {
            view_projection,
            jitter_px: jitter.unwrap_or(Vec2::ZERO),
            position: matrices.position_world,
            forward,
            orthographic: is_orthographic(&matrices.projection),
        });
    }

    /// The unjittered view-projection of the last frame that was rendered
    /// (after [`Self::prepare_frame`]: this frame's).
    pub(crate) fn rendered_view_projection(&self) -> Option<glam::Mat4> {
        self.rendered.map(|r| r.view_projection)
    }

    /// Whether the camera moved so far between the previous rendered frame
    /// and this one that it is a cut nobody flagged: it turned by more than
    /// 30°, or travelled more than 20% of its view distance (the focus
    /// distance). Temporal history is reset on it. A guard, not the main
    /// mechanism: apps call `reset_temporal_history` on cuts they know of.
    pub fn jumped(&self) -> bool {
        self.jumped
    }

    /// Evaluates [`Self::jumped`] for the frame about to render: the camera
    /// as last set against the camera of the last rendered frame. Call once
    /// per rendered frame BEFORE choosing the frame's jitter, so a reset the
    /// jump triggers restarts the jitter cycle on this very frame.
    ///
    /// `view_distance`: how far what the camera looks at is, the scale of the
    /// translation test (the caller passes the distance to the scene bounds,
    /// floored by their radius). `None`: only the rotation test applies.
    pub(crate) fn detect_jump(&mut self, view_distance: Option<f32>) -> bool {
        self.jumped = match (&self.last_matrices, self.rendered) {
            // A perspective <-> orthographic flip is a cut whatever the
            // camera's pose: nothing reprojects across it.
            (Some(now), Some(prev)) => {
                prev.orthographic != is_orthographic(&now.projection)
                    || camera_jumped(
                        prev.position,
                        prev.forward,
                        now.position_world,
                        -now.view.row(2).truncate().normalize_or_zero(),
                        view_distance,
                    )
            }
            _ => false,
        };
        self.jumped
    }

    /// The jitter (render pixels) in the uniform as packed, zero if none.
    pub fn jitter_px(&self) -> Vec2 {
        let at = Self::BYTE_SIZE - 32;
        let f = |i: usize| {
            f32::from_ne_bytes(
                self.raw_data[at + i * 4..at + i * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        Vec2::new(f(0), f(1))
    }

    /// Packs the uniform. `jitter` is in render pixels; `screen_width` /
    /// `screen_height` are the render size.
    #[allow(clippy::too_many_arguments)]
    fn pack(
        &mut self,
        camera_matrices_orig: &CameraMatrices,
        jitter: Option<Vec2>,
        screen_width: f32,
        screen_height: f32,
        convention: crate::depth_convention::DepthConvention,
        prev_view_projection: Mat4,
        prev_jitter: Vec2,
        prev_position: Vec3,
    ) {
        let mut camera_matrices = camera_matrices_orig.clone();
        if let Some(jitter) = jitter {
            // TAA jitter (see `crate::taa`): constant amplitude, one render
            // pixel is `2 / size` in NDC, y flipped.
            camera_matrices.projection = crate::taa::jittered_projection(
                camera_matrices.projection,
                jitter,
                Vec2::new(screen_width, screen_height),
            );
        }
        self.raw_is_jittered = jitter.is_some();

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

        // DoF parameters: focus_distance, aperture; zw = TAA reactive
        // factors (k_transparent, k_material), zeros without TAA.
        write_f32_slice(
            &mut self.raw_data,
            &mut offset,
            &[
                camera_matrices.focus_distance,
                camera_matrices.aperture,
                self.reactive_factors[0],
                self.reactive_factors[1],
            ],
        );

        // M3 SSR temporal / TAA: the prior frame's unjittered
        // view-projection. Non-temporal shaders ignore the trailing fields
        // (see `BYTE_SIZE`'s rationale).
        write_f32_slice(
            &mut self.raw_data,
            &mut offset,
            &prev_view_projection.to_cols_array(),
        );

        // TAA: this frame's UNJITTERED view-projection (motion vectors must
        // not contain jitter), then the jitter itself so the resolve can
        // remove it where the current frame is sampled.
        write_f32_slice(
            &mut self.raw_data,
            &mut offset,
            &camera_matrices_orig.view_projection().to_cols_array(),
        );
        let jitter = jitter.unwrap_or(Vec2::ZERO);
        write_f32_slice(
            &mut self.raw_data,
            &mut offset,
            &[jitter.x, jitter.y, prev_jitter.x, prev_jitter.y],
        );
        write_f32_slice(
            &mut self.raw_data,
            &mut offset,
            &prev_position.extend(self.mip_bias).to_array(),
        );

        debug_assert_eq!(offset, Self::BYTE_SIZE, "Buffer layout mismatch!");

        self.gpu_dirty = true;
    }

    /// Creates (`want`) or drops the HUD twin buffer. Returns `true` when it
    /// was created or dropped: the geometry camera / masked bind groups must
    /// then be rebuilt (`BindGroupCreate::CameraHud`).
    pub(crate) fn sync_hud_buffer(&mut self, gpu: &AwsmRendererWebGpu, want: bool) -> Result<bool> {
        match (want, self.hud_gpu_buffer.is_some()) {
            (true, false) => {
                self.hud_gpu_buffer = Some(
                    gpu.create_buffer(
                        &BufferDescriptor::new(
                            Some("Camera (HUD, unjittered)"),
                            Self::BYTE_SIZE,
                            BufferUsage::new().with_uniform().with_copy_dst(),
                        )
                        .into(),
                    )?,
                );
                self.hud_uploader = Some(crate::buffer::mapped_uploader::MappedUploader::new(
                    "Camera (HUD)",
                ));
                Ok(true)
            }
            (false, true) => {
                if let Some(buffer) = self.hud_gpu_buffer.take() {
                    buffer.destroy();
                }
                self.hud_raw = None;
                self.hud_dirty = false;
                self.hud_uploader = None;
                Ok(true)
            }
            _ => Ok(false),
        }
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
        if self.hud_dirty {
            if let (Some(buffer), Some(raw), Some(uploader)) = (
                self.hud_gpu_buffer.as_ref(),
                self.hud_raw.as_ref(),
                self.hud_uploader.as_mut(),
            ) {
                uploader.write_dirty_ranges(
                    gpu,
                    buffer,
                    Self::BYTE_SIZE,
                    raw.as_slice(),
                    &[(0, Self::BYTE_SIZE)],
                )?;
            }
            self.hud_dirty = false;
        }

        Ok(())
    }
}

/// Whether a projection matrix is orthographic: its last row is (0, 0, 0, 1),
/// where a perspective projection has (0, 0, ∓1, 0).
fn is_orthographic(projection: &Mat4) -> bool {
    projection.w_axis.w.abs() > 0.5
}

/// The camera-jump test behind [`CameraBuffer::jumped`]: turned by more than
/// 30°, or (when the scale is known) moved more than 20% of `view_distance`.
fn camera_jumped(
    prev_position: Vec3,
    prev_forward: Vec3,
    position: Vec3,
    forward: Vec3,
    view_distance: Option<f32>,
) -> bool {
    const MAX_ROTATION_COS: f32 = 0.866_025_4; // cos 30°
    const MAX_TRANSLATION: f32 = 0.2;
    let turned = prev_forward.dot(forward) < MAX_ROTATION_COS;
    let moved = match view_distance {
        Some(d) if d.is_finite() && d > 0.0 => {
            prev_position.distance(position) > MAX_TRANSLATION * d
        }
        _ => false,
    };
    turned || moved
}

/// The Halton low-discrepancy sequence — `index`-th term in `base`.
///
/// Shared rather than private because more than one feature wants a jitter
/// sequence and they must not each grow their own: volumetrics jitters its
/// froxel sample point with it and the TAA camera jitter (`crate::taa`) uses
/// it. What is NOT
/// shared is the SPACE each applies it in — NDC pixels for a camera jitter,
/// froxel units for a volume — because that is genuinely per-feature.
pub(crate) fn halton(mut index: u32, base: u32) -> f32 {
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

    let bytes = unsafe { std::slice::from_raw_parts(values.as_ptr() as *const u8, byte_len) };
    buffer[*offset..*offset + byte_len].copy_from_slice(bytes);
    *offset += byte_len;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_guard() {
        let f = Vec3::NEG_Z;
        let d = Some(10.0);
        // Still, and ordinary motion: no jump.
        assert!(!camera_jumped(Vec3::ZERO, f, Vec3::ZERO, f, d));
        assert!(!camera_jumped(
            Vec3::ZERO,
            f,
            Vec3::new(0.5, 0.0, 0.0),
            f,
            d
        ));
        let turned_10 = Vec3::new(10f32.to_radians().sin(), 0.0, -10f32.to_radians().cos());
        assert!(!camera_jumped(Vec3::ZERO, f, Vec3::ZERO, turned_10, d));
        // More than 30° in one frame, or more than 20% of the view distance.
        let turned_45 = Vec3::new(45f32.to_radians().sin(), 0.0, -45f32.to_radians().cos());
        assert!(camera_jumped(Vec3::ZERO, f, Vec3::ZERO, turned_45, d));
        assert!(camera_jumped(Vec3::ZERO, f, Vec3::new(2.5, 0.0, 0.0), f, d));
        // The translation test scales with the scene: the same 2.5-unit step
        // is ordinary motion 100 units away from a large scene.
        assert!(!camera_jumped(
            Vec3::ZERO,
            f,
            Vec3::new(2.5, 0.0, 0.0),
            f,
            Some(100.0)
        ));
        // Unknown scale (no scene bounds) or a degenerate one: rotation only.
        assert!(!camera_jumped(
            Vec3::ZERO,
            f,
            Vec3::new(1.0e6, 0.0, 0.0),
            f,
            None
        ));
        assert!(!camera_jumped(
            Vec3::ZERO,
            f,
            Vec3::new(0.1, 0.0, 0.0),
            f,
            Some(0.0)
        ));
        assert!(camera_jumped(Vec3::ZERO, f, Vec3::ZERO, turned_45, None));
    }
}
