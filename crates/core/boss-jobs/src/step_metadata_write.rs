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

/// Metadata keys the PROTOCOL writes and no step writer may (backlog
/// b433bdf3). `outcome_kind` is materialised from the spec's
/// `metadata_defaults`, and the step PUT reads the stored value to let
/// an `aborted` terminal complete past its blockers — so a writer that
/// could set it on an ordinary terminal, through either door, and then
/// PUT a bare completion walked round the blocker gate; one that could
/// delete it turned a real abort into a gated step.
pub const PROTOCOL_KEYS: &[&str] = &["outcome_kind"];

/// The hint a refused protocol key carries, on both doors.
pub const PROTOCOL_KEYS_HINT: &str = "these metadata keys are materialised from the step's \
     protocol and are not a writer's to set, change or delete. Send the stored value back \
     unchanged, or leave the key out of a merge.";

/// PURE: the [`PROTOCOL_KEYS`] whose value in `after` (the row as the
/// write would leave it) differs from `stored` — added, changed or
/// removed. Empty for an unchanged re-send.
pub fn protocol_keys_changed(stored: &Value, after: &Value) -> Vec<&'static str> {
    PROTOCOL_KEYS
        .iter()
        .copied()
        .filter(|k| stored.get(k) != after.get(k))
        .collect()
}

/// The step fields a PUT body may not move, in the order it reports
/// them (backlog b433bdf3). Each is the step's place in its protocol:
/// `kind` chooses the kind bundle's required fields and the assurance
/// floor; `spec_slug` names the step to every predicate and rule;
/// `sort_order` is the index a completed step is paired back to its
/// spec by, and so chooses the terminal that closes the packet;
/// `blocked_by` is what the blocker gate reads; `fields` is the
/// required-at-done contract. A read-merge-write sends each back as it
/// stands and is not judged.
///
/// `assurance_required` is the weakest stamp the step accepts — the
/// Workflow may raise it, never a writer (36352452: `null` lowered a
/// Presence step in memory, while the Pg UPDATE, which never names the
/// column, answered 204 over a write that did not land).
/// `step_plugin_version` is the plugin bundle the step was pinned to
/// when it was written, and drifted between the adapters the same way.
pub fn reshaped_fields(
    stored: &boss_core::job::Step,
    after: &boss_core::job::Step,
) -> Vec<&'static str> {
    [
        ("kind", stored.kind != after.kind),
        ("spec_slug", stored.spec_slug != after.spec_slug),
        ("sort_order", stored.sort_order != after.sort_order),
        ("blocked_by", stored.blocked_by != after.blocked_by),
        ("fields", stored.fields != after.fields),
        (
            "assurance_required",
            stored.assurance_required != after.assurance_required,
        ),
        (
            "step_plugin_version",
            stored.step_plugin_version != after.step_plugin_version,
        ),
    ]
    .into_iter()
    .filter_map(|(name, moved)| moved.then_some(name))
    .collect()
}

/// The hint the step PUT's reshape refusal carries.
pub const RESHAPED_FIELDS_HINT: &str = "a step's kind, slug, index, blocker edges, \
     required-at-done fields, assurance requirement and plugin version are its place in the \
     protocol the packet was admitted under, and a step PUT does not move them. Send them back as read, or leave them out. A packet moves \
     between protocol versions only through `boss job convert <packet> [--to vN]`, which \
     records the move.";

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
    fn a_protocol_key_added_changed_or_removed_is_named_and_a_resend_is_not() {
        let stored = json!({"outcome_kind": "completed", "note": 1});
        assert!(protocol_keys_changed(&stored, &stored).is_empty());
        assert!(protocol_keys_changed(&stored, &json!({"outcome_kind": "completed"})).is_empty());
        assert_eq!(
            protocol_keys_changed(&stored, &json!({"outcome_kind": "aborted"})),
            vec!["outcome_kind"]
        );
        assert_eq!(
            protocol_keys_changed(&stored, &json!({})),
            vec!["outcome_kind"]
        );
        assert_eq!(
            protocol_keys_changed(&json!({}), &json!({"outcome_kind": "aborted"})),
            vec!["outcome_kind"]
        );
        assert!(protocol_keys_changed(&Value::Null, &json!({"other": 1})).is_empty());
    }

    #[test]
    fn a_step_sent_back_as_read_moves_nothing_and_each_move_is_named() {
        let job = boss_core::job::JobId::new();
        let stored = boss_core::job::Step::new(job, "task", "Work", 0);
        assert!(reshaped_fields(&stored, &stored.clone()).is_empty());
        let mut after = stored.clone();
        after.title = "Retitled".into();
        after.status = boss_core::job::StepStatus::Active;
        assert!(
            reshaped_fields(&stored, &after).is_empty(),
            "title and status are not the protocol's"
        );
        after.kind = "outcome".into();
        after.sort_order = 2;
        after.blocked_by = vec![boss_core::job::StepId::new()];
        after.spec_slug = Some("done".into());
        after.fields = vec![];
        let mut with_fields = stored.clone();
        with_fields.fields = vec![boss_core::job::StepField {
            name: "evidence".into(),
            field_type: "string".into(),
            required: true,
            filled_by: Default::default(),
            item_keys: Vec::new(),
            covers: None,
            binds: None,
            item_value_max_bytes: None,
            item_one_of: Vec::new(),
        }];
        assert_eq!(
            reshaped_fields(&with_fields, &after),
            vec!["kind", "spec_slug", "sort_order", "blocked_by", "fields"]
        );
    }

    /// 36352452: the assurance requirement and the plugin version were
    /// taken from the body — stored in memory, dropped by Pg — so `null`
    /// lowered a Presence step in one adapter and 204'd over nothing in
    /// the other. Each is named when it moves, lowered or raised.
    #[test]
    fn the_assurance_requirement_and_plugin_version_are_named_when_moved() {
        let job = boss_core::job::JobId::new();
        let mut stored = boss_core::job::Step::new(job, "task", "Approve", 1);
        stored.assurance_required = Some(boss_core::job::Assurance::Presence);
        stored.step_plugin_version = 3;
        assert!(reshaped_fields(&stored, &stored.clone()).is_empty());

        let mut lowered = stored.clone();
        lowered.assurance_required = None;
        assert_eq!(
            reshaped_fields(&stored, &lowered),
            vec!["assurance_required"]
        );
        let mut raised = lowered.clone();
        raised.assurance_required = Some(boss_core::job::Assurance::Presence);
        assert_eq!(
            reshaped_fields(&lowered, &raised),
            vec!["assurance_required"]
        );
        let mut repinned = stored.clone();
        repinned.step_plugin_version = 4;
        assert_eq!(
            reshaped_fields(&stored, &repinned),
            vec!["step_plugin_version"]
        );
        assert!(RESHAPED_FIELDS_HINT.contains("assurance requirement"));
    }

    #[test]
    fn the_hint_names_the_merge_door_and_the_read_merge_write() {
        assert!(OMITTED_KEYS_HINT.contains("PATCH /api/jobs/{id}/steps/{step_id}/metadata"));
        assert!(OMITTED_KEYS_HINT.contains("sent as null is deleted"));
        assert!(OMITTED_KEYS_HINT.contains("read-merge-write"));
    }
}
