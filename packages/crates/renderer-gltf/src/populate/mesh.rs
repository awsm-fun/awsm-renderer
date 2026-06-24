use std::{future::Future, pin::Pin, sync::Arc};

use awsm_renderer::{
    bounds::Aabb,
    meshes::{
        buffer_info::{
            MeshBufferCustomVertexAttributeInfo, MeshBufferInfo, MeshBufferVertexAttributeInfo,
        },
        geometry::GeometrySource,
        MeshKey,
    },
    raw_mesh::AddMeshOpts,
    transforms::{Transform, TransformKey},
    AwsmRenderer,
};
use glam::{Mat4, Vec3};

use crate::{
    error::{AwsmGltfError, Result},
    populate::material::pbr_material_mapper,
};

use super::animation::GltfAnimationExt;
use super::GltfMaterialLookupKey;
use super::GltfMaterialSource;
use super::GltfPopulateContext;

/// Per-crate extension trait carrying mesh-population methods on
/// `AwsmRenderer`. Internal to this crate.
pub(crate) trait GltfMeshExt {
    fn populate_gltf_node_mesh<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_node: &'b gltf::Node<'b>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    #[allow(async_fn_in_trait)]
    async fn populate_gltf_primitive(
        &mut self,
        ctx: &GltfPopulateContext,
        gltf_node: &gltf::Node<'_>,
        gltf_mesh: &gltf::Mesh<'_>,
        gltf_primitive: gltf::Primitive<'_>,
        transform_key: TransformKey,
        skin_transform: Option<Arc<(Vec<TransformKey>, Vec<Mat4>)>>,
    ) -> Result<MeshKey>;
}

impl GltfMeshExt for AwsmRenderer {
    fn populate_gltf_node_mesh<'a, 'b: 'a, 'c: 'a>(
        &'a mut self,
        ctx: &'c GltfPopulateContext,
        gltf_node: &'b gltf::Node<'b>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            if let Some(gltf_mesh) = gltf_node.mesh() {
                // from the spec: "Only the joint transforms are applied to the skinned mesh; the transform of the skinned mesh node MUST be ignored."
                // so we swap out this node's transform with an identity matrix, but keep the hierarchy intact
                // might need to pass the joint transform key down too, not sure yet
                let mesh_transform_key = {
                    let node_to_transform =
                        &ctx.key_lookups.lock().unwrap().node_index_to_transform;
                    let transform_key = node_to_transform.get(&gltf_node.index()).cloned().unwrap();
                    if ctx
                        .transform_is_joint
                        .lock()
                        .unwrap()
                        .contains(&transform_key)
                    {
                        let parent_transform_key = self.transforms.get_parent(transform_key).ok();
                        self.transforms
                            .insert(Transform::IDENTITY, parent_transform_key)
                    } else {
                        transform_key
                    }
                };

                // We use the same matrices across the primitives
                // but the skin as a whole is defined on the mesh
                // from the spec: "When defined, mesh MUST also be defined."
                let mesh_skin_transform = {
                    let mesh_skin_transform = ctx.node_to_skin_transform.lock().unwrap();
                    mesh_skin_transform.get(&gltf_node.index()).cloned()
                };

                for gltf_primitive in gltf_mesh.primitives() {
                    let mesh_key = self
                        .populate_gltf_primitive(
                            ctx,
                            gltf_node,
                            &gltf_mesh,
                            gltf_primitive,
                            mesh_transform_key,
                            mesh_skin_transform.clone(),
                        )
                        .await?;

                    ctx.key_lookups
                        .lock()
                        .unwrap()
                        .insert_mesh(gltf_node, &gltf_mesh, mesh_key);
                }
            }

            for child in gltf_node.children() {
                self.populate_gltf_node_mesh(ctx, &child).await?;
            }

