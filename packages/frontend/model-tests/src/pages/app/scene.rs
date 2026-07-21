pub mod editor;
mod ibl;
mod skybox;

use std::cell::Cell;
use std::collections::HashMap;

use awsm_renderer::bounds::Aabb;
use awsm_renderer::core::command::color::Color;
use awsm_renderer::core::cubemap::images::CubemapBitmapColors;
use awsm_renderer::core::cubemap::CubemapImage;
use awsm_renderer::environment::Skybox;
use awsm_renderer::lights::ibl::Ibl;
use awsm_renderer::lights::ibl::IblTexture;
use awsm_renderer::lights::Light;
use awsm_renderer::lights::LightKey;
use awsm_renderer::transforms::TransformKey;
use awsm_renderer_gltf::data::GltfData;
use awsm_renderer_gltf::loader::GltfLoader;
use awsm_renderer_gltf::AwsmRendererGltfExt;

use awsm_renderer::materials::Material;
use awsm_renderer::picker::PickResult;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_web_shared::util::free_camera::{FreeCamera as Camera, ProjectionMode};
use awsm_renderer_web_shared::viewport3d::transform_controller::{
    GizmoSpace, TransformController, TransformObject,
};
use awsm_web::dom::resize::ResizeObserver;
use gloo_events::EventListener;
use wasm_bindgen_futures::spawn_local;
use web_sys::PointerEvent;

use crate::models::collections::GltfId;
use crate::pages::app::context::{IblId, SkyboxId};
use crate::pages::app::scene::editor::AppSceneEditor;
use crate::pages::app::sidebar::material::FragmentShaderKind;
use crate::prelude::*;

use super::context::AppContext;

/// §C.1 perf bench: parse `?stress=N` from the URL (the count of duplicate meshes
/// to spawn in a grid). `None` when the param is absent / unparseable / zero, so
/// the bench is fully inert in normal use.
fn stress_grid_count() -> Option<u32> {
    let search = web_sys::window()?.location().search().ok()?;
    let query = search.trim_start_matches('?');
    for pair in query.split('&') {
        if let Some(val) = pair.strip_prefix("stress=") {
            return val.parse::<u32>().ok().filter(|n| *n > 0);
        }
    }
    None
}

/// `?lod` — enable the renderer's discrete-LOD feature so a loaded bundle's
/// baked level chains (`<id>.lod{N}.glb` + `.lod.toml`) drive per-instance
/// screen-error level selection. Default off ⇒ byte-identical to today.
pub fn lod_enabled() -> bool {
    let Some(search) = web_sys::window().and_then(|w| w.location().search().ok()) else {
        return false;
    };
    let q = search.trim_start_matches('?');
    q.split('&').any(|p| p == "lod" || p.starts_with("lod="))
}

/// `?ourformat=1` — route the load through the TWO-STAGE our-format path (§0):
/// STAGE 1 import the source glTF → our clean glb (GPU-free `reexport_clean`),
/// STAGE 2 materialise that clean glb via `populate_gltf`. Opt-in dev toggle so the
/// default stays the direct glTF path (default-equals-today). NOTE: `reexport_clean`
/// currently drops animations + lights + the KHR_* material extensions (clearcoat /
/// sheen / transmission / iridescence / anisotropy / …), so under this flag those
/// samples regress — the routing infra is proven on static core-PBR samples; the
/// animation-remap + KHR_* round-trip are the remaining Phase-5 work.
fn our_format_enabled() -> bool {
    let Some(search) = web_sys::window().and_then(|w| w.location().search().ok()) else {
        return false;
    };
    let query = search.trim_start_matches('?');
    query
        .split('&')
        .any(|pair| matches!(pair.strip_prefix("ourformat="), Some("1") | Some("true")))
}

/// STAGE 1 (GPU-free): import a source glTF into our clean glb + re-parse it back to
/// `GltfData` so STAGE 2 (`populate_gltf`) materialises OUR format, not glTF directly.
/// Returns the original data unchanged on any failure (so the toggle never bricks a
/// load — it just falls back to the direct path + logs).
async fn import_to_our_format(data: &GltfData) -> anyhow::Result<GltfData> {
    // Pass the loader's retained ENCODED image bytes so EXTERNAL-file textures
    // (the glTF/ sample variant) re-embed into the clean glb.
    let clean = awsm_renderer_glb_export::reexport_clean_scene_with_images(
        &data.doc,
        &data.buffers.raw,
        &data.encoded_images,
    )
    .ok_or_else(|| anyhow::anyhow!("reexport_clean produced no scene"))?;
    let glb = awsm_renderer_glb_export::write_glb(&clean);
    let loader = GltfLoader::from_glb_bytes(&glb).await?;
    loader.into_data(None).map_err(|e| anyhow::anyhow!("{e}"))
}

/// GAP 2: `reexport_clean` drops animations + DFS-renumbers nodes, so a
/// model routed through `?ourformat=1` renders at bind pose. Re-load the ORIGINAL doc's
/// animations onto the clean glb's transforms: extract each channel (original node
/// index), remap original→clean(flat) via `scene_node_flat_indices`, resolve the clean
/// transform via the populate context, and insert a loose player — exactly the binding
/// `populate_gltf` does for the direct path, just with remapped targets. Mirrors the
/// player's "clips loaded separately from the geometry glb" model (clips are a sidecar,
/// not baked into the clean glb — keeping the editor/player rig glb animation-free).
fn load_remapped_animations(
    renderer: &mut AwsmRenderer,
    original: &GltfData,
    ctx: &awsm_renderer_gltf::GltfPopulateContext,
) {
    use awsm_renderer::animation::AnimationPlayer;
    use awsm_renderer_gltf::ExtractedProperty;

    let anims = match awsm_renderer_gltf::extract_animations(&original.doc, &original.buffers.raw) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("ourformat: extract_animations failed: {e}");
            return;
        }
    };
    // original glTF node index → clean glb node index (DFS flatten == write order).
    let flat_of = awsm_renderer_glb_export::scene_node_flat_indices(&original.doc);
    // clean glb node index → renderer TransformKey (from the clean populate).
    let node_to_tk = ctx
        .key_lookups
        .lock()
        .unwrap()
        .node_index_to_transform
        .clone();

    let (mut inserted, mut skipped_morph, mut unresolved) = (0u32, 0u32, 0u32);
    for anim in anims {
        for ch in anim.channels {
            let Some(tk) = flat_of
                .get(&ch.node_index)
                .and_then(|clean| node_to_tk.get(clean))
                .copied()
            else {
                unresolved += 1;
                continue;
            };
            match ch.property {
                ExtractedProperty::Translation
                | ExtractedProperty::Rotation
                | ExtractedProperty::Scale => {
                    renderer
                        .animations
                        .insert_transform(AnimationPlayer::new(ch.clip), tk);
                    inserted += 1;
                }
                // Morph-weight channels need the node's mesh morph key — a follow-up
                // (Fox + the common animated samples are T/R/S; not silently lost —
                // counted + logged).
                ExtractedProperty::MorphWeights => skipped_morph += 1,
            }
        }
    }
    tracing::info!(
        "ourformat: loaded {inserted} transform anim channels ({skipped_morph} morph skipped, {unresolved} unresolved)"
    );
}

