//! Texture export: which images ship as-is, which get packed, and what the
//! packed pixels hold. The fixture is generated per test (tiny PNGs + a
//! `.usda`) in a temp directory, so every expected pixel is known exactly.

use std::path::PathBuf;

use awsm_renderer_isaac_export_cli::{export, Export, Options};
use awsm_renderer_mujoco_format::sidecar::{AlphaMode, Material, Wrap};
use image::{Rgba, RgbaImage};

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("awsm-isaac-tex-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tex")).unwrap();
        Self { dir }
    }

    /// A 2×2 PNG with one colour per pixel (row-major).
    fn png(&self, name: &str, px: [[u8; 4]; 4]) -> PathBuf {
        let img = RgbaImage::from_fn(2, 2, |x, y| Rgba(px[(y * 2 + x) as usize]));
        let path = self.dir.join("tex").join(name);
        img.save(&path).unwrap();
        path
    }

    fn solid(&self, name: &str, c: [u8; 4]) -> PathBuf {
        self.png(name, [c; 4])
    }

    /// A one-cube robot whose mesh binds `material_body` (the `def Material`
    /// block's contents).
    fn stage(&self, material_body: &str) -> Export {
        let usda = format!(
            r#"#usda 1.0
(
    defaultPrim = "robot"
    metersPerUnit = 1
    upAxis = "Z"
)
def Xform "robot"
{{
    def Xform "link" (
        prepend apiSchemas = ["PhysicsRigidBodyAPI"]
    )
    {{
        def Mesh "mesh" (
            prepend apiSchemas = ["MaterialBindingAPI"]
        )
        {{
            point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
                interpolation = "vertex"
            )
            rel material:binding = </robot/Looks/M>
        }}
    }}
    def Scope "Looks"
    {{
        def Material "M"
        {{
{material_body}
        }}
    }}
}}
"#
        );
        let path = self.dir.join("robot.usda");
        std::fs::write(&path, usda).unwrap();
        export(&path, &Options::default()).expect("export")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An OmniPBR shader block with `inputs` (one per line).
fn omni(inputs: &str) -> String {
    format!(
        r#"            token outputs:mdl:surface.connect = </robot/Looks/M/Shader.outputs:out>
            def Shader "Shader"
            {{
                uniform token info:implementationSource = "sourceAsset"
                uniform asset info:mdl:sourceAsset = @OmniPBR.mdl@
                uniform token info:mdl:sourceAsset:subIdentifier = "OmniPBR"
{inputs}
                token outputs:out
            }}"#
    )
}

fn material(out: &Export) -> &Material {
    assert_eq!(
        out.sidecar.materials.len(),
        1,
        "{:?}",
        out.sidecar.materials
    );
    &out.sidecar.materials[0]
}

/// The bytes the export would write at sidecar-relative `rel`.
fn image<'a>(out: &'a Export, rel: &str) -> &'a [u8] {
    &out.images
        .iter()
        .find(|(p, _)| p == rel)
        .unwrap_or_else(|| {
            panic!(
                "{rel} not among {:?}",
                out.images.iter().map(|i| &i.0).collect::<Vec<_>>()
            )
        })
        .1
}

fn decoded(out: &Export, rel: &str) -> RgbaImage {
    image::load_from_memory(image(out, rel)).unwrap().to_rgba8()
}

fn assert_valid(out: &Export) {
    out.sidecar.validate().expect("valid sidecar");
    for (rel, _) in &out.images {
        assert!(rel.starts_with("textures/"), "{rel}");
    }
}

#[test]
fn a_diffuse_texture_ships_byte_for_byte_and_replaces_the_constant() {
    let f = Fixture::new("diffuse");
    let src = f.png(
        "albedo.png",
        [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [9, 9, 9, 255],
        ],
    );
    let out = f.stage(&omni(
        r#"                asset inputs:diffuse_texture = @tex/albedo.png@
                color3f inputs:diffuse_color_constant = (0.2, 0.3, 0.4)
                color3f inputs:diffuse_tint = (0.5, 1, 1)"#,
    ));
    let m = material(&out);
    let base = m.textures.base_color.as_ref().expect("base colour texture");
    assert_eq!(
        image(&out, &base.image),
        std::fs::read(src).unwrap().as_slice()
    );
    // OmniPBR: the texture REPLACES the constant; the tint still multiplies.
    assert_eq!(m.rgba, [0.5, 1.0, 1.0, 1.0]);
    assert_eq!(m.alpha_mode, None);
    assert_valid(&out);
}

