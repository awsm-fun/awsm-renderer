//! `InstancerDef.material` — the ONE material an instancer's instances render
//! with (there is no variant palette on an instancer; `add_material_variant`
//! rejects it). These lock the wire behaviour the
//! `dynamic-material-attributes` test scene depends on:
//!
//! - `patch_kind {instancer: {material: ..}}` is the setter (RFC 7386 merge
//!   into the serialized kind) — before the field existed, that exact patch
//!   was rejected as an unrecognized path (and before THAT fix, silently
//!   ignored, which is how the scene became a false positive).
//! - projects saved before the field existed still load (missing field →
//!   `None` = flat default).

use awsm_renderer_editor_protocol::dynamic_material::MaterialInstance;
use awsm_renderer_editor_protocol::{json_merge_patch, AssetId, InstancerDef, NodeKind};

fn instancer_kind() -> NodeKind {
    NodeKind::Instancer(InstancerDef {
        mesh: awsm_renderer_editor_protocol::MeshRef(AssetId::new()),
        transforms: vec![Default::default(), Default::default()],
        per_instance_colors: vec![[1.0, 0.0, 0.0, 1.0]],
        ..Default::default()
    })
}

/// The `patch_kind` flow (merge into the serialized kind, re-deserialize,
/// round-trip guard): setting `material` must survive and must NOT disturb the
/// def's other fields. This is the exact JSON shape the
/// `dynamic-material-attributes` author.js sends.
#[test]
fn patch_kind_merge_sets_instancer_material() {
    let prev = instancer_kind();
    let material_id = AssetId::new();

    let mut json = serde_json::to_value(&prev).unwrap();
    let patch = serde_json::json!({
        "instancer": { "material": { "asset": material_id } }
    });
    json_merge_patch(&mut json, &patch);

    let next: NodeKind = serde_json::from_value(json.clone()).unwrap();
    // The PatchKind handler's dropped-path guard semantics: every merged key
    // must still EXIST after the round-trip (extra default-filled keys are
    // fine; a field serde silently ignored would vanish — which is exactly
    // what happened before `material` existed on the def).
    let round_trip = serde_json::to_value(&next).unwrap();
    let survived = round_trip
        .get("instancer")
        .and_then(|i| i.get("material"))
        .and_then(|m| m.get("asset"));
    assert_eq!(
        survived,
        Some(&serde_json::json!(material_id)),
        "material.asset must survive the deserialize round-trip (the dropped-path guard)"
    );

    let NodeKind::Instancer(def) = next else {
        panic!("patched kind changed variants");
    };
    assert_eq!(
        def.material.as_ref().map(|m| m.asset),
        Some(material_id),
        "merge must set the material asset"
    );
    // Merge-patch semantics: sibling fields untouched.
    let NodeKind::Instancer(prev_def) = prev else {
        unreachable!()
    };
    assert_eq!(def.mesh, prev_def.mesh);
    assert_eq!(def.transforms, prev_def.transforms);
    assert_eq!(def.per_instance_colors, prev_def.per_instance_colors);
}

/// Clearing works the RFC 7386 way: `{"material": null}` removes the key →
/// back to `None` (flat default).
#[test]
fn patch_kind_merge_clears_instancer_material() {
    let mut kind = instancer_kind();
    if let NodeKind::Instancer(def) = &mut kind {
        def.material = Some(MaterialInstance {
            asset: AssetId::new(),
            inline: Default::default(),
            uniform_overrides: Default::default(),
            texture_overrides: Default::default(),
            buffer_overrides: Default::default(),
        });
    }
    let mut json = serde_json::to_value(&kind).unwrap();
    json_merge_patch(
        &mut json,
        &serde_json::json!({"instancer": {"material": null}}),
    );
    let NodeKind::Instancer(def) = serde_json::from_value::<NodeKind>(json).unwrap() else {
        panic!("variant changed");
    };
    assert_eq!(def.material, None);
}

/// Projects saved BEFORE the field existed (no `material` key in the TOML/JSON)
/// still deserialize — missing field → `None` = the historical flat default.
#[test]
fn instancer_without_material_field_still_parses() {
    let json = serde_json::json!({
        "instancer": {
            "mesh": AssetId::new(),
            "transforms": [],
        }
    });
    let NodeKind::Instancer(def) = serde_json::from_value::<NodeKind>(json).unwrap() else {
        panic!("wrong variant");
    };
    assert_eq!(def.material, None);
}

/// TOML round-trip (the project.toml format): `None` serializes as an absent
/// key, `Some` survives verbatim.
#[test]
fn instancer_material_toml_round_trip() {
    let mut def = InstancerDef {
        mesh: awsm_renderer_editor_protocol::MeshRef(AssetId::new()),
        transforms: vec![Default::default()],
        ..Default::default()
    };
    let toml_none = toml::to_string(&def).unwrap();
    assert!(
        !toml_none.contains("material"),
        "None must serialize as an absent key, got:\n{toml_none}"
    );

    def.material = Some(MaterialInstance {
        asset: AssetId::new(),
        inline: Default::default(),
        uniform_overrides: Default::default(),
        texture_overrides: Default::default(),
        buffer_overrides: Default::default(),
    });
    let toml_some = toml::to_string(&def).unwrap();
    let back: InstancerDef = toml::from_str(&toml_some).unwrap();
    assert_eq!(
        back, def,
        "authored material must survive the TOML round-trip"
    );
}
