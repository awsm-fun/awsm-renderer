//! Renderer host for the material-editor.
//!
//! Owns the live `AwsmRenderer` + the currently-registered material's
//! `MaterialShaderId`. Implements [`RecompileSink`] so the debounced
//! recompile loop in [`crate::recompile`] can swap in new
//! [`MaterialRegistration`]s as the user edits.
//!
//! The render loop itself lives in `main.rs`'s `spawn_local` — this
//! file just provides the shared state + the registration plumbing.

use std::cell::RefCell;
use std::rc::Rc;

use awsm_materials::dynamic::DynamicMaterial;
use awsm_materials::dynamic_layout::{FieldType, UniformValue};
use awsm_materials::MaterialShaderId;
use awsm_meshgen::primitives::{box_mesh, cylinder_mesh, plane_mesh, sphere_mesh, torus_mesh};
use awsm_renderer::dynamic_materials::MaterialRegistration;
use awsm_renderer::materials::{Material, MaterialKey};
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::{Transform, TransformKey};
use awsm_renderer::AwsmRenderer;
use glam::Vec3;

use crate::recompile::RecompileSink;
use crate::state::PreviewMeshKind;

/// Shared renderer state. The `Option` is `None` between page load
/// and the async `AwsmRendererBuilder::build` completing.
pub type RendererHandle = Rc<RefCell<Option<RendererHost>>>;

/// Owned renderer + current material registration + the stub preview
/// quad's mesh/transform/material handles. The recompile sink
/// reassigns the quad's material on each successful registration so
/// the preview canvas shows the live shader output.
pub struct RendererHost {
    /// The live renderer driving the preview canvas.
    pub renderer: AwsmRenderer,
    /// The shader_id of the most recently successfully-registered
    /// material. `None` between init and the first registration.
    pub current_material: Option<MaterialShaderId>,
    /// `(layout_hash, wgsl_hash)` of the most-recent successful
    /// registration. The sink skips the unregister/register churn
    /// when an incoming `MaterialRegistration` matches both values
    /// exactly.
    pub current_hashes: Option<(u64, u64)>,
    /// Stub preview-quad mesh key. `None` until the first
    /// successful registration; on subsequent registrations the
    /// mesh stays alive and only its material reference flips.
    pub quad_mesh: Option<MeshKey>,
    /// Stub preview-quad transform key. Mirrors `quad_mesh`.
    pub quad_transform: Option<TransformKey>,
    /// The Material::Custom MaterialKey currently bound to the
    /// preview quad. Recreated each registration so the new shader
    /// id takes effect.
    pub quad_material: Option<MaterialKey>,
    /// Latest registration the sink processed — kept so the
    /// `apply_quad_for_current_registration` path can build a
    /// `Material::Custom` with the right default values without
    /// re-reading from the registry on every render frame.
    pub last_registration: Option<MaterialRegistration>,
    /// Preview-mesh kind currently bound to `quad_mesh`. Compared
    /// against the desired kind on each `apply_quad_for_current_registration`
    /// — when they differ the mesh is removed and rebuilt; when they
    /// match only the material is reassigned. `None` until the first
    /// mesh is spawned.
    pub current_preview_mesh: Option<PreviewMeshKind>,
}

impl RendererHost {
    /// Construct a new host wrapping an already-built renderer.
    pub fn new(renderer: AwsmRenderer) -> Self {
        Self {
            renderer,
            current_material: None,
            current_hashes: None,
            quad_mesh: None,
            quad_transform: None,
            quad_material: None,
            last_registration: None,
            current_preview_mesh: None,
        }
    }

