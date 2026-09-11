//! Custom assertion helpers with agent-friendly failure messages.

use serde_json::Value;

/// Assert that a JSON value contains a specific field with an expected value.
/// Useful for asserting on response bodies.
pub fn assert_json_field(value: &Value, field: &str, expected: &Value) {
    let actual = value.get(field);
    if actual != Some(expected) {
        panic!(
            "\n  JSON field assertion failed\n  field: {}\n  expected: {}\n  actual: {}\n  full value: {}\n",
            field,
            expected,
            actual
                .map(|v| v.to_string())
                .unwrap_or_else(|| "MISSING".to_string()),
            value,
        );
    }
}

/// Assert that a JSON value has a specific field present (any value).
pub fn assert_json_has_field(value: &Value, field: &str) {
    if value.get(field).is_none() {
        panic!(
            "\n  JSON field {} not found\n  full value: {}\n",
            field, value,
        );
    }
}

/// Assert that `field` is PRESENT in `value` and carries an **explicit
/// `null`** — not merely that indexing it answers null.
///
/// ```ignore
/// assert_explicit_null!(patch, "backlog_item");
/// assert_explicit_null!(patch, "backlog_item", "the arrival rule must find no edge");
/// ```
///
/// WHY THIS EXISTS RATHER THAN `assert!(value["field"].is_null())`.
/// `serde_json`'s `Index` on an object answers `Value::Null` for a key
/// that was never written, so `is_null()` is true for a deliberate null
/// and for a key nobody wrote alike — an assertion that cannot fail, and
/// therefore not an assertion. Found by the builder of
/// `feat/a-car-states-its-item-at-build-start` (backlog 2e4c200f) in a
/// test whose own name claimed the distinction its one line could not make.
///
/// BOTH PLACES BOSS RELIES ON THE DISTINCTION want this, and they mean
/// different things by it, which is why the macro is named for what it
/// CHECKS rather than for either meaning — say the meaning in the
/// message:
/// - `PATCH /api/jobs/{id}/metadata` reads present-and-null as **delete
///   this key** and an absent key as **leave the recorded value alone**:
///   opposite instructions, relied on by park / adopt / supersede.
/// - An event payload or wire form states an explicit null to say
///   **nothing measured this**, where a missing key reads as a payload
///   that forgot to say (`agent_runs`).
///
/// The inverse is [`assert_absent!`].
#[macro_export]
macro_rules! assert_explicit_null {
    ($value:expr, $field:expr $(,)?) => {
        $crate::assertions::assert_explicit_null_impl(&$value, $field, None)
    };
    ($value:expr, $field:expr, $($arg:tt)+) => {
        $crate::assertions::assert_explicit_null_impl(&$value, $field, Some(format!($($arg)+)))
    };
}

/// Assert that `value` does NOT MENTION `field` at all.
///
/// The inverse of [`assert_explicit_null!`], and the reason that one
/// exists: absent and present-and-null are different facts, and a test
/// that means one must not be satisfied by the other.
#[macro_export]
macro_rules! assert_absent {
    ($value:expr, $field:expr $(,)?) => {
        $crate::assertions::assert_absent_impl(&$value, $field, None)
    };
    ($value:expr, $field:expr, $($arg:tt)+) => {
        $crate::assertions::assert_absent_impl(&$value, $field, Some(format!($($arg)+)))
    };
}

/// Behind [`assert_explicit_null!`]. `#[track_caller]` so the panic names
/// the failing assertion's own line rather than this file's — a verdict
/// must name what failed.
#[track_caller]
pub fn assert_explicit_null_impl(value: &Value, field: &str, context: Option<String>) {
    let why = context.map(|c| format!("\n  {c}")).unwrap_or_default();
    let Some(map) = value.as_object() else {
        panic!(
            "\n  `{field}` cannot be explicitly null in something that is not a JSON object: {value}{why}\n"
        );
    };
    match map.get(field) {
        Some(&Value::Null) => {}
        Some(v) => {
            panic!("\n  `{field}` must be PRESENT AND NULL, but it carries {v}: {value}{why}\n")
        }
        None => panic!(
            "\n  `{field}` must be PRESENT AND NULL, but it was never mentioned — absent is a \
             different fact from an explicit null, and the reader acts on the difference: {value}{why}\n"
        ),
    }
}

/// Behind [`assert_absent!`].
#[track_caller]
pub fn assert_absent_impl(value: &Value, field: &str, context: Option<String>) {
    let why = context.map(|c| format!("\n  {c}")).unwrap_or_default();
    match value.as_object().and_then(|m| m.get(field)) {
        None => {}
        Some(&Value::Null) => panic!(
            "\n  `{field}` must be ABSENT, but it is present and explicitly null — a different \
             fact, which a metadata door reads as DELETE: {value}{why}\n"
        ),
        Some(v) => panic!("\n  `{field}` must be ABSENT, but it carries {v}: {value}{why}\n"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// THE CLAIM THESE MACROS EXIST FOR, PINNED RATHER THAN ASSERTED IN
    /// PROSE. `serde_json`'s `Index` answers `Value::Null` for a key that
    /// was never written, so the two facts the macros separate are
    /// INDISTINGUISHABLE through `[...]` — and a comment saying so is not
    /// a mechanism (§9a). If a future `serde_json` ever made indexing
    /// panic or answer something else instead, this is the line that says
    /// the macros' reason for existing has changed.
    #[test]
    fn indexing_cannot_tell_an_absent_key_from_an_explicit_null() {
        let absent = json!({});
        let explicit = json!({ "hold": null });
        assert!(absent["hold"].is_null(), "an ABSENT key indexes to null");
        assert!(explicit["hold"].is_null(), "so does an explicit null");
        assert_eq!(
            absent["hold"], explicit["hold"],
            "indexing gives one answer to two different questions — which is why \
             `value[\"k\"].is_null()` asserts nothing about either"
        );
        // The distinction survives only through the map.
        assert_ne!(absent.get("hold"), explicit.get("hold"));
    }

    #[test]
    fn an_explicit_null_is_accepted() {
        assert_explicit_null!(json!({ "hold": null }), "hold");
        assert_explicit_null!(&json!({ "hold": null }), "hold", "and takes prose too");
    }

    /// THE TEST THE DEFECT CLASS DID NOT HAVE, and the only reason this
    /// macro is worth writing: it FAILS on an absent key, which is the
    /// one thing `value[field].is_null()` cannot do.
    #[test]
    #[should_panic(expected = "never mentioned")]
    fn an_absent_key_is_not_an_explicit_null() {
        assert_explicit_null!(json!({ "other": 1 }), "hold");
    }

    #[test]
    #[should_panic(expected = "but it carries \"yes\"")]
    fn a_real_value_is_not_an_explicit_null() {
        assert_explicit_null!(json!({ "hold": "yes" }), "hold");
    }

    #[test]
    fn assert_absent_is_the_inverse() {
        assert_absent!(json!({ "other": 1 }), "hold");
    }

    /// THE PAIR ARE OPPOSITES, not two spellings of one check — which is
    /// the whole fact the defect erased.
    #[test]
    #[should_panic(expected = "present and explicitly null")]
    fn an_explicit_null_is_not_absent() {
        assert_absent!(json!({ "hold": null }), "hold");
    }

    /// Not every `Value` is an object — a function that returned
    /// `Value::Null` where a patch belongs must not read as a null field.
    #[test]
    #[should_panic(expected = "not a JSON object")]
    fn a_non_object_has_no_fields_to_null() {
        assert_explicit_null!(json!(null), "hold");
    }
}