            Ok(())
        })
    }

    async fn populate_gltf_primitive(
        &mut self,
        ctx: &GltfPopulateContext,
        gltf_node: &gltf::Node<'_>,
        gltf_mesh: &gltf::Mesh<'_>,
        gltf_primitive: gltf::Primitive<'_>,
        transform_key: TransformKey,
        skin_transform: Option<Arc<(Vec<TransformKey>, Vec<Mat4>)>>,
    ) -> Result<MeshKey> {
        let primitive_buffer_info =
            &ctx.data.buffers.meshes[gltf_mesh.index()][gltf_primitive.index()];

        let native_primitive_buffer_info = MeshBufferInfo::from(primitive_buffer_info.clone());
        let vertex_color_set_index =
            extract_vertex_color_set_index(&primitive_buffer_info.triangles.vertex_attributes);

        let gltf_material = gltf_primitive.material();
        let gltf_material_index = gltf_material.index();
        let material_lookup_key = GltfMaterialLookupKey {
            material_index: gltf_material_index,
            vertex_color_set_index,
            hud: ctx.data.hints.hud,
        };

        let geometry_morph_key = match primitive_buffer_info.geometry_morph.clone() {
            None => None,
            Some(morph_buffer_info) => {
                let values = &ctx.data.buffers.geometry_morph_bytes;
                let values = &values[morph_buffer_info.values_offset
                    ..morph_buffer_info.values_offset + morph_buffer_info.values_size];

                // from spec: "The number of array elements MUST match the number of morph targets."
                // this is generally verified in the insert() call too
                let weights = gltf_mesh.weights().unwrap();
                let weights_u8 = unsafe {
                    std::slice::from_raw_parts(weights.as_ptr() as *const u8, weights.len() * 4)
                };

                Some(self.meshes.morphs.geometry.insert_raw(
                    morph_buffer_info.into(),
                    weights_u8,
                    values,
                )?)
            }
        };

        // Material morphs are deprecated - all morphs (position, normal, tangent) are now in geometry_morph
        let material_morph_key = None;

        let skin_key = match (skin_transform, primitive_buffer_info.skin.clone()) {
            (None, None) => None,
            (Some(_), None) => {
                return Err(AwsmGltfError::SkinPartialData(
                    "Got transform but no buffers".to_string(),
                ));
            }
            (None, Some(_)) => {
                return Err(AwsmGltfError::SkinPartialData(
                    "Got buffers but no transform".to_string(),
                ));
            }
            (Some(data), Some(info)) => {
                let joints = &data.0;
                let inverse_bind_matrices = &data.1;
                let index_weights = &ctx.data.buffers.skin_joint_index_weight_bytes;
                let index_weights = &index_weights[info.index_weights_offset
                    ..info.index_weights_offset + info.index_weights_size];
                Some(self.meshes.skins.insert(
                    joints.clone(),
                    inverse_bind_matrices,
                    info.set_count,
                    index_weights,
                )?)
            }
        };

        let double_sided = gltf_material.double_sided()
            && !should_force_single_sided_for_opaque_thin_shell(
                &gltf_primitive,
                &gltf_material,
                &ctx.data.buffers.raw,
            );

        let material_key = if let GltfMaterialSource::Single(key) = ctx.material_source {
            // The caller supplied the material (our runtime glb: one per node,
            // from scene.toml). Skip glTF material + texture creation + pipeline
            // scheduling entirely — no throwaway default to mint and replace.
            key
        } else {
            let existing = ctx
                .material_keys
                .lock()
                .unwrap()
                .get(&material_lookup_key)
                .copied();

            match existing {
                Some(key) => key,
                None => {
                    // `AWSM_materials_none` (primitive-level, plural — emitted by
                    // awsm-renderer-glb-export for custom-WGSL / Toon materials): the GLB
                    // carries NO embedded glTF material. Render geometry-only
                    // (Unlit) here rather than fabricating a PBR default; the
                    // editor/player re-binds the real material via scene-level
                    // assignment on import. (Distinct from the legacy
                    // material-level singular `AWSM_material_none` handled in
                    // `pbr_material_mapper`.)
                    let material = if gltf_primitive
                        .extension_value("AWSM_materials_none")
                        .is_some()
                    {
                        awsm_renderer::materials::Material::Unlit(
                            awsm_renderer::materials::unlit::UnlitMaterial::new(
                                awsm_renderer::materials::MaterialAlphaMode::Opaque,
                                gltf_material.double_sided(),
                            ),
                        )
                    } else {
                        pbr_material_mapper(self, ctx, primitive_buffer_info, gltf_material).await?
                    };
                    // Block A.3: also bridge first-party materials
                    // through the pipeline-readiness scheduler so the
                    // scheduler's view of "what materials are in this
                    // scene" stays accurate. First-party pipelines are
                    // pre-compiled in the cold-boot eager set so we can
                    // mark Ready immediately — the entry is purely
                    // observability (frontends watching the status
                    // stream see PBR / UNLIT / TOON / FLIPBOOK
                    // materials register here). Dynamic materials
                    // route through `register_material` which already
                    // bridges via A.1.
                    let shader_id = material.shader_id();
                    let alpha_mode_for_def = match &material {
                        awsm_renderer::materials::Material::Pbr(m) => *m.alpha_mode(),
                        awsm_renderer::materials::Material::Unlit(m) => *m.alpha_mode(),
                        awsm_renderer::materials::Material::Toon(m) => *m.alpha_mode(),
                        awsm_renderer::materials::Material::FlipBook(m) => *m.alpha_mode(),
                        awsm_renderer::materials::Material::Custom(m) => m.alpha_mode,
                    };
                    let double_sided_for_def = material.double_sided();
                    let key = self.materials.insert(
                        material,
                        &self.textures,
                        &self.dynamic_materials,
                        &self.extras_pool,
                    );
                    // Submit MaterialDef::FirstParty for first-party
                    // shader_ids only (dynamic flow uses
                    // register_material's A.1 bridge). Failures here
                    // are non-fatal — the mesh still routes through
                    // the existing material_key path; only scheduler
                    // observability degrades.
                    if !shader_id.is_dynamic() {
                        use awsm_renderer::pipeline_scheduler::{
                            MaterialDef, MaterialDefKind, PipelineConfigSnapshot, PipelineGroupDef,
                            PipelineGroupId,
                        };
                        // De-duplicate scheduler submissions per `shader_id`.
                        //
                        // First-party scheduler entries are tracked per-
                        // shader-id, not per-gltf-material: the underlying
                        // compile (`launch_first_party_material_compile`)
                        // looks up a single matching entry by `shader_id`
                        // and only marks THAT one Ready. If we submitted a
                        // fresh entry for every gltf material in the
                        // scene (a scene with two PBR materials would
                        // push two entries here), the second + subsequent
                        // entries would stay Pending forever — and
                        // `drain_pipeline_status_events` + the compile
                        // modal would never balance. The per-shader-id
                        // pipeline cache is what the dispatch site reads,
                        // so one scheduler entry per `shader_id` is the
                        // right tracking shape.
                        if !shader_id.is_dynamic()
                            && self
                                .pipeline_scheduler
                                .find_material_by_shader_id(shader_id)
                                .is_none()
                        {
                            let snapshot = PipelineConfigSnapshot {
                                msaa: self.anti_aliasing.clone(),
                                mipmap: if self.anti_aliasing.mipmap {
                                    awsm_renderer::render_passes::material_opaque::shader::template::MipmapMode::Gradient
                                } else {
                                    awsm_renderer::render_passes::material_opaque::shader::template::MipmapMode::None
                                },
                                gpu_culling: self.features.gpu_culling,
                                coverage_lod: self.features.coverage_lod,
                                debug_bitmask: 0,
                                default_cull_mode:
                                    awsm_renderer_core::pipeline::primitive::CullMode::Back,
                            };
                            let def = MaterialDef {
                                shader_id,
                                alpha_mode: alpha_mode_for_def,
                                double_sided: double_sided_for_def,
                                kind: MaterialDefKind::FirstParty,
                                config_snapshot: snapshot,
                            };
                            let _ids = self
                                .pipeline_scheduler
                                .submit_pipeline_group_batch(vec![PipelineGroupDef::Material(def)]);
                            let _ = PipelineGroupId::Material; // silence unused-import warning
                        }
                        // The actual compile is render-driven: flag the
                        // reconcile so the next render preamble's
                        // `ensure_scene_pipelines` compiles this
                        // shader_id's opaque pipeline (charged to the
                        // scheduler group just submitted above) for the
                        // active AA config. Idempotent — a second gltf
                        // material with the same shader_id cache-hits.
                        self.materials.mark_variants_dirty();
                    }
                    ctx.material_keys
                        .lock()
                        .unwrap()
                        .insert(material_lookup_key, key);
                    key
                }
            }
        };

        let aabb = try_position_aabb(&gltf_primitive);

        // Pass-INDEPENDENT custom-attribute bytes (UVs/colors), AoS, one record per
        // original vertex — the same slice the legacy `meshes.insert` consumed. Owned
        // by the retained `GeometrySource` until commit packs + frees it (§1 ②).
        let custom_attribute_data_start = primitive_buffer_info.triangles.vertex_attributes_offset;
        let custom_attribute_data_end =
            custom_attribute_data_start + primitive_buffer_info.triangles.vertex_attributes_size;
        let custom_attribute_bytes = ctx.data.buffers.custom_attribute_vertex_bytes
            [custom_attribute_data_start..custom_attribute_data_end]
            .to_vec();

        // Pass-INDEPENDENT per-triangle attribute-index bytes (3 × u32 per triangle),
        // sliced from the shared index buffer via the custom-attribute index offsets.
        let custom_attribute_index_start = primitive_buffer_info
            .triangles
            .vertex_attribute_indices
            .offset;
        let custom_attribute_index_size = primitive_buffer_info
            .triangles
            .vertex_attribute_indices
            .checked_total_size()
            .ok_or_else(|| {
                AwsmGltfError::AttributeData(
                    "Custom attribute index byte size overflowed usize".to_string(),
                )
            })?;
        let custom_attribute_index_end = custom_attribute_index_start
            .checked_add(custom_attribute_index_size)
            .ok_or_else(|| {
                AwsmGltfError::AttributeData(
                    "Custom attribute index byte range overflowed usize".to_string(),
                )
            })?;
        if custom_attribute_index_end > ctx.data.buffers.index_bytes.len() {
            return Err(AwsmGltfError::AttributeData(format!(
                "Custom attribute index byte range [{}..{}) exceeds index buffer length {}",
                custom_attribute_index_start,
                custom_attribute_index_end,
                ctx.data.buffers.index_bytes.len()
            )));
        }
        let attribute_index_bytes = ctx.data.buffers.index_bytes
            [custom_attribute_index_start..custom_attribute_index_end]
            .to_vec();

        // Build the retained source (load-transaction "declare"): the per-pass GPU
        // representations + tangents are derived at the next `commit_load` from this,
        // per the union of bound materials (§1) — no kind decision here. Morph/skin
        // layout travels with the source (deltas are kind-independent); the keys were
        // inserted above.
        let source = GeometrySource {
            positions: primitive_buffer_info.source_positions.clone(),
            normals: primitive_buffer_info.source_normals.clone(),
            uvs0: primitive_buffer_info.source_uvs0.clone(),
            tangents: primitive_buffer_info.source_tangents.clone(),
            indices: primitive_buffer_info.source_indices.clone(),
            front_face: primitive_buffer_info.source_front_face,
            vertex_attributes: native_primitive_buffer_info
                .triangles
                .vertex_attributes
                .clone(),
            custom_attribute_bytes,
            attribute_index_bytes,
            aabb,
            geometry_morph_key,
            geometry_morph_info: native_primitive_buffer_info.geometry_morph.clone(),
            material_morph_key,
            material_morph_info: native_primitive_buffer_info.material_morph.clone(),
            skin_key,
            skin_info: native_primitive_buffer_info.skin.clone(),
        };

        let geometry_key = self.register_geometry(source);
        let mesh_key = self.add_mesh(
            geometry_key,
            material_key,
            transform_key,
            AddMeshOpts {
                instanced: ctx
                    .transform_is_instanced
                    .lock()
                    .unwrap()
                    .contains(&transform_key),
                hud: ctx.data.hints.hud,
                hidden: ctx.data.hints.hidden,
                // Preserve the glTF-only single-sided thin-shell heuristic the bound
                // material alone can't express (`should_force_single_sided_for_opaque_thin_shell`).
                double_sided: Some(double_sided),
            },
        )?;

        // Record the originating glTF material index so downstream
        // consumers (notably the editor) can override the baked
        // material with an editable extraction at instantiate time.
        ctx.key_lookups
            .lock()
            .unwrap()
            .mesh_key_to_gltf_material_index
            .insert(mesh_key, gltf_material_index);

        if let Some(sampler_ref) = ctx
            .node_animation_samplers
            .get(&gltf_node.index())
            .and_then(|samplers| samplers.morph)
        {
            self.populate_gltf_animation_morph(
                ctx,
                ctx.resolve_animation_sampler(sampler_ref)?,
                geometry_morph_key,
                material_morph_key,
            )?;
        }

        Ok(mesh_key)
    }
}