    /// Bind the most-recently-registered material to the preview
    /// mesh of the requested `desired_mesh` kind. Called from the
    /// recompile sink after each successful registration AND whenever
    /// the user changes the Preview-pane mesh selector.
    ///
    /// Three cases:
    /// - **First call (no mesh yet)**: spawn mesh + transform +
    ///   `Material::Custom`; record everything.
    /// - **Same `desired_mesh` as before**: rewrite the material
    ///   binding in place via `update_material` so the geometry stays
    ///   put and only the shader flips. This is the dominant case
    ///   (every recompile cycle, user keeps the same shape).
    /// - **Different `desired_mesh`**: remove the old mesh + spawn a
    ///   fresh one. The material key is reused — only the geometry
    ///   changes. (Removing the mesh handles its own transform +
    ///   material cleanup in `AwsmRenderer::remove_mesh`.)
    pub async fn apply_quad_for_current_registration(
        &mut self,
        shader_id: MaterialShaderId,
        reg: &MaterialRegistration,
        desired_mesh: PreviewMeshKind,
    ) -> anyhow::Result<()> {
        let dynamic_material = build_default_dynamic_material(shader_id, reg);
        let mesh_kind_changed = self
            .current_preview_mesh
            .map(|prev| prev != desired_mesh)
            .unwrap_or(true);

        // The in-place `materials.update` fast-path below only swaps the
        // material on the *existing* mesh. But a mesh is built with EITHER
        // opaque visibility geometry OR a transparency vertex pack + a
        // per-mesh forward pipeline, chosen by the material's pass at
        // `add_raw_mesh_transparent` time. If the new material flips
        // transparency-pass-ness (e.g. Opaque → Blend, switching starters),
        // the existing mesh has the wrong geometry + pipeline and the
        // transparent pass fails with `TransparencyGeometryBufferNotFound`.
        // Treat that like a mesh-kind change so the mesh is rebuilt.
        let is_transp = |m: &awsm_materials::MaterialAlphaMode| {
            matches!(
                m,
                awsm_materials::MaterialAlphaMode::Blend
                    | awsm_materials::MaterialAlphaMode::Mask { .. }
            )
        };
        let transparency_changed = self
            .last_registration
            .as_ref()
            .is_some_and(|prev| is_transp(&prev.alpha_mode) != is_transp(&reg.alpha_mode));

        if !mesh_kind_changed && !transparency_changed {
            if let Some(key) = self.quad_material {
                // Steady-state path: same mesh kind, new shader.
                // Rewrite the material in place.
                let renderer = &mut self.renderer;
                renderer.materials.update(
                    key,
                    &renderer.textures,
                    &renderer.dynamic_materials,
                    &renderer.extras_pool,
                    |m| {
                        *m = Material::Custom(Box::new(dynamic_material.clone()));
                    },
                );
                self.last_registration = Some(reg.clone());
                return Ok(());
            }
        }

        // Either first call or the user picked a different mesh
        // kind. If there's an existing mesh + transform + material,
        // remove them first; otherwise spawn fresh.
        if let Some(mesh) = self.quad_mesh.take() {
            // remove_mesh also drops the transform + material binding
            // — see AwsmRenderer::remove_mesh.
            let _ = self.renderer.remove_mesh(mesh);
            self.quad_transform = None;
            self.quad_material = None;
        }

        let (raw, transform) = build_preview_geometry(desired_mesh);

        let transform_key = self.renderer.transforms.insert(transform, None);

        let material_key = {
            let renderer = &mut self.renderer;
            renderer.materials.insert(
                Material::Custom(Box::new(dynamic_material)),
                &renderer.textures,
                &renderer.dynamic_materials,
                &renderer.extras_pool,
            )
        };

        // `add_raw_mesh_transparent` routes by alpha mode: it builds the
        // forward transparent pipeline for Blend/Mask materials and
        // delegates to the sync opaque `add_raw_mesh` otherwise — so this
        // single call correctly handles every preview material.
        let mesh_key = self
            .renderer
            .add_raw_mesh_transparent(raw, transform_key, material_key)
            .await
            .map_err(|e| anyhow::anyhow!("add_raw_mesh_transparent failed: {e:?}"))?;

        self.quad_mesh = Some(mesh_key);
        self.quad_transform = Some(transform_key);
        self.quad_material = Some(material_key);
        self.current_preview_mesh = Some(desired_mesh);
        self.last_registration = Some(reg.clone());
        Ok(())
    }
}

