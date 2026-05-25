//! Editor-side procedural-node materialization.
//!
//! Mirrors the player's `scene::procedural` materializers but lives inside
//! the editor's async renderer-bridge. When a node's kind switches to (or
//! lands on) a procedural variant — `Primitive`, `Line`, `Sprite`,
//! `SweepAlongCurve`, `InstancesAlongCurve`, `Curve`, `ParticleEmitter` —
//! this module builds the corresponding renderer-side resources and tracks
//! the resulting keys on the `RendererNode` so the standard `apply_kind`
//! teardown path cleans them up on the next variant change.
//!
//! Coverage:
//! - `Primitive` → opaque mesh through `add_raw_mesh`; material resolves
//!   via `material_cache` (asset-ref → cache key, else inline def).
//! - `Sprite` → quad mesh through `add_raw_mesh` + `set_mesh_billboard_mode`.
//! - `Line` → fat-line registration through `add_line_strip` (B-2).
//! - `SweepAlongCurve` + `InstancesAlongCurve` → both look up the
//!   referenced curve node by id (`app_state().scene.nodes`) at
//!   materialize time. Inside prefab subtrees the lookup currently
//!   logs a warning and skips — gameplay-level prefab support is
//!   tracked separately.
//! - `Curve` → no game-side renderer output, but spawns an editor-only
//!   fat-line strip (`materialize_curve_viz`) so the curve is visible in
//!   the viewport without going through a sweep / instances consumer.
//! - `ParticleEmitter` → registers the per-NodeId "playing" observer in
//!   `particles_sync` (the actual simulator runtime spins up only when
//!   the inspector's Play button toggles the flag).
//!
//! Reactivity model: per-kind re-materialization fires on every
//! `kind.set(...)` (whole-`NodeKind` mutation). That's coarse but
//! straightforward — within a single param drag the inspector mutates
//! the kind once per pointer event, so the re-materialize cost is
//! bounded. Field-level signal-driven dedup would let very heavy kinds
//! (e.g. a `SweepAlongCurve` with 4096 samples) skip work when an
//! unrelated knob changes; that optimization is tracked separately.

use std::sync::Arc;

use awsm_curves::{CatmullRomCurve, Curve3, FrameSequence};
use awsm_meshgen::{
    box_mesh, cone_mesh, cylinder_mesh, plane_mesh, sphere_mesh, sprite_quad, sweep_along_curve,
    torus_mesh, CrossSection, MeshData, SweepOpts, UvMode,
};
use awsm_renderer::{
    instances::InstanceAttr,
    materials::{pbr::PbrMaterial, Material, MaterialAlphaMode},
    meshes::{mesh::BillboardMode, MeshKey},
    raw_mesh::RawMeshData,
    transforms::{Transform, TransformKey},
    AwsmRenderer,
};
use awsm_scene_schema::{
    CrossSectionDef, CurveDef, InstancesAlongCurveDef, LineDef, MaterialDef, MaterialRef, NodeId,
    NodeKind, PrimitiveShape, SpriteDef, SweepAlongCurveDef, SweepUvMode,
};
use glam::{Quat, Vec3};

use crate::context::{renderer_handle, with_renderer_mut};
use crate::renderer_bridge::node_sync::RendererNode;
use crate::state::app_state;

