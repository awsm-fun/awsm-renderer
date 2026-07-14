//! Integration test for `AssetSource::Material(MaterialDef)` save/load
//! round-trip.
//!
//! The editor's UI mutates a `MaterialDef` stored inside an asset entry;
//! Save serializes the whole `EditorProject` (today as JSON; the build
//! pipeline also goes through bitcode). Load deserializes and the
//! materializer reads the def back. If serde drifts from the runtime
//! shape (rename, missing-field default, enum-tag change), the inspector
//! would silently lose data on the next reload.
//!
//! This test covers the lockstep_game_data side end-to-end: build an
//! `EditorProject` carrying a material asset whose def exercises every
//! field (incl. the `Toon` shading variant landed in F2 and the alpha
//! channel surfaced by F3), serialize through both serde-JSON and
//! bitcode, deserialize, and assert deep equality.

use awsm_renderer_editor_protocol::{
    AssetEntry, AssetId, AssetSource, AssetTable, EditorProject, EnvironmentConfig, MaterialDef,
    MaterialShading, SpecularExt, StoredMaterial,
};

fn material_asset(_id: AssetId) -> AssetEntry {
    AssetEntry::new(AssetSource::Material(MaterialDef {
        label: "Spec material".to_string(),
        // Non-trivial values per field so a "wrong default" regression
        // would visibly diverge — defaults are 1.0/0.0/0.7/false-zeros.
        base_color: [0.25, 0.5, 0.75, 0.4],
        metallic: 0.6,
        roughness: 0.35,
        emissive: [0.1, 0.2, 0.3],
        double_sided: true,
        vertex_colors_enabled: true,
        shading: MaterialShading::Toon {
            diffuse_bands: 7,
            rim_strength: 0.42,
            specular_steps: 3,
            shininess: 48.0,
            rim_power: 1.5,
        },
        ..MaterialDef::default()
    }))
}

fn sample_project_with_material(asset_id: AssetId) -> EditorProject {
    let mut assets = AssetTable::new();
    assets.entries.insert(asset_id, material_asset(asset_id));
    EditorProject {
        name: String::new(),
        environment: EnvironmentConfig::default(),
        shadows: Default::default(),
        post_process: Default::default(),
        assets,
        custom_materials: Vec::new(),
        editor_materials: Vec::new(),
        custom_animations: Vec::new(),
        editor_animations: Vec::new(),
        anim_mixer: Default::default(),
        bundle_options: Default::default(),
        nodes: Vec::new(),
    }
}

#[test]
fn material_json_roundtrip() {
    let asset_id = AssetId::new();
    let project = sample_project_with_material(asset_id);
    let json = serde_json::to_string(&project).expect("serialize");
    let back: EditorProject = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(project, back, "JSON round-trip drifted");

    // Spot-check the material survived end-to-end (the assert_eq! above
    // already covers it; this is a readable failure if it ever breaks).
    let entry = back.assets.entries.get(&asset_id).expect("asset present");
    match &entry.source {
        AssetSource::Material(def) => match def.shading {
            MaterialShading::Toon {
                diffuse_bands,
                rim_strength,
                specular_steps,
                shininess,
                rim_power,
            } => {
                assert_eq!(diffuse_bands, 7);
                assert!((rim_strength - 0.42).abs() < 1.0e-6);
                assert_eq!(specular_steps, 3);
                assert!((shininess - 48.0).abs() < 1.0e-6);
                assert!((rim_power - 1.5).abs() < 1.0e-6);
            }
            other => panic!("expected Toon shading, got {other:?}"),
        },
        other => panic!("expected Material source, got {other:?}"),
    }
}

#[test]
fn editor_material_library_roundtrips_through_toml() {
    // A built-in PBR library material with a KHR extension enabled — the exact
    // thing that must survive Save→Load so the Material pane + assigned meshes
    // come back. Round-trip the whole EditorProject through TOML (the on-disk
    // project format) and assert the stored material is byte-for-byte preserved.
    let id = AssetId::new();
    let mut builtin = MaterialDef {
        shading: MaterialShading::Pbr,
        metallic: 0.3,
        roughness: 0.55,
        ..MaterialDef::default()
    };
    builtin.extensions.specular = Some(SpecularExt {
        factor: 0.8,
        color_factor: [0.9, 0.8, 1.0],
        ..Default::default()
    });

    let stored = StoredMaterial {
        id,
        name: "Glossy".to_string(),
        builtin: Some(builtin.clone()),
        registered: true,
        ..StoredMaterial {
            id,
            name: String::new(),
            builtin: None,
            wgsl: String::new(),
            alpha_wgsl: String::new(),
            vertex_wgsl: String::new(),
            alpha: "opaque".to_string(),
            cutoff: 0.5,
            double_sided: false,
            color: "#8aa0b8".to_string(),
            uniforms: Vec::new(),
            textures: Vec::new(),
            buffers: Vec::new(),
            registered: false,
            shader_includes: Vec::new(),
            fragment_inputs: Vec::new(),
        }
    };

    let project = EditorProject {
        name: "p".to_string(),
        environment: EnvironmentConfig::default(),
        shadows: Default::default(),
        post_process: Default::default(),
        assets: AssetTable::new(),
        custom_materials: Vec::new(),
        editor_materials: vec![stored.clone()],
        custom_animations: Vec::new(),
        editor_animations: Vec::new(),
        anim_mixer: Default::default(),
        bundle_options: Default::default(),
        nodes: Vec::new(),
    };

    let toml_str = toml::to_string_pretty(&project).expect("toml serialize");
    let back: EditorProject = toml::from_str(&toml_str).expect("toml deserialize");
    assert_eq!(project, back, "editor_materials drifted through TOML");

    let m = &back.editor_materials[0];
    assert_eq!(m.id, id);
    assert_eq!(m.name, "Glossy");
    let def = m.builtin.as_ref().expect("builtin survived");
    let spec = def.extensions.specular.expect("specular ext survived");
    assert!((spec.factor - 0.8).abs() < 1.0e-6);
}

#[test]
fn material_bitcode_roundtrip() {
    let asset_id = AssetId::new();
    let project = sample_project_with_material(asset_id);
    let bytes = bitcode::serialize(&project).expect("bitcode serialize");
    let back: EditorProject = bitcode::deserialize(&bytes).expect("bitcode deserialize");
    assert_eq!(project, back, "bitcode round-trip drifted");
}

#[test]
fn material_mutation_survives_roundtrip() {
    // The editor's flow is: open project → mutate the MaterialDef behind
    // an asset entry → Save. Reproduce that ordering and assert the
    // mutated values survive both encodings.
    let asset_id = AssetId::new();
    let mut project = sample_project_with_material(asset_id);

    // Mutate after construction, the way the inspector does.
    if let Some(entry) = project.assets.entries.get_mut(&asset_id) {
        if let AssetSource::Material(def) = &mut entry.source {
            def.base_color = [0.9, 0.1, 0.05, 0.85];
            def.metallic = 0.95;
            def.roughness = 0.05;
            def.emissive = [0.0, 0.0, 1.5];
            def.shading = MaterialShading::Unlit;
        }
    }

    let json = serde_json::to_string(&project).unwrap();
    let back_json: EditorProject = serde_json::from_str(&json).unwrap();
    assert_eq!(project, back_json);

    let bytes = bitcode::serialize(&project).unwrap();
    let back_bin: EditorProject = bitcode::deserialize(&bytes).unwrap();
    assert_eq!(project, back_bin);
}
