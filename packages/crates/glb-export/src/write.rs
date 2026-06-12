//! [`write_glb`] — lower a [`GlbScene`] to a self-contained binary glTF (`.glb`).
//!
//! All geometry, image, and animation bytes go into a single buffer (the GLB
//! `BIN` chunk); the JSON model (`gltf_json::Root`) references them via buffer
//! views + accessors. The container is the standard 12-byte GLB header followed
//! by the `JSON` and (when non-empty) `BIN` chunks.

use gltf_json::validation::{Checked, USize64};
use gltf_json::{
    accessor, animation, buffer, camera, extensions, material, mesh, scene, texture, Accessor,
    Animation, Camera, Image, Index, Material, Mesh, Node, Root, Scene, Skin, Texture,
};
use serde_json::json;

use crate::{
    AlphaMode, AnimInterp, AnimPath, ExportCamera, ExportLight, ExportMaterial, ExportNode,
    ExportSkin, GlbScene, PbrMaterial, TexRef, UnlitMaterial, AWSM_MATERIALS_NONE,
};

const GLB_VERSION: u32 = 2;

const LIGHTS_PUNCTUAL: &str = "KHR_lights_punctual";
const MATERIALS_UNLIT: &str = "KHR_materials_unlit";

/// Serialize a [`GlbScene`] to a binary glTF (`.glb`) byte vector.
///
/// The scene's node forest is emitted in depth-first order (so animation
/// channels' `node_index` matches the glTF node indices), materials are mapped
/// per the crate's material policy, and only the images present in
/// [`GlbScene::images`] are embedded (referenced-only).
pub fn write_glb(scene: &GlbScene) -> Vec<u8> {
    let mut b = Builder::default();
    b.build(scene);
    b.into_glb()
}

#[derive(Default)]
struct Builder {
    root: Root,
    bin: Vec<u8>,
}

impl Builder {
    fn build(&mut self, scene: &GlbScene) {
        self.root.asset = gltf_json::Asset {
            generator: Some("awsm-glb-export".to_string()),
            ..Default::default()
        };

        // 1. Images + one texture each (referenced-only: caller curates the pool).
        let texture_indices: Vec<Index<Texture>> = scene
            .images
            .iter()
            .map(|img| self.push_image_texture(img))
            .collect();

        // 2. DFS flatten so node indices are stable + match animation channels.
        let mut flat: Vec<&ExportNode> = Vec::new();
        let mut child_idx: Vec<Vec<usize>> = Vec::new();
        let roots = flatten(&scene.nodes, &mut flat, &mut child_idx);

        // 3. Per-node payloads (mesh / material / camera / light).
        let mut mesh_idx: Vec<Option<Index<Mesh>>> = Vec::with_capacity(flat.len());
        let mut camera_idx: Vec<Option<Index<Camera>>> = Vec::with_capacity(flat.len());
        let mut light_ext: Vec<Option<extensions::scene::Node>> = Vec::with_capacity(flat.len());
        for n in &flat {
            mesh_idx.push(match &n.mesh {
                Some(m) if !m.positions.is_empty() => Some(self.build_mesh(n, &texture_indices)),
                _ => None,
            });
            camera_idx.push(n.camera.map(|c| self.build_camera(&c, &n.name)));
            light_ext.push(n.light.map(|l| self.build_light(&l, &n.name)));
        }

        // 3b. Skins (joint refs are flat node indices, set above; inverse-bind
        // accessors go in the BIN buffer). A node binds to one via `n.skin`.
        let skin_idx: Vec<Index<Skin>> = scene.skins.iter().map(|s| self.build_skin(s)).collect();

        // 4. Nodes, in flat order — index i is node i.
        for (i, n) in flat.iter().enumerate() {
            let children = &child_idx[i];
            let node = Node {
                camera: camera_idx[i],
                children: if children.is_empty() {
                    None
                } else {
                    Some(children.iter().map(|c| Index::new(*c as u32)).collect())
                },
                extensions: light_ext[i].clone(),
                extras: Default::default(),
                matrix: None,
                mesh: mesh_idx[i],
                name: Some(n.name.clone()),
                rotation: Some(scene::UnitQuaternion(n.transform.rotation)),
                scale: Some(n.transform.scale),
                translation: Some(n.transform.translation),
                skin: n.skin.map(|s| skin_idx[s]),
                weights: None,
            };
            self.root.nodes.push(node);
        }

        // 5. Animations (reference nodes by their flat/glTF index).
        for anim in &scene.animations {
            self.build_animation(anim);
        }

        // 6. Single scene + buffer.
        let scene_obj = Scene {
            extensions: Default::default(),
            extras: Default::default(),
            name: Some("scene".to_string()),
            nodes: roots.iter().map(|r| Index::new(*r as u32)).collect(),
        };
        self.root.scenes.push(scene_obj);
        self.root.scene = Some(Index::new(0));

        if !self.bin.is_empty() {
            self.root.buffers.push(buffer::Buffer {
                byte_length: USize64(self.bin.len() as u64),
                name: None,
                uri: None,
                extensions: Default::default(),
                extras: Default::default(),
            });
        }
    }

