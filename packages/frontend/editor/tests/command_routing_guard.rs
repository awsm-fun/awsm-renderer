//! Guard: UI / engine code must not mutate authoritative `EditorController`
//! reactive state directly — every edit routes through a dispatched
//! `EditorCommand` (the "all via controller" rule; the chokepoint is
//! `controller/state.rs::apply`). Reaching into a controller field and calling
//! `.set(...)` / `.set_neq(...)` from outside `src/controller/` bypasses the
//! command log (undo/redo, MCP visibility, cross-tab broadcast).
//!
//! This test scans the editor source (a plain text scan — no crate internals
//! needed) for `.<field>(.<sub>)*.set(_neq)?(` chains rooted at an
//! `EditorController` field, anywhere outside `src/controller/`, and fails on
//! any occurrence that isn't an explicitly allow-listed view-only / engine
//! exception.
//!
//! It is alias-robust: it keys on the field NAME, so `controller().settings.
//! cam_clip_near.set_neq(...)` AND `let ctrl = controller(); ctrl.playhead.
//! set_neq(...)` are both caught. Adding a genuinely-intentional exception
//! means adding a precise `(field, file)` row to `ALLOW` below with a rationale
//! — which is the point: the exemption is reviewed, not silent.
//!
//! The `EditorController` field list is parsed from `state.rs` at test time, so
//! a newly-added field is covered automatically (any out-of-controller `.set`
//! on it fails until allow-listed or routed through a command).

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Intentional exceptions: `(controller field, source file relative to `src/`)`.
/// Each is view-only session/UI state or engine-owned transient state that is
/// deliberately NOT an authoritative, undoable edit. Keep this list tight and
/// justified — a new row is a claim that the mutation genuinely should not be a
/// command.
const ALLOW: &[(&str, &str)] = &[
    // Which scene camera the viewport looks through — a view preference, not
    // scene state.
    ("active_camera", "scene_mode/viewport.rs"),
    // Settings drawer open/closed — pure UI chrome.
    ("settings_open", "app.rs"),
    // Content-browser drawer open/closed — pure UI chrome.
    ("content_browser_open", "scene_mode/content_browser.rs"),
    // Unsaved-changes flag — session/UI bookkeeping (there is no SetDirty
    // command; it is derived state toggled on load/save and on edits).
    ("dirty", "app.rs"),
    ("dirty", "material_mode/studio.rs"),
    // The loaded project's display name — reflects a load, session/UI only.
    ("project_name", "app.rs"),
    // The playback clock: the render loop advances it per-frame locally — "no
    // command, no broadcast" by design (scrubs/edits DO go through SetPlayhead).
    ("playhead", "engine/render_loop.rs"),
    // "Watch-it-work": mirror the agent's ALREADY-dispatched mode into the local
    // view when follow is enabled — reflecting a remote action, not a new edit.
    ("mode", "remote.rs"),
];

/// Parse the `pub` field names of `struct EditorController` from `state.rs`.
fn controller_fields() -> Vec<String> {
    let text = std::fs::read_to_string(src_dir().join("controller/state.rs"))
        .expect("read controller/state.rs");
    let start = text
        .find("pub struct EditorController")
        .expect("EditorController struct not found");
    // The struct body ends at the first line that is exactly "}" (column 0).
    let after = &text[start..];
    let end = after
        .find("\n}")
        .expect("EditorController struct end not found");
    let body = &after[..end];

    let mut fields = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("pub ") {
            // `pub name: Type,` — take the ident up to ':'.
            if let Some(colon) = rest.find(':') {
                let name = rest[..colon].trim();
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    fields.push(name.to_string());
                }
            }
        }
    }
    assert!(
        fields.contains(&"settings".to_string()) && fields.contains(&"scene".to_string()),
        "field parse looks wrong: {fields:?}"
    );
    fields
}

fn ident_end(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    i
}

/// Does `line` contain `.<field>(.<sub>)*.set(_neq)?(` — a mutation chain rooted
/// at `field`? Returns true on the first such match.
fn line_sets_field(line: &str, field: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = format!(".{field}");
    let mut from = 0;
    while let Some(rel) = line[from..].find(&needle) {
        let start = from + rel;
        let after_field = start + needle.len();
        from = start + 1; // advance for the next search regardless
                          // `.field` must be a whole segment: the next char begins a `.method` or
                          // `.subfield` — i.e. it must be '.'. (`.field(` is a method call named
                          // `field`, `.field_x` is a different identifier.)
        if after_field >= bytes.len() || bytes[after_field] != b'.' {
            continue;
        }
        // Walk the dotted chain from here; a terminal `.set(`/`.set_neq(` is a hit.
        let mut i = after_field; // points at '.'
        loop {
            if i >= bytes.len() || bytes[i] != b'.' {
                break;
            }
            let seg_start = i + 1;
            let seg_end = ident_end(bytes, seg_start);
            if seg_end == seg_start {
                break; // not an identifier segment
            }
            let seg = &line[seg_start..seg_end];
            let next = bytes.get(seg_end).copied();
            match next {
                Some(b'(') => {
                    if seg == "set" || seg == "set_neq" {
                        return true;
                    }
                    break; // some other method (get/signal/clone/...)
                }
                Some(b'.') => {
                    i = seg_end; // keep walking the field chain
                }
                _ => break,
            }
        }
    }
    false
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn matcher_catches_bypass_shapes() {
    // Positive: direct sets on a controller field (incl. through a sub-field
    // chain, and through a `let ctrl = controller()` alias).
    assert!(line_sets_field(
        "controller().settings.cam_clip_near.set_neq(v.max(0.0001))",
        "settings"
    ));
    assert!(line_sets_field("ctrl.playhead.set_neq(next);", "playhead"));
    assert!(line_sets_field(
        "controller().project_name.set(dir.name());",
        "project_name"
    ));
    assert!(line_sets_field(
        "controller().active_camera.set_neq(Some(id));",
        "active_camera"
    ));

    // Negative: reads, dispatches, clones, and the wrong field must NOT match.
    assert!(!line_sets_field(
        "let _ = controller().dispatch(EditorCommand::SetCameraClip { near: Some(near), .. }).await;",
        "settings"
    ));
    assert!(!line_sets_field(
        "let ortho = controller().settings.editor_ortho.get();",
        "settings"
    ));
    assert!(!line_sets_field(
        "toggle(s.cam_clip_manual.clone())",
        "settings"
    ));
    assert!(!line_sets_field("controller().playhead.get()", "playhead"));
    // Field-name must be a whole segment (no substring false-positive).
    assert!(!line_sets_field(
        "controller().scene_tree.foo.set(x)",
        "scene"
    ));
}

#[test]
fn all_edits_route_through_editor_command() {
    let src = src_dir();
    let controller_dir = src.join("controller");
    let fields = controller_fields();

    let mut files = Vec::new();
    rs_files(&src, &mut files);

    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        if file.starts_with(&controller_dir) {
            continue; // direct sets are legitimate at the chokepoint
        }
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for (lineno, line) in text.lines().enumerate() {
            for field in &fields {
                if line_sets_field(line, field) {
                    let allowed = ALLOW.iter().any(|(f, path)| f == field && rel == *path);
                    if !allowed {
                        violations.push(format!(
                            "{rel}:{}: `.{field}.…set()` bypasses EditorCommand — dispatch a \
                             command instead (or allow-list if genuinely view-only): {}",
                            lineno + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "controller state mutated outside src/controller/ without a command:\n{}",
        violations.join("\n")
    );
}