/// Build the raw-mesh + initial-transform pair for a given preview-mesh
/// kind. Sizes / orientations are tuned so the camera-at-(0, 0, 1.5)
/// preview rig (see `main.rs`) frames each shape comfortably.
fn build_preview_geometry(kind: PreviewMeshKind) -> (RawMeshData, Transform) {
    let translation = Vec3::new(0.0, 0.0, -3.0);
    match kind {
        PreviewMeshKind::Plane => {
            // The plane primitive ships its vertices in the XZ plane
            // with normals pointing +Y. We rotate by +90° around X so
            // the normal lands at +Z, pointing back toward the camera
            // at z=+1.5. (Rotating by -90° — the obvious "tilt it
            // forward" direction — pushes the normal to -Z and
            // backface-culls the plane, producing a fully black
            // preview canvas with no pipeline errors. This was the
            // root cause of the earlier "preview never paints" bug.)
            let mesh = plane_mesh(2.0, 2.0, 1, 1);
            let raw = RawMeshData {
                positions: mesh.positions,
                normals: mesh.normals,
                uvs: mesh.uvs,
                colors: mesh.colors,
                indices: mesh.indices,
            };
            (
                raw,
                Transform {
                    translation,
                    rotation: glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                    ..Default::default()
                },
            )
        }
        PreviewMeshKind::Sphere => {
            let mesh = sphere_mesh(1.0, 32, 16);
            let raw = RawMeshData {
                positions: mesh.positions,
                normals: mesh.normals,
                uvs: mesh.uvs,
                colors: mesh.colors,
                indices: mesh.indices,
            };
            (
                raw,
                Transform {
                    translation,
                    ..Default::default()
                },
            )
        }
        PreviewMeshKind::Box => {
            let mesh = box_mesh(Vec3::splat(1.5));
            let raw = RawMeshData {
                positions: mesh.positions,
                normals: mesh.normals,
                uvs: mesh.uvs,
                colors: mesh.colors,
                indices: mesh.indices,
            };
            // Slight rotation so all three visible faces are visible —
            // otherwise a face-on view hides the silhouette info that
            // makes a box more interesting than a plane.
            (
                raw,
                Transform {
                    translation,
                    rotation: glam::Quat::from_euler(
                        glam::EulerRot::YXZ,
                        std::f32::consts::FRAC_PI_4,
                        -std::f32::consts::FRAC_PI_8,
                        0.0,
                    ),
                    ..Default::default()
                },
            )
        }
        PreviewMeshKind::Cylinder => {
            let mesh = cylinder_mesh(0.75, 2.0, 32);
            let raw = RawMeshData {
                positions: mesh.positions,
                normals: mesh.normals,
                uvs: mesh.uvs,
                colors: mesh.colors,
                indices: mesh.indices,
            };
            // Tilt slightly so the curved side dominates the view.
            (
                raw,
                Transform {
                    translation,
                    rotation: glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_8),
                    ..Default::default()
                },
            )
        }
        PreviewMeshKind::Torus => {
            let mesh = torus_mesh(1.0, 0.3, 48, 24);
            let raw = RawMeshData {
                positions: mesh.positions,
                normals: mesh.normals,
                uvs: mesh.uvs,
                colors: mesh.colors,
                indices: mesh.indices,
            };
            (
                raw,
                Transform {
                    translation,
                    rotation: glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_3),
                    ..Default::default()
                },
            )
        }
    }
}

/// Build a `DynamicMaterial` whose uniform values come from the
/// registration's authored defaults when present, falling back to
/// type-zero for any uniform the registration didn't supply a
/// default for. Texture / buffer slots are left unbound — the WGSL
/// fallback paths (texture index `u32::MAX`, buffer length 0)
/// handle that gracefully.
///
/// The `uniform_defaults` indirection matters: without it, every
/// preview-quad uniform reads zero, which for a material like
/// `scanline` (where `scan_freq=80`, `tint=(0.6, 0.9, 0.6)` are
/// declared on the schema side) flattens the visible output to the
/// uniform base colour with no scanline pattern visible.
fn build_default_dynamic_material(
    shader_id: MaterialShaderId,
    reg: &MaterialRegistration,
) -> DynamicMaterial {
    let defaults: Vec<UniformValue> = reg
        .layout
        .uniforms
        .iter()
        .enumerate()
        .map(|(i, f)| {
            reg.uniform_defaults
                .get(i)
                .cloned()
                .filter(|v| v.field_type() == f.ty)
                .unwrap_or_else(|| default_uniform_value(f.ty))
        })
        .collect();
    DynamicMaterial::new(
        shader_id,
        reg.alpha_mode,
        reg.double_sided,
        &reg.layout,
        defaults,
    )
}

