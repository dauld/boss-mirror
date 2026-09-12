//! The `spawn-car-on-sweep-remediated` rule at v3 — a sweep files a
//! delivery packet only when it said a code change is owed, and a
//! recurring finding still mints at most one car.
//!
//! Pins the exact `when` the rule file ships against the expr engine,
//! and the shape of the spawn it produces. Defect e74b32a1: two cars
//! sat on the board a day apart, both titled "Stale build cache
//! sweep", both from the same target, and the only way to tell them
//! apart was to open each one.
//!
//! v3 (backlog 21edde87, measured 2026-09-11) moves the trigger from
//! `outcome = "remediated"` to `outcome = "change-needed"`. Of the 21
//! remediated sweeps the registry had closed by then, NINETEEN were
//! discharged operationally — disk reclaimed, a build cache pruned, an
//! orphan object deleted, a CI image rebuilt — and each minted a
//! `ship-a-change` packet that could never progress, because there was
//! no code to ship: no branch was ever cut and it sat at `scope` until
//! a human recognised it as residue and abandoned it by hand.
//! `remediated` answers "did something have to change"; it was being
//! read as "does the TREE have to change". The sweep protocol now says
//! which it is, as a field on the `remediate` step, and routes to a
//! terminal per answer — so this rule keeps reading the one field the
//! closed-packet payload already carries, and stops having to be right
//! about intent. The bundle side is pinned by
//! `boss-jobs/tests/platform_bundle_maintenance_sweep.rs`.
//!
//! The interesting assertion is the KEY. Dedup has to key on the
//! sweep's subject, because the other two candidates cannot separate
//! "the same finding again" from "a different finding": `id` is fresh
//! every firing and `title` is templated per target. The last test
//! here is the one that would have caught the original defect.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

const RULE: &str = r#"
[[rule]]
name = "spawn-car-on-sweep-remediated"
on_event = "jobs.job.closed"
when = "kind = \"maintenance-sweep\" AND outcome = \"change-needed\" AND NOT open_car_exists(subject_id)"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"ship-a-change\"", subject_kind = "\"custom\"", subject = "id", title = "title", "metadata.backlog_item" = "id", "metadata.sweep_target" = "subject_id" }
"#;

/// `open_car_exists` answering a fixed value, recording what it was
/// asked about so a test can assert the dedup KEY, not just the
/// verdict.
struct StubCars {
    answer: bool,
    asked: std::sync::Mutex<Vec<String>>,
}

impl StubCars {
    fn new(answer: bool) -> Self {
        Self {
            answer,
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn asked_about(&self) -> Vec<String> {
        self.asked.lock().expect("stub lock").clone()
    }
}

impl HelperResolver for StubCars {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_car_exists" => {
                if let Some(Value::String(s)) = args.first() {
                    self.asked.lock().expect("stub lock").push(s.clone());
                }
                Ok(Value::Bool(self.answer))
            }
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

fn closed_sweep(outcome: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "3241aa67-0000-4000-8000-000000000001",
        "closed_on": "2026-08-17T06:12:00Z",
        "kind": "maintenance-sweep",
        "outcome": outcome,
        "title": "Stale build cache sweep",
        "subject_id": "stale-build-caches",
        "parent_step_id": null,
    })
}

#[test]
fn a_sweep_that_said_a_code_change_is_owed_spawns_a_car() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("change-needed"),
        &StubCars::new(false),
    )
    .matched;
    assert_eq!(hits.len(), 1, "change-needed + no open car → spawn");

    let args = &hits[0].invocations[0].args;
    let get = |k: &str| {
        args.iter()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("arg {k} missing"))
    };
    assert_eq!(
        get("metadata.sweep_target"),
        Value::String("stale-build-caches".into()),
        "the car must RECORD the target it was deduped on — the guard \
         reads this key back on the next firing, so a car spawned \
         without it is invisible to the dedup and the defect returns"
    );
    assert_eq!(
        get("metadata.backlog_item"),
        Value::String("3241aa67-0000-4000-8000-000000000001".into()),
        "the originating sweep is still recorded"
    );
}

#[test]
fn a_second_firing_for_the_same_finding_does_not_spawn() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("change-needed"),
        &StubCars::new(true),
    )
    .matched;
    assert!(
        hits.is_empty(),
        "a car for this target is already open — this is the defect: \
         one car per day for a condition that has not changed"
    );
}

#[test]
fn a_sweep_that_found_nothing_never_spawns() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("clear"),
        &StubCars::new(false),
    )
    .matched;
    assert!(hits.is_empty(), "only `change-needed` spawns a car");
}

/// THE DEFECT v3 EXISTS FOR. An operational remediation — 19 of the 21
/// the registry had closed by 2026-09-11 — closes `remediated`, and
/// must spawn NOTHING. The work is already done; a delivery packet for
/// it has no branch to cut and no scope to fill, and every one of them
/// had to be recognised and abandoned by hand.
#[test]
fn an_operational_remediation_does_not_spawn_a_car() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let outcome = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("remediated"),
        &StubCars::new(false),
    );
    assert!(
        outcome.skipped.is_empty(),
        "the predicate must evaluate, not skip: {:?}",
        outcome.skipped
    );
    assert!(
        outcome.matched.is_empty(),
        "a sweep that fixed the thing itself owes no delivery packet — this is the \
         residue backlog 21edde87 measured: unbranched ship-a-change packets stuck \
         at `scope`, each abandoned by a human who recognised it"
    );
}

#[test]
fn some_other_packet_closing_is_not_a_sweep() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let mut payload = closed_sweep("change-needed");
    payload["kind"] = serde_json::json!("ship-a-change");
    let hits = match_event(&reg, "jobs.job.closed", &payload, &StubCars::new(false)).matched;
    assert!(hits.is_empty(), "the rule is scoped to maintenance-sweep");
}

#[test]
fn the_dedup_asks_about_the_target_not_the_id_or_the_title() {
    // The assertion that would have caught e74b32a1. Keying on `id`
    // dedupes nothing (fresh uuid every firing) and keying on `title`
    // cannot tell one finding from another (templated per target).
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let stub = StubCars::new(false);
    // Called for its effect on the stub; the assertion below reads
    // what it asked about. The skipped check keeps the guarantee the
    // old .expect("eval") gave - that the predicates actually
    // evaluated - which is easy to lose now that a failure is a
    // quiet skip rather than an error.
    let outcome = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("change-needed"),
        &stub,
    );
    assert!(
        outcome.skipped.is_empty(),
        "predicates must evaluate: {:?}",
        outcome.skipped
    );
    assert_eq!(
        stub.asked_about(),
        vec!["stale-build-caches".to_string()],
        "dedup must key on the sweep's subject — the one stable \
         identity for a recurring finding"
    );
}
