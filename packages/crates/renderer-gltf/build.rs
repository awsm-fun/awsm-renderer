use std::path::Path;

/// The paid acceptance fixtures live in the gitignored `fixtures/local/` and
/// are never committed, so tests over them only exist when the bytes are
/// present on this machine (same pattern as codec-meshopt's build.rs).
fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_local_fixtures)");
    println!("cargo::rustc-check-cfg=cfg(has_local_fixtures_astrabot)");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let police = Path::new(&manifest_dir).join("../../../fixtures/local/police-meshopt.glb");
    let astrabot = Path::new(&manifest_dir).join("../../../fixtures/local/astrabot-meshopt.glb");
    println!("cargo::rerun-if-changed={}", police.display());
    println!("cargo::rerun-if-changed={}", astrabot.display());
    if police.exists() {
        println!("cargo::rustc-cfg=has_local_fixtures");
    }
    if astrabot.exists() {
        println!("cargo::rustc-cfg=has_local_fixtures_astrabot");
    }
}