/// Materialize the procedural geometry for a node into the renderer +
/// record the resulting mesh / transform keys on the bridge entry so a
/// later kind change can tear them down via the existing
/// `clear_model_instance` cleanup path (we reuse `model_meshes` +
/// `model_transforms` since the cleanup walk is identical).
pub async fn materialize_procedural(
    entry: Arc<RendererNode>,
    kind: NodeKind,
    parent_tk: TransformKey,
) {
    // Capture the per-mesh shadow config (when present) up-front so we
    // can apply it after each materialize path produces `model_meshes`.
    // Without this, the per-node Cast/Receive toggles in the inspector
    // would round-trip through `project.json` but never reach the
    // renderer — `Mesh::cast_shadows` / `receive_shadows` would stay at
    // their renderer defaults (`true` / `true`). Sprites override to
    // (false, false) inside `materialize_sprite` since the shadow VS
    // doesn't run the billboard rotation.
    let mesh_shadow_cfg: Option<awsm_scene_schema::MeshShadowConfig> = match &kind {
        NodeKind::Primitive { shadow, .. } => Some(*shadow),
        NodeKind::Mesh { shadow, .. } => Some(*shadow),
        NodeKind::SweepAlongCurve { shadow, .. } => Some(*shadow),
        NodeKind::InstancesAlongCurve(def) => Some(def.shadow),
        _ => None,
    };

    match kind {
        NodeKind::Primitive {
            shape,
            material,
            inline_material,
            ..
        } => {
            materialize_primitive(entry.clone(), parent_tk, shape, material, inline_material).await
        }
        NodeKind::Line(def) => materialize_line(entry.clone(), parent_tk, def).await,
        NodeKind::Sprite(def) => materialize_sprite(entry.clone(), parent_tk, def).await,
        NodeKind::SweepAlongCurve {
            def,
            material,
            inline_material,
            ..
        } => {
            let curve_def = lookup_curve_def(def.curve_node);
            match curve_def {
                Some(c) => {
                    // Record the geometry hash so node_sync's fast path
                    // can detect material-only edits on the next kind.set
                    // and skip the expensive sweep rebuild.
                    let hash = sweep_geometry_hash(&def, &c);
                    materialize_sweep(
                        entry.clone(),
                        parent_tk,
                        c,
                        def.cross_section.clone(),
                        def.uv_mode,
                        def.samples,
                        def.up_hint,
                        material,
                        inline_material,
                    )
                    .await;
                    *entry.sweep_geometry_hash.lock().unwrap() = Some(hash);
                }
                None => {
                    tracing::warn!(
                        "procedural_sync: SweepAlongCurve references missing curve_node"
                    );
                }
            }
        }
        NodeKind::InstancesAlongCurve(def) => {
            let curve_def = lookup_curve_def(def.curve_node);
            let source_kind = lookup_node_kind(def.source_node);
            match (curve_def, source_kind) {
                (
                    Some(c),
                    Some(NodeKind::Primitive {
                        shape,
                        material,
                        inline_material,
                        ..
                    }),
                ) => {
                    materialize_instances_along_curve(
                        entry.clone(),
                        parent_tk,
                        c,
                        def,
                        shape,
                        material,
                        inline_material,
                    )
                    .await
                }
                _ => {
                    tracing::warn!(
                        "procedural_sync: InstancesAlongCurve references missing curve or non-Primitive source"
                    );
                }
            }
        }
        NodeKind::Curve(c) => materialize_curve_viz(entry.clone(), parent_tk, c).await,
        NodeKind::ParticleEmitter(_) => {
            // Spawn the per-emitter "playing" observer. The inspector's
            // Play/Stop button writes into the per-node Mutable<bool>;
            // the observer materializes / tears down the runtime via
            // `renderer_bridge::particles_sync`.
            super::particles_sync::start_observer(&entry, entry.node_id, parent_tk);
        }
        NodeKind::Mesh {
            mesh,
            material,
            inline_material,
            ..
        } => {
            // Captured-procedural-mesh asset (F10). `mesh_cache` loads
            // the bytes on-demand — from `pending_assets` if captured
            // this session, or from the project's
            // `assets/<asset-id>.mesh.bin` side file otherwise — then
            // bitcode-decodes once and caches. Materialize via the same
            // raw-mesh + material path Primitive/Sweep take.
            let Some(captured) = super::mesh_cache::get_or_load(mesh).await else {
                tracing::warn!(
                    "procedural_sync: NodeKind::Mesh {mesh:?} couldn't be loaded; node will render empty"
                );
                return;
            };
            let raw = captured_to_raw_mesh(captured);
            upload_and_track(entry.clone(), parent_tk, raw, material, inline_material).await;
        }
        _ => {}
    }

    if let Some(cfg) = mesh_shadow_cfg {
        let flags = super::node_sync::mesh_shadow_flags_from_config(&cfg);
        let mesh_keys: Vec<awsm_renderer::meshes::MeshKey> =
            entry.model_meshes.lock().unwrap().clone();
        if !mesh_keys.is_empty() {
            with_renderer_mut(move |r| {
                for mk in mesh_keys {
                    let _ = r.set_mesh_shadow_flags(mk, flags);
                }
            })
            .await;
        }
    }
}

/// Curve viz: spawn an editor-only fat-line strip following the sampled
/// curve so the user can see the curve in 3D without going through a
/// sweep/instances consumer. The line is parked on the node's `line_keys`
/// so the standard kind-change cleanup tears it down.
async fn materialize_curve_viz(entry: Arc<RendererNode>, parent_tk: TransformKey, def: CurveDef) {
    if def.control_points.len() < 2 {
        return;
    }
    let entry_for_line = entry.clone();
    let line_key = with_renderer_mut(move |r| {
        use awsm_curves::{CatmullRomCurve, Curve3};
        let curve = CatmullRomCurve::new(
            def.control_points
                .iter()
                .map(|p| Vec3::from_array(*p))
                .collect(),
            def.closed,
        );
        let samples = def.sample_count.max(2) as usize;
        // `get_spaced_points` returns `samples + 1` points sampled at
        // even arc-length intervals across the curve.
        let mut local_points = curve.get_spaced_points(samples);
        if local_points.is_empty() {
            return None;
        }
        // The fat-line pipeline reads world-space positions directly —
        // bake the parent transform into each sampled point.
        let world = r
            .transforms
            .get_world(parent_tk)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        for p in local_points.iter_mut() {
            *p = world.transform_point3(*p);
        }
        // Magenta viz tint with full alpha. Closed curves get the
        // closing segment back-to-start so the loop renders.
        let mut positions: Vec<Vec3> = local_points;
        if def.closed {
            if let Some(first) = positions.first().copied() {
                positions.push(first);
            }
        }
        let viz_color = glam::Vec4::new(1.0, 0.45, 0.85, 0.95);
        let colors: Vec<glam::Vec4> = vec![viz_color; positions.len()];
        match r.add_line_strip(&positions, &colors, 1.5, false) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!(
                    "procedural_sync::materialize_curve_viz: add_line_strip failed: {err}"
                );
                None
            }
        }
    })
    .await;
    if let Some(key) = line_key {
        entry_for_line.line_keys.lock().unwrap().push(key);
    }
}

