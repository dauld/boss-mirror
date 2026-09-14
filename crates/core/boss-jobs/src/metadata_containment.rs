//! WHAT A METADATA CONTAINMENT DOCUMENT MAY BE — one definition, both doors.
//!
//! `GET /api/jobs?metadata=<json>` binds the document to `metadata @> $n`.
//! The port's contract (`JobFilter::metadata_contains`) is flat
//! string-valued objects only — that is all `metadata_equals` expresses
//! and all the in-memory adapter mirrors. A nested object, an array or a
//! number bound as-is would be accepted by the SQL, match nothing, and
//! answer `total: 0` with a straight face — the wrong-target shape
//! CLAUDE.md §Doors names. So the server refuses anything else, and
//! `boss job list --where` refuses the same thing BEFORE the round trip.
//!
//! Until 2026-09-14 the shape was decided twice with nothing between the
//! deciders but a comment (backlog 88a3b072): server
//! `metadata_containment_from_query` (`http/jobs.rs`) validated a parsed
//! document, CLI `where_object` (`boss-cli/src/job.rs`) composed one from
//! `key=value` pairs and trusted its own construction. Widen the port on
//! one side and the terminal builds a document the server 400s, or the
//! server takes one the terminal will not send. This module is the one
//! copy (CLAUDE.md §9a), the same collapse `metadata_key` made for
//! `metadata_has`. Each door adds only the NAME of its own parameter in
//! front of what [`check`] says and whatever its wire form needs (the
//! server parses text and shows the url-encoded example; the terminal
//! composes and shows nothing more); the rule itself is not theirs to
//! word.

use serde_json::{Map, Value};

/// The rule, as both the 400 and the terminal say it. A door prefixes
/// its parameter's name (`metadata`, `--where`) and nothing else.
pub const RULE: &str = "must be a flat JSON object of string values — nested objects, arrays, \
                        numbers, booleans and null are not accepted";

/// Accept `value` if it is an object whose every value is a string — the
/// map is handed back, possibly empty; what an empty one means (the
/// server narrows nothing and sends `None`) is the door's — or say why
/// not. The `Err` is the whole sentence a door prints after its
/// parameter's name: the [`RULE`], then which of the two ways it was
/// broken, naming the offending key when there is one.
pub fn check(value: &Value) -> Result<Map<String, Value>, String> {
    let Value::Object(doc) = value else {
        return Err(format!("{RULE}: got a non-object"));
    };
    if let Some((key, _)) = doc.iter().find(|(_, v)| !v.is_string()) {
        return Err(format!("{RULE}: value of {key:?} is not a string"));
    }
    Ok(doc.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_flat_object_of_strings_passes_through_unchanged() {
        let doc = json!({"branch": "feat/x", "merged": "true", "note": "a=b"});
        let got = check(&doc).unwrap();
        assert_eq!(Value::Object(got), doc);
        // Empty is flat; what it MEANS is the door's decision, not the rule's.
        assert!(check(&json!({})).unwrap().is_empty());
    }

    #[test]
    fn a_nested_object_is_refused_with_the_rule_and_the_key() {
        let why = check(&json!({"a": {"b": "c"}})).unwrap_err();
        assert!(why.starts_with(RULE), "{why}");
        assert!(why.ends_with("value of \"a\" is not a string"), "{why}");
    }

    #[test]
    fn a_non_string_value_is_refused_with_the_rule_and_the_key() {
        for (doc, key) in [
            (json!({"n": 1}), "n"),
            (json!({"ok": "yes", "merged": true}), "merged"),
            (json!({"gone": null}), "gone"),
            (json!({"tags": ["a"]}), "tags"),
        ] {
            let why = check(&doc).unwrap_err();
            assert!(why.starts_with(RULE), "{doc}: {why}");
            assert!(
                why.ends_with(&format!("value of {key:?} is not a string")),
                "{doc}: {why}"
            );
        }
    }

    #[test]
    fn a_non_object_is_refused_with_the_rule() {
        for doc in [json!(["a"]), json!("branch=feat/x"), json!(1), json!(null)] {
            let why = check(&doc).unwrap_err();
            assert!(why.starts_with(RULE), "{doc}: {why}");
            assert!(why.ends_with("got a non-object"), "{doc}: {why}");
        }
    }
}
