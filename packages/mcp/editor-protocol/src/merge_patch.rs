//! RFC 7386 JSON Merge Patch — the pure, host-testable core of the `patch_kind`
//! command (§3 in `docs/plans/mcp-improvements.md`).
//!
//! Lets an agent edit a node's `NodeKind` by sending only the fields it wants to
//! change (paired with `get_node_details`, which shows the exact current shape +
//! field names) instead of reconstructing and resending the entire blob via
//! `SetKind` — the escape-hatch papercut §3 describes.

use serde_json::Value;

/// Coerce a merge-patch payload that arrived as a JSON **string** back into
/// structured JSON.
///
/// A bare `serde_json::Value` MCP parameter derives an unconstrained (`true`) JSON
/// Schema, so some clients serialize the patch object as a *string* (e.g.
/// `"{\"mesh\":{\"shadow\":{\"cast\":false}}}"`) rather than an object. Left as-is,
/// that `Value::String` would hit [`json_merge_patch`]'s "patch is not an object →
/// replace wholesale" branch, clobbering the whole `NodeKind` with a string and
/// failing the downstream `from_value::<NodeKind>` with a misleading "unknown
/// variant" error.
///
/// A top-level string is **never** a valid `NodeKind` merge-patch, so parsing it is
/// safe: if it parses as JSON we use the structured value; if it doesn't, that's a
/// clear error rather than a silent wholesale replace. Non-string patches pass
/// through unchanged.
pub fn coerce_patch(patch: Value) -> Result<Value, String> {
    match patch {
        Value::String(s) => serde_json::from_str(&s)
            .map_err(|e| format!("patch arrived as a JSON string but did not parse as JSON: {e}")),
        other => Ok(other),
    }
}

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

    #[test]
    fn coerce_stringified_patch_then_merge_flips_only_named_field() {
        // F1 regression: a client that stringifies the patch (because `Value`
        // derives an unconstrained schema) sends the merge-patch as a JSON string.
        // `coerce_patch` must parse it back to an object so the merge flips ONLY
        // `shadow.cast` and leaves siblings intact — instead of the old failure
        // where the string replaced the whole kind and `NodeKind` deserialize blew
        // up with "unknown variant".
        let stringified = Value::String(r#"{"mesh":{"shadow":{"cast":false}}}"#.into());
        let coerced = coerce_patch(stringified).expect("valid JSON string coerces");
        assert!(coerced.is_object(), "coerced patch must be an object");

        let target = json!({
            "mesh": {
                "shadow": {"cast": true, "receive": true},
                "material": "mat-1",
                "mesh": "geo-1"
            }
        });
        let got = patched(target, coerced);
        assert_eq!(
            got,
            json!({
                "mesh": {
                    "shadow": {"cast": false, "receive": true},
                    "material": "mat-1",
                    "mesh": "geo-1"
                }
            })
        );
    }

    #[test]
    fn coerce_passes_through_a_genuine_object() {
        let obj = json!({"mesh": {"shadow": {"cast": false}}});
        assert_eq!(coerce_patch(obj.clone()).unwrap(), obj);
    }

    #[test]
    fn coerce_rejects_a_non_json_string() {
        let err = coerce_patch(Value::String("not json at all".into())).unwrap_err();
        assert!(err.contains("did not parse"), "got: {err}");
    }
}
