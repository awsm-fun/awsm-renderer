//! 2nd-renderer live material preview: a second `AwsmRenderer` with its own GPU
//! caches (device-scoping makes this possible). A standalone
//! renderer on the Studio's preview canvas renders a sphere lit by a key light,
//! shaded by the current custom material — re-syncing whenever the material is
//! registered. Its dynamic-material ids live on this renderer, separate from the
//! main scene renderer's, so the two never collide.

#![allow(clippy::arc_with_non_send_sync)]

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use awsm_renderer::lights::Light;
use awsm_renderer::materials::pbr::PbrMaterial;
use awsm_renderer::materials::{Material, MaterialAlphaMode, MaterialKey};
use awsm_renderer::raw_mesh::RawMeshData;
use awsm_renderer::transforms::Transform;
use awsm_renderer::{debug::AwsmRendererLogging, AwsmRenderer, AwsmRendererBuilder};
use awsm_renderer_core::command::color::Color;
use awsm_renderer_core::configuration::{
    CanvasAlphaMode, CanvasConfiguration, CanvasToneMappingMode,
};
use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};
use awsm_renderer_materials::MaterialShaderId;
use awsm_renderer_meshgen::sphere_mesh;
use awsm_renderer_web_shared::util::free_camera::FreeCamera as Camera;
use gloo_render::AnimationFrame;
use wasm_bindgen_futures::spawn_local;

use super::bridge::dynamic;
use crate::controller::CustomMaterial;

thread_local! {
    static PREVIEW: RefCell<Option<Arc<PreviewCtx>>> = const { RefCell::new(None) };
}

struct PreviewCtx {
    renderer: Arc<xutex::AsyncMutex<AwsmRenderer>>,
    camera: Arc<Mutex<Camera>>,
    mesh: MeshKeyCell,
    /// The preview renderer's id for the current material (for unregister on edit).
    shader: Mutex<Option<MaterialShaderId>>,
    raf: Mutex<Option<AnimationFrame>>,
    /// Kept so a query can snapshot the preview to PNG (the example sphere).
    canvas: web_sys::HtmlCanvasElement,
}
type MeshKeyCell = Mutex<Option<awsm_renderer::meshes::MeshKey>>;

/// The preview canvas (the material-mode example sphere), if the Studio is
/// mounted. Used by the PNG query seam.
pub fn preview_canvas() -> Option<web_sys::HtmlCanvasElement> {
    PREVIEW.with(|p| p.borrow().as_ref().map(|ctx| ctx.canvas.clone()))
}

/// Mount a preview renderer on `canvas` (idempotent-ish: replaces any prior one).
pub fn mount(canvas: web_sys::HtmlCanvasElement) {
    spawn_local(async move {
        if let Err(e) = build(canvas).await {
            tracing::warn!("material preview build failed: {e}");
        }
    });
}

/// Drop the preview renderer + its RAF (when the Studio unmounts).
pub fn unmount() {
    PREVIEW.with(|p| *p.borrow_mut() = None);
}

/// Re-shade the preview sphere with `mat` (registers it on the preview renderer).
pub fn set_material(mat: Arc<CustomMaterial>) {
    spawn_local(async move {
        let ctx = PREVIEW.with(|p| p.borrow().clone());
        if let Some(ctx) = ctx {
            if let Err(e) = sync_material(&ctx, &mat).await {
                tracing::warn!("material preview sync failed: {e}");
            }
        }
    });
}

async fn build(canvas: web_sys::HtmlCanvasElement) -> Result<(), String> {
    // Size the canvas buffer to its CSS box before building, else the GPU context
    // configures for the default 300×150 and the render target is wrong.
    let w = canvas.client_width().max(1) as u32;
    let h = canvas.client_height().max(1) as u32;
    canvas.set_width(w);
    canvas.set_height(h);
    let aspect = w as f32 / h as f32;

    let renderer = build_renderer(canvas.clone()).await?;
    let renderer = Arc::new(xutex::AsyncMutex::new(renderer));
    let camera = Arc::new(Mutex::new(Camera::new_default_cube(aspect)));

    let mesh = {
        let mut r = renderer.lock().await;
        // A key light so PBR materials read; custom flat materials ignore it.
        let _ = r.insert_light(
            Light::Directional {
                color: [1.0, 1.0, 1.0],
                intensity: 3.0,
                direction: [-0.4, -0.75, -0.55],
            },
            None,
        );
        // A default-PBR sphere; set_material swaps the material in place.
        let tk = r.transforms.insert(Transform::IDENTITY, None);
        let mat_key = insert_default_material(&mut r);
        let raw = preview_sphere();
        let mk = r
            .add_raw_mesh(raw, tk, mat_key)
            .map_err(|e| format!("{e}"))?;
        // Commit the staged preview content (also flips this preview
        // renderer's gate open).
        if let Err(e) = r.commit_load(|_| {}).await {
            tracing::warn!("preview commit_load: {e}");
        }
        mk
    };

    let ctx = Arc::new(PreviewCtx {
        renderer,
        camera,
        mesh: Mutex::new(Some(mesh)),
        shader: Mutex::new(None),
        raf: Mutex::new(None),
        canvas,
    });
    PREVIEW.with(|p| *p.borrow_mut() = Some(ctx.clone()));
    start_raf(ctx.clone());

    // Shade with the Studio's current material immediately, if one is selected.
    if let Some(id) = crate::controller::controller().current_material.get() {
        if let Some(mat) = crate::controller::custom_material::find_material(
            &crate::controller::controller().custom_materials,
            id,
        ) {
            let _ = sync_material(&ctx, &mat).await;
        }
    }
    Ok(())
}