/// Hash of the inputs that determine a SweepAlongCurve's *geometry*
/// (the heavy bit). Used by `node_sync::apply_kind`'s fast path to
/// detect "geometry unchanged, only material differs" transitions.
///
/// Includes everything that flows into `sweep_along_curve` — curve
/// control points + closed flag, cross section, uv mode, samples, up
/// hint. Deliberately excludes `material` / `inline_material` because
/// those drive the material binding, not the geometry.
pub(super) fn sweep_geometry_hash(def: &SweepAlongCurveDef, curve_def: &CurveDef) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Floats aren't `Hash` — feed their raw bit patterns.
    for p in &curve_def.control_points {
        for c in p {
            c.to_bits().hash(&mut hasher);
        }
    }
    curve_def.closed.hash(&mut hasher);
    match &def.cross_section {
        CrossSectionDef::Strip { width, y_offset } => {
            0u8.hash(&mut hasher);
            width.to_bits().hash(&mut hasher);
            y_offset.to_bits().hash(&mut hasher);
        }
        CrossSectionDef::Tube {
            radius,
            radial_segments,
        } => {
            1u8.hash(&mut hasher);
            radius.to_bits().hash(&mut hasher);
            radial_segments.hash(&mut hasher);
        }
        CrossSectionDef::Wall { width, height } => {
            2u8.hash(&mut hasher);
            width.to_bits().hash(&mut hasher);
            height.to_bits().hash(&mut hasher);
        }
        CrossSectionDef::Profile { points, closed } => {
            3u8.hash(&mut hasher);
            for p in points {
                for c in p {
                    c.to_bits().hash(&mut hasher);
                }
            }
            closed.hash(&mut hasher);
        }
    }
    match def.uv_mode {
        SweepUvMode::StretchOnce => 0u8.hash(&mut hasher),
        SweepUvMode::RepeatByLength {
            u_repeat,
            v_repeat_per_unit,
        } => {
            1u8.hash(&mut hasher);
            u_repeat.to_bits().hash(&mut hasher);
            v_repeat_per_unit.to_bits().hash(&mut hasher);
        }
    }
    def.samples.hash(&mut hasher);
    for c in def.up_hint {
        c.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) fn lookup_curve_def(node_id: NodeId) -> Option<CurveDef> {
    // O(1) bridge-map lookup. Was a full-tree DFS via `find_curve_recursive`;
    // called every frame from `behavior_rail` and on every Sweep
    // materialize. Scenes with deep node trees (typical art glb)
    // were paying a per-frame O(N) walk for an O(1) hash answer.
    let entry = super::node_sync::bridge()
        .nodes
        .lock()
        .unwrap()
        .get(&node_id)
        .cloned()?;
    let kind = entry.node.kind.lock_ref().clone();
    if let NodeKind::Curve(c) = kind {
        Some(c)
    } else {
        None
    }
}

fn lookup_node_kind(node_id: NodeId) -> Option<NodeKind> {
    // Same O(1) bridge-map win as `lookup_curve_def`.
    let entry = super::node_sync::bridge()
        .nodes
        .lock()
        .unwrap()
        .get(&node_id)
        .cloned()?;
    let kind = entry.node.kind.lock_ref().clone();
    Some(kind)
}

async fn materialize_primitive(
    entry: Arc<RendererNode>,
    parent_tk: TransformKey,
    shape: PrimitiveShape,
    material_ref: Option<MaterialRef>,
    inline: MaterialDef,
) {
    let mesh_data = primitive_to_mesh(&shape);
    let raw = to_raw_mesh(mesh_data);
    upload_and_track(entry, parent_tk, raw, material_ref, inline).await;
}