fn extract_vertex_color_set_index(attributes: &[MeshBufferVertexAttributeInfo]) -> Option<usize> {
    attributes.iter().find_map(|attr| {
        if let MeshBufferVertexAttributeInfo::Custom(
            MeshBufferCustomVertexAttributeInfo::Colors { index, .. },
        ) = attr
        {
            Some(*index as usize)
        } else {
            None
        }
    })
}

fn should_force_single_sided_for_opaque_thin_shell(
    primitive: &gltf::Primitive<'_>,
    material: &gltf::Material<'_>,
    buffers: &[Vec<u8>],
) -> bool {
    // Tuned for opaque "thin shell" meshes where double-sided rendering causes unstable depth
    // ordering; values are conservative to avoid forcing single-sided on regular solids.
    const THIN_SHELL_RATIO_THRESHOLD: f32 = 0.02;
    const AXIS_NORMAL_MIN: f32 = 0.25;
    const MIN_STRONG_NORMAL_SAMPLES: usize = 16;
    const MIN_AXIS_SIDE_RATIO: f32 = 0.2;

    if !material.double_sided() {
        return false;
    }

    match material.alpha_mode() {
        gltf::material::AlphaMode::Opaque => {}
        _ => return false,
    }

    if let Some(transmission) = material.transmission() {
        if transmission.transmission_factor() > 0.0 || transmission.transmission_texture().is_some()
        {
            return false;
        }
    }

    let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|b| b.as_slice()));

    let Some(positions) = reader.read_positions() else {
        return false;
    };

    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for p in positions {
        let p = Vec3::from_array(p);
        min = min.min(p);
        max = max.max(p);
    }

    let size = max - min;
    let (thin_axis, thin_extent, thick_extent) = if size.x <= size.y && size.x <= size.z {
        (0usize, size.x, size.y.max(size.z))
    } else if size.y <= size.x && size.y <= size.z {
        (1usize, size.y, size.x.max(size.z))
    } else {
        (2usize, size.z, size.x.max(size.y))
    };

    if thick_extent <= f32::EPSILON {
        return false;
    }

    // Heuristic: if one axis is very thin and normals strongly point in opposite directions
    // along that axis (both +axis and -axis present), geometry likely has top+bottom layers
    // and culling back faces is more stable than honoring double-sided rendering.
    if thin_extent / thick_extent > THIN_SHELL_RATIO_THRESHOLD {
        return false;
    }

    let Some(normals) = reader.read_normals() else {
        return false;
    };

    let mut pos_count = 0usize;
    let mut neg_count = 0usize;
    let mut strong_count = 0usize;

    for n in normals {
        let axis = n[thin_axis];
        if axis >= AXIS_NORMAL_MIN {
            pos_count += 1;
            strong_count += 1;
        } else if axis <= -AXIS_NORMAL_MIN {
            neg_count += 1;
            strong_count += 1;
        }
    }

    if strong_count < MIN_STRONG_NORMAL_SAMPLES {
        return false;
    }

    let pos_ratio = pos_count as f32 / strong_count as f32;
    let neg_ratio = neg_count as f32 / strong_count as f32;

    pos_ratio > MIN_AXIS_SIDE_RATIO && neg_ratio > MIN_AXIS_SIDE_RATIO
}

fn try_position_aabb(gltf_primitive: &gltf::Primitive<'_>) -> Option<Aabb> {
    let positions_attribute = gltf_primitive
        .attributes()
        .find_map(|(semantic, attribute)| {
            if semantic == gltf::Semantic::Positions {
                Some(attribute)
            } else {
                None
            }
        })?;

    let min = positions_attribute.min()?;
    let min = min.as_array()?;
    let max = positions_attribute.max()?;
    let max = max.as_array()?;

    if min.len() != 3 || max.len() != 3 {
        return None;
    }

    let min_x = min[0].as_f64()?;
    let min_y = min[1].as_f64()?;
    let min_z = min[2].as_f64()?;
    let max_x = max[0].as_f64()?;
    let max_y = max[1].as_f64()?;
    let max_z = max[2].as_f64()?;

    Some(Aabb {
        min: Vec3::new(min_x as f32, min_y as f32, min_z as f32),
        max: Vec3::new(max_x as f32, max_y as f32, max_z as f32),
    })
}