#[test]
fn roughness_and_metallic_maps_pack_into_one_image_with_their_influence() {
    let f = Fixture::new("mr");
    f.solid("rough.png", [200, 0, 0, 255]); // OmniPBR reads the first channel
    f.solid("metal.png", [100, 0, 0, 255]);
    let out = f.stage(&omni(
        r#"                asset inputs:reflectionroughness_texture = @tex/rough.png@
                float inputs:reflection_roughness_texture_influence = 1
                float inputs:reflection_roughness_constant = 0.2
                asset inputs:metallic_texture = @tex/metal.png@
                float inputs:metallic_texture_influence = 0.5
                float inputs:metallic_constant = 0"#,
    ));
    let m = material(&out);
    let mr = decoded(
        &out,
        &m.textures.metallic_roughness.as_ref().expect("mr").image,
    );
    let px = mr.get_pixel(0, 0).0;
    // G = roughness (influence 1 → the map), B = lerp(0, 100, 0.5) = 50.
    assert_eq!((px[1], px[2]), (200, 50), "{px:?}");
    // The maps carry the values, so the factors are 1: roughness 1 =
    // shininess 0, metallic 1 = reflectance 1.
    assert_eq!((m.shininess, m.reflectance), (0.0, 1.0));
    assert_valid(&out);
}

#[test]
fn a_map_with_no_influence_does_nothing_as_in_omnipbr() {
    let f = Fixture::new("noinfluence");
    f.solid("rough.png", [200, 0, 0, 255]);
    let out = f.stage(&omni(
        r#"                asset inputs:reflectionroughness_texture = @tex/rough.png@
                float inputs:reflection_roughness_constant = 0.25"#,
    ));
    let m = material(&out);
    assert!(m.textures.metallic_roughness.is_none());
    assert!((m.shininess - 0.75).abs() < 1e-6);
    assert!(
        out.report.notes.iter().any(|n| n.contains("influence 0")),
        "{:?}",
        out.report.notes
    );
}

#[test]
fn an_opacity_map_packs_into_alpha_and_a_threshold_cuts_out() {
    let f = Fixture::new("opacity");
    f.solid("albedo.png", [10, 20, 30, 255]);
    // mono_average of (90, 90, 90) = 90; of (0, 30, 60) = 30.
    f.png(
        "opacity.png",
        [
            [90, 90, 90, 255],
            [0, 30, 60, 255],
            [90, 90, 90, 255],
            [0, 30, 60, 255],
        ],
    );
    let out = f.stage(&omni(
        r#"                asset inputs:diffuse_texture = @tex/albedo.png@
                bool inputs:enable_opacity = 1
                bool inputs:enable_opacity_texture = 1
                asset inputs:opacity_texture = @tex/opacity.png@
                float inputs:opacity_threshold = 0.25"#,
    ));
    let m = material(&out);
    assert_eq!(m.alpha_mode, Some(AlphaMode::Mask { cutoff: 0.25 }));
    let base = decoded(&out, &m.textures.base_color.as_ref().unwrap().image);
    assert_eq!(base.get_pixel(0, 0).0, [10, 20, 30, 90]);
    assert_eq!(base.get_pixel(1, 0).0, [10, 20, 30, 30]);
    assert_valid(&out);
}

#[test]
fn opacity_without_a_threshold_blends() {
    let f = Fixture::new("blend");
    let out = f.stage(&omni(
        r#"                bool inputs:enable_opacity = 1
                float inputs:opacity_constant = 0.4"#,
    ));
    let m = material(&out);
    assert_eq!(m.alpha_mode, Some(AlphaMode::Blend));
    assert!((m.rgba[3] - 0.4).abs() < 1e-6);
}