fn default_uniform_value(ty: FieldType) -> UniformValue {
    match ty {
        FieldType::F32 => UniformValue::F32(0.0),
        FieldType::Vec2 => UniformValue::Vec2([0.0; 2]),
        FieldType::Vec3 => UniformValue::Vec3([0.0; 3]),
        FieldType::Vec4 => UniformValue::Vec4([0.0; 4]),
        FieldType::U32 => UniformValue::U32(0),
        FieldType::IVec2 => UniformValue::IVec2([0; 2]),
        FieldType::IVec3 => UniformValue::IVec3([0; 3]),
        FieldType::IVec4 => UniformValue::IVec4([0; 4]),
        FieldType::Mat3 => UniformValue::Mat3([0.0; 9]),
        FieldType::Mat4 => UniformValue::Mat4([0.0; 16]),
        // Color3/Color4 default to opaque white so a freshly-loaded
        // material isn't invisible against a black clear.
        FieldType::Color3 => UniformValue::Color3([1.0, 1.0, 1.0]),
        FieldType::Color4 => UniformValue::Color4([1.0, 1.0, 1.0, 1.0]),
        FieldType::Bool => UniformValue::Bool(false),
    }
}

/// Sink wrapping a [`RendererHandle`] that the recompile loop drives.
///
/// On `try_apply`:
/// 1. If a previous material was registered, attempt to unregister
///    it. (Failures here are logged but non-fatal — registration
///    will overwrite.)
/// 2. Call `register_material` with the new payload. On success,
///    record the new shader_id as `current_material`. On
///    `WgslCompile` error, leave `current_material` untouched so the
///    preview continues drawing the last-good shader.
/// 3. Call `prewarm_pipelines()` so the classify + per-shader-id
///    opaque pipelines are warm before the next render frame.
pub struct RendererRecompileSink {
    handle: RendererHandle,
    /// Clone of `EditState.preview_mesh` so the sink can read the
    /// currently-selected preview-mesh kind at the moment of apply.
    /// Mutating the selector triggers a debounce-window recompile via
    /// the spawn loop in `recompile.rs`; this field carries the kind
    /// across into the host.
    preview_mesh: std::sync::Arc<futures_signals::signal::Mutable<crate::state::PreviewMeshKind>>,
}

impl RendererRecompileSink {
    /// Construct a sink wrapping the shared renderer handle + a clone
    /// of the editor's `preview_mesh` selector.
    pub fn new(
        handle: RendererHandle,
        preview_mesh: std::sync::Arc<
            futures_signals::signal::Mutable<crate::state::PreviewMeshKind>,
        >,
    ) -> Self {
        Self {
            handle,
            preview_mesh,
        }
    }
}

// Prewarm runs as an async fire-and-forget — it acquires the
// RefCell borrow, drives `prewarm_pipelines().await`, then releases.
// We INTENTIONALLY hold the borrow for the entire await: the wasm32
// runtime is single-threaded so the lint's cross-thread concern is
// moot, and the RAF tick's `try_borrow_mut` returning `Err` during
// compile is the desired behavior — we don't want to render a frame
// while pipelines are still being compiled (the dispatch would skip
// or use a stale per-shader-id pipeline).
#[allow(clippy::await_holding_refcell_ref)]
async fn prewarm_holding_borrow(handle: RendererHandle) {
    let mut guard = handle.borrow_mut();
    if let Some(host) = guard.as_mut() {
        if let Err(e) = host.renderer.wait_for_pipelines_ready().await {
            tracing::warn!("[material-editor] wait_for_pipelines_ready failed: {e:?}");
        }
    }
}

impl RecompileSink for RendererRecompileSink {
    fn try_apply<'a>(
        &'a mut self,
        reg: MaterialRegistration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + 'a>> {
        Box::pin(self.try_apply_inner(reg))
    }
}