/// Diagnostic bench: parse `?variants=M` — with `?stress=N`, assign M distinct
/// first-party PBR feature-mask variants round-robin across the stress meshes (each
/// a clone of the source material with a different texture subset turned off, so
/// each is a distinct `PbrFeatures` mask → distinct variant pipeline). Used to
/// investigate whether a scene with many distinct PBR variants renders correctly.
/// `None` when absent / `<= 1`, so it is fully inert in normal use.
fn variant_count() -> Option<u32> {
    let search = web_sys::window()?.location().search().ok()?;
    let query = search.trim_start_matches('?');
    for pair in query.split('&') {
        if let Some(val) = pair.strip_prefix("variants=") {
            return val.parse::<u32>().ok().filter(|n| *n > 1);
        }
    }
    None
}

pub struct AppScene {
    pub ctx: AppContext,
    pub renderer: Arc<futures::lock::Mutex<AwsmRenderer>>,
    pub editor: Mutex<Option<editor::AppSceneEditor>>,
    pub gltf_cache: Mutex<HashMap<GltfId, GltfLoader>>,
    pub latest_gltf_data: Mutex<Option<GltfData>>,
    pub ibl_cache: Mutex<HashMap<IblId, Ibl>>,
    pub skybox_by_ibl_cache: Mutex<HashMap<IblId, Skybox>>,
    pub camera: Arc<Mutex<Option<Camera>>>,
    pub resize_observer: Mutex<Option<ResizeObserver>>,
    pub request_animation_frame: Mutex<Option<gloo_render::AnimationFrame>>,
    pub last_request_animation_frame: Cell<Option<f64>>,
    pub event_listeners: Mutex<Vec<EventListener>>,
    lights: Mutex<Option<Vec<LightKey>>>,
    /// Lights inserted by `populate_gltf` from `KHR_lights_punctual`. When
    /// non-empty, we skip the default directional fill in
    /// `reset_punctual_lights` so the model's own lighting drives the scene.
    gltf_punctual_lights: Mutex<Vec<LightKey>>,
    /// `glTF node index -> TransformKey` from the last populate, kept so
    /// `reset_punctual_lights` can re-bind re-inserted lights to their
    /// animated nodes (the transform tree is stable across light
    /// re-inserts). Without this, toggling the punctual-lights mode would
    /// freeze animated lights like the DiffuseTransmissionPlant fireflies.
    gltf_node_transforms: Mutex<HashMap<usize, TransformKey>>,
    move_action: Cell<Option<MoveAction>>,
    last_size: Cell<(f64, f64)>,
    last_camera_id: Cell<ProjectionMode>,
    last_shader_kind: Cell<Option<FragmentShaderKind>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MoveAction {
    CameraMoving,
    GizmoTransforming,
}

impl AppScene {
    pub async fn new(ctx: AppContext, renderer: AwsmRenderer) -> Result<Arc<Self>> {
        let canvas = renderer.gpu.canvas();

        let state = Arc::new(Self {
            ctx,
            renderer: Arc::new(futures::lock::Mutex::new(renderer)),
            gltf_cache: Mutex::new(HashMap::new()),
            ibl_cache: Mutex::new(HashMap::new()),
            skybox_by_ibl_cache: Mutex::new(HashMap::new()),
            latest_gltf_data: Mutex::new(None),
            camera: Arc::new(Mutex::new(None)),
            resize_observer: Mutex::new(None),
            request_animation_frame: Mutex::new(None),
            last_request_animation_frame: Cell::new(None),
            event_listeners: Mutex::new(Vec::new()),
            last_size: Cell::new((0.0, 0.0)),
            last_camera_id: Cell::new(ProjectionMode::Orthographic),
            last_shader_kind: Cell::new(None),
            editor: Mutex::new(None),
            move_action: Cell::new(None),
            lights: Mutex::new(None),
            gltf_punctual_lights: Mutex::new(Vec::new()),
            gltf_node_transforms: Mutex::new(HashMap::new()),
        });

        let resize_observer = ResizeObserver::new(
            clone!(canvas, state => move |entries| {
                if let Some(entry) = entries.first() {
                    let width = entry.content_box_sizes[0].inline_size;
                    let height = entry.content_box_sizes[0].block_size;
                    canvas.set_width(width);
                    canvas.set_height(height);

                    state.on_viewport_change();
                }
            }),
            None,
        );

        resize_observer.observe(&canvas);

        *state.resize_observer.lock().unwrap() = Some(resize_observer);

        let event_listeners = vec![
            EventListener::new(
                &canvas,
                "pointerdown",
                clone!(state => move |event| {
                    spawn_local(clone!(state, event => async move {
                        let mut renderer = state.renderer.lock().await;
                        let event = event.unchecked_into::<PointerEvent>();
                        let (x, y) = renderer.gpu.pointer_event_to_canvas_coords_i32(&event);

                        // 1) Analytic gizmo grab takes priority (CPU ray-cast,
                        //    tolerance band — no GPU pick needed).
                        let grabbed = {
                            let editor_guard = state.editor.lock().unwrap();
                            match editor_guard.as_ref() {
                                Some(editor) => {
                                    let mut tc = editor.transform_controller.lock().unwrap();
                                    match tc.as_mut() {
                                        Some(tc) => tc.try_grab(&mut renderer, x, y).is_some(),
                                        None => false,
                                    }
                                }
                                None => false,
                            }
                        };
                        if grabbed {
                            state.move_action.set(Some(MoveAction::GizmoTransforming));
                            return;
                        }

                        // 2) Otherwise GPU-pick → object selection.
                        match renderer.pick(x,y).await {
                            Err(err) => {
                                tracing::error!("Pick error: {:?}", err);
                            }
                            Ok(PickResult::Hit(mesh_key)) => {
                                if let Ok(mesh) = renderer.meshes.get(mesh_key) {
                                    let obj = TransformObject {
                                        key: mesh.transform_key,
                                        instance: mesh.instanced.then_some(0),
                                    };
                                    if let Some(editor) = state.editor.lock().unwrap().as_ref() {
                                        editor.selected_object.set_neq(Some(obj));
                                    }
                                }
                            }
                            Ok(_) => {}
                        }

                        if let Some(camera) = state.camera.lock().unwrap().as_mut() {
                            camera.on_pointer_down();
                        }
                        state.move_action.set(Some(MoveAction::CameraMoving));
                    }));
                }),
            ),
            EventListener::new(
                &web_sys::window().unwrap(),
                "pointermove",
                clone!(state => move |event| {
                    let event = event.unchecked_ref::<web_sys::PointerEvent>();
                    match state.move_action.get() {
                        Some(MoveAction::GizmoTransforming) => {
                            spawn_local(clone!(state, event => async move {
                                let mut renderer = state.renderer.lock().await;
                                if let Some(editor) = state.editor.lock().unwrap().as_mut() {
                                    if let Some(transform_controller) = editor.transform_controller.lock().unwrap().as_mut() {
                                        transform_controller.update_transform(&mut renderer, event.movement_x(), event.movement_y());
                                    }
                                }
                            }));
                        }
                        Some(MoveAction::CameraMoving) => {
                            if let Some(camera) = state.camera.lock().unwrap().as_mut() {
                                camera.on_pointer_move(event.movement_x(), event.movement_y(), false);
                            }
                        }
                        None => {}
                    }
                }),
            ),
            EventListener::new(
                &web_sys::window().unwrap(),
                "pointerup",
                clone!(state => move |_event| {

                    if let Some(camera) = state.camera.lock().unwrap().as_mut() {
                        camera.on_pointer_up();
                    }
                    // End any in-flight gizmo drag (clears drag state + highlight).
                    if let Some(editor) = state.editor.lock().unwrap().as_ref() {
                        if let Some(tc) = editor.transform_controller.lock().unwrap().as_mut() {
                            tc.end_drag();
                        }
                    }
                    state.move_action.set(None);

                }),
            ),
            EventListener::new(
                &canvas,
                "wheel",
                clone!(state => move |event| {
                    if let Some(camera) = state.camera.lock().unwrap().as_mut() {
                        let event = event.unchecked_ref::<web_sys::WheelEvent>();
                        camera.on_wheel(event.delta_y());
                    }
                }),
            ),
        ];

        *state.event_listeners.lock().unwrap() = event_listeners;

        spawn_local(clone!(state => async move {
            state.ctx.camera_id.signal().for_each(clone!(state => move |_| clone!(state => async move {
                state.on_viewport_change();
            }))).await;
        }));

        spawn_local(clone!(state => async move {
            state.ctx.ibl_id.signal().for_each(clone!(state => move |ibl_id| clone!(state => async move {
                state.load_ibl(ibl_id).await;

                match state.ctx.skybox_id.get() {
                    SkyboxId::None => { /* do nothing */}
                    id => {
                        state.load_skybox(id).await;
                    },
                }
            }))).await;
        }));

        spawn_local(clone!(state => async move {
            state.ctx.skybox_id.signal().for_each(clone!(state => move |skybox_id| clone!(state => async move {
                match skybox_id  {
                    SkyboxId::None => { /* do nothing */}
                    id => {
                        state.load_skybox(id).await;
                    },
                }

            }))).await;
        }));

        spawn_local(clone!(state => async move {
            state.ctx.skybox_id.signal().for_each(clone!(state => move |skybox_id| clone!(state => async move {
                state.load_skybox(skybox_id).await;
            }))).await;
        }));

        Ok(state)
    }

