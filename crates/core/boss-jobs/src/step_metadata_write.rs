//! THE TWO STEP-METADATA DOORS, AS PURE RULES (backlog e39a9d2a).
//!
//! A step's metadata has two writers: the step PUT, which overlays its
//! body onto the stored step and so REPLACES `metadata` wholesale, and
//! the merge door, `PATCH /api/jobs/{id}/steps/{step_id}/metadata`,
//! which lands keys one at a time and deletes a key sent as `null`.
//!
//! The PUT refuses a metadata body that OMITS a stored key
//! ([`omitted_keys`]); the merge door answers a re-send that changes
//! nothing on a terminal step with success ([`patch_is_noop`]). Both
//! rules are here, pure, so the handlers do only the I/O and the test
//! doubles that answer the way the real API does (the boss-cli stub,
//! the dispatcher mocks) can call the same function rather than
//! retyping it (CLAUDE.md §9a).

use serde_json::{Map, Value};

/// The hint the step PUT's omission refusal carries — ONE copy, the
/// same shape as [`crate::corrections::TERMINAL_STEP_HINT`]: the door
/// that works, named, because the caller is by definition trying to
/// change some keys and leave the rest alone.
pub const OMITTED_KEYS_HINT: &str = "a step PUT replaces metadata wholesale, so a key its body \
     leaves out would be deleted. Send only the keys you change through the step merge door, \
     PATCH /api/jobs/{id}/steps/{step_id}/metadata, where a key sent as null is deleted; or \
     read the step and send every stored key back with the PUT, which is what a \
     read-merge-write does. A PUT without a metadata key is never judged.";

/// PURE: the keys `stored` holds that `sent` leaves out, in stored
/// order. Empty when `sent` carries every stored key (a read-merge-write)
/// or when nothing is stored. A `sent` that is not an object leaves out
/// every stored key.
pub fn omitted_keys<'a>(stored: &'a Value, sent: &Value) -> Vec<&'a str> {
    let sent = sent.as_object();
    stored
        .as_object()
        .map(|stored| {
            stored
                .keys()
                .filter(|k| !sent.is_some_and(|s| s.contains_key(*k)))
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default()
}

/// PURE: would merging `patch` into `stored` leave it exactly as it is?
/// Every key already holds the value sent, and every key sent as `null`
/// (a delete) is already absent. The merge door's idempotent re-send
/// test: the writers moved onto it (e39a9d2a) re-send on a redelivery
/// or a retry, and the PUT they replaced answered an unchanged re-send
/// to a terminal step with success — so the door they moved to must too.
pub fn patch_is_noop(stored: &Value, patch: &Map<String, Value>) -> bool {
    let stored = stored.as_object();
    patch
        .iter()
        .all(|(k, v)| match (v, stored.and_then(|s| s.get(k))) {
            (Value::Null, held) => held.is_none(),
            (v, held) => held == Some(v),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a_body_that_leaves_a_stored_key_out_names_it() {
        let stored = json!({"authority_role": "platform-admin", "station": "dock", "hold": "x"});
        assert_eq!(
            omitted_keys(&stored, &json!({"station": "dock"})),
            vec!["authority_role", "hold"]
        );
    }

    #[test]
    fn a_read_merge_write_leaves_nothing_out_and_may_add_keys() {
        let stored = json!({"authority_role": "platform-admin", "station": "dock"});
        let sent = json!({"authority_role": "platform-admin", "station": "dock", "new": 1});
        assert!(omitted_keys(&stored, &sent).is_empty());
        // A key sent as null is still SENT: the PUT stores the null.
        let sent = json!({"authority_role": null, "station": null});
        assert!(omitted_keys(&stored, &sent).is_empty());
    }

    #[test]
    fn nothing_stored_means_nothing_can_be_left_out() {
        assert!(omitted_keys(&json!({}), &json!({})).is_empty());
        assert!(omitted_keys(&Value::Null, &json!({"a": 1})).is_empty());
    }

    #[test]
    fn a_non_object_body_leaves_out_every_stored_key() {
        assert_eq!(omitted_keys(&json!({"a": 1}), &Value::Null), vec!["a"]);
    }

    #[test]
    fn a_patch_whose_keys_already_hold_its_values_is_a_noop() {
        let stored = json!({"verdict": "green", "receipt": {"sha": "abc"}});
        assert!(patch_is_noop(&stored, &obj(json!({"verdict": "green"}))));
        assert!(patch_is_noop(
            &stored,
            &obj(json!({"receipt": {"sha": "abc"}}))
        ));
        assert!(patch_is_noop(&stored, &obj(json!({"absent": null}))));
        assert!(patch_is_noop(&stored, &Map::new()));
    }

    #[test]
    fn a_patch_that_changes_adds_or_deletes_a_key_is_not() {
        let stored = json!({"verdict": "green", "gone": null});
        assert!(!patch_is_noop(&stored, &obj(json!({"verdict": "red"}))));
        assert!(!patch_is_noop(&stored, &obj(json!({"new": 1}))));
        assert!(!patch_is_noop(&stored, &obj(json!({"verdict": null}))));
        // A stored null is still a stored KEY; a null patch deletes it.
        assert!(!patch_is_noop(&stored, &obj(json!({"gone": null}))));
    }

    #[test]
    fn the_hint_names_the_merge_door_and_the_read_merge_write() {
        assert!(OMITTED_KEYS_HINT.contains("PATCH /api/jobs/{id}/steps/{step_id}/metadata"));
        assert!(OMITTED_KEYS_HINT.contains("sent as null is deleted"));
        assert!(OMITTED_KEYS_HINT.contains("read-merge-write"));
    }
}
