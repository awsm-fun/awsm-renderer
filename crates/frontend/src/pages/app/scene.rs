pub mod camera;
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
use awsm_renderer::gltf::data::GltfData;
use awsm_renderer::gltf::loader::GltfLoader;
use awsm_renderer::lights::ibl::Ibl;
use awsm_renderer::lights::ibl::IblTexture;
use awsm_renderer::lights::Light;
use awsm_renderer::lights::LightKey;

use awsm_renderer::materials::Material;
use awsm_renderer::picker::PickResult;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_editor::transform_controller::{
    GizmoSpace, TransformController, TransformTarget,
};
use awsm_web::dom::resize::ResizeObserver;
use camera::{Camera, CameraId};
use gloo_events::EventListener;
use wasm_bindgen_futures::spawn_local;
use web_sys::PointerEvent;

use crate::models::collections::GltfId;
use crate::pages::app::context::{IblId, SkyboxId};
use crate::pages::app::scene::editor::AppSceneEditor;
use crate::pages::app::sidebar::material::FragmentShaderKind;
use crate::prelude::*;

use super::context::AppContext;

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
    move_action: Cell<Option<MoveAction>>,
    last_size: Cell<(f64, f64)>,
    last_camera_id: Cell<CameraId>,
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
            last_camera_id: Cell::new(CameraId::default()),
            last_shader_kind: Cell::new(None),
            editor: Mutex::new(None),
            move_action: Cell::new(None),
            lights: Mutex::new(None),
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
                        match renderer.pick(x,y).await {
                            Err(err) => {
                                tracing::error!("Pick error: {:?}", err);
                            }
                            Ok(res) => {
                                if let PickResult::Hit(mesh_key) = res {
                                    if let Some(editor) = state.editor.lock().unwrap().as_ref() {
                                        if let Some(transform_controller) = editor.transform_controller.lock().unwrap().as_mut() {
                                            match transform_controller.start_pick(&mut renderer, mesh_key, x, y) {
                                                Some(TransformTarget::GizmoHit(_)) => {
                                                    state.move_action.set(Some(MoveAction::GizmoTransforming));
                                                }
                                                Some(TransformTarget::ObjectHit(transform)) => {
                                                    editor.selected_object.set_neq(Some(transform));
                                                }
                                                None => { }
                                            }
                                        }
                                    }
                                }

                            }
                        }

                        if state.move_action.get() != Some(MoveAction::GizmoTransforming) {
                            if let Some(camera) = state.camera.lock().unwrap().as_mut() {
                                camera.on_pointer_down();
                            }
                            state.move_action.set(Some(MoveAction::CameraMoving));
                        }
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
                                camera.on_pointer_move(event.movement_x(), event.movement_y());
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
            state.camera.clone(),
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

            let loader = GltfLoader::load(&gltf_id.url(), None).await?;

            scene
                .gltf_cache
                .lock()
                .unwrap()
                .insert(gltf_id, loader.heavy_clone());

            Ok(loader)
        }

        self.ctx.loading_status.lock_mut().gltf_net = Ok(true);
        match inner(self, gltf_id).await {
            Ok(loader) => {
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
        async fn inner(scene: &Arc<AppScene>) -> Result<()> {
            {
                let data = {
                    let data = scene.latest_gltf_data.lock().unwrap();
                    data.as_ref()
                        .expect("No GLTF data to populate")
                        .heavy_clone()
                };

                let mut renderer = scene.renderer.lock().await;

                renderer.populate_gltf(data, None).await?;

                let editor_gizmo_gltf_data = {
                    let editor_guard = scene.editor.lock().unwrap();
                    editor_guard
                        .as_ref()
                        .map(|editor| editor.gizmo_gltf_data.clone())
                };

                if let Some(editor_gizmo_gltf_data) = editor_gizmo_gltf_data {
                    let ctx = renderer.populate_gltf(editor_gizmo_gltf_data, None).await?;

                    if let Some(editor) = scene.editor.lock().unwrap().as_ref() {
                        *editor.transform_controller.lock().unwrap() =
                            Some(TransformController::new(
                                ctx.key_lookups.clone(),
                                GizmoSpace::default(),
                            )?);
                    }
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

        self.ctx.loading_status.lock_mut().populate = Ok(true);
        match inner(self).await {
            Ok(()) => {
                self.ctx.loading_status.lock_mut().populate = Ok(false);
            }
            Err(err) => {
                self.ctx.loading_status.lock_mut().populate = Err(err.to_string());
            }
        }
    }

    pub async fn reset_punctual_lights(self: &Arc<Self>) -> Result<()> {
        let mut renderer = self.renderer.lock().await;

        if let Some(lights) = self.lights.lock().unwrap().take() {
            for light_key in lights {
                renderer.lights.remove(light_key);
            }
        }

        if !self.ctx.punctual_lights.get() {
            return Ok(());
        }

        let lights = vec![
            renderer.lights.insert(Light::Directional {
                color: [1.0, 0.97, 0.92],
                intensity: 1.4,
                direction: [0.1, -0.35, -1.0],
            })?,
            renderer.lights.insert(Light::Directional {
                color: [0.9, 0.95, 1.0],
                intensity: 0.6,
                direction: [0.0, -0.2, -1.0],
            })?,
            renderer.lights.insert(Light::Directional {
                color: [0.8, 0.9, 1.0],
                intensity: 0.7,
                direction: [-0.05, -0.25, 1.0],
            })?,
            renderer.lights.insert(Light::Directional {
                color: [1.0, 0.96, 0.9],
                intensity: 0.5,
                direction: [-1.0, -0.2, 0.2],
            })?,
        ];

        *self.lights.lock().unwrap() = Some(lights);

        Ok(())
    }

    pub async fn reset_material_debug(self: &Arc<Self>) -> Result<()> {
        let mut renderer = self.renderer.lock().await;

        let material_debug = self.ctx.material_debug.get_cloned();

        let keys = renderer.materials.keys().collect::<Vec<_>>();

        for key in keys {
            renderer.update_material(key, |mat| {
                match mat {
                    Material::Pbr(pbr_material) => {
                        pbr_material.debug = material_debug;
                    }
                    Material::Unlit(_) => {
                        // TODO
                    }
                }
            });
        }

        Ok(())
    }

    pub async fn reset_anti_aliasing(self: &Arc<Self>) -> Result<()> {
        let mut renderer = self.renderer.lock().await;

        let anti_aliasing = self.ctx.anti_alias.get_cloned();

        renderer.set_anti_aliasing(anti_aliasing).await?;

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
            camera.aperture = self.ctx.camera_aperture.get();
            camera.focus_distance = self.ctx.camera_focus_distance.get();

            renderer.update_camera(camera.matrices())?;
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

        let (canvas_width, canvas_height) = renderer.gpu.canvas_size(false);

        // call these first so we can get the extents
        renderer.update_animations(0.0)?;
        renderer.update_transforms();

        let camera_aspect = canvas_width as f32 / canvas_height as f32;
        let camera_id = self.ctx.camera_id.get();

        // Check if we can just resize the existing camera
        let mut camera_guard = self.camera.lock().unwrap();
        let needs_new_camera = force_new_camera
            || match camera_guard.as_ref() {
                None => true,
                Some(camera) => {
                    // Need new camera if type changed
                    match camera_id {
                        CameraId::Orthographic => !camera.is_orthographic(),
                        CameraId::Perspective => !camera.is_perspective(),
                    }
                }
            };

        if !needs_new_camera {
            // Just update the aspect ratio on the existing camera
            if let Some(camera) = camera_guard.as_mut() {
                camera.on_resize(camera_aspect);
                // Update renderer's camera matrices so gizmo interactions work correctly
                renderer.update_camera(camera.matrices())?;
            }
            return Ok(());
        }

        // Need to create a new camera - compute scene bounds
        let mut scene_aabb: Option<Aabb> = None;

        for (key, mesh) in renderer.meshes.iter() {
            if self
                .editor
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|editor| {
                    editor
                        .transform_controller
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|tc| tc.is_gizmo_mesh_key(key))
                })
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(mesh_aabb) = mesh.world_aabb.clone() {
                if let Some(current_scene_aabb) = &mut scene_aabb {
                    current_scene_aabb.extend(&mesh_aabb);
                } else {
                    scene_aabb = Some(mesh_aabb);
                }
            }
        }

        let gltf_doc = self
            .latest_gltf_data
            .lock()
            .unwrap()
            .as_ref()
            .map(|data| data.doc.clone());

        let new_camera = match camera_id {
            CameraId::Orthographic => Camera::new_orthographic(
                scene_aabb,
                gltf_doc,
                camera_aspect,
                self.ctx.camera_aperture.get(),
                self.ctx.camera_focus_distance.get(),
            ),
            CameraId::Perspective => Camera::new_perspective(
                scene_aabb,
                gltf_doc,
                camera_aspect,
                self.ctx.camera_aperture.get(),
                self.ctx.camera_focus_distance.get(),
            ),
        };

        // Update renderer's camera matrices immediately so gizmo interactions work correctly
        renderer.update_camera(new_camera.matrices())?;

        *camera_guard = Some(new_camera);

        Ok(())
    }

    pub async fn update_all(self: &Arc<Self>, global_time_delta: f64) -> Result<()> {
        let camera = { self.camera.lock().unwrap().clone() };
        if let Some(camera) = camera {
            self.renderer
                .lock()
                .await
                .update_all(global_time_delta, camera.matrices())?;
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