    // ───────────────────────── geometry ─────────────────────────

    fn build_mesh(&mut self, n: &ExportNode, texture_indices: &[Index<Texture>]) -> Index<Mesh> {
        let m = n.mesh.as_ref().expect("build_mesh requires a mesh");
        let material = n.material.as_ref();
        let name = n.name.as_str();
        let vcount = m.positions.len();
        let mut attributes = std::collections::BTreeMap::new();

        // POSITION (with required min/max).
        let (min, max) = position_bounds(&m.positions);
        let pos_acc = self.push_accessor(
            &flatten_f32x3(&m.positions),
            m.positions.len(),
            accessor::ComponentType::F32,
            accessor::Type::Vec3,
            Some(json!(min)),
            Some(json!(max)),
        );
        attributes.insert(Checked::Valid(mesh::Semantic::Positions), pos_acc);

        if let Some(normals) = &m.normals {
            if normals.len() == m.positions.len() {
                let acc = self.push_accessor(
                    &flatten_f32x3(normals),
                    normals.len(),
                    accessor::ComponentType::F32,
                    accessor::Type::Vec3,
                    None,
                    None,
                );
                attributes.insert(Checked::Valid(mesh::Semantic::Normals), acc);
            }
        }
        if let Some(uvs) = &m.uvs {
            if uvs.len() == m.positions.len() {
                let acc = self.push_accessor(
                    &flatten_f32x2(uvs),
                    uvs.len(),
                    accessor::ComponentType::F32,
                    accessor::Type::Vec2,
                    None,
                    None,
                );
                attributes.insert(Checked::Valid(mesh::Semantic::TexCoords(0)), acc);
            }
        }
        if let Some(colors) = &m.colors {
            if colors.len() == m.positions.len() {
                let acc = self.push_accessor(
                    &flatten_f32x4(colors),
                    colors.len(),
                    accessor::ComponentType::F32,
                    accessor::Type::Vec4,
                    None,
                    None,
                );
                attributes.insert(Checked::Valid(mesh::Semantic::Colors(0)), acc);
            }
        }

        // TANGENT (vec4: xyz + handedness). Baked here from normals+uvs via
        // MikkTSpace so the canonical/exported glb is self-contained and the
        // population path is a dumb upload (it skips generation when tangents are
        // present). Generated whenever normals+uvs exist — see `tangents` mod.
        if let (Some(normals), Some(uvs)) = (&m.normals, &m.uvs) {
            if let Some(tangents) =
                crate::tangents::generate_tangents(&m.positions, normals, uvs, &m.indices)
            {
                let acc = self.push_accessor(
                    &flatten_f32x4(&tangents),
                    tangents.len(),
                    accessor::ComponentType::F32,
                    accessor::Type::Vec4,
                    None,
                    None,
                );
                attributes.insert(Checked::Valid(mesh::Semantic::Tangents), acc);
            }
        }

        // JOINTS_0 / WEIGHTS_0 (skinned meshes). u16 joint indices + f32 weights,
        // one vec4 per vertex.
        if let (Some(joints), Some(weights)) = (&n.joints, &n.weights) {
            if joints.len() == vcount && weights.len() == vcount {
                let jbytes: Vec<u8> = joints
                    .iter()
                    .flat_map(|j| j.iter().flat_map(|v| v.to_le_bytes()))
                    .collect();
                let jacc = self.push_accessor(
                    &jbytes,
                    vcount,
                    accessor::ComponentType::U16,
                    accessor::Type::Vec4,
                    None,
                    None,
                );
                attributes.insert(Checked::Valid(mesh::Semantic::Joints(0)), jacc);
                let wacc = self.push_accessor(
                    &flatten_f32x4(weights),
                    vcount,
                    accessor::ComponentType::F32,
                    accessor::Type::Vec4,
                    None,
                    None,
                );
                attributes.insert(Checked::Valid(mesh::Semantic::Weights(0)), wacc);
            }
        }

        // Morph targets (position / optional normal deltas).
        let mut targets: Vec<mesh::MorphTarget> = Vec::new();
        for t in &n.morph_targets {
            if t.positions.len() != vcount {
                continue;
            }
            let (tmin, tmax) = position_bounds(&t.positions);
            let positions = Some(self.push_accessor(
                &flatten_f32x3(&t.positions),
                vcount,
                accessor::ComponentType::F32,
                accessor::Type::Vec3,
                Some(json!(tmin)),
                Some(json!(tmax)),
            ));
            let normals = t
                .normals
                .as_ref()
                .filter(|nn| nn.len() == vcount)
                .map(|nn| {
                    self.push_accessor(
                        &flatten_f32x3(nn),
                        vcount,
                        accessor::ComponentType::F32,
                        accessor::Type::Vec3,
                        None,
                        None,
                    )
                });
            targets.push(mesh::MorphTarget {
                positions,
                normals,
                tangents: None,
            });
        }
        // glTF's interoperable home for morph names: mesh.extras.targetNames.
        let mesh_extras: gltf_json::Extras = if n.morph_targets.iter().any(|t| t.name.is_some()) {
            let names: Vec<String> = n
                .morph_targets
                .iter()
                .map(|t| t.name.clone().unwrap_or_default())
                .collect();
            serde_json::value::RawValue::from_string(
                serde_json::json!({ "targetNames": names }).to_string(),
            )
            .ok()
        } else {
            Default::default()
        };

        // Indices (u32 SCALAR).
        let idx_bytes: Vec<u8> = m.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let idx_acc = self.push_accessor(
            &idx_bytes,
            m.indices.len(),
            accessor::ComponentType::U32,
            accessor::Type::Scalar,
            None,
            None,
        );

        // Material → either a glTF material index or the AWSM_materials_none ext.
        let (material_index, primitive_ext) = match material {
            Some(ExportMaterial::Pbr(p)) => (Some(self.build_pbr(p, texture_indices)), None),
            Some(ExportMaterial::Unlit(u)) => (Some(self.build_unlit(u, texture_indices)), None),
            Some(ExportMaterial::None { id }) => (None, Some(self.none_extension(id.as_deref()))),
            None => (None, None),
        };

        let primitive = mesh::Primitive {
            attributes,
            extensions: primitive_ext,
            extras: Default::default(),
            indices: Some(idx_acc),
            material: material_index,
            mode: Checked::Valid(mesh::Mode::Triangles),
            targets: if targets.is_empty() {
                None
            } else {
                Some(targets)
            },
        };

        let mesh_obj = Mesh {
            extensions: Default::default(),
            extras: mesh_extras,
            name: Some(name.to_string()),
            primitives: vec![primitive],
            weights: if n.morph_weights.is_empty() {
                None
            } else {
                Some(n.morph_weights.clone())
            },
        };
        self.root.push(mesh_obj)
    }

