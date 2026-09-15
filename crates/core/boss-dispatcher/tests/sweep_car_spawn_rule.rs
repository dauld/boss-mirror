//! The `spawn-car-on-sweep-remediated` rule at v4 — a sweep that said
//! a code change is owed files a BACKLOG ITEM (a claim to triage), not
//! a `ship-a-change` car (a change in flight), and a recurring finding
//! still mints at most one.
//!
//! Pins the exact `when` the rule file ships against the expr engine —
//! read FROM that file, not copied into this one — and the shape of the
//! spawn it produces. Defect e74b32a1: two cars
//! sat on the board a day apart, both titled "Stale build cache
//! sweep", both from the same target, and the only way to tell them
//! apart was to open each one.
//!
//! v3 (backlog 21edde87, measured 2026-09-11) moved the trigger from
//! `outcome = "remediated"` to `outcome = "change-needed"`. Of the 21
//! remediated sweeps the registry had closed by then, NINETEEN were
//! discharged operationally — disk reclaimed, a build cache pruned, an
//! orphan object deleted, a CI image rebuilt — and each minted a
//! `ship-a-change` packet that could never progress, because there was
//! no code to ship. The sweep protocol now says which it is, as a field
//! on the `remediate` step, and routes to a terminal per answer. The
//! bundle side is pinned by
//! `boss-jobs/tests/platform_bundle_maintenance_sweep.rs`.
//!
//! v4 (backlog 655c5917, measured 2026-09-15) changes WHAT is spawned.
//! All 15 packets this rule ever filed were `ship-a-change` cars with
//! no branch, no scope and no builder: 12 were abandoned, and the
//! reasons written on them were TRIAGE dispositions — "satisfied by
//! cb019e2e" (duplicate), "overtaken by delivery" (stale), "the class
//! lives outside the repo" (decline) — for which `ship-a-change` has one
//! word, `abandoned`, and no required reason (five carried none). The
//! three that merged were adopted by a person who read the sweep, chose
//! to build, and cut a branch: triage, done by hand on a car that
//! existed before the branch did. `backlog-item` is the protocol whose
//! open step IS that decision, so the rule files one. The item's
//! subject is the sweep's target — "the area it touches", per the
//! Workflow — and the dedup is the generic `open_job_exists` on
//! `(backlog-item, target)`, so the one-kind `open_car_exists` helper
//! retires with the spawn it guarded.
//!
//! The interesting assertion is still the KEY. Dedup has to key on the
//! sweep's subject, because the other two candidates cannot separate
//! "the same finding again" from "a different finding": `id` is fresh
//! every firing and `title` is templated per target.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

/// The rule as the dispatcher boots it: its file under
/// `infra/dispatcher/rules/`, not a copy. Until 2026-09-14 this was an
/// inline TOML literal of the rule (backlog 94f150f9) — one that declared
/// no `version` while the file said `version = 3`, so a test whose header
/// says "at v3" was passing against v1 text; the `when` and the spawn
/// args happened to match.
const RULE: &str = "spawn-car-on-sweep-remediated";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

/// `open_job_exists` answering a fixed value, recording the
/// `(kind, subject)` it was asked about so a test can assert the dedup
/// KEY, not just the verdict.
struct StubItems {
    answer: bool,
    asked: std::sync::Mutex<Vec<(String, String)>>,
}