impl RendererRecompileSink {
    /// Inherent async body for [`RecompileSink::try_apply`]. Split out so
    /// the trait method can box the future (`async fn` in traits isn't
    /// dyn-compatible). We hold the renderer's `RefCell` borrow across the
    /// transparent-pipeline compile await inside
    /// `apply_quad_for_current_registration`; wasm32 is single-threaded so
    /// the cross-thread `await_holding_refcell_ref` concern is moot, and
    /// the RAF tick skipping a frame while we compile is desired (same
    /// rationale as `prewarm_holding_borrow`).
    #[allow(clippy::await_holding_refcell_ref)]
    async fn try_apply_inner(&mut self, reg: MaterialRegistration) -> Result<(), String> {
        // A fire-and-forget `prewarm_holding_borrow` spawned by a *previous*
        // compile may still be holding the renderer borrow across its own
        // `wait_for_pipelines_ready().await`. This path is driven by user
        // keystrokes (debounced recompile), so colliding with that in-flight
        // prewarm is plausible — a plain `borrow_mut()` would *panic* (single
        // threaded, so it can't block). Acquire via `try_borrow_mut` instead,
        // yielding to the event loop until the prewarm releases. Yield-and-
        // retry rather than skip-this-edit: the user's latest edit must still
        // apply. The borrow is free in the overwhelmingly common case, so this
        // loop runs exactly once.
        let mut guard = loop {
            match self.handle.try_borrow_mut() {
                Ok(guard) => break guard,
                Err(_) => gloo_timers::future::TimeoutFuture::new(0).await,
            }
        };
        let host = match guard.as_mut() {
            Some(h) => h,
            None => {
                // Renderer not yet booted. Defer silently — the next
                // edit after boot will pick this up.
                return Ok(());
            }
        };

        // Idempotency gate: if the (layout_hash, wgsl_hash) matches
        // the active registration, the material itself is unchanged.
        // But the trigger ALSO fires on preview-mesh selector changes
        // — so before the early-return, check whether the desired
        // preview-mesh kind drifted; if so, re-apply with the existing
        // material so the geometry updates without churning the
        // pipeline cache.
        let incoming_hashes = (reg.layout_hash, reg.wgsl_hash);
        let desired_mesh = self.preview_mesh.get();
        if host.current_hashes == Some(incoming_hashes) {
            if host.current_preview_mesh == Some(desired_mesh) {
                return Ok(());
            }
            if let (Some(id), Some(prev_reg)) =
                (host.current_material, host.last_registration.clone())
            {
                if let Err(e) = host
                    .apply_quad_for_current_registration(id, &prev_reg, desired_mesh)
                    .await
                {
                    return Err(format!("apply_quad (mesh-only): {e}"));
                }
            }
            return Ok(());
        }

        // Unregister the previous material FIRST so the registry's
        // by-name uniqueness check passes when we re-register under
        // the same name (the common case — only the WGSL body
        // changed). The brief window where the registry has no entry
        // for the old id is safe because we hold the renderer's
        // RefCell borrow_mut for the entire try_apply, so the RAF
        // tick can't sneak a render in between.
        if let Some(prev_id) = host.current_material.take() {
            if let Err(e) = host.renderer.unregister_material(prev_id) {
                tracing::warn!(
                    "[material-editor] unregister_material({:?}) failed: {e:?}",
                    prev_id
                );
            }
        }

        // Register the new material. WgslCompile errors propagate
        // back through the recompile sink as Err strings, and we
        // leave `current_hashes` untouched so the previous
        // last-good material keeps drawing. We clone the registration
        // because `register_material` consumes it but we also need
        // the layout for the preview quad's default uniform values.
        let reg_for_quad = reg.clone();
        let new_id = match host.renderer.register_material(reg) {
            Ok(id) => id,
            Err(e) => {
                return Err(format!("{e}"));
            }
        };
        host.current_material = Some(new_id);
        host.current_hashes = Some(incoming_hashes);

        // Bind the new shader to the preview quad. On first
        // registration this spawns the plane + transform + material;
        // on subsequent registrations only the material flips.
        // Failures here surface back as Err so the editor's errors
        // pane shows them rather than silently rendering a stale
        // shader.
        if let Err(e) = host
            .apply_quad_for_current_registration(new_id, &reg_for_quad, desired_mesh)
            .await
        {
            return Err(format!("apply_quad: {e}"));
        }

        // prewarm_pipelines is async; for the editor's preview
        // it's fine to fire-and-forget on the JS event loop. The
        // next render frame after compilation completes picks up
        // the new pipelines.
        //
        // We hold the RefCell borrow for the ENTIRE
        // prewarm_pipelines await — there's no clean way to split
        // the borrow across the internal shader / pipeline compile
        // awaits without making AwsmRenderer split-refable. The
        // RAF render-loop's try_borrow_mut skips frames for the
        // compile duration (a few hundred ms on cold cache) which
        // is acceptable for the preview's use case. Clippy's
        // `await_holding_refcell_ref` warning is technically right
        // about cross-thread misuse but wasm32 is single-threaded.
        let handle = self.handle.clone();
        wasm_bindgen_futures::spawn_local(prewarm_holding_borrow(handle));

        Ok(())
    }
}