    fn on_viewport_change(self: &Arc<Self>) {
        let state = self;

        spawn_local(clone!(state => async move {
            let last_size = state.last_size.get();
            let last_camera_id = state.last_camera_id.get();
            let camera_id = state.ctx.camera_id.get();

            {
                let renderer = state.renderer.lock().await;
                let (canvas_width, canvas_height) = renderer.gpu.canvas_size(false);
                if (canvas_width, canvas_height) == last_size && camera_id == last_camera_id {
                    return;
                }
                state.last_size.set((canvas_width, canvas_height));
                state.last_camera_id.set(camera_id);
            }


            if let Err(err) = state.setup_viewport().await {
                tracing::error!("Failed to setup scene after canvas resize: {:?}", err);
            }

            if let Err(err) = state.render().await {
                tracing::error!("Failed to render after canvas resize: {:?}", err);
            }
        }));
    }

    pub async fn clear(self: &Arc<Self>) {
        let state = self;

        state.stop_animation_loop();
        if let Err(err) = state.renderer.lock().await.remove_all().await {
            tracing::error!("Failed to clear renderer: {:?}", err);
        }

        match AppSceneEditor::new(
            state.renderer.clone(),
            state.ctx.editor_grid_enabled.clone(),
            state.ctx.editor_gizmo_translation_enabled.clone(),
            state.ctx.editor_gizmo_rotation_enabled.clone(),
            state.ctx.editor_gizmo_scale_enabled.clone(),
        )
        .await
        {
            Ok(editor) => {
                *state.editor.lock().unwrap() = Some(editor);
            }
            Err(err) => {
                tracing::error!("Failed to recreate scene editor after clear: {:?}", err);
            }
        }

        if let Err(err) = self.render().await {
            tracing::error!("Failed to render after clear: {:?}", err);
        }
    }