#[test]
fn emission_keeps_its_own_colour_apart_from_the_base() {
    let f = Fixture::new("emissive");
    let out = f.stage(&omni(
        r#"                color3f inputs:diffuse_color_constant = (1, 1, 1)
                bool inputs:enable_emission = 1
                color3f inputs:emissive_color = (0, 0.1, 1)
                float inputs:emissive_intensity = 29000"#,
    ));
    let m = material(&out);
    assert_eq!(m.emissive, Some([0.0, 0.1, 1.0]));
    assert_eq!(&m.rgba[..3], &[1.0, 1.0, 1.0]);
}

#[test]
fn a_normal_map_ships_as_is_unless_a_tangent_flip_is_authored() {
    let f = Fixture::new("normal");
    let src = f.solid("n.png", [128, 200, 255, 255]);
    let plain = f.stage(&omni(
        r#"                asset inputs:normalmap_texture = @tex/n.png@
                float inputs:bump_factor = 0.5"#,
    ));
    let m = material(&plain);
    let n = m.textures.normal.as_ref().unwrap();
    assert_eq!(
        image(&plain, &n.image),
        std::fs::read(&src).unwrap().as_slice()
    );
    assert_eq!(m.textures.normal_scale, 0.5);

    let flipped = f.stage(&omni(
        r#"                asset inputs:normalmap_texture = @tex/n.png@
                bool inputs:flip_tangent_v = 0"#,
    ));
    let m = material(&flipped);
    let img = decoded(&flipped, &m.textures.normal.as_ref().unwrap().image);
    assert_eq!(img.get_pixel(0, 0).0, [128, 55, 255, 255]);
}

#[test]
fn preview_surface_reads_channels_wraps_and_uv_transforms() {
    let f = Fixture::new("preview");
    let src = f.solid("rgba.png", [40, 80, 120, 200]);
    f.solid("orm.png", [11, 170, 60, 255]);
    let out = f.stage(
        r#"            token outputs:surface.connect = </robot/Looks/M/Surface.outputs:surface>
            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </robot/Looks/M/Albedo.outputs:rgb>
                float inputs:opacity.connect = </robot/Looks/M/Albedo.outputs:a>
                float inputs:opacityThreshold = 0.5
                float inputs:roughness.connect = </robot/Looks/M/Orm.outputs:g>
                float inputs:metallic.connect = </robot/Looks/M/Orm.outputs:b>
                token outputs:surface
            }
            def Shader "Albedo"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @tex/rgba.png@
                token inputs:wrapS = "clamp"
                token inputs:wrapT = "mirror"
                float2 inputs:st.connect = </robot/Looks/M/Xf.outputs:result>
                float3 outputs:rgb
                float outputs:a
            }
            def Shader "Xf"
            {
                uniform token info:id = "UsdTransform2d"
                float2 inputs:scale = (2, 2)
                float2 inputs:in.connect = </robot/Looks/M/Reader.outputs:result>
                float2 outputs:result
            }
            def Shader "Reader"
            {
                uniform token info:id = "UsdPrimvarReader_float2"
                string inputs:varname = "st"
                float2 outputs:result
            }
            def Shader "Orm"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @tex/orm.png@
                float outputs:g
                float outputs:b
            }"#,
    );
    let m = material(&out);
    // Colour and opacity come from ONE RGBA file: it ships untouched.
    let base = m
        .textures
        .base_color
        .as_ref()
        .unwrap_or_else(|| panic!("no base texture: {m:?} {:?}", out.report.notes));
    assert_eq!(
        image(&out, &base.image),
        std::fs::read(src).unwrap().as_slice()
    );
    assert_eq!(base.wrap, [Wrap::Clamp, Wrap::Mirror]);
    let t = base.transform.expect("UsdTransform2d → UV transform");
    assert_eq!(t.scale, [2.0, 2.0]);
    // Scale 2 about the origin in USD's V-up space is, in the GLB's V-down
    // space, scale 2 about v = 1: offset v = 1 - 2 = -1.
    assert_eq!(t.offset, [0.0, -1.0]);
    assert_eq!(m.alpha_mode, Some(AlphaMode::Mask { cutoff: 0.5 }));

    // Roughness from G and metallic from B of one image: repacked to glTF's
    // G/B (here the same channels, so the values carry straight over).
    let mr = decoded(&out, &m.textures.metallic_roughness.as_ref().unwrap().image);
    let px = mr.get_pixel(0, 0).0;
    assert_eq!((px[1], px[2]), (170, 60));
    assert_valid(&out);
}

