//! RFC 7386 JSON Merge Patch — the pure, host-testable core of the `patch_kind`
//! command (§3 in `docs/plans/mcp-improvements.md`).
//!
//! Lets an agent edit a node's `NodeKind` by sending only the fields it wants to
//! change (paired with `get_node_details`, which shows the exact current shape +
//! field names) instead of reconstructing and resending the entire blob via
//! `SetKind` — the escape-hatch papercut §3 describes.

use serde_json::Value;

/// Apply an [RFC 7386](https://datatracker.ietf.org/doc/html/rfc7386) JSON Merge
/// Patch to `target` in place:
/// - a `null` in `patch` **removes** that key from `target`;
/// - an object value merges **recursively**;
/// - any other value (scalar, array) **replaces** wholesale (arrays are not
///   element-merged — that is the RFC's defined behavior).
///
/// If `patch` is not an object, it replaces `target` entirely.
pub fn json_merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch_map) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    let target_map = target.as_object_mut().expect("set to object above");
    for (key, patch_val) in patch_map {
        if patch_val.is_null() {
            target_map.remove(key);
        } else {
            json_merge_patch(
                target_map.entry(key.clone()).or_insert(Value::Null),
                patch_val,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patched(mut target: Value, patch: Value) -> Value {
        json_merge_patch(&mut target, &patch);
        target
    }

    #[test]
    fn merges_nested_objects_without_clobbering_siblings() {
        let got = patched(
            json!({"mesh": {"shadow": {"cast": true, "receive": true}, "keep": 1}}),
            json!({"mesh": {"shadow": {"cast": false}}}),
        );
        assert_eq!(
            got,
            json!({"mesh": {"shadow": {"cast": false, "receive": true}, "keep": 1}})
        );
    }

    #[test]
    fn null_removes_a_key() {
        let got = patched(json!({"a": 1, "b": 2}), json!({"b": null}));
        assert_eq!(got, json!({"a": 1}));
    }

    #[test]
    fn scalar_replaces() {
        let got = patched(json!({"a": {"x": 1}}), json!({"a": 5}));
        assert_eq!(got, json!({"a": 5}));
    }

    #[test]
    fn array_replaces_wholesale_not_elementwise() {
        // RFC 7386: arrays are replaced, never merged.
        let got = patched(json!({"v": [1, 2, 3]}), json!({"v": [9]}));
        assert_eq!(got, json!({"v": [9]}));
    }

    #[test]
    fn non_object_patch_replaces_target() {
        let got = patched(json!({"a": 1}), json!(42));
        assert_eq!(got, json!(42));
    }

    #[test]
    fn adds_new_keys() {
        let got = patched(json!({"a": 1}), json!({"b": {"c": 2}}));
        assert_eq!(got, json!({"a": 1, "b": {"c": 2}}));
    }

    #[test]
    fn patch_into_non_object_target_becomes_object() {
        let got = patched(json!(7), json!({"a": 1}));
        assert_eq!(got, json!({"a": 1}));
    }
}