    pub async fn render(self: &Arc<Self>) -> Result<()> {
        let state = self;
        let mut renderer = state.renderer.lock().await;

        // Surface background pipeline-scheduler compiles in the loading
        // overlay so a frame that is still compiling never reads as a black
        // hang. Drive the count from the scheduler's AUTHORITATIVE
        // `materials_pending` rather than a +1/-1 event tally: the push
        // tally leaked once the specialize-only relaunch began re-marking
        // already-pending materials `Pending` (more +1s than -1s), so the
        // "N remaining" banner never reached 0. The authoritative count is
        // recomputed from scheduler state each frame, so it self-corrects
        // and drains to 0 exactly when every material is Ready/Failed.
        // (Mirrors the scene-editor renderer_bridge fix.) The event queue
        // is still drained so it stays bounded.
        let _ = renderer.drain_pipeline_status_events();
        let pending = renderer.compile_progress().materials_pending;
        if state.ctx.loading_status.lock_ref().compile_pending != pending {
            state.ctx.loading_status.lock_mut().compile_pending = pending;
        }

        let editor_guard = state.editor.lock().unwrap();
        let hooks = editor_guard
            .as_ref()
            .and_then(|editor| editor.render_hooks.read().unwrap().clone());

        Ok(renderer.render(hooks.as_deref())?)
    }

    pub async fn load_gltf(self: &Arc<Self>, gltf_id: GltfId) -> Option<GltfLoader> {
        async fn inner(scene: &Arc<AppScene>, gltf_id: GltfId) -> Result<GltfLoader> {
            if let Some(loader) = scene
                .gltf_cache
                .lock()
                .unwrap()
                .get(&gltf_id)
                .map(|loader| loader.heavy_clone())
            {
                return Ok(loader);
            }

            // bypass_http_cache = false: the model tester loads stable published
            // model URLs and keeps its own in-memory cache above — normal browser
            // HTTP caching is fine (and desirable) here.
            let loader = GltfLoader::load(&gltf_id.url(), None, false).await?;

            scene
                .gltf_cache
                .lock()
                .unwrap()
                .insert(gltf_id, loader.heavy_clone());

            Ok(loader)
        }

        self.ctx.loading_status.lock_mut().gltf_net = Ok(true);
        let t_start = web_sys::js_sys::Date::now();
        match inner(self, gltf_id).await {
            Ok(loader) => {
                let dt_ms = web_sys::js_sys::Date::now() - t_start;
                // Single, easily-greppable line so cold-boot
                // success is visible at a glance. Cache-hit loads
                // typically log <1ms; first-load times surface the
                // network + parse + upload wall-clock.
                tracing::info!("[scene] model loaded: {:?} ({:.0}ms)", gltf_id, dt_ms);
                self.ctx.loading_status.lock_mut().gltf_net = Ok(false);
                Some(loader)
            }
            Err(err) => {
                tracing::error!("Failed to load GLTF {:?}: {:?}", gltf_id, err);
                self.ctx.loading_status.lock_mut().gltf_net = Err(err.to_string());
                None
            }
        }
    }

    async fn load_skybox(self: &Arc<Self>, skybox_id: SkyboxId) {
        async fn inner(scene: &Arc<AppScene>, skybox_id: SkyboxId) -> Result<()> {
            let ibl_id = match skybox_id {
                SkyboxId::SameAsIbl => scene.ctx.ibl_id.get_cloned(),
                SkyboxId::SpecificIbl(ibl_id) => ibl_id,
                SkyboxId::None => return Ok(()),
            };
            let skybox = {
                let maybe_cached = {
                    // need to drop this lock before awaiting
                    scene
                        .skybox_by_ibl_cache
                        .lock()
                        .unwrap()
                        .get(&ibl_id)
                        .cloned()
                };
                match maybe_cached {
                    Some(skybox) => skybox,
                    None => {
                        let skybox_cubemap = match ibl_id {
                            IblId::PhotoStudio => skybox::load_from_path("photo_studio").await?,
                            IblId::AllWhite => {
                                skybox::load_from_colors(CubemapBitmapColors::all(Color::WHITE))
                                    .await?
                            }
                            IblId::SimpleSky => skybox::load_simple_sky().await?,
                        };

                        let skybox = {
                            let (texture, view, mip_count) = {
                                let renderer = &mut *scene.renderer.lock().await;
                                skybox_cubemap
                                    .create_texture_and_view(&renderer.gpu, Some("Skybox"))
                                    .await?
                            };

                            {
                                let renderer = &mut *scene.renderer.lock().await;
                                let key = renderer.textures.insert_cubemap(texture);

                                let sampler_key = renderer
                                    .textures
                                    .get_sampler_key(&renderer.gpu, Skybox::sampler_cache_key())?;

                                let sampler = renderer.textures.get_sampler(sampler_key)?.clone();

                                Skybox::new(key, view, sampler, mip_count)
                            }
                        };

                        scene
                            .skybox_by_ibl_cache
                            .lock()
                            .unwrap()
                            .insert(ibl_id, skybox.clone());

                        skybox
                    }
                }
            };

            scene.renderer.lock().await.set_skybox(skybox);

            Ok(())
        }

        self.ctx.loading_status.lock_mut().skybox = Ok(true);
        match inner(self, skybox_id).await {
            Ok(()) => {
                self.ctx.loading_status.lock_mut().skybox = Ok(false);
            }
            Err(err) => {
                tracing::error!("Failed to load Skybox {:?}: {:?}", skybox_id, err);
                self.ctx.loading_status.lock_mut().skybox = Err(err.to_string());
            }
        }
    }