#[test]
fn identical_images_are_written_once() {
    let f = Fixture::new("dedupe");
    f.solid("a.png", [1, 2, 3, 255]);
    f.solid("b.png", [1, 2, 3, 255]);
    let out = f.stage(&omni(
        r#"                asset inputs:diffuse_texture = @tex/a.png@
                bool inputs:enable_emission = 1
                asset inputs:emissive_color_texture = @tex/b.png@"#,
    ));
    let m = material(&out);
    assert_eq!(
        m.textures.base_color.as_ref().unwrap().image,
        m.textures.emissive.as_ref().unwrap().image
    );
    assert_eq!(out.images.len(), 1);
}

#[test]
fn a_missing_texture_drops_its_slot_and_says_so() {
    let f = Fixture::new("missing");
    let out = f.stage(&omni(
        r#"                asset inputs:diffuse_texture = @tex/nope.png@"#,
    ));
    let m = material(&out);
    assert!(m.textures.base_color.is_none());
    assert!(
        out.report.notes.iter().any(|n| n.contains("not found")),
        "{:?}",
        out.report.notes
    );
    // OmniPBR's own default colour, not white.
    assert_eq!(&m.rgba[..3], &[0.2, 0.2, 0.2]);
}

#[test]
fn double_sidedness_rides_the_material_and_splits_it_when_mixed() {
    let f = Fixture::new("doublesided");
    let usda = r#"#usda 1.0
(
    defaultPrim = "robot"
    metersPerUnit = 1
    upAxis = "Z"
)
def Xform "robot"
{
    def Xform "link" ( prepend apiSchemas = ["PhysicsRigidBodyAPI"] )
    {
        def Mesh "sheet" ( prepend apiSchemas = ["MaterialBindingAPI"] )
        {
            point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0)]
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            uniform bool doubleSided = 1
            rel material:binding = </robot/Looks/M>
        }
        def Mesh "solid" ( prepend apiSchemas = ["MaterialBindingAPI"] )
        {
            point3f[] points = [(0, 0, 1), (1, 0, 1), (1, 1, 1)]
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            rel material:binding = </robot/Looks/M>
        }
        def Mesh "bare"
        {
            point3f[] points = [(0, 0, 2), (1, 0, 2), (1, 1, 2)]
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            uniform bool doubleSided = 1
            color3f[] primvars:displayColor = [(0, 1, 0)]
        }
    }
    def Scope "Looks"
    {
        def Material "M"
        {
            token outputs:surface.connect = </robot/Looks/M/S.outputs:surface>
            def Shader "S"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (1, 0, 0)
                token outputs:surface
            }
        }
    }
}
"#;
    let path = f.dir.join("robot.usda");
    std::fs::write(&path, usda).unwrap();
    let out = export(&path, &Options::default()).unwrap();
    let mat = |name: &str| {
        let g = out
            .sidecar
            .geoms
            .iter()
            .find(|g| g.name.as_deref() == Some(name))
            .unwrap();
        &out.sidecar.materials[g.material.expect("a material")]
    };
    assert!(mat("sheet").double_sided);
    assert!(!mat("solid").double_sided);
    assert_eq!(
        mat("sheet").rgba,
        mat("solid").rgba,
        "same look, two entries"
    );
    // No bound material, still two-sided, in its display colour.
    assert!(mat("bare").double_sided);
    assert_eq!(mat("bare").rgba, [0.0, 1.0, 0.0, 1.0]);
    assert_valid(&out);
}