async fn materialize_line(entry: Arc<RendererNode>, parent_tk: TransformKey, def: LineDef) {
    if def.points.len() < 2 {
        return;
    }
    let positions: Vec<Vec3> = def.points.iter().map(|p| Vec3::from_array(p.pos)).collect();
    let colors: Vec<glam::Vec4> = def
        .points
        .iter()
        .map(|p| glam::Vec4::from_array(p.color))
        .collect();
    let entry_for_line = entry.clone();
    let line_key = with_renderer_mut(move |r| {
        // The fat-line pipeline reads world-space positions directly,
        // so apply the parent transform CPU-side.
        let world = r
            .transforms
            .get_world(parent_tk)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY);
        let positions_world: Vec<Vec3> = positions
            .iter()
            .map(|p| world.transform_point3(*p))
            .collect();
        match r.add_line_strip(
            &positions_world,
            &colors,
            def.width_px,
            def.depth_test_always,
        ) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!("procedural_sync::materialize_line: add_line_strip failed: {err}");
                None
            }
        }
    })
    .await;
    if let Some(key) = line_key {
        entry_for_line.line_keys.lock().unwrap().push(key);
    }
}

async fn materialize_sprite(entry: Arc<RendererNode>, parent_tk: TransformKey, def: SpriteDef) {
    let mesh_data = sprite_quad(def.size[0], def.size[1]);
    let raw = to_raw_mesh(mesh_data);
    let mode = match def.billboard {
        awsm_scene_schema::BillboardMode::None => BillboardMode::None,
        awsm_scene_schema::BillboardMode::YAxis => BillboardMode::YAxis,
        awsm_scene_schema::BillboardMode::Full => BillboardMode::Full,
    };

    let mesh_key = if let Some(flipbook_def) = def.flipbook {
        // Flipbook-animated sprite. Build the FlipBookMaterial directly
        // (no MaterialDef detour — the material-cache shape was authored
        // for PBR-shaped data and would force-multiplex this through a
        // bunch of irrelevant fields).
        spawn_flipbook_sprite(entry.clone(), parent_tk, raw, &def, flipbook_def).await
    } else {
        let sprite_mat = MaterialDef {
            base_color: def.tint,
            metallic: 0.0,
            roughness: 1.0,
            emissive: [def.tint[0] * 1.8, def.tint[1] * 1.8, def.tint[2] * 1.8],
            double_sided: true,
            ..MaterialDef::default()
        };
        upload_and_track_returning(entry, parent_tk, raw, None, sprite_mat).await
    };
    if let Some(mk) = mesh_key {
        with_renderer_mut(move |r| {
            let _ = r.set_mesh_billboard_mode(mk, mode);
            // Sprites don't cast or receive shadows in v1 — the
            // shadow VS doesn't run the billboard rotation, so the
            // shadow would be authored-orientation (wrong).
            let _ = r.set_mesh_shadow_flags(
                mk,
                awsm_renderer::shadows::MeshShadowFlags {
                    cast: false,
                    receive: false,
                },
            );
        })
        .await;
    }
}

