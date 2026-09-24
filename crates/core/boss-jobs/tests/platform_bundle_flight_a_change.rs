//! `infra/platform/workflows/flight-a-change.toml` keeps its decided
//! shape (design c4c2a607, backlog 73c31776 car 1). One pin file per
//! kind file (see `platform_bundle.rs`), so a new protocol touches no
//! shared line.
//!
//! The flights read takes a flight's state from THIS row — the
//! `flight_state` each step declares — so the declarations are the
//! protocol's whole meaning to the read, and a reshape that drops one
//! turns a flight silently off (or on). The review's one answer is
//! pinned beside them: an agent may ship, turn on and pull; widening to
//! everyone and deciding are a person's.

use boss_jobs::flights::{FlightState, STATE_KEY};
use boss_jobs::registry::{StepSpec, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == "flight-a-change")
        .expect("flight-a-change ships in the platform bundle")
}

fn step<'a>(w: &'a WorkflowSpec, slug: &str) -> &'a StepSpec {
    w.steps
        .iter()
        .find(|s| s.title == slug)
        .unwrap_or_else(|| panic!("flight-a-change has no `{slug}` step"))
}

fn declared_state(s: &StepSpec) -> Option<FlightState> {
    s.metadata_defaults
        .get(STATE_KEY)
        .and_then(|v| v.as_str())
        .map(|v| FlightState::parse(v).unwrap_or_else(|| panic!("`{}`: bad state {v}", s.title)))
}

#[test]
fn exactly_turn_on_widen_and_pull_change_a_flights_state() {
    let w = bundled();
    let declared: Vec<(String, FlightState)> = w
        .steps
        .iter()
        .filter_map(|s| declared_state(s).map(|st| (s.title.clone(), st)))
        .collect();
    assert_eq!(
        declared,
        vec![
            ("turn-on".to_string(), FlightState::OnForAudience),
            ("widen".to_string(), FlightState::OnForAll),
            ("pull".to_string(), FlightState::Off),
        ],
    );
}

/// David, 2026-09-24: an agent may file, ship, turn on for him, record
/// its reading and pull at any time; widening to everyone and a promote
/// verdict need his sign-off while he is the only human user.
#[test]
fn widening_and_deciding_are_a_persons_and_pulling_is_anyones() {
    let w = bundled();
    for slug in ["widen", "decide"] {
        assert!(
            boss_jobs::human_only::declared(&step(&w, slug).metadata_defaults),
            "`{slug}` must be human_only"
        );
    }
    for slug in [
        "ship",
        "turn-on",
        "observe",
        "extend",
        "reobserve",
        "pull",
        "cleanup",
    ] {
        assert!(
            !boss_jobs::human_only::declared(&step(&w, slug).metadata_defaults),
            "`{slug}` is open to an agent"
        );
    }
    assert_eq!(
        step(&w, "pull").ready_when,
        "steps.opened.done",
        "a flight can be pulled at any time after it is filed"
    );
}

/// The verdict is exclusive, and off is final: a pull reaches `pulled`
/// whatever `decide` said, and `promoted` is refused once pulled.
#[test]
fn the_terminals_are_promoted_and_pulled_and_a_pull_wins() {
    let w = bundled();
    let terminals: Vec<&str> = w
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    assert_eq!(terminals, vec!["promoted", "pulled"]);
    assert!(
        step(&w, "promoted")
            .ready_when
            .contains("NOT steps.pull.done")
    );
    assert!(step(&w, "pulled").ready_when.contains("steps.pull.done OR"));
    assert_eq!(
        step(&w, "decide")
            .fields
            .iter()
            .find(|f| f.name == "verdict")
            .map(|f| f.field_type.as_str()),
        Some("promote|pull")
    );
}

/// The declaration rides in job metadata, and admission refuses a
/// packet that does not carry it.
#[test]
fn the_flight_block_is_required_at_admission() {
    let w = bundled();
    assert_eq!(w.metadata_schema["required"], serde_json::json!(["flight"]));
    let inner = &w.metadata_schema["properties"]["flight"]["required"];
    for key in [
        "code",
        "hypothesis",
        "signal",
        "audience",
        "owner",
        "observe_days",
    ] {
        assert!(
            inner.as_array().is_some_and(|a| a.iter().any(|v| v == key)),
            "`flight.{key}` is required: {inner}"
        );
    }
}