    pub async fn wait_for_skybox_loaded(self: &Arc<Self>) {
        loop {
            let skybox_id = self.ctx.skybox_id.get_cloned();
            let skybox_loaded = {
                match skybox_id {
                    SkyboxId::None => true,
                    SkyboxId::SameAsIbl => {
                        let ibl_id = self.ctx.ibl_id.get_cloned();
                        let skybox_cache = self.skybox_by_ibl_cache.lock().unwrap();
                        skybox_cache.contains_key(&ibl_id)
                    }
                    SkyboxId::SpecificIbl(ibl_id) => {
                        let skybox_cache = self.skybox_by_ibl_cache.lock().unwrap();
                        skybox_cache.contains_key(&ibl_id)
                    }
                }
            };

            if skybox_loaded {
                break;
            }

            gloo_timers::future::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub async fn load_ibl(self: &Arc<Self>, ibl_id: IblId) {
        async fn inner(scene: &Arc<AppScene>, ibl_id: IblId) -> Result<()> {
            async fn create_ibl_texture(
                renderer: &mut AwsmRenderer,
                cubemap_image: CubemapImage,
            ) -> Result<IblTexture> {
                let (texture, view, mip_count) = cubemap_image
                    .create_texture_and_view(&renderer.gpu, Some("IBL Cubemap"))
                    .await?;

                let texture_key = renderer.textures.insert_cubemap(texture);

                let sampler_key = renderer
                    .textures
                    .get_sampler_key(&renderer.gpu, IblTexture::sampler_cache_key())?;

                let sampler = renderer.textures.get_sampler(sampler_key)?.clone();

                Ok(IblTexture::new(texture_key, view, sampler, mip_count))
            }

            let ibl = {
                let maybe_cached = {
                    // need to drop this lock before awaiting
                    scene.ibl_cache.lock().unwrap().get(&ibl_id).cloned()
                };

                match maybe_cached {
                    Some(ibl) => ibl.clone(),
                    None => {
                        let ibl_cubemaps = match ibl_id {
                            IblId::PhotoStudio => ibl::load_from_path("photo_studio").await?,
                            IblId::AllWhite => {
                                ibl::load_from_colors(CubemapBitmapColors::all(Color::WHITE))
                                    .await?
                            }
                            IblId::SimpleSky => ibl::load_simple_sky().await?,
                        };

                        let ibl = {
                            let mut renderer = scene.renderer.lock().await;

                            let prefiltered_env_texture =
                                create_ibl_texture(&mut renderer, ibl_cubemaps.prefiltered_env)
                                    .await?;
                            let irradiance_texture =
                                create_ibl_texture(&mut renderer, ibl_cubemaps.irradiance).await?;

                            Ibl::new(prefiltered_env_texture, irradiance_texture)
                        };

                        scene.ibl_cache.lock().unwrap().insert(ibl_id, ibl.clone());

                        ibl
                    }
                }
            };

            scene.renderer.lock().await.set_ibl(ibl.clone());

            Ok(())
        }

        self.ctx.loading_status.lock_mut().ibl = Ok(true);
        match inner(self, ibl_id).await {
            Ok(()) => {
                self.ctx.loading_status.lock_mut().ibl = Ok(false);
            }
            Err(err) => {
                tracing::error!("Failed to load IBL {:?}: {:?}", ibl_id, err);
                self.ctx.loading_status.lock_mut().ibl = Err(err.to_string());
            }
        }
    }

    pub async fn wait_for_ibl_loaded(self: &Arc<Self>) {
        loop {
            let ibl_id = self.ctx.ibl_id.get_cloned();
            let ibl_loaded = {
                let ibl_cache = self.ibl_cache.lock().unwrap();
                ibl_cache.contains_key(&ibl_id)
            };

            if ibl_loaded {
                break;
            }

            gloo_timers::future::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub async fn upload_data(self: &Arc<Self>, _gltf_id: GltfId, loader: GltfLoader) {
        self.ctx.loading_status.lock_mut().gltf_data = Ok(true);
        match loader.into_data(None) {
            Err(err) => {
                self.ctx.loading_status.lock_mut().gltf_data = Err(err.to_string());
            }
            Ok(data) => {
                self.ctx.loading_status.lock_mut().gltf_data = Ok(false);
                *self.latest_gltf_data.lock().unwrap() = Some(data);
            }
        }
    }

    pub async fn populate(self: &Arc<Self>) {
        // Phase 1: GPU upload (the slow one on cold loads).
        //
        // Inside `populate_gltf`: per-mesh meta/material/buffer
        // allocation, the per-image texture uploads, and the
        // mipmap-generation finalize step. On a fresh visit to a
        // multi-MB glTF this can dominate the visible loading window,
        // so it gets its own label — previously this whole chunk
        // hid behind a single opaque "Populating scene".
        async fn upload_phase(scene: &Arc<AppScene>) -> Result<()> {
            let data = {
                let data = scene.latest_gltf_data.lock().unwrap();
                data.as_ref()
                    .expect("No GLTF data to populate")
                    .heavy_clone()
            };

            // STAGE 1 (GPU-free, no renderer lock): when `?ourformat=1`, convert the
            // source glTF to our clean glb + re-parse, so STAGE 2 below materialises
            // OUR format rather than rendering glTF directly (§0 north star). Falls
            // back to the direct path (+ logs) if the conversion fails.
            // Keep the ORIGINAL data when routing through our-format: reexport_clean
            // drops animations, so we re-load them (remapped) after populate (GAP 2).
            let (data, original_for_anim) = if our_format_enabled() {
                match import_to_our_format(&data).await {
                    Ok(clean) => (clean, Some(data)),
                    Err(e) => {
                        tracing::warn!(
                            "ourformat: import_to_our_format failed, using direct glTF: {e}"
                        );
                        (data, None)
                    }
                }
            } else {
                (data, None)
            };

            let mut renderer = scene.renderer.lock().await;

            // Drop any lights that came from a previous gltf load before
            // populating the next one, so KHR_lights_punctual additions
            // stay scoped to the model that owns them.
            {
                let mut prev = scene.gltf_punctual_lights.lock().unwrap();
                for key in prev.drain(..) {
                    renderer.remove_light(key);
                }
            }
            let populate_ctx = renderer.populate_gltf(data, None).await?;
            *scene.gltf_punctual_lights.lock().unwrap() = populate_ctx.punctual_lights.clone();
            // Keep the node->transform map so a later light re-insert (the
            // punctual-lights mode toggle) can re-bind animated lights.
            *scene.gltf_node_transforms.lock().unwrap() = populate_ctx
                .key_lookups
                .lock()
                .unwrap()
                .node_index_to_transform
                .clone();

            // GAP 2: re-load the original animations remapped onto the clean nodes.
            if let Some(original) = original_for_anim.as_ref() {
                load_remapped_animations(&mut renderer, original, &populate_ctx);
            }

            // §C.1 perf bench (dev-only, inert without the param): `?stress=N`
            // duplicates the loaded model's meshes into an N-cell grid to profile
            // per-frame CPU at thousands of renderables. Each duplicate shares the
            // source GPU geometry/material (cheap upload) but is a DISTINCT
            // renderable — so it stresses the per-frame renderable walk / classify
            // setup / transform tree / per-mesh meta exactly like a thousands-of-mesh
            // scene. Capture a `performance_*` trace + render timing with this on.
            if let Some(n) = stress_grid_count() {
                let source_keys: Vec<_> = populate_ctx
                    .key_lookups
                    .lock()
                    .unwrap()
                    .all_mesh_keys
                    .values()
                    .flatten()
                    .copied()
                    .collect();
                if let Some(&src) = source_keys.first() {
                    // `?variants=M`: build M distinct first-party PBR feature-mask
                    // variants by cloning the source PBR material and toggling off a
                    // distinct subset of its textures (5 texture bits → up to 32
                    // distinct masks). Each becomes a distinct variant pipeline.
                    use awsm_renderer::materials::{Material, MaterialKey};
                    let variant_keys: Vec<MaterialKey> = match variant_count() {
                        Some(m) => {
                            let src_pbr = renderer
                                .meshes
                                .get(src)
                                .ok()
                                .map(|mesh| mesh.material_key)
                                .and_then(|k| match renderer.materials.get(k) {
                                    Ok(Material::Pbr(p)) => Some((**p).clone()),
                                    _ => None,
                                });
                            if let Some(base_pbr) = src_pbr {
                                let awsm_renderer::AwsmRenderer {
                                    materials,
                                    textures,
                                    dynamic_materials,
                                    extras_pool,
                                    ..
                                } = &mut *renderer;
                                (0..m)
                                    .map(|v| {
                                        let mut pbr = base_pbr.clone();
                                        let mask = v % 32;
                                        if mask & 0b00001 != 0 {
                                            pbr.metallic_roughness_tex = None;
                                        }
                                        if mask & 0b00010 != 0 {
                                            pbr.normal_tex = None;
                                        }
                                        if mask & 0b00100 != 0 {
                                            pbr.emissive_tex = None;
                                        }
                                        if mask & 0b01000 != 0 {
                                            pbr.occlusion_tex = None;
                                        }
                                        if mask & 0b10000 != 0 {
                                            pbr.base_color_tex = None;
                                        }
                                        materials.insert(
                                            Material::Pbr(Box::new(pbr)),
                                            textures,
                                            dynamic_materials,
                                            extras_pool,
                                        )
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            }
                        }
                        None => Vec::new(),
                    };

                    let cols = (n as f32).sqrt().ceil() as i64;
                    let mut made = 0usize;
                    for i in 0..n as i64 {
                        let x = (i % cols) as f32 * 2.0;
                        let z = (i / cols) as f32 * 2.0;
                        let tk = renderer.transforms.insert(
                            awsm_renderer::transforms::Transform {
                                translation: glam::Vec3::new(x, 0.0, z),
                                rotation: glam::Quat::IDENTITY,
                                scale: glam::Vec3::ONE,
                            },
                            None,
                        );
                        if let Ok(new_mesh_key) = renderer.duplicate_mesh_with_transform(src, tk) {
                            made += 1;
                            if !variant_keys.is_empty() {
                                let mat = variant_keys[(i as usize) % variant_keys.len()];
                                let _ = renderer.set_mesh_material(new_mesh_key, mat);
                            }
                        }
                    }
                    tracing::warn!(
                        "§C.1 stress bench: duplicated {made} meshes (grid {cols}x), {} variants",
                        variant_keys.len()
                    );
                }
            }

            Ok(())
        }

        // Phase 2: finalise — gizmo + IBL + skybox + light/material
        // resets. Fast on warm cache; surfaced separately so the
        // user sees the bar advance past the heavy upload phase
        // instead of staying parked on a single label.
        async fn finalize_phase(scene: &Arc<AppScene>) -> Result<()> {
            {
                let mut renderer = scene.renderer.lock().await;

                // The gizmo is generated procedurally (fat lines) — no `.glb`.
                let controller = TransformController::new(&mut renderer, GizmoSpace::default())?;
                if let Some(editor) = scene.editor.lock().unwrap().as_ref() {
                    *editor.transform_controller.lock().unwrap() = Some(controller);
                }

                if let Some(ibl) = scene
                    .ibl_cache
                    .lock()
                    .unwrap()
                    .get(&scene.ctx.ibl_id.get())
                    .cloned()
                {
                    renderer.set_ibl(ibl);
                }

                let skybox_id = scene.ctx.skybox_id.get_cloned();

                let ibl_id = match skybox_id {
                    SkyboxId::SameAsIbl => Some(scene.ctx.ibl_id.get_cloned()),
                    SkyboxId::SpecificIbl(ibl_id) => Some(ibl_id),
                    SkyboxId::None => None,
                };

                if let Some(ibl_id) = ibl_id {
                    if let Some(skybox) = scene
                        .skybox_by_ibl_cache
                        .lock()
                        .unwrap()
                        .get(&ibl_id)
                        .cloned()
                    {
                        renderer.set_skybox(skybox);
                    }
                }
            }

            // takes the renderer lock so do it after we freed it
            scene.reset_punctual_lights().await?;
            scene.reset_material_debug().await?;
            scene.reset_anti_aliasing().await?;
            scene.reset_post_processing().await?;

            Ok(())
        }

        self.ctx.loading_status.lock_mut().populate_gpu_upload = Ok(true);
        if let Err(err) = upload_phase(self).await {
            self.ctx.loading_status.lock_mut().populate_gpu_upload = Err(err.to_string());
            return;
        }
        self.ctx.loading_status.lock_mut().populate_gpu_upload = Ok(false);

        self.ctx.loading_status.lock_mut().populate_finalize = Ok(true);
        if let Err(err) = finalize_phase(self).await {
            self.ctx.loading_status.lock_mut().populate_finalize = Err(err.to_string());
            return;
        }
        self.ctx.loading_status.lock_mut().populate_finalize = Ok(false);
    }

    /// Open the load gate before a cold / full model load. The render gate then
    /// clears to the clear-color (loading overlay on top) until [`Self::commit`]
    /// lands — so the scene never reveals a half-compiled frame.
    pub async fn begin_load(self: &Arc<Self>) {
        self.renderer.lock().await.begin_load();
    }

    /// THE commit point of the load: finalize the texture pool ONCE + compile
    /// every pipeline the scene needs (opaque / classify / MSAA edge-resolve),
    /// then flip the render gate open. Gating the reveal here is what keeps the
    /// first shown frame fully specialized + anti-aliased (no black / aliased
    /// transient). Drives the loading overlay from `LoadingStats`.
    pub async fn commit(self: &Arc<Self>) {
        // Hold the gate up from the start of the commit (Idle snapshot → no banner
        // line yet, but `is_loading()` is true) until the first granular callback.
        self.ctx.loading_status.lock_mut().commit = Some(awsm_renderer::LoadingStats::default());
        {
            let ctx = self.ctx.clone();
            let mut renderer = self.renderer.lock().await;
            let result = renderer
                .commit_load(|stats| {
                    // Feed the FULL snapshot so the overlay renders the active phase
                    // (geometry X/Y → textures X/Y → pipelines N) via the shared
                    // `LoadingStats::phase_label()`.
                    ctx.loading_status.lock_mut().commit = Some(stats);
                })
                .await;
            if let Err(err) = result {
                tracing::error!("commit_load failed: {:?}", err);
            }
        }
        {
            let mut status = self.ctx.loading_status.lock_mut();
            status.commit = None;
            status.compile_pending = 0;
        }
    }

    pub async fn reset_punctual_lights(self: &Arc<Self>) -> Result<()> {
        use crate::pages::app::context::PunctualLightsMode;
        use awsm_renderer_gltf::populate::lights::populate_lights_from_doc_with_transforms;

        let mut renderer = self.renderer.lock().await;
        let mode = self.ctx.punctual_lights.get();

        // 1. Drop the existing additional-fill lights (we'll re-add them
        //    below if the mode wants them). Tracked separately from the
        //    gltf-derived lights so we never confuse the two.
        if let Some(lights) = self.lights.lock().unwrap().take() {
            for light_key in lights {
                renderer.remove_light(light_key);
            }
        }

        // 2. Reconcile gltf-derived lights against the mode.
        //      Off / AdditionalOnly: gltf lights should NOT be present.
        //      ModelOnly / On / Auto: gltf lights should be present
        //                             (Auto only effectively uses them if
        //                             the asset has any).
        //    We re-walk the cached gltf data to re-insert them if they
        //    were previously stripped, so toggling between modes is
        //    non-destructive.
        let wants_model_lights = matches!(
            mode,
            PunctualLightsMode::ModelOnly | PunctualLightsMode::On | PunctualLightsMode::Auto
        );
        let has_model_lights = !self.gltf_punctual_lights.lock().unwrap().is_empty();

        if !wants_model_lights && has_model_lights {
            let prev = std::mem::take(&mut *self.gltf_punctual_lights.lock().unwrap());
            for key in prev {
                renderer.remove_light(key);
            }
        } else if wants_model_lights && !has_model_lights {
            // Re-populate from the last loaded gltf data, if any. This
            // makes the model-only / on / auto modes reversible without
            // re-running the whole gltf populate.
            let data_clone = self
                .latest_gltf_data
                .lock()
                .unwrap()
                .as_ref()
                .map(|d| d.heavy_clone());
            if let Some(data) = data_clone {
                // Re-bind to the persisted node->transform map so animated
                // lights (e.g. the firefly point lights) keep following
                // their nodes instead of freezing at their load-time pose.
                let node_transforms = self.gltf_node_transforms.lock().unwrap().clone();
                let keys = populate_lights_from_doc_with_transforms(
                    &mut renderer,
                    &data,
                    &node_transforms,
                )?;
                *self.gltf_punctual_lights.lock().unwrap() = keys;
            }
        }

        // 3. Re-add the additional four-directional fill when the mode
        //    asks for it. `Auto` falls back to fill ONLY if the model
        //    didn't bring its own lights — otherwise the model's
        //    authored lighting wins.
        let auto_wants_fill = matches!(mode, PunctualLightsMode::Auto)
            && self.gltf_punctual_lights.lock().unwrap().is_empty();
        let wants_additional = matches!(
            mode,
            PunctualLightsMode::AdditionalOnly | PunctualLightsMode::On
        ) || auto_wants_fill;
        if !wants_additional {
            return Ok(());
        }

        let lights = vec![
            renderer.insert_light(
                Light::Directional {
                    color: [1.0, 0.97, 0.92],
                    intensity: 1.4,
                    direction: [0.1, -0.35, -1.0],
                },
                None,
            )?,
            renderer.insert_light(
                Light::Directional {
                    color: [0.9, 0.95, 1.0],
                    intensity: 0.6,
                    direction: [0.0, -0.2, -1.0],
                },
                None,
            )?,
            renderer.insert_light(
                Light::Directional {
                    color: [0.8, 0.9, 1.0],
                    intensity: 0.7,
                    direction: [-0.05, -0.25, 1.0],
                },
                None,
            )?,
            renderer.insert_light(
                Light::Directional {
                    color: [1.0, 0.96, 0.9],
                    intensity: 0.5,
                    direction: [-1.0, -0.2, 0.2],
                },
                None,
            )?,
        ];

        *self.lights.lock().unwrap() = Some(lights);

        Ok(())
    }

    pub async fn reset_material_debug(self: &Arc<Self>) -> Result<()> {
        let mut renderer = self.renderer.lock().await;

        let material_debug = self.ctx.material_debug.get_cloned();

        let keys = renderer.materials.keys().collect::<Vec<_>>();

        for key in keys {
            renderer.update_material(key, |mat| match mat {
                Material::Pbr(pbr_material) => {
                    pbr_material.debug = material_debug;
                }
                Material::Unlit(_)
                | Material::Toon(_)
                | Material::FlipBook(_)
                | Material::Custom(_) => {
                    // Non-PBR materials don't carry the per-shading debug
                    // bitmask; ignore the per-frame override on those.
                }
            });
        }

        Ok(())
    }

    pub async fn reset_anti_aliasing(self: &Arc<Self>) -> Result<()> {
        let mut renderer = self.renderer.lock().await;

        let anti_aliasing = self.ctx.anti_alias.get_cloned();

        renderer.set_anti_aliasing(anti_aliasing).await?;
        // An AA flip is a config change that needs recompilation: the MSAA
        // edge-resolve set is rebuilt by `commit_load` (the one compile path),
        // not by a render-preamble side channel. Live (no `begin_load`) so the
        // scene stays on screen across the flip.
        renderer.commit_load(|_| {}).await?;

        Ok(())
    }

    pub async fn reset_post_processing(self: &Arc<Self>) -> Result<()> {
        let mut renderer = self.renderer.lock().await;

        let post_processing = self.ctx.post_processing.get_cloned();

        renderer.set_post_processing(post_processing).await?;

        Ok(())
    }

    pub async fn reset_camera(self: &Arc<Self>) -> Result<()> {
        let mut renderer = self.renderer.lock().await;
        if let Some(camera) = self.camera.lock().unwrap().as_mut() {
            camera.set_aperture(self.ctx.camera_aperture.get());
            camera.set_focus_distance(self.ctx.camera_focus_distance.get());

            renderer.set_camera(camera.view(), camera.params())?;
        }

        Ok(())
    }

    pub async fn setup_all(self: &Arc<Self>) -> Result<()> {
        self.last_shader_kind.set(None);

        self.setup_viewport_inner(true).await?;

        Ok(())
    }

    pub async fn setup_viewport(self: &Arc<Self>) -> Result<()> {
        self.setup_viewport_inner(false).await
    }

    async fn setup_viewport_inner(self: &Arc<Self>, force_new_camera: bool) -> Result<()> {
        let mut renderer = self.renderer.lock().await;

        // Ensure canvas buffer size matches CSS display size
        renderer.gpu.sync_canvas_buffer_with_css();

        // call these first so we can get the extents
        renderer.update_animations(0.0)?;
        renderer.update_transforms();

        let mode = self.ctx.camera_id.get();

        let mut camera_guard = self.camera.lock().unwrap();
        if !force_new_camera {
            if let Some(camera) = camera_guard.as_mut() {
                // View and projection are decoupled: a projection switch is a
                // cheap mode flip that keeps the orbit pose, and the aspect is
                // the renderer's business at `set_camera` — nothing to resize.
                camera.set_projection_mode(mode);
                renderer.set_camera(camera.view(), camera.params())?;
                return Ok(());
            }
        }

        // Need to create a new camera - compute scene bounds
        let mut scene_aabb: Option<Aabb> = None;

        for (_key, mesh) in renderer.meshes.iter() {
            // The gizmo is drawn as fat lines (not meshes), so nothing here needs
            // to be excluded from the camera-fit bounds.
            if let Some(mesh_aabb) = mesh.world_aabb.clone() {
                if let Some(current_scene_aabb) = &mut scene_aabb {
                    current_scene_aabb.extend(&mesh_aabb);
                } else {
                    scene_aabb = Some(mesh_aabb);
                }
            }
        }

        let aabb = scene_aabb.unwrap_or_else(|| {
            let doc = self.latest_gltf_data.lock().unwrap();
            match doc.as_ref() {
                Some(data) => awsm_renderer_gltf::aabb_from_gltf_doc(&data.doc),
                None => Aabb::new_unit_cube(),
            }
        });

        let mut new_camera = Camera::new_aabb(aabb, 1.1, renderer.features.depth());
        new_camera.set_projection_mode(mode);
        new_camera.set_aperture(self.ctx.camera_aperture.get());
        new_camera.set_focus_distance(self.ctx.camera_focus_distance.get());
        apply_cam_url_override(&mut new_camera);

        // Push the camera immediately so gizmo interactions work correctly
        renderer.set_camera(new_camera.view(), new_camera.params())?;

        *camera_guard = Some(new_camera);

        Ok(())
    }

    pub async fn update_all(self: &Arc<Self>, global_time_delta: f64) -> Result<()> {
        let camera = { self.camera.lock().unwrap().clone() };
        if let Some(camera) = camera {
            self.renderer.lock().await.update_all(
                global_time_delta,
                camera.view(),
                camera.params(),
            )?;
        }

        Ok(())
    }

    pub fn start_animation_loop(self: &Arc<Self>) {
        let state = self;

        state.stop_animation_loop();
        *state.request_animation_frame.lock().unwrap() = Some(
            gloo_render::request_animation_frame(clone!(state => move |timestamp| {
                state.fire_raf(timestamp);
            })),
        );
    }

    pub fn stop_animation_loop(self: &Arc<Self>) {
        self.request_animation_frame.lock().unwrap().take();
        self.last_request_animation_frame.set(None);
    }

    fn fire_raf(self: &Arc<Self>, timestamp: f64) {
        let state = self;
        spawn_local(clone!(state => async move {
            if let Some(last_timestamp) = state.last_request_animation_frame.get() {
                let time_delta = timestamp - last_timestamp;
                if let Err(err) = state.update_all(time_delta).await {
                    tracing::error!("Failed to animate: {:?}", err);
                }

                if let Err(err) = state.render().await {
                    tracing::error!("Failed to render during animation loop: {:?}", err);
                }
            }

            let mut lock = state.request_animation_frame.lock().unwrap();

            if lock.take().is_some() {
                state.last_request_animation_frame.set(Some(timestamp));

                *lock = Some(gloo_render::request_animation_frame(clone!(state => move |timestamp| {
                    state.fire_raf(timestamp);
                })));
            }
        }));
    }
}

/// Optional `?cam=yaw,pitch,radius,lx,ly,lz` URL override for reproducible
/// repro shots — applied to a freshly built viewport camera. Ignored unless
/// all six comma-separated floats parse.
fn apply_cam_url_override(camera: &mut Camera) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(query) = window.location().search() else {
        return;
    };
    for pair in query.trim_start_matches('?').split('&') {
        if let Some(val) = pair.strip_prefix("cam=") {
            let parts: Vec<f32> = val
                .split(',')
                .filter_map(|s| s.parse::<f32>().ok())
                .collect();
            tracing::info!(
                target: "awsm_renderer::camera_debug",
                "cam= URL override seen: parts={:?}",
                parts,
            );
            if let [yaw, pitch, radius, lx, ly, lz] = parts[..] {
                camera.set_orbit(yaw, pitch, radius, glam::Vec3::new(lx, ly, lz));
                tracing::info!(
                    target: "awsm_renderer::camera_debug",
                    "cam= URL override APPLIED: yaw={yaw} pitch={pitch} radius={radius} look_at=({lx}, {ly}, {lz})",
                );
            }
        }
    }
}