    // ───────────────────────── skin ─────────────────────────

    fn build_skin(&mut self, s: &ExportSkin) -> Index<Skin> {
        // inverseBindMatrices: one Mat4 (16 f32, column-major) per joint.
        let inverse_bind_matrices = if s.inverse_bind_matrices.is_empty() {
            None
        } else {
            let bytes: Vec<u8> = s
                .inverse_bind_matrices
                .iter()
                .flat_map(|mtx| mtx.iter().flat_map(|f| f.to_le_bytes()))
                .collect();
            Some(self.push_accessor(
                &bytes,
                s.inverse_bind_matrices.len(),
                accessor::ComponentType::F32,
                accessor::Type::Mat4,
                None,
                None,
            ))
        };
        let skin = Skin {
            extensions: Default::default(),
            extras: Default::default(),
            inverse_bind_matrices,
            joints: s.joints.iter().map(|j| Index::new(*j as u32)).collect(),
            name: None,
            skeleton: s.skeleton.map(|sk| Index::new(sk as u32)),
        };
        self.root.push(skin)
    }

    // ───────────────────────── materials ─────────────────────────

    fn build_pbr(&mut self, p: &PbrMaterial, tex: &[Index<Texture>]) -> Index<Material> {
        let pbr = material::PbrMetallicRoughness {
            base_color_factor: material::PbrBaseColorFactor(p.base_color),
            base_color_texture: p.base_color_texture.map(|t| tex_info(t, tex)),
            metallic_factor: material::StrengthFactor(p.metallic),
            roughness_factor: material::StrengthFactor(p.roughness),
            metallic_roughness_texture: p.metallic_roughness_texture.map(|t| tex_info(t, tex)),
            ..Default::default()
        };
        let mat = Material {
            alpha_cutoff: alpha_cutoff(p.alpha_mode),
            alpha_mode: Checked::Valid(gltf_alpha_mode(p.alpha_mode)),
            double_sided: p.double_sided,
            name: Some(p.name.clone()),
            pbr_metallic_roughness: pbr,
            normal_texture: p.normal_texture.map(|t| material::NormalTexture {
                index: tex[t.image],
                scale: 1.0,
                tex_coord: t.tex_coord,
                extensions: Default::default(),
                extras: Default::default(),
            }),
            occlusion_texture: p.occlusion_texture.map(|t| material::OcclusionTexture {
                index: tex[t.image],
                strength: material::StrengthFactor(1.0),
                tex_coord: t.tex_coord,
                extensions: Default::default(),
                extras: Default::default(),
            }),
            emissive_texture: p.emissive_texture.map(|t| tex_info(t, tex)),
            emissive_factor: material::EmissiveFactor(p.emissive),
            ..Default::default()
        };
        self.root.push(mat)
    }