impl StubItems {
    fn new(answer: bool) -> Self {
        Self {
            answer,
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn asked_about(&self) -> Vec<(String, String)> {
        self.asked.lock().expect("stub lock").clone()
    }
}

impl HelperResolver for StubItems {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_job_exists" => {
                if let (Some(Value::String(kind)), Some(Value::String(subject))) =
                    (args.first(), args.get(1))
                {
                    self.asked
                        .lock()
                        .expect("stub lock")
                        .push((kind.clone(), subject.clone()));
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

/// THE SHAPE v4 EXISTS FOR. What a sweep files is a claim for an
/// operator to triage, addressed to the area the sweep watches, with
/// the sweep it came from on it — never a car, which is a change in
/// flight and needs a branch this rule cannot cut.
#[test]
fn a_sweep_that_said_a_code_change_is_owed_files_a_backlog_item() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("change-needed"),
        &StubItems::new(false),
    )
    .matched;
    assert_eq!(hits.len(), 1, "change-needed + no open item → spawn");

    let args = &hits[0].invocations[0].args;
    let get = |k: &str| {
        args.iter()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("arg {k} missing"))
    };
    assert_eq!(
        get("kind"),
        Value::String("backlog-item".into()),
        "a claim to triage, not a car in flight: every ship-a-change this \
         rule filed had no branch and no builder, and 12 of 15 died on the \
         dock's approach (655c5917)"
    );
    assert_eq!(
        get("subject_kind"),
        Value::String("custom".into()),
        "backlog-item's only subject kind"
    );
    assert_eq!(
        get("subject"),
        Value::String("stale-build-caches".into()),
        "the item is ABOUT the sweep's target — the area it touches, per \
         the backlog-item Workflow — and that is also the key the guard \
         reads back, so an item filed against anything else is invisible \
         to the dedup and the defect returns"
    );
    assert_eq!(
        get("metadata.for_sweep"),
        Value::String("3241aa67-0000-4000-8000-000000000001".into()),
        "the originating sweep is recorded, under the key every other \
         sweep-spawned child already uses"
    );
    match get("metadata.claim") {
        Value::String(claim) => assert!(
            claim.contains("for_sweep"),
            "the claim must tell triage WHERE the findings are — the close \
             payload carries no step metadata and the arg language has no \
             concatenation, so the sweep's findings cannot ride here \
             verbatim; the claim points at them instead: {claim}"
        ),
        other => panic!("metadata.claim must be a string, got {other:?}"),
    }
    assert!(
        args.iter().all(|(name, _)| name != "metadata.backlog_item"),
        "`backlog_item` is a CAR's edge to the item it delivers; on an \
         item it would point a landed-car obligation at a closed sweep"
    );
}

#[test]
fn a_second_firing_for_the_same_finding_does_not_spawn() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("change-needed"),
        &StubItems::new(true),
    )
    .matched;
    assert!(
        hits.is_empty(),
        "an item for this target is already open — this is the defect: \
         one packet per day for a condition that has not changed"
    );
}

#[test]
fn a_sweep_that_found_nothing_never_spawns() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("clear"),
        &StubItems::new(false),
    )
    .matched;
    assert!(hits.is_empty(), "only `change-needed` files an item");
}

/// THE DEFECT v3 EXISTS FOR. An operational remediation — 19 of the 21
/// the registry had closed by 2026-09-11 — closes `remediated`, and
/// must spawn NOTHING. The work is already done; a delivery packet for
/// it has no branch to cut and no scope to fill, and every one of them
/// had to be recognised and abandoned by hand.
#[test]
fn an_operational_remediation_does_not_spawn_a_car() {
    let reg = rule();
    let outcome = match_event(
        &reg,
        "jobs.job.closed",
        &closed_sweep("remediated"),
        &StubItems::new(false),
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
    let reg = rule();
    let mut payload = closed_sweep("change-needed");
    payload["kind"] = serde_json::json!("ship-a-change");
    let hits = match_event(&reg, "jobs.job.closed", &payload, &StubItems::new(false)).matched;
    assert!(hits.is_empty(), "the rule is scoped to maintenance-sweep");
}

#[test]
fn the_dedup_asks_about_the_target_not_the_id_or_the_title() {
    // The assertion that would have caught e74b32a1. Keying on `id`
    // dedupes nothing (fresh uuid every firing) and keying on `title`
    // cannot tell one finding from another (templated per target).
    // v4: the question is the generic `(kind, subject)` one, and the
    // kind asked about must be the kind SPAWNED — a guard that still
    // asked about ship-a-change cars would never see the items.
    let reg = rule();
    let stub = StubItems::new(false);
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
        vec![("backlog-item".to_string(), "stale-build-caches".to_string())],
        "dedup must key on the spawned kind and the sweep's subject — the \
         one stable identity for a recurring finding"
    );
}
