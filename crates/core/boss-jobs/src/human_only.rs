//! The human-only declaration on a step, and the one question it asks
//! at assignment and claim time — is the assignee a person? — and again
//! at completion: is the actor signing the flip a person? (The second
//! was missing until backlog adac8fa4: an unheld human-only step could
//! be completed by an agent through the step PUT.)
//!
//! WHY (c17871fe, David 2026-09-08: "A human decision must be
//! unambiguous on the step: who, when, how"). Measured that day: all
//! 48 workable assigned steps sat on the agent actor and none on a
//! person — among them `Kill the old one` on a credential rotation, a
//! step its protocol marks `human_only` and `destructive`, which the
//! dispatcher's executor pick had handed to the filer. The declaration
//! existed; nothing read it.
//!
//! THE DECLARATION IS PROTOCOL DATA, NOT A NEW FIELD. A Workflow step
//! authors `metadata_defaults.human_only`, and materialisation carries
//! it onto the packet's step metadata like every other default — the
//! same channel `authority_role` and `claimable` ride. The live registry
//! spells it two ways (`"true"` on rotate-a-credential v2, `false` on
//! protocol-retro v7), so [`declared`] reads both. my-day.md Q3 wanted
//! the requirement expressed as a predicate over actor attributes rather
//! than a sibling boolean; until that predicate exists, the key the
//! protocols already carry is the one declaration, and this module is
//! the one reader (§9a — do not add a second spelling).
//!
//! WHO IS A PERSON. The employee registry is the source: an assignee is
//! a person when its id is not machine-shaped (`automation:<slug>`,
//! `<mode>:<model>`, `system*`) AND, where a roster is wired, the roster
//! lists it as an active employee. A session identity that is not on
//! the roster — `claude@algedonic.dev` parses as `ActorId::Human` by
//! shape alone — is refused by the roster, which is the point: the
//! actor model cannot tell a login from a person, and the roster can.
//! Since design 6fda05ae that login is resolved at the jobs API's door
//! to `agent-claude`, which IS machine-shaped (`ActorId::RegisteredAgent`),
//! so the shape test refuses it before the roster is asked; the roster
//! arm remains for an address that reaches here unresolved.
//! A roster that cannot answer keeps the human-shaped id (the same
//! grace owner resolution extends) so a people-api blip never locks a
//! person out of the one step that needs them.

use serde_json::Value;

use crate::owner_resolution::{RosterLookup, is_automation_shaped};

/// The metadata key a Workflow step authors to require a person.
pub const KEY: &str = "human_only";

/// Whether this step metadata declares the step human-only. Reads the
/// bool `true` and the string `"true"` (case-insensitive); anything
/// else — absent, `false`, `"false"`, `null` — is not a declaration.
pub fn declared(metadata: &Value) -> bool {
    declaration(metadata) == Some(true)
}

/// The declaration this metadata makes about who may execute the step,
/// when it makes one at all: `Some(true)` for human-only, `Some(false)`
/// for "an agent may do this", `None` when the key is absent or holds a
/// value neither spelling reads.
///
/// ABSENT IS NOT `false`. Almost every step in the bundle says nothing
/// here, and saying nothing is not a claim about agents — which is why
/// [`declared`] alone could not answer the question the publish lint
/// needed (backlog bf7cebc2): the lint refuses a step that CLAIMS
/// agent-workability and funds no agent, and a step that never made
/// the claim must be left alone. Both spellings the live registry
/// carries are read here, so the claim is the same fact whichever way
/// a protocol author wrote it.
pub fn declaration(metadata: &Value) -> Option<bool> {
    match metadata.get(KEY) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.eq_ignore_ascii_case("true") {
                Some(true)
            } else if s.eq_ignore_ascii_case("false") {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Why an assignee is not a person. Carried into the refusal so the
/// caller learns which test failed, not just that one did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotAPerson {
    /// The id is shaped like an automation or an agent session.
    MachineShaped,
    /// The roster does not list the id as an active employee.
    NotOnTheRoster,
}

impl std::fmt::Display for NotAPerson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MachineShaped => {
                f.write_str("the id is machine-shaped (an automation or an agent session)")
            }
            Self::NotOnTheRoster => {
                f.write_str("the employee registry does not list an active employee with this id")
            }
        }
    }
}

/// Is `id` a person? `Ok(())` when it is; `Err(why)` when it is not.
/// `roster = None` (in-memory stacks without a people service) falls
/// back to shape alone.
pub async fn person_check(roster: Option<&dyn RosterLookup>, id: &str) -> Result<(), NotAPerson> {
    if is_automation_shaped(id) {
        return Err(NotAPerson::MachineShaped);
    }
    let Some(roster) = roster else {
        return Ok(());
    };
    match roster.is_active_employee(id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(NotAPerson::NotOnTheRoster),
        Err(e) => {
            // Grace, not refusal: the roster failing to answer says
            // nothing about the assignee, and refusing here would let a
            // people-api blip hold a credential kill hostage.
            tracing::warn!(
                assignee = id,
                error = %e,
                "roster unavailable; keeping the human-shaped assignee on a human-only step"
            );
            Ok(())
        }
    }
}

