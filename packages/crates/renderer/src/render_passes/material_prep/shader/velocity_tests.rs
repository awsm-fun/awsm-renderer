//! Rendered-source tests for the prep shader's TAA velocity variant (the
//! prep-pass fallback, 03 §2.4).

use crate::render_passes::material_prep::shader::{
    cache_key::ShaderCacheKeyMaterialPrep, template::ShaderTemplateMaterialPrep,
};

fn key(msaa: Option<u32>, velocity: bool) -> ShaderCacheKeyMaterialPrep {
    ShaderCacheKeyMaterialPrep {
        msaa_sample_count: msaa,
        max_shadow_casters: 4,
        sscs_enabled: false,
        sscs_step_count: 16,
        reverse_z: true,
        velocity,
    }
}

fn source(key: &ShaderCacheKeyMaterialPrep) -> String {
    ShaderTemplateMaterialPrep::try_from(key)
        .unwrap()
        .into_source()
        .unwrap()
}

fn naga_validate(src: &str, label: &str) {
    let module = naga::front::wgsl::parse_str(src)
        .unwrap_or_else(|e| panic!("{label}: parse failed:\n{}", e.emit_to_string(src)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{label}: validation failed:\n{}", e.emit_to_string(src)));
}

/// Names only the velocity variant may contain.
const VELOCITY_NAMES: [&str; 4] = [
    "velocity_out",
    "prep_transforms",
    "prep_prev_state",
    "taa_motion_vector",
];

#[test]
fn prep_velocity_variant_validates() {
    let label = "prep velocity";
    let src = source(&key(None, true));
    naga_validate(&src, label);
    for name in VELOCITY_NAMES {
        assert!(src.contains(name), "{label}: missing {name}");
    }
    // The sky branch clears the vector; the surface branch writes it.
    assert_eq!(
        src.split_whitespace()
            .collect::<String>()
            .matches("textureStore(velocity_out")
            .count(),
        2,
        "{label}"
    );
    // Unjittered on both sides (G2): never the jittered view-projection.
    assert!(src.contains("cam.view_proj_unjittered * vec4<f32>(world_pos, 1.0)"));
    assert!(src.contains("cam.prev_view_proj * vec4<f32>(world_prev, 1.0)"));
    // inverse(M) from the stored inverse-transpose normal matrix (rows 4-6 of
    // the 112-byte transform entry).
    assert!(src.contains("dot(prep_transforms[t + 4u].xyz, d)"));
    // The surface branch writes the vector unconditionally: nothing between
    // the world-position reconstruction and the store can return early (the
    // velocity block lives inside the shadow block, which every prep variant
    // has).
    let cs = src.split("fn cs_prep(").nth(1).expect("cs_prep");
    let start = cs.find("let world_pos =").expect("world_pos");
    let end = start
        + cs[start..]
            .find("velocity_out")
            .expect("velocity store after world_pos");
    let between = &cs[start..end];
    assert!(
        !between.contains("return"),
        "{label}: early return before the vector"
    );
    assert!(!between.contains("discard"), "{label}");
    // HUD pixels get the static world's vector; non-finite vectors become 0.
    assert!(src.contains("mesh_meta.is_hud == 1u"));
    assert!(src.contains("select(vec2<f32>(0.0), v, finite)"));
}

#[test]
fn prep_without_velocity_has_no_taa_code() {
    // The TAA-off (and geometry-path) prep shader is the pre-TAA one: no
    // velocity binding, read or write. (Byte identity against the pre-TAA
    // template was checked by dumping every TAA-off variant when the flag
    // landed.)
    for msaa in [None, Some(4)] {
        let src = source(&key(msaa, false));
        for name in VELOCITY_NAMES {
            assert!(
                !src.contains(name),
                "msaa={msaa:?}: TAA-off prep mentions {name}"
            );
        }
        assert!(!src.contains("binding(10)"), "msaa={msaa:?}");
    }
}