/// Builds a FlipBookMaterial-backed sprite. Uploaded via the
/// transparent path when `alpha_mode == Blend` so blending composes
/// against the opaque scene; opaque / mask paths use the sync
/// `add_raw_mesh`.
async fn spawn_flipbook_sprite(
    entry: Arc<RendererNode>,
    parent_tk: TransformKey,
    raw: RawMeshData,
    sprite: &SpriteDef,
    flipbook: awsm_scene_schema::SpriteFlipBookDef,
) -> Option<awsm_renderer::meshes::MeshKey> {
    use awsm_renderer::materials::flipbook::{FlipBookMaterial, FlipBookMode};
    let alpha_mode = match sprite.alpha_mode {
        awsm_scene_schema::SpriteAlphaMode::Opaque => MaterialAlphaMode::Opaque,
        awsm_scene_schema::SpriteAlphaMode::Mask { cutoff_x1000 } => MaterialAlphaMode::Mask {
            cutoff: cutoff_x1000 as f32 / 1000.0,
        },
        awsm_scene_schema::SpriteAlphaMode::Blend => MaterialAlphaMode::Blend,
    };
    let mode = match flipbook.mode {
        awsm_scene_schema::FlipBookModeDef::Loop => FlipBookMode::Loop,
        awsm_scene_schema::FlipBookModeDef::PingPong => FlipBookMode::PingPong,
        awsm_scene_schema::FlipBookModeDef::Clamp => FlipBookMode::Clamp,
        awsm_scene_schema::FlipBookModeDef::Once => FlipBookMode::Once,
    };
    let is_blend = matches!(alpha_mode, MaterialAlphaMode::Blend);
    let texture_ref = sprite.texture;
    let tint = sprite.tint;
    let entry_for_track = entry.clone();
    let mut fb = FlipBookMaterial::new(alpha_mode, true);
    fb.tint = tint;
    fb.cols = flipbook.cols;
    fb.rows = flipbook.rows;
    fb.frame_count = flipbook.frame_count;
    fb.fps = flipbook.fps;
    fb.time_offset = flipbook.time_offset;
    fb.mode = mode;
    fb.flip_y = flipbook.flip_y;

    if is_blend {
        // Transparent path is async because `add_raw_mesh_transparent`
        // builds + caches a transparent pipeline for the mesh.
        let handle = renderer_handle();
        let mut r = handle.lock().await;
        fb.atlas_tex = texture_ref.and_then(|t| {
            resolve_material_texture(
                &mut r,
                Some(t),
                super::texture_cache::TextureColorRole::Srgb,
            )
        });
        // Split borrow: `materials.insert` takes `&mut self.materials`
        // and `&self.textures` from disjoint fields on AwsmRenderer.
        // Re-borrow through `&mut *r` so the compiler sees the field
        // split explicitly instead of one fat `&mut MutexGuard` borrow.
        let renderer_ref: &mut AwsmRenderer = &mut r;
        let material_key = renderer_ref
            .materials
            .insert(Material::FlipBook(Box::new(fb)), &renderer_ref.textures);
        let tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
        let mesh_key = match r.add_raw_mesh_transparent(raw, tk, material_key).await {
            Ok(mk) => mk,
            Err(err) => {
                tracing::warn!("flipbook sprite: add_raw_mesh_transparent failed: {err}");
                r.remove_material(material_key);
                r.transforms.remove(tk);
                return None;
            }
        };
        entry_for_track.model_meshes.lock().unwrap().push(mesh_key);
        entry_for_track.model_transforms.lock().unwrap().push(tk);
        entry_for_track
            .material_keys
            .lock()
            .unwrap()
            .push(material_key);
        // Ensure any freshly-uploaded atlas texture is visible to the
        // transparent pipeline's bind groups.
        if let Err(err) = r.finalize_gpu_textures().await {
            tracing::warn!("flipbook sprite: finalize_gpu_textures failed: {err}");
        }
        Some(mesh_key)
    } else {
        with_renderer_mut(move |r| {
            fb.atlas_tex = texture_ref.and_then(|t| {
                resolve_material_texture(
                    r,
                    Some(t),
                    super::texture_cache::TextureColorRole::Srgb,
                )
            });
            let material_key = r.materials.insert(Material::FlipBook(Box::new(fb)), &r.textures);
            let tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
            match r.add_raw_mesh(raw, tk, material_key) {
                Ok(mk) => {
                    entry_for_track.model_meshes.lock().unwrap().push(mk);
                    entry_for_track.model_transforms.lock().unwrap().push(tk);
                    entry_for_track
                        .material_keys
                        .lock()
                        .unwrap()
                        .push(material_key);
                    Some(mk)
                }
                Err(err) => {
                    tracing::warn!("flipbook sprite: add_raw_mesh failed: {err}");
                    r.remove_material(material_key);
                    r.transforms.remove(tk);
                    None
                }
            }
        })
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn materialize_sweep(
    entry: Arc<RendererNode>,
    parent_tk: TransformKey,
    curve_def: CurveDef,
    cross_section: CrossSectionDef,
    uv_mode: SweepUvMode,
    samples: u32,
    up_hint: [f32; 3],
    material_ref: Option<MaterialRef>,
    inline: MaterialDef,
) {
    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    let cs = match cross_section {
        CrossSectionDef::Strip { width, y_offset } => CrossSection::Strip { width, y_offset },
        CrossSectionDef::Tube {
            radius,
            radial_segments,
        } => CrossSection::Tube {
            radius,
            radial_segments,
        },
        CrossSectionDef::Wall { width, height } => CrossSection::Wall { width, height },
        CrossSectionDef::Profile { points, closed } => CrossSection::Profile { points, closed },
    };
    let opts = SweepOpts {
        samples,
        uv_mode: match uv_mode {
            SweepUvMode::StretchOnce => UvMode::StretchOnce,
            SweepUvMode::RepeatByLength {
                u_repeat,
                v_repeat_per_unit,
            } => UvMode::RepeatByLength {
                u_repeat,
                v_repeat_per_unit,
            },
        },
        up_hint,
    };
    let mesh_data = sweep_along_curve(&curve, &cs, &opts);
    let raw = to_raw_mesh(mesh_data);
    upload_and_track(entry, parent_tk, raw, material_ref, inline).await;
}

#[allow(clippy::too_many_arguments)]
async fn materialize_instances_along_curve(
    entry: Arc<RendererNode>,
    parent_tk: TransformKey,
    curve_def: CurveDef,
    def: InstancesAlongCurveDef,
    shape: PrimitiveShape,
    material_ref: Option<MaterialRef>,
    inline: MaterialDef,
) {
    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    let total_len = curve.total_length(curve_def.sample_count.max(8) as usize);
    let spacing = def.spacing.max(0.05);
    let count = ((total_len / spacing).floor() as usize).max(1);
    let frames = FrameSequence::parallel_transport(&curve, count.max(2), Vec3::Y);

    let mut transforms = Vec::with_capacity(count);
    let mut attrs = Vec::with_capacity(count);
    let has_colors = !def.per_instance_colors.is_empty();
    for (i, frame) in frames.frames.iter().enumerate() {
        let mut translation = frame.position;
        if def.side_offset.abs() > 1.0e-4 {
            translation += frame.binormal * def.side_offset;
        }
        let rotation = if def.orient_to_tangent {
            frame.rotation()
        } else {
            Quat::IDENTITY
        };
        transforms.push(Transform {
            translation,
            rotation,
            scale: Vec3::ONE,
        });
        let rgba = if has_colors {
            let idx = i.min(def.per_instance_colors.len() - 1);
            def.per_instance_colors[idx]
        } else {
            [1.0, 1.0, 1.0, 1.0]
        };
        attrs.push(InstanceAttr::from_rgba_alpha_size(rgba, 1.0, 1.0));
    }

    let mesh_data = primitive_to_mesh(&shape);
    let raw = to_raw_mesh(mesh_data);
    let mesh_key = upload_and_track_returning(entry, parent_tk, raw, material_ref, inline).await;
    if let Some(mk) = mesh_key {
        with_renderer_mut(move |r| {
            if let Err(err) = r.enable_mesh_instancing_opaque(mk, &transforms) {
                tracing::warn!("procedural_sync: enable_mesh_instancing_opaque failed: {err}");
            }
            if has_colors {
                let tk = r.meshes.get(mk).map(|m| m.transform_key).ok();
                if let Some(tk) = tk {
                    if let Err(err) = r.set_mesh_instance_attrs(tk, &attrs) {
                        tracing::warn!("procedural_sync: set_mesh_instance_attrs failed: {err}");
                    }
                }
            }
        })
        .await;
    }
}

async fn upload_and_track(
    entry: Arc<RendererNode>,
    parent_tk: TransformKey,
    raw: RawMeshData,
    material_ref: Option<MaterialRef>,
    inline: MaterialDef,
) {
    upload_and_track_returning(entry, parent_tk, raw, material_ref, inline).await;
}

async fn upload_and_track_returning(
    entry: Arc<RendererNode>,
    parent_tk: TransformKey,
    raw: RawMeshData,
    material_ref: Option<MaterialRef>,
    inline: MaterialDef,
) -> Option<MeshKey> {
    let entry_for_track = entry.clone();
    with_renderer_mut(move |r: &mut AwsmRenderer| {
        let scene = app_state().scene.clone();
        let resolved = super::material_cache::resolve(r, &scene, material_ref, &inline);
        let material_key = resolved.key();
        // Spawn a sub-transform under the node's transform so the
        // procedural mesh is positioned by the editor's transform rather
        // than identity. This matches how `instance_template` parents
        // sub-meshes under the model's transform.
        let tk = r.transforms.insert(Transform::IDENTITY, Some(parent_tk));
        match r.add_raw_mesh(raw, tk, material_key) {
            Ok(mk) => {
                entry_for_track.model_meshes.lock().unwrap().push(mk);
                entry_for_track.model_transforms.lock().unwrap().push(tk);
                // Inline materials are owned by this node — park the key
                // so `clear_model_instance` / `remove_node` can free it.
                // Shared (asset-cache) keys are kept alive by the cache.
                if let super::material_cache::ResolvedMaterial::Owned(k) = resolved {
                    entry_for_track.material_keys.lock().unwrap().push(k);
                }
                Some(mk)
            }
            Err(err) => {
                tracing::warn!("procedural_sync: add_raw_mesh failed: {err}");
                // Mesh insert failed — free the just-inserted owned
                // material so we don't leak on the error path.
                if let super::material_cache::ResolvedMaterial::Owned(k) = resolved {
                    r.remove_material(k);
                }
                r.transforms.remove(tk);
                None
            }
        }
    })
    .await
}

/// Snapshot the current mesh-producing geometry for a `NodeKind` into
/// raw `MeshData`. Used by the "Capture as Mesh asset" action so the
/// capture path reuses the same meshgen code the materializer does.
///
/// Returns `None` if the kind doesn't produce mesh data (Group, Light,
/// Curve, etc.) OR if a referenced curve is missing for Sweep.
/// Today's capturable set: Primitive, SweepAlongCurve.
pub fn kind_to_mesh_data(kind: &NodeKind) -> Option<MeshData> {
    match kind {
        NodeKind::Primitive { shape, .. } => Some(primitive_to_mesh(shape)),
        NodeKind::SweepAlongCurve { def, .. } => {
            let curve_def = lookup_curve_def(def.curve_node)?;
            Some(sweep_to_mesh_data(def, &curve_def))
        }
        _ => None,
    }
}

/// Pure-CPU sweep evaluator — extracted so `materialize_sweep` and the
/// capture action share the same `MeshData` shape.
pub(super) fn sweep_to_mesh_data(def: &SweepAlongCurveDef, curve_def: &CurveDef) -> MeshData {
    let curve = CatmullRomCurve::new(
        curve_def
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        curve_def.closed,
    );
    let cs = match def.cross_section.clone() {
        CrossSectionDef::Strip { width, y_offset } => CrossSection::Strip { width, y_offset },
        CrossSectionDef::Tube {
            radius,
            radial_segments,
        } => CrossSection::Tube {
            radius,
            radial_segments,
        },
        CrossSectionDef::Wall { width, height } => CrossSection::Wall { width, height },
        CrossSectionDef::Profile { points, closed } => CrossSection::Profile { points, closed },
    };
    let opts = SweepOpts {
        samples: def.samples,
        uv_mode: match def.uv_mode {
            SweepUvMode::StretchOnce => UvMode::StretchOnce,
            SweepUvMode::RepeatByLength {
                u_repeat,
                v_repeat_per_unit,
            } => UvMode::RepeatByLength {
                u_repeat,
                v_repeat_per_unit,
            },
        },
        up_hint: def.up_hint,
    };
    sweep_along_curve(&curve, &cs, &opts)
}

pub(super) fn primitive_to_mesh(shape: &PrimitiveShape) -> MeshData {
    match shape {
        PrimitiveShape::Plane {
            width,
            depth,
            segments_x,
            segments_z,
        } => plane_mesh(*width, *depth, *segments_x, *segments_z),
        PrimitiveShape::Box { dims } => box_mesh(Vec3::from_array(*dims)),
        PrimitiveShape::Sphere {
            radius,
            segments_long,
            segments_lat,
        } => sphere_mesh(*radius, *segments_long, *segments_lat),
        PrimitiveShape::Cylinder {
            radius,
            height,
            radial_segments,
        } => cylinder_mesh(*radius, *height, *radial_segments),
        PrimitiveShape::Cone {
            radius,
            height,
            radial_segments,
        } => cone_mesh(*radius, *height, *radial_segments),
        PrimitiveShape::Torus {
            radius,
            thickness,
            segments_major,
            segments_minor,
        } => torus_mesh(*radius, *thickness, *segments_major, *segments_minor),
    }
}

fn to_raw_mesh(m: MeshData) -> RawMeshData {
    RawMeshData {
        positions: m.positions,
        normals: m.normals,
        uvs: m.uvs,
        colors: m.colors,
        indices: m.indices,
    }
}

fn captured_to_raw_mesh(c: awsm_scene_schema::CapturedMesh) -> RawMeshData {
    RawMeshData {
        positions: c.positions,
        normals: c.normals,
        uvs: c.uvs,
        colors: c.colors,
        indices: c.indices,
    }
}

/// Wrap an authored `MaterialDef` into the renderer's `Material` enum.
/// Used by `material_cache::get_or_create` to upload assets keyed by id.
pub fn material_def_to_renderer(renderer: &mut AwsmRenderer, def: &MaterialDef) -> Material {
    Material::Pbr(Box::new(material_to_pbr(renderer, def)))
}

fn material_to_pbr(renderer: &mut AwsmRenderer, def: &MaterialDef) -> PbrMaterial {
    let alpha_mode = match def.alpha_mode {
        awsm_scene_schema::MaterialAlphaMode::Opaque => {
            // Back-compat fallback for pre-extension authored materials
            // (alpha_mode defaults to Opaque on load): if the base color
            // has an explicit alpha < 1 we still treat it as a Blend
            // intent, matching the editor's long-standing behaviour for
            // inline-edited materials.
            if def.base_color[3] < 0.999 {
                MaterialAlphaMode::Blend
            } else {
                MaterialAlphaMode::Opaque
            }
        }
        awsm_scene_schema::MaterialAlphaMode::Mask { cutoff } => MaterialAlphaMode::Mask { cutoff },
        awsm_scene_schema::MaterialAlphaMode::Blend => MaterialAlphaMode::Blend,
    };
    let mut pbr = PbrMaterial::new(alpha_mode, def.double_sided);
    pbr.base_color_factor = def.base_color;
    pbr.metallic_factor = def.metallic;
    pbr.roughness_factor = def.roughness;
    pbr.emissive_factor = def.emissive;
    // Honour `vertex_colors_enabled`: the geometry pass picks up the COLOR_0
    // vertex attribute and multiplies it into the base color when this is
    // set. Set-index 0 matches the convention used everywhere else.
    if def.vertex_colors_enabled {
        pbr.vertex_color_info =
            Some(awsm_renderer::materials::pbr::PbrMaterialVertexColorInfo { set_index: 0 });
    }

    // Resolve each of MaterialDef's texture refs to a renderer
    // `TextureKey` via the editor-side texture cache. The cache handles
    // both procedurally-generated and raster (gltf-extracted) textures;
    // missing entries silently leave the corresponding pbr_*_tex None.
    //
    // Color-space tagging mirrors glTF + the renderer-gltf path: only
    // base_color + emissive carry sRGB-encoded pixels; metallic-
    // roughness / normal / occlusion ship as linear data and must NOT
    // be gamma-decoded on upload (the renderer reads those channels
    // directly).
    use super::texture_cache::TextureColorRole;
    pbr.base_color_tex =
        resolve_material_texture(renderer, def.base_color_texture, TextureColorRole::Srgb);
    pbr.metallic_roughness_tex = resolve_material_texture(
        renderer,
        def.metallic_roughness_texture,
        TextureColorRole::Linear,
    );
    pbr.emissive_tex =
        resolve_material_texture(renderer, def.emissive_texture, TextureColorRole::Srgb);
    pbr.normal_tex =
        resolve_material_texture(renderer, def.normal_texture, TextureColorRole::Linear);
    pbr.occlusion_tex =
        resolve_material_texture(renderer, def.occlusion_texture, TextureColorRole::Linear);
    pbr
}

/// Look up a renderer-side `TextureKey` for an authored texture
/// reference and pair it with a default sampler. Both halves are
/// required: `map_texture` in `awsm-materials::writer` returns
/// `SkipTexture` when `sampler_key` is `None`, which silently drops
/// the binding at material-buffer-write time and is what made the
/// pre-extension procedural texture path render flat-coloured even
/// when the cache had uploaded the bitmap successfully.
fn resolve_material_texture(
    renderer: &mut AwsmRenderer,
    texture_ref: Option<awsm_scene_schema::TextureRef>,
    role: super::texture_cache::TextureColorRole,
) -> Option<awsm_renderer::materials::MaterialTexture> {
    let texture_ref = texture_ref?;
    let source = super::texture_cache::asset_source(texture_ref.0)?;
    let key = super::texture_cache::get_or_upload(renderer, texture_ref.0, &source, role)?;
    let sampler_key = renderer
        .textures
        .get_sampler_key(&renderer.gpu, default_material_sampler_key())
        .ok()?;
    // Register the sampler in `pool_sampler_set` (the bind-group's
    // sampler array). Without this, a cache-hit `TextureKey` bound
    // to a not-yet-seen sampler resolves to `sampler_index = None`
    // at draw time and the WGSL writer emits `SkipTexture` —
    // textures vanish and the material renders its base-color
    // factor alone (white for the default [1,1,1,1]). The override
    // path on glTFs whose textures were *all* seeded from
    // renderer-gltf (e.g. DamagedHelmet — 5 textures, each used in
    // exactly one role → all 5 seeds succeed → `add_image` never
    // runs on the editor side → default sampler never reaches the
    // pool) is the canonical reproduction.
    //
    // `ensure_sampler_in_pool` returns `true` on first insertion and
    // sets the renderer-side `sampler_pool_dirty` flag, which
    // `finalize_gpu_textures` ORs into its rebuild gate so the
    // texture-pool bind group + dependent pipeline layouts get
    // refreshed before the next frame. `instance_batcher` already
    // calls `finalize_gpu_textures` at the end of each materialize
    // batch — no extra trigger needed here.
    renderer.textures.ensure_sampler_in_pool(sampler_key);
    Some(awsm_renderer::materials::MaterialTexture {
        key,
        sampler_key: Some(sampler_key),
        uv_index: Some(0),
        transform_key: None,
    })
}

/// Default sampler config for material textures bound through the
/// editor's override path. Matches `renderer-gltf`'s
/// `create_sampler_key` defaults: Linear min/mag/mip filter, Repeat
/// wrap, max anisotropy 16. `SamplerCacheKey::default()` leaves every
/// field as `None`, which the WebGPU driver resolves to
/// Nearest / ClampToEdge / no anisotropy — pixelated and dim on
/// any non-trivial PBR texture. Matching gltf's defaults keeps the
/// editor's `MaterialDef` override visually equivalent to the
/// renderer-baked path on freshly-inserted glTFs.
fn default_material_sampler_key() -> awsm_renderer::textures::SamplerCacheKey {
    use awsm_renderer::textures::SamplerCacheKey;
    use awsm_renderer_core::sampler::{AddressMode, FilterMode, MipmapFilterMode};
    SamplerCacheKey {
        min_filter: Some(FilterMode::Linear),
        mag_filter: Some(FilterMode::Linear),
        mipmap_filter: Some(MipmapFilterMode::Linear),
        address_mode_u: Some(AddressMode::Repeat),
        address_mode_v: Some(AddressMode::Repeat),
        address_mode_w: Some(AddressMode::Repeat),
        max_anisotropy: Some(16),
        ..Default::default()
    }
}