/// The one sentence a refusal carries as its `rule`.
pub const RULE: &str = "metadata.human_only = true: only an active employee may be assigned \
                        or claim this step; automations and agent sessions are refused";

/// The refusal body: names the step, the assignee, and the rule, so the
/// caller can act without re-deriving any of it (§Diagnosis: a verdict
/// must name what failed).
pub fn refusal_body(
    step_id: &str,
    step_title: &str,
    authority_role: Option<&str>,
    assignee_id: &str,
    why: &NotAPerson,
) -> Value {
    serde_json::json!({
        "error": "human-only step refuses a non-human assignee",
        "step_id": step_id,
        "step_title": step_title,
        "assignee_id": assignee_id,
        "why": why.to_string(),
        "rule": RULE,
        "authority_role": authority_role,
        "hint": "leave the step unassigned for a holder of its authority_role to claim, \
                 or assign an employee id from the people registry",
    })
}

/// The sentence a COMPLETION refusal carries as its `rule` (backlog
/// adac8fa4). The assignment rule above guards who may HOLD the step;
/// this one guards the act itself, because an unheld step could be
/// flipped by anyone the policy lets write steps — measured on the
/// in-memory API 2026-09-24: an agent's `{"status":"completed"}` on an
/// unassigned human-only step answered 204 and stamped the agent as
/// `completed_by`.
pub const COMPLETION_RULE: &str = "metadata.human_only = true: only an active employee may \
                                   complete or skip this step; automations and agent sessions \
                                   are refused, whoever holds it";

/// The completion refusal: names the step, the actor that signed the
/// write, which test it failed, and the rule.
pub fn completion_refusal_body(
    step_id: &str,
    step_title: &str,
    authority_role: Option<&str>,
    actor_id: &str,
    why: &NotAPerson,
) -> Value {
    serde_json::json!({
        "error": "human-only step refuses completion by a non-human actor",
        "step_id": step_id,
        "step_title": step_title,
        "actor_id": actor_id,
        "why": why.to_string(),
        "rule": COMPLETION_RULE,
        "authority_role": authority_role,
        "hint": "a person completes this step: a holder of its authority_role, signed in \
                 as themselves — the declaration is the protocol's, so an agent that \
                 finished the work leaves the flip to them",
    })
}

/// Whether a write would change the declaration the STORED step makes.
/// `next` is the metadata as it would stand after the write (a merge
/// door `null` already applied). A stored row that says nothing has
/// nothing to protect, and an unchanged re-send is not a change.
///
/// WHY THE DECLARATION IS FROZEN ON THE STEP. The completion check reads
/// the stored row, and the row is the only thing a step write can
/// reach — so if a write could set `human_only` to `false` or delete it
/// through the merge door, the next status PUT would sail past the
/// check. Which acts need a person is the protocol's decision, made in
/// the Workflow row and changed by publishing a new version, never by a
/// write to one in-flight step.
pub fn declaration_changed(stored: &Value, next: &Value) -> bool {
    stored.get(KEY).is_some() && stored.get(KEY) != next.get(KEY)
}