    fn build_unlit(&mut self, u: &UnlitMaterial, tex: &[Index<Texture>]) -> Index<Material> {
        self.use_extension(MATERIALS_UNLIT);
        let pbr = material::PbrMetallicRoughness {
            base_color_factor: material::PbrBaseColorFactor(u.base_color),
            base_color_texture: u.base_color_texture.map(|t| tex_info(t, tex)),
            ..Default::default()
        };
        let mat = Material {
            alpha_cutoff: alpha_cutoff(u.alpha_mode),
            alpha_mode: Checked::Valid(gltf_alpha_mode(u.alpha_mode)),
            double_sided: u.double_sided,
            name: Some(u.name.clone()),
            pbr_metallic_roughness: pbr,
            extensions: Some(extensions::material::Material {
                unlit: Some(extensions::material::Unlit {}),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.root.push(mat)
    }

    fn none_extension(&mut self, id: Option<&str>) -> extensions::mesh::Primitive {
        self.use_extension(AWSM_MATERIALS_NONE);
        let mut others = serde_json::Map::new();
        others.insert(AWSM_MATERIALS_NONE.to_string(), json!({ "id": id }));
        // The `extensions` gltf-json feature gives `Primitive` an `others` map for
        // unrecognized extensions; that's the only field under this workspace's
        // feature set (no `KHR_materials_variants`).
        extensions::mesh::Primitive { others }
    }

    fn push_image_texture(&mut self, img: &crate::ExportImage) -> Index<Texture> {
        let view = self.push_view(&img.bytes);
        let image = Image {
            buffer_view: Some(view),
            mime_type: Some(gltf_json::image::MimeType(img.mime.as_str().to_string())),
            name: Some(img.name.clone()),
            uri: None,
            extensions: Default::default(),
            extras: Default::default(),
        };
        let image_index = self.root.push(image);
        let tex = Texture {
            name: Some(img.name.clone()),
            sampler: None,
            source: image_index,
            extensions: Default::default(),
            extras: Default::default(),
        };
        self.root.push(tex)
    }

    // ───────────────────────── camera / light ─────────────────────────

    fn build_camera(&mut self, c: &ExportCamera, name: &str) -> Index<Camera> {
        let (perspective, orthographic, ty) = match *c {
            ExportCamera::Perspective {
                yfov,
                aspect_ratio,
                znear,
                zfar,
            } => (
                Some(camera::Perspective {
                    aspect_ratio,
                    yfov,
                    zfar,
                    znear,
                    extensions: Default::default(),
                    extras: Default::default(),
                }),
                None,
                camera::Type::Perspective,
            ),
            ExportCamera::Orthographic {
                xmag,
                ymag,
                znear,
                zfar,
            } => (
                None,
                Some(camera::Orthographic {
                    xmag,
                    ymag,
                    zfar,
                    znear,
                    extensions: Default::default(),
                    extras: Default::default(),
                }),
                camera::Type::Orthographic,
            ),
        };
        let cam = Camera {
            name: Some(name.to_string()),
            orthographic,
            perspective,
            type_: Checked::Valid(ty),
            extensions: Default::default(),
            extras: Default::default(),
        };
        self.root.push(cam)
    }

    fn build_light(&mut self, l: &ExportLight, name: &str) -> extensions::scene::Node {
        use extensions::scene::khr_lights_punctual as klp;
        self.use_extension(LIGHTS_PUNCTUAL);
        let (color, intensity, range, spot, ty) = match *l {
            ExportLight::Directional { color, intensity } => {
                (color, intensity, None, None, klp::Type::Directional)
            }
            ExportLight::Point {
                color,
                intensity,
                range,
            } => (color, intensity, range, None, klp::Type::Point),
            ExportLight::Spot {
                color,
                intensity,
                range,
                inner_cone_angle,
                outer_cone_angle,
            } => (
                color,
                intensity,
                range,
                Some(klp::Spot {
                    inner_cone_angle,
                    outer_cone_angle,
                }),
                klp::Type::Spot,
            ),
        };
        let light = klp::Light {
            color,
            extensions: None,
            extras: Default::default(),
            intensity,
            name: Some(name.to_string()),
            range,
            spot,
            type_: Checked::Valid(ty),
        };
        let light_index: Index<klp::Light> = self.root.push(light);
        extensions::scene::Node {
            khr_lights_punctual: Some(klp::KhrLightsPunctual { light: light_index }),
            ..Default::default()
        }
    }

    // ───────────────────────── animation ─────────────────────────

    fn build_animation(&mut self, anim: &crate::ExportAnimation) {
        let mut samplers = Vec::new();
        let mut channels = Vec::new();
        for ch in &anim.channels {
            let (tmin, tmax) = scalar_bounds(&ch.times);
            let time_bytes: Vec<u8> = ch.times.iter().flat_map(|t| t.to_le_bytes()).collect();
            let input = self.push_accessor(
                &time_bytes,
                ch.times.len(),
                accessor::ComponentType::F32,
                accessor::Type::Scalar,
                Some(json!([tmin])),
                Some(json!([tmax])),
            );
            let out_type = match ch.path {
                AnimPath::Translation | AnimPath::Scale => accessor::Type::Vec3,
                AnimPath::Rotation => accessor::Type::Vec4,
                AnimPath::Weights => accessor::Type::Scalar,
            };
            let comps = match out_type {
                accessor::Type::Vec4 => 4,
                accessor::Type::Vec3 => 3,
                _ => 1,
            };
            let out_bytes: Vec<u8> = ch.values.iter().flat_map(|v| v.to_le_bytes()).collect();
            let output = self.push_accessor(
                &out_bytes,
                ch.values.len() / comps,
                accessor::ComponentType::F32,
                out_type,
                None,
                None,
            );
            let sampler_index = Index::new(samplers.len() as u32);
            samplers.push(animation::Sampler {
                extensions: Default::default(),
                extras: Default::default(),
                input,
                interpolation: Checked::Valid(gltf_interp(ch.interpolation)),
                output,
            });
            channels.push(animation::Channel {
                sampler: sampler_index,
                target: animation::Target {
                    extensions: Default::default(),
                    extras: Default::default(),
                    node: Index::new(ch.node_index as u32),
                    path: Checked::Valid(gltf_anim_path(ch.path)),
                },
                extensions: Default::default(),
                extras: Default::default(),
            });
        }
        let animation = Animation {
            extensions: Default::default(),
            extras: Default::default(),
            channels,
            name: Some(anim.name.clone()),
            samplers,
        };
        self.root.push(animation);
    }

    // ───────────────────────── buffer plumbing ─────────────────────────

    /// Append `data` to the BIN buffer (4-byte aligned) and return a buffer view
    /// covering it.
    fn push_view(&mut self, data: &[u8]) -> Index<buffer::View> {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(data);
        let view = buffer::View {
            buffer: Index::new(0),
            byte_length: USize64(data.len() as u64),
            byte_offset: Some(USize64(offset as u64)),
            byte_stride: None,
            name: None,
            target: None,
            extensions: Default::default(),
            extras: Default::default(),
        };
        self.root.push(view)
    }

    fn push_accessor(
        &mut self,
        data: &[u8],
        count: usize,
        comp: accessor::ComponentType,
        ty: accessor::Type,
        min: Option<serde_json::Value>,
        max: Option<serde_json::Value>,
    ) -> Index<Accessor> {
        let view = self.push_view(data);
        let acc = Accessor {
            buffer_view: Some(view),
            byte_offset: Some(USize64(0)),
            count: USize64(count as u64),
            component_type: Checked::Valid(accessor::GenericComponentType(comp)),
            extensions: Default::default(),
            extras: Default::default(),
            type_: Checked::Valid(ty),
            min,
            max,
            name: None,
            normalized: false,
            sparse: None,
        };
        self.root.push(acc)
    }

    fn use_extension(&mut self, name: &str) {
        if !self.root.extensions_used.iter().any(|e| e == name) {
            self.root.extensions_used.push(name.to_string());
        }
    }

    // ───────────────────────── GLB container ─────────────────────────

    fn into_glb(self) -> Vec<u8> {
        let mut json = serde_json::to_vec(&self.root).expect("serialize gltf json");
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let mut bin = self.bin;
        while bin.len() % 4 != 0 {
            bin.push(0);
        }

        let has_bin = !bin.is_empty();
        let bin_chunk = if has_bin { 8 + bin.len() } else { 0 };
        let total = 12 + 8 + json.len() + bin_chunk;

        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&GLB_VERSION.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());

        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json);

        if has_bin {
            out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
            out.extend_from_slice(b"BIN\0");
            out.extend_from_slice(&bin);
        }
        out
    }
}

// ───────────────────────── free helpers ─────────────────────────

fn flatten<'a>(
    nodes: &'a [ExportNode],
    out: &mut Vec<&'a ExportNode>,
    child_idx: &mut Vec<Vec<usize>>,
) -> Vec<usize> {
    let mut here = Vec::with_capacity(nodes.len());
    for n in nodes {
        let my = out.len();
        out.push(n);
        child_idx.push(Vec::new());
        here.push(my);
        let kids = flatten(&n.children, out, child_idx);
        child_idx[my] = kids;
    }
    here
}

