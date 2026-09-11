//! HOW an `answer-question` step was decided: `accepted_as_proposed`.
//!
//! WHY (c17871fe). On 2026-09-08 David approved two design reviews in
//! the UI. Each completed step carried an `answer` equal to the
//! packet's `proposed` text and nothing else — so the operator session
//! could not tell a human accepting the proposal from a rule copying
//! `proposed` into `answer`, and had to ask. Car 1 stamped WHO and WHEN
//! (`completed_by` / `completed_at`); this stamps HOW.
//!
//! SERVER-SIDE, BY COMPARISON, NEVER CLIENT-SUPPLIED. The answer-question
//! plugin's accept button copies `job.metadata.proposed` into the
//! answer; a rule doing the same copy produces the same bytes. Either
//! way the step ends up with `answer == proposed`, and that equality
//! is the fact worth recording — so the server computes it at the
//! flip to `completed` and overwrites whatever the body said. A UI
//! accept and a rule copy are then equally legible, and a client
//! cannot claim acceptance it did not make.
//!
//! Leading and trailing whitespace is not an edit: a textarea that
//! appends a newline did not change the decision.

use serde_json::Value;

/// The step kind this applies to.
pub const KIND: &str = "answer-question";
/// The metadata key stamped at completion.
pub const KEY: &str = "accepted_as_proposed";

/// Whether `answer` is the proposal verbatim (modulo surrounding
/// whitespace). No proposal, or no answer, is never an acceptance.
pub fn accepted(answer: Option<&str>, proposed: Option<&str>) -> bool {
    match (answer, proposed) {
        (Some(a), Some(p)) => a.trim() == p.trim(),
        _ => false,
    }
}

/// Stamp `metadata` for a write to an answer-question step.
///
/// At the flip to `completed`, `accepted_as_proposed` is computed from
/// `metadata.answer` against `proposed` (the packet's
/// `job.metadata.proposed`). On every other write the stored value is
/// carried forward when there is one, and a client-supplied value is
/// dropped when there is not — the key is the server's, whichever way
/// the write goes.
pub fn stamp(metadata: &mut Value, stored: &Value, flipping_to_done: bool, proposed: Option<&str>) {
    let Some(obj) = metadata.as_object_mut() else {
        return;
    };
    if flipping_to_done {
        let answer = obj.get("answer").and_then(Value::as_str);
        let value = accepted(answer, proposed);
        obj.insert(KEY.to_string(), Value::Bool(value));
        return;
    }
    match stored.get(KEY) {
        Some(v) => {
            obj.insert(KEY.to_string(), v.clone());
        }
        None => {
            obj.remove(KEY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepted_is_verbatim_equality_modulo_surrounding_whitespace() {
        assert!(accepted(Some("Ship it"), Some("Ship it")));
        assert!(accepted(Some("Ship it\n"), Some("  Ship it")));
        assert!(!accepted(Some("Ship it, but later"), Some("Ship it")));
        assert!(!accepted(None, Some("Ship it")));
        assert!(!accepted(Some("Ship it"), None));
        assert!(!accepted(None, None));
    }

    #[test]
    fn the_flip_stamps_true_when_the_answer_is_the_proposal() {
        let mut md = json!({ "verdict": "approved", "answer": "Ship it" });
        stamp(&mut md, &json!({}), true, Some("Ship it"));
        assert_eq!(md[KEY], true);
    }

    #[test]
    fn the_flip_stamps_false_when_the_answer_was_edited() {
        let mut md = json!({ "verdict": "approved", "answer": "Ship it, but later" });
        stamp(&mut md, &json!({}), true, Some("Ship it"));
        assert_eq!(md[KEY], false);
    }

    #[test]
    fn a_client_supplied_value_is_overwritten_at_the_flip() {
        let mut md = json!({ "verdict": "approved", "answer": "edited", KEY: true });
        stamp(&mut md, &json!({}), true, Some("Ship it"));
        assert_eq!(md[KEY], false);
    }

    #[test]
    fn a_client_supplied_value_is_dropped_off_the_flip() {
        let mut md = json!({ "answer": "draft", KEY: true });
        stamp(&mut md, &json!({}), false, Some("Ship it"));
        assert!(md.get(KEY).is_none());
    }

    #[test]
    fn a_stored_value_rides_through_a_later_write_unchanged() {
        // A completed step's stamp survives a metadata re-send that
        // omits it (PATCH-on-PUT callers) and one that contradicts it.
        let stored = json!({ "answer": "Ship it", KEY: true });
        let mut md = json!({ "answer": "Ship it" });
        stamp(&mut md, &stored, false, None);
        assert_eq!(md[KEY], true);
        let mut md = json!({ "answer": "Ship it", KEY: false });
        stamp(&mut md, &stored, false, None);
        assert_eq!(md[KEY], true);
    }
}