/// The refusal for a write that would change the declaration.
pub fn change_refusal_body(step_id: &str, step_title: &str, stored: &Value) -> Value {
    serde_json::json!({
        "error": "human_only is the protocol's declaration and a step write cannot change it",
        "step_id": step_id,
        "step_title": step_title,
        "stored": stored.get(KEY),
        "rule": COMPLETION_RULE,
        "hint": "send the stored value back unchanged; which steps need a person is \
                 changed by publishing a new workflow version",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    #[test]
    fn declared_reads_both_spellings_the_live_registry_carries() {
        // rotate-a-credential v2 authors the string.
        assert!(declared(&json!({ "human_only": "true" })));
        assert!(declared(&json!({ "human_only": "TRUE" })));
        // A bool is the natural authoring.
        assert!(declared(&json!({ "human_only": true })));
        // protocol-retro v7 authors `false`; absence is not a declaration.
        assert!(!declared(&json!({ "human_only": false })));
        assert!(!declared(&json!({ "human_only": "false" })));
        assert!(!declared(&json!({ "human_only": null })));
        assert!(!declared(&json!({ "authority_role": "platform-admin" })));
        assert!(!declared(&json!(null)));
    }

    /// The publish lint asks a question `declared` cannot answer: did
    /// the step CLAIM agent-workability, or say nothing? Absent and
    /// `false` are both "not human-only" and only the second is a
    /// claim (backlog bf7cebc2).
    #[test]
    fn declaration_separates_an_explicit_false_from_an_absent_key() {
        assert_eq!(declaration(&json!({ "human_only": false })), Some(false));
        assert_eq!(declaration(&json!({ "human_only": "false" })), Some(false));
        assert_eq!(declaration(&json!({ "human_only": "FALSE" })), Some(false));
        assert_eq!(
            declaration(&json!({ "human_only": " false " })),
            Some(false)
        );
        assert_eq!(declaration(&json!({ "human_only": true })), Some(true));
        assert_eq!(declaration(&json!({ "human_only": "true" })), Some(true));
        // No key, a null, and a value neither spelling reads are all
        // silence — nothing to hold to an agent block.
        assert_eq!(declaration(&json!({ "authority_role": "x" })), None);
        assert_eq!(declaration(&json!({ "human_only": null })), None);
        assert_eq!(declaration(&json!({ "human_only": "maybe" })), None);
        assert_eq!(declaration(&json!(null)), None);
    }

    struct Roster(Vec<&'static str>);

    #[async_trait]
    impl RosterLookup for Roster {
        async fn active_holders(&self, _role: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
        async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
            Ok(self.0.contains(&id))
        }
    }

    struct BrokenRoster;

    #[async_trait]
    impl RosterLookup for BrokenRoster {
        async fn active_holders(&self, _role: &str) -> Result<Vec<String>, String> {
            Err("people-api 503".into())
        }
        async fn is_active_employee(&self, _id: &str) -> Result<bool, String> {
            Err("people-api 503".into())
        }
    }

    #[tokio::test]
    async fn machine_shaped_ids_are_refused_before_the_roster_is_asked() {
        let roster = Roster(vec!["emp-david"]);
        for id in [
            "automation:rule:assign",
            "claude:opus-5",
            "system",
            "system:dispatcher",
            "",
        ] {
            assert_eq!(
                person_check(Some(&roster), id).await,
                Err(NotAPerson::MachineShaped),
                "{id}"
            );
            // Without a roster, shape is the whole test.
            assert_eq!(person_check(None, id).await, Err(NotAPerson::MachineShaped));
        }
    }

    #[tokio::test]
    async fn the_roster_decides_a_human_shaped_id() {
        let roster = Roster(vec!["emp-david"]);
        assert_eq!(person_check(Some(&roster), "emp-david").await, Ok(()));
        // A session identity is human-shaped and NOT a person: the
        // actor model cannot tell, the roster can.
        assert_eq!(
            person_check(Some(&roster), "claude@algedonic.dev").await,
            Err(NotAPerson::NotOnTheRoster)
        );
        // No roster wired: shape alone, so the login passes.
        assert_eq!(person_check(None, "claude@algedonic.dev").await, Ok(()));
        // The login RESOLVED (design 6fda05ae) is machine-shaped by
        // its own id, so the roster is not needed to refuse it.
        assert_eq!(
            person_check(None, "agent-claude").await,
            Err(NotAPerson::MachineShaped)
        );
    }

    #[tokio::test]
    async fn a_roster_that_cannot_answer_keeps_the_human_shaped_id() {
        assert_eq!(person_check(Some(&BrokenRoster), "emp-david").await, Ok(()));
        // ...but never a machine-shaped one.
        assert_eq!(
            person_check(Some(&BrokenRoster), "automation:x").await,
            Err(NotAPerson::MachineShaped)
        );
    }

    #[test]
    fn a_change_is_only_a_change_to_a_declaration_the_row_already_makes() {
        let stored = json!({ "human_only": "true", "authority_role": "platform-admin" });
        // Deleted (merge-door null), flipped, or respelled: all changes.
        assert!(declaration_changed(
            &stored,
            &json!({ "authority_role": "platform-admin" })
        ));
        assert!(declaration_changed(
            &stored,
            &json!({ "human_only": false })
        ));
        assert!(declaration_changed(&stored, &json!({ "human_only": true })));
        // Sent back as stored: not a change.
        assert!(!declaration_changed(
            &stored,
            &json!({ "human_only": "true", "note": "x" })
        ));
        // A row that says nothing has nothing to protect.
        assert!(!declaration_changed(
            &json!({ "note": "x" }),
            &json!({ "human_only": true })
        ));
    }

    #[test]
    fn the_completion_refusal_names_the_step_the_actor_and_the_rule() {
        let body = completion_refusal_body(
            "step-1",
            "Human review of the findings",
            Some("platform-admin"),
            "agent-claude",
            &NotAPerson::MachineShaped,
        );
        assert_eq!(body["step_id"], "step-1");
        assert_eq!(body["actor_id"], "agent-claude");
        assert!(body["rule"].as_str().unwrap().contains("human_only"));
        assert!(body["rule"].as_str().unwrap().contains("complete"));
        assert!(body["why"].as_str().unwrap().contains("machine-shaped"));
    }

    #[test]
    fn the_refusal_names_the_step_the_assignee_and_the_rule() {
        let body = refusal_body(
            "step-1",
            "Kill the old one",
            Some("platform-admin"),
            "claude@algedonic.dev",
            &NotAPerson::NotOnTheRoster,
        );
        assert_eq!(body["step_id"], "step-1");
        assert_eq!(body["step_title"], "Kill the old one");
        assert_eq!(body["assignee_id"], "claude@algedonic.dev");
        assert_eq!(body["authority_role"], "platform-admin");
        assert!(body["rule"].as_str().unwrap().contains("human_only"));
        assert!(body["why"].as_str().unwrap().contains("employee registry"));
    }
}
