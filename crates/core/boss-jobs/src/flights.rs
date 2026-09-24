//! Flights — a change shipped to our own instance behind a flag, turned
//! on for an audience, watched, then promoted or pulled (design
//! c4c2a607, decided by David 2026-09-24; backlog 73c31776, car 1).
//!
//! A FLIGHT IS A PACKET, AND THE REGISTRY IS A READ OVER OPEN PACKETS.
//! There is no flights table. A flight is an open packet whose JOB
//! metadata carries a [`FLIGHT_KEY`] block — `code`, `hypothesis`,
//! `signal`, `audience`, `owner`, `observe_days` — and its state is
//! what its completed steps say. A second store would be the same fact
//! living twice (CLAUDE.md §9a), and the packet is already versioned,
//! audited and rebuildable.
//!
//! THE STATE IS THE COMPLETIONS, READ AGAINST THE PINNED PROTOCOL — not
//! a `state` field on the packet. The protocol row declares, on each
//! step whose completion changes a flight, the state that completion
//! sets (`metadata_defaults.flight_state`); this module reads that off
//! the packet's PINNED workflow version and never off the packet's own
//! step metadata, which a completing actor writes. So every state
//! change is a step completion — already an event in the log, with its
//! actor and instant — and a completion cannot claim a state its step
//! does not declare. The design (§1) also listed `state` among the
//! block's keys; storing it there beside the completions would let the
//! two disagree, so the completions are the one copy.
//!
//! THE RULE, stated once:
//!   - a completed step whose spec declares `off` turns the flight off
//!     for good — pulling is the safe direction, and no later widen
//!     undoes it;
//!   - otherwise the strongest state any completed step declares;
//!   - no such completion: off.
//! A state declared on a `human_only` step counts only when a PERSON
//! completed it (`completed_by` is human): widening to everyone and
//! promoting need David's sign-off while he is the only human user
//! (the design review's one answer), and the record, not a belief,
//! is what shows it was his.
//!
//! GENERIC BY CONSTRUCTION. Nothing here names a workflow kind or a
//! step slug: the read is every open packet carrying the block, and the
//! meaning is the protocol row's. `flight-a-change` can be replaced
//! without a deploy.
//!
//! A CODE THE READ DOES NOT LIST IS OFF — retired, unknown, ambiguous,
//! or a dark read. A code two open packets both declare is ambiguous
//! and answers off, the safe direction, rather than picking one.

use std::collections::BTreeMap;

use boss_core::job::{Step, StepStatus};
use serde_json::Value;

use crate::registry::WorkflowSpec;

/// The job-metadata key holding a flight's declaration.
pub const FLIGHT_KEY: &str = "flight";

/// The step-spec `metadata_defaults` key a protocol declares a
/// completion's flight state under.
pub const STATE_KEY: &str = "flight_state";

/// A flight's state. Ordered by reach: `Off` < `OnForAudience` <
/// `OnForAll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlightState {
    Off,
    OnForAudience,
    OnForAll,
}

impl FlightState {
    /// The wire spelling the protocol row uses.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "on-for-audience" => Some(Self::OnForAudience),
            "on-for-all" => Some(Self::OnForAll),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::OnForAudience => "on-for-audience",
            Self::OnForAll => "on-for-all",
        }
    }
}

/// The part of a flight's declaration the read needs: its code and its
/// audience (`audience = {actors: [ids], roles: [codes]}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub code: String,
    pub actors: Vec<String>,
    pub roles: Vec<String>,
}

