//! Catching misspelled fields in raw command JSON.
//!
//! The command vocabulary is deserialized with serde's default tolerance:
//! a field the target type does not have is dropped without a word. For a
//! file format that is the right behaviour (old files, newer files). For a
//! command typed by an agent it is a trap — `{"cmd":"set_transform",
//! "transfrom":{…}}` parses, applies nothing, and reports success.
//!
//! `deny_unknown_fields` is not usable here: the payload types (`NodeKind`,
//! `MaterialDef`, `PostProcessConfig`, …) are also the project/bundle schema,
//! which must stay tolerant. So the check is done on the way in instead:
//! deserialize, serialize the typed value back, and report every key the
//! caller sent that the typed value did not keep.

use serde_json::Value;

/// Keys present in `input` that the typed value (re-serialized as `kept`)
/// did not keep, as dotted paths (`transform.rotatoin`, `commands[2].nmae`).
///
/// A key whose value is "nothing" (`null`, `false`, `0`, `""`, `[]`, `{}`) is
/// never reported: optional fields that are skipped when empty/default would
/// otherwise look unknown when a caller spells out the default.
pub fn unknown_fields(input: &Value, kept: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(input, kept, String::new(), &mut out);
    out
}

fn is_nothing(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

fn walk(input: &Value, kept: &Value, path: String, out: &mut Vec<String>) {
    match (input, kept) {
        (Value::Object(i), Value::Object(k)) => {
            for (key, iv) in i {
                let here = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match k.get(key) {
                    Some(kv) => walk(iv, kv, here, out),
                    None => {
                        if !is_nothing(iv) {
                            out.push(here);
                        }
                    }
                }
            }
        }
        (Value::Array(i), Value::Array(k)) if i.len() == k.len() => {
            for (n, (iv, kv)) in i.iter().zip(k).enumerate() {
                walk(iv, kv, format!("{path}[{n}]"), out);
            }
        }
        // Different shapes: an accepted alternative encoding (a text form of a
        // value, a string-or-object argument) — nothing to compare key-wise.
        _ => {}
    }
}

/// `Err` with a message naming the unknown fields, or `Ok` when there are none.
pub fn check<T: serde::Serialize>(input: &Value, parsed: &T, what: &str) -> Result<(), String> {
    let Ok(kept) = serde_json::to_value(parsed) else {
        return Ok(());
    };
    let unknown = unknown_fields(input, &kept);
    if unknown.is_empty() {
        return Ok(());
    }
    let list = unknown
        .iter()
        .map(|f| format!("`{f}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "unknown field{} in {what}: {list} — nothing was applied. Check the spelling against \
         list_commands {{cmd}} (a misspelled field would otherwise be ignored silently).",
        if unknown.len() == 1 { "" } else { "s" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use awsm_renderer_editor_protocol::EditorCommand;
    use serde_json::json;

    fn check_cmd(v: Value) -> Result<(), String> {
        let cmd: EditorCommand = serde_json::from_value(v.clone()).expect("parses");
        check(&v, &cmd, "command")
    }

    #[test]
    fn a_correct_command_passes() {
        check_cmd(json!({
            "cmd": "set_visible",
            "id": "00000000-0000-0000-0000-000000000001",
            "visible": false
        }))
        .unwrap();
    }

    #[test]
    fn a_misspelled_top_level_field_is_named() {
        let err = check_cmd(json!({
            "cmd": "set_view_options",
            "msaa": true,
            "griid": false,
            "skeleton_vis": true
        }))
        .unwrap_err();
        assert!(err.contains("`skeleton_vis`"), "{err}");
        // `griid: false` is "nothing" and so cannot be told from a skipped
        // default — it is not reported.
        assert!(!err.contains("`griid`"), "{err}");
    }

    #[test]
    fn a_misspelled_nested_field_is_named_with_its_path() {
        let err = check_cmd(json!({
            "cmd": "set_transform",
            "id": "00000000-0000-0000-0000-000000000001",
            "transform": {
                "translation": [0.0, 1.0, 0.0],
                "rotation": [0.0, 0.0, 0.0, 1.0],
                "scale": [1.0, 1.0, 1.0],
                "rotatoin": [0.0, 0.7, 0.0, 0.7]
            }
        }))
        .unwrap_err();
        assert!(err.contains("`transform.rotatoin`"), "{err}");
    }

    #[test]
    fn omitted_optional_fields_and_explicit_nulls_pass() {
        check_cmd(json!({ "cmd": "set_active_camera", "camera": null })).unwrap();
        check_cmd(json!({ "cmd": "load_player_bundle" })).unwrap();
        check_cmd(json!({ "cmd": "load_player_bundle", "camera": "cam_title" })).unwrap();
    }
}