async fn sync_material(ctx: &Arc<PreviewCtx>, mat: &CustomMaterial) -> Result<(), String> {
    let reg = dynamic::build_registration(mat);
    let mut r = ctx.renderer.lock().await;
    // Recompile: drop this material's prior registration on the preview renderer.
    if let Some(old) = ctx.shader.lock().unwrap().take() {
        let _ = r.unregister_material(old);
    }
    let id = r.register_material(reg).map_err(|e| format!("{e}"))?;
    *ctx.shader.lock().unwrap() = Some(id);
    let material = dynamic::build_custom_for_shader(&r, id).ok_or("build custom failed")?;
    let mat_key = insert_material_into(&mut r, material);
    if let Some(mk) = *ctx.mesh.lock().unwrap() {
        let _ = r.set_mesh_material(mk, mat_key);
    }
    if let Err(e) = r.commit_load(|_| {}).await {
        tracing::warn!("preview commit_load: {e}");
    }
    Ok(())
}

fn start_raf(ctx: Arc<PreviewCtx>) {
    let again = ctx.clone();
    let raf = gloo_render::request_animation_frame(move |_| {
        // Stop (and let this renderer drop) once a newer preview supersedes us.
        let current = PREVIEW.with(|p| p.borrow().as_ref().is_some_and(|c| Arc::ptr_eq(c, &again)));
        if !current {
            return;
        }
        render_frame(&again);
        start_raf(again.clone());
    });
    *ctx.raf.lock().unwrap() = Some(raf);
}

fn render_frame(ctx: &Arc<PreviewCtx>) {
    let mut guard = ctx.renderer.try_lock();
    if let Some(r) = guard.as_mut() {
        // Keep the canvas buffer matched to its CSS box. The preview mounts
        // before material-mode layout settles, so the buffer starts 1×1 (the
        // sphere would render to a single pixel); this resizes it once the box
        // has a real size, and again on any panel resize. Done inside the render
        // lock so the surface/texture recreation can't race an in-flight submit.
        let (cw, ch) = (ctx.canvas.client_width(), ctx.canvas.client_height());
        if cw > 0 && ch > 0 && (ctx.canvas.width() != cw as u32 || ctx.canvas.height() != ch as u32)
        {
            ctx.canvas.set_width(cw as u32);
            ctx.canvas.set_height(ch as u32);
            r.gpu.sync_canvas_buffer_with_css();
            ctx.camera.lock().unwrap().set_aspect(cw as f32 / ch as f32);
        }
        let matrices = {
            let c = ctx.camera.lock().unwrap();
            c.matrices()
        };
        let _ = r.update_camera(matrices);
        r.update_transforms();
        let _ = r.render(None);
    }
}

/// Insert a `Material` into a renderer (a real `&mut AwsmRenderer` so the
/// disjoint field borrows compile — they don't through a lock guard inline).
/// `pub(super)` so the thumbnail renderer (sibling module) reuses it.
pub(super) fn insert_material_into(r: &mut AwsmRenderer, material: Material) -> MaterialKey {
    r.materials
        .insert(material, &r.textures, &r.dynamic_materials, &r.extras_pool)
}

fn insert_default_material(r: &mut AwsmRenderer) -> MaterialKey {
    let mut pbr = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
    pbr.base_color_factor = [0.55, 0.6, 0.68, 1.0];
    pbr.metallic_factor = 0.0;
    pbr.roughness_factor = 0.6;
    insert_material_into(r, Material::Pbr(Box::new(pbr)))
}

pub(super) fn preview_sphere() -> RawMeshData {
    let mesh = sphere_mesh(0.85, 48, 32);
    RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uv_sets: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
        ..Default::default()
    }
}

pub(super) async fn build_renderer(
    canvas: web_sys::HtmlCanvasElement,
) -> Result<AwsmRenderer, String> {
    use awsm_renderer::features::{FeatureToggle, RendererFeatures};
    let gpu = web_sys::window().unwrap().navigator().gpu();
    let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas)
        .with_configuration(
            CanvasConfiguration::default()
                .with_alpha_mode(CanvasAlphaMode::Opaque)
                .with_tone_mapping(CanvasToneMappingMode::Standard)
                // COPY_SRC so the preview swapchain is readable via toDataURL
                // (same Chrome requirement as the main canvas — see context.rs).
                .with_usage(
                    awsm_renderer_core::texture::TextureUsage::new()
                        .with_render_attachment()
                        .with_copy_src(),
                ),
        )
        .with_device_request_limits(DeviceRequestLimits::max_all());

    let profile = awsm_renderer_web_shared::perf::resolve_renderer_profile(
        awsm_renderer::profile::RendererProfile::Desktop,
    );
    AwsmRendererBuilder::new(gpu_builder)
        .with_profile(profile)
        .with_logging(AwsmRendererLogging::default())
        .with_clear_color(Color::new_values(0.10, 0.11, 0.13, 1.0))
        .with_features(RendererFeatures {
            // Follows the main viewport's staged reverse-Z rollout (003).
            reverse_z: super::context::reverse_z_flag(),
            gpu_culling: false,
            decals: false,
            coverage_lod: false,
            picking: false,
            lod: false,
            virtual_geometry: false,
            cluster_streaming: false,
            cluster_streaming_budget: None,
            cluster_paging: false,
            indirect_first_instance: FeatureToggle::Auto,
        })
        .build()
        .await
        .map_err(|e| format!("{e}"))
}