/// Is `code` a flight code — kebab-case, `[a-z0-9]` words joined by
/// single hyphens? The code is what source names in `flightOn('<code>')`
/// and what the cleanup probe greps for, so it must be a plain token.
pub fn is_code(code: &str) -> bool {
    !code.is_empty()
        && code.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// The declaration a packet's job metadata carries, when it carries a
/// well-formed one. A block with no code, a code that is not a plain
/// kebab token, or an audience that is not lists of strings is `None` —
/// a malformed flight is off, never a guess.
pub fn declaration(job_metadata: &Value) -> Option<Declaration> {
    let block = job_metadata.get(FLIGHT_KEY)?.as_object()?;
    let code = block.get("code")?.as_str()?.trim();
    if !is_code(code) {
        return None;
    }
    let names = |key: &str| -> Option<Vec<String>> {
        match block.get("audience").and_then(|a| a.get(key)) {
            None | Some(Value::Null) => Some(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(|s| s.trim().to_string()))
                .collect(),
            Some(_) => None,
        }
    };
    Some(Declaration {
        code: code.to_string(),
        actors: names("actors")?,
        roles: names("roles")?,
    })
}

/// The flight state a packet's completed steps set, read against the
/// protocol version it is pinned to. See the module docs for the rule.
pub fn state_of(steps: &[Step], spec: &WorkflowSpec) -> FlightState {
    let declared: Vec<FlightState> = steps
        .iter()
        .filter(|s| s.status == StepStatus::Completed)
        .filter_map(|s| {
            let slug = s.spec_slug.as_deref()?;
            let spec_step = spec.steps.iter().find(|p| p.title == slug)?;
            let state = spec_step
                .metadata_defaults
                .get(STATE_KEY)
                .and_then(Value::as_str)
                .and_then(FlightState::parse)?;
            let needs_a_person = crate::human_only::declared(&spec_step.metadata_defaults);
            let by_a_person = s.completed_by.as_ref().is_some_and(|a| a.is_human());
            (!needs_a_person || by_a_person).then_some(state)
        })
        .collect();
    if declared.contains(&FlightState::Off) {
        return FlightState::Off;
    }
    declared.into_iter().max().unwrap_or(FlightState::Off)
}

/// Is a flight in `state` on for the viewer `(id, role)`?
pub fn on_for(decl: &Declaration, state: FlightState, id: &str, role: &str) -> bool {
    match state {
        FlightState::Off => false,
        FlightState::OnForAll => true,
        FlightState::OnForAudience => {
            decl.actors.iter().any(|a| a == id) || decl.roles.iter().any(|r| r == role)
        }
    }
}

/// The codes on for the viewer, sorted — the whole answer of
/// `GET /api/flights/mine`. A code declared by more than one open
/// packet is ambiguous and is left out.
pub fn codes_on_for(flights: &[(Declaration, FlightState)], id: &str, role: &str) -> Vec<String> {
    let mut by_code: BTreeMap<&str, Vec<bool>> = BTreeMap::new();
    for (decl, state) in flights {
        by_code
            .entry(decl.code.as_str())
            .or_default()
            .push(on_for(decl, *state, id, role));
    }
    by_code
        .into_iter()
        .filter(|(_, on)| on.len() == 1 && on[0])
        .map(|(code, _)| code.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::StepSpec;
    use boss_core::actor::ActorId;
    use boss_core::job::JobId;
    use serde_json::json;

    fn spec() -> WorkflowSpec {
        let step = |slug: &str, defaults: Value| StepSpec {
            title: slug.into(),
            kind: "task".into(),
            ready_when: "true".into(),
            metadata_defaults: defaults,
            ..Default::default()
        };
        WorkflowSpec::platform_seed(
            "a-flight-protocol",
            "Flight",
            "test",
            vec!["custom".into()],
            vec![
                step("ship", json!({})),
                step("turn-on", json!({STATE_KEY: "on-for-audience"})),
                step(
                    "widen",
                    json!({STATE_KEY: "on-for-all", "human_only": true}),
                ),
                step("pull", json!({STATE_KEY: "off"})),
            ],
        )
    }

    fn done(slug: &str, by: ActorId) -> Step {
        let mut s = Step::new(JobId::new(), "task", slug, 0);
        s.spec_slug = Some(slug.into());
        s.status = StepStatus::Completed;
        s.completed_by = Some(by);
        s
    }

    fn agent() -> ActorId {
        ActorId::RegisteredAgent("agent-claude".into())
    }

    fn david() -> ActorId {
        ActorId::Human("emp-david".into())
    }

    fn decl(code: &str, actors: &[&str], roles: &[&str]) -> Declaration {
        Declaration {
            code: code.into(),
            actors: actors.iter().map(|s| s.to_string()).collect(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_declaration_is_read_off_the_flight_block() {
        let md = json!({FLIGHT_KEY: {
            "code": "it-map-motion", "hypothesis": "h", "signal": "s",
            "audience": {"actors": ["emp-david"]}, "owner": "agent-claude", "observe_days": 7,
        }});
        assert_eq!(
            declaration(&md),
            Some(decl("it-map-motion", &["emp-david"], &[]))
        );
    }

    #[test]
    fn a_malformed_block_declares_no_flight() {
        for md in [
            json!({}),
            json!({FLIGHT_KEY: "it-map-motion"}),
            json!({FLIGHT_KEY: {"hypothesis": "no code"}}),
            json!({FLIGHT_KEY: {"code": "It Map Motion"}}),
            json!({FLIGHT_KEY: {"code": "it-map-motion')"}}),
            json!({FLIGHT_KEY: {"code": "-it"}}),
            json!({FLIGHT_KEY: {"code": "it--map"}}),
            json!({FLIGHT_KEY: {"code": "it", "audience": {"actors": "emp-david"}}}),
            json!({FLIGHT_KEY: {"code": "it", "audience": {"roles": [7]}}}),
        ] {
            assert_eq!(declaration(&md), None, "{md}");
        }
    }

    #[test]
    fn a_flight_with_no_state_bearing_completion_is_off() {
        assert_eq!(state_of(&[], &spec()), FlightState::Off);
        assert_eq!(
            state_of(&[done("ship", agent())], &spec()),
            FlightState::Off
        );
    }

    #[test]
    fn turning_on_is_on_for_the_audience() {
        let steps = [done("ship", agent()), done("turn-on", agent())];
        assert_eq!(state_of(&steps, &spec()), FlightState::OnForAudience);
    }

    /// Widening to everyone is a person's act: an agent's completion of
    /// the human-only step changes nothing, David's does.
    #[test]
    fn widening_counts_only_when_a_person_completed_it() {
        let by_agent = [done("turn-on", agent()), done("widen", agent())];
        assert_eq!(state_of(&by_agent, &spec()), FlightState::OnForAudience);
        let by_david = [done("turn-on", agent()), done("widen", david())];
        assert_eq!(state_of(&by_david, &spec()), FlightState::OnForAll);
    }

    #[test]
    fn a_pull_is_off_for_good() {
        let steps = [
            done("turn-on", agent()),
            done("pull", agent()),
            done("widen", david()),
        ];
        assert_eq!(state_of(&steps, &spec()), FlightState::Off);
    }

    /// The state is the PROTOCOL's word, not the packet's: a completion
    /// that writes `flight_state` into its own step metadata claims
    /// nothing, and a step not yet completed sets nothing.
    #[test]
    fn a_completion_cannot_claim_a_state_its_step_does_not_declare() {
        let mut ship = done("ship", agent());
        ship.metadata = json!({STATE_KEY: "on-for-all"});
        let mut widen = done("widen", david());
        widen.status = StepStatus::Ready;
        assert_eq!(state_of(&[ship, widen], &spec()), FlightState::Off);
    }

    #[test]
    fn on_for_audience_reaches_named_actors_and_roles_only() {
        let d = decl("x", &["emp-david"], &["platform-admin"]);
        assert!(on_for(&d, FlightState::OnForAudience, "emp-david", "user"));
        assert!(on_for(
            &d,
            FlightState::OnForAudience,
            "emp-2",
            "platform-admin"
        ));
        assert!(!on_for(&d, FlightState::OnForAudience, "emp-2", "user"));
        assert!(on_for(&d, FlightState::OnForAll, "emp-2", "user"));
        assert!(!on_for(&d, FlightState::Off, "emp-david", "platform-admin"));
    }

    #[test]
    fn the_answer_is_the_viewers_codes_sorted_and_an_ambiguous_code_is_off() {
        let flights = vec![
            (
                decl("zeta", &["emp-david"], &[]),
                FlightState::OnForAudience,
            ),
            (decl("alpha", &[], &[]), FlightState::OnForAll),
            (decl("hidden", &["emp-2"], &[]), FlightState::OnForAudience),
            (decl("twice", &[], &[]), FlightState::OnForAll),
            (decl("twice", &[], &[]), FlightState::OnForAll),
        ];
        assert_eq!(
            codes_on_for(&flights, "emp-david", "platform-admin"),
            vec!["alpha".to_string(), "zeta".to_string()]
        );
        assert_eq!(
            codes_on_for(&flights, "emp-2", "user"),
            vec!["alpha".to_string(), "hidden".to_string()]
        );
    }
}
