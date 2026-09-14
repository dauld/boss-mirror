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
//! server hands its text to [`parse`] and shows the url-encoded example;
//! the terminal composes and shows nothing more); the rule itself is
//! not theirs to word.
//!
//! One thing only the wire text can show: a REPEATED key. A JSON object
//! holds a key once, so `{"k":"1","k":"2"}` read the ordinary way is
//! `{"k":"2"}` and the first value is gone without a word — a caller who
//! meant an AND of two values gets one, and a `total` that is
//! confidently wrong. The terminal refuses `--where k=1 --where k=2` at
//! composition, where it can see the repeat; until 2026-09-14 the server
//! parsed and kept the last, so the doors disagreed on exactly the input
//! the rule cannot see once it is a `Value` (backlog 03852b47). [`parse`]
//! is the server's side of that refusal.

use std::fmt;

use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// The rule, as both the 400 and the terminal say it. A door prefixes
/// its parameter's name (`metadata`, `--where`) and nothing else.
pub const RULE: &str = "must be a flat JSON object of string values — nested objects, arrays, \
                        numbers, booleans and null are not accepted";

/// The second of the two ways the shape is broken that [`check`] and
/// [`parse`] both say — one spelling, so the two paths cannot drift.
const NON_OBJECT: &str = "got a non-object";

/// Accept `value` if it is an object whose every value is a string — the
/// map is handed back, possibly empty; what an empty one means (the
/// server narrows nothing and sends `None`) is the door's — or say why
/// not. The `Err` is the whole sentence a door prints after its
/// parameter's name: the [`RULE`], then which of the two ways it was
/// broken, naming the offending key when there is one.
pub fn check(value: &Value) -> Result<Map<String, Value>, String> {
    let Value::Object(doc) = value else {
        return Err(format!("{RULE}: {NON_OBJECT}"));
    };
    if let Some((key, _)) = doc.iter().find(|(_, v)| !v.is_string()) {
        return Err(format!("{RULE}: value of {key:?} is not a string"));
    }
    Ok(doc.clone())
}

/// Read a document from its wire text (`metadata=<json>`, url-decoded)
/// and judge it by [`check`] — refusing first the one thing a parsed
/// `Value` can no longer show, a key that appears twice. The `Err` is
/// the same whole sentence `check` gives: the [`RULE`], then what broke
/// it (`not JSON (...)`, `key "k" repeated`, or `check`'s own two).
///
/// serde_json has no duplicate-refusing parse — the workspace enables
/// no feature and none exists; `preserve_order` only swaps the map type
/// and still keeps the last — so the object's entries are collected IN
/// ORDER by a visitor, repeats included, and the second sighting of a
/// key is the refusal. Keys are compared after JSON decoding, which is
/// exactly where serde_json would have collided them: `"a"` and the
/// escaped spelling `"\u0061"` are one key here as they would have been
/// there. A scan of the raw text for quoted keys would call them two.
pub fn parse(text: &str) -> Result<Map<String, Value>, String> {
    let entries = match serde_json::from_str::<Parsed>(text) {
        Err(e) => return Err(format!("{RULE}: not JSON ({e})")),
        Ok(Parsed::Other) => return Err(format!("{RULE}: {NON_OBJECT}")),
        Ok(Parsed::Object(entries)) => entries,
    };
    let mut doc = Map::new();
    for (key, value) in entries {
        if doc.insert(key.clone(), value).is_some() {
            return Err(format!("{RULE}: key {key:?} repeated"));
        }
    }
    check(&Value::Object(doc))
}

/// A JSON value as the text spells it: an object as its entries in
/// order, repeats included — or something that is not an object, which
/// `check` will name. Only the map arm collects anything; the other
/// arms exist because `deserialize_any` may land on them, and a
/// sequence must be walked to its `]` or the parser reports trailing
/// characters instead of the shape.
enum Parsed {
    Object(Vec<(String, Value)>),
    Other,
}

impl<'de> Deserialize<'de> for Parsed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;

        impl<'de> Visitor<'de> for Entries {
            type Value = Parsed;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON value")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Parsed, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, Value>()? {
                    entries.push(entry);
                }
                Ok(Parsed::Object(entries))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Parsed, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Parsed::Other)
            }

            fn visit_bool<E>(self, _: bool) -> Result<Parsed, E> {
                Ok(Parsed::Other)
            }

            fn visit_i64<E>(self, _: i64) -> Result<Parsed, E> {
                Ok(Parsed::Other)
            }

            fn visit_u64<E>(self, _: u64) -> Result<Parsed, E> {
                Ok(Parsed::Other)
            }

            fn visit_f64<E>(self, _: f64) -> Result<Parsed, E> {
                Ok(Parsed::Other)
            }

            fn visit_str<E>(self, _: &str) -> Result<Parsed, E> {
                Ok(Parsed::Other)
            }

            fn visit_unit<E>(self) -> Result<Parsed, E> {
                Ok(Parsed::Other)
            }
        }

        deserializer.deserialize_any(Entries)
    }
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

    #[test]
    fn parse_reads_a_normal_document_exactly_as_check_would() {
        let text = r#"{"branch":"feat/x","outcome":"arrived"}"#;
        let got = parse(text).unwrap();
        assert_eq!(
            Value::Object(got),
            json!({"branch": "feat/x", "outcome": "arrived"})
        );
        assert!(parse("{}").unwrap().is_empty());
    }

    #[test]
    fn parse_refuses_a_repeated_key_with_the_rule_and_the_key() {
        // Ordinary parsing keeps the last value: {"k":"2"}. Refused instead.
        let why = parse(r#"{"k":"1","k":"2"}"#).unwrap_err();
        assert!(why.starts_with(RULE), "{why}");
        assert!(why.ends_with("key \"k\" repeated"), "{why}");

        // A repeat is judged before the values are, so the key is named
        // even when the second value would also have failed the rule.
        let why = parse(r#"{"a":"1","b":"2","a":3}"#).unwrap_err();
        assert!(why.ends_with("key \"a\" repeated"), "{why}");

        // One key, two JSON spellings — still a repeat once decoded,
        // which is where serde_json would have collided them.
        let why = parse(r#"{"a":"1","\u0061":"2"}"#).unwrap_err();
        assert!(why.ends_with("key \"a\" repeated"), "{why}");
    }

    #[test]
    fn parse_says_what_check_says_for_the_other_breakages() {
        for (text, tail) in [
            (r#"{"a":{"b":"c"}}"#, "value of \"a\" is not a string"),
            (r#"{"n":1}"#, "value of \"n\" is not a string"),
            (r#"["a"]"#, NON_OBJECT),
            (r#"["a",{"b":1},[2]]"#, NON_OBJECT),
            (r#""branch=feat/x""#, NON_OBJECT),
            ("1", NON_OBJECT),
            ("1.5", NON_OBJECT),
            ("-1", NON_OBJECT),
            ("true", NON_OBJECT),
            ("null", NON_OBJECT),
        ] {
            let why = parse(text).unwrap_err();
            assert!(why.starts_with(RULE), "{text}: {why}");
            assert!(why.ends_with(tail), "{text}: {why}");
        }
        let why = parse("branch=feat/x").unwrap_err();
        assert!(why.starts_with(RULE), "{why}");
        assert!(why.contains("not JSON"), "{why}");
    }
}