fn tex_info(t: TexRef, tex: &[Index<Texture>]) -> texture::Info {
    texture::Info {
        index: tex[t.image],
        tex_coord: t.tex_coord,
        extensions: Default::default(),
        extras: Default::default(),
    }
}

fn gltf_alpha_mode(m: AlphaMode) -> material::AlphaMode {
    match m {
        AlphaMode::Opaque => material::AlphaMode::Opaque,
        AlphaMode::Mask { .. } => material::AlphaMode::Mask,
        AlphaMode::Blend => material::AlphaMode::Blend,
    }
}

fn alpha_cutoff(m: AlphaMode) -> Option<material::AlphaCutoff> {
    match m {
        AlphaMode::Mask { cutoff } => Some(material::AlphaCutoff(cutoff)),
        _ => None,
    }
}

fn gltf_interp(i: AnimInterp) -> animation::Interpolation {
    match i {
        AnimInterp::Linear => animation::Interpolation::Linear,
        AnimInterp::Step => animation::Interpolation::Step,
        AnimInterp::CubicSpline => animation::Interpolation::CubicSpline,
    }
}

fn gltf_anim_path(p: AnimPath) -> animation::Property {
    match p {
        AnimPath::Translation => animation::Property::Translation,
        AnimPath::Rotation => animation::Property::Rotation,
        AnimPath::Scale => animation::Property::Scale,
        AnimPath::Weights => animation::Property::MorphTargetWeights,
    }
}

fn position_bounds(p: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in p {
        for i in 0..3 {
            min[i] = min[i].min(v[i]);
            max[i] = max[i].max(v[i]);
        }
    }
    if p.is_empty() {
        ([0.0; 3], [0.0; 3])
    } else {
        (min, max)
    }
}

fn scalar_bounds(v: &[f32]) -> (f32, f32) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &x in v {
        min = min.min(x);
        max = max.max(x);
    }
    if v.is_empty() {
        (0.0, 0.0)
    } else {
        (min, max)
    }
}

fn flatten_f32x2(v: &[[f32; 2]]) -> Vec<u8> {
    v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect()
}
fn flatten_f32x3(v: &[[f32; 3]]) -> Vec<u8> {
    v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect()
}
fn flatten_f32x4(v: &[[f32; 4]]) -> Vec<u8> {
    v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect()
}
