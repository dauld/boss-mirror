//! The human-only declaration on a step, and the one question it asks
//! at assignment time: is the assignee a person?
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
    match metadata.get(KEY) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
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
