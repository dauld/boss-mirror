//! `infra/platform/workflows/agent-run.toml` keeps its decided shape
//! (design c87fb59b, decided 2026-09-18; car 2, backlog 39d0b528). One
//! pin file per kind file (see `platform_bundle.rs`), so a new protocol
//! touches no shared line.
//!
//! WHAT THIS PINS, AND WHY IT EXISTS
//! ---------------------------------
//! An agent's run of a protocol step is a packet: claimed at dispatch,
//! briefed with the exact prompt, building until one of several hands
//! closes it. The three terminals are reached by different hands — a
//! rule reading the gate, or a hand recording work delivered with no
//! car (`landed`), a person recording a refusal (`refused`), a clock
//! rule reading silence (`died`) — and the fork that routes them is
//! one required enum on `building`. If that field stopped being
//! required, a run could close without saying which way it went; if a
//! value lost its branch, a run could wedge open with no terminal
//! reachable. Both are pinned below, along with the one fact that
//! lives twice: the silence bound the clock rule declares is 2x the
//! `duration_hours` this file authors (CLAUDE.md 9a).
//!
//! `delivered` is the ANALYST's ending (backlog a9c6ed5b, 2026-09-19):
//! a run that ships no car has no gate to go green, so `gated` was
//! untrue of it and `died` was both untrue and a failure. Measured on
//! the tree that day: thirteen analyst blocks across six kinds, and
//! the page march alone queues roughly 94 such runs. It routes through
//! the SAME `reported` step as `gated`, because an analyst's handback
//! is the work.

use boss_jobs::registry::{StepSpec, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use serde_json::json;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

fn step<'a>(spec: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    spec.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("agent-run has a `{title}` step"))
}

/// Evaluate a bundled step's `ready_when` against a synthetic packet
/// where `building` has completed carrying `building_md` and `reported`
/// is done or not — the same context shape `registry::build_context`
/// builds at run time.
fn reaches(
    spec: &WorkflowSpec,
    title: &str,
    building_md: serde_json::Value,
    reported_done: bool,
) -> bool {
    reaches_with_report(spec, title, building_md, reported_done, json!({}))
}

/// The same, with the `reported` step's own metadata — the SECOND fork
/// in this protocol (backlog 6a5f4214): a handback the agent gave and
/// an absence the clock recorded route to different terminals.
fn reaches_with_report(
    spec: &WorkflowSpec,
    title: &str,
    building_md: serde_json::Value,
    reported_done: bool,
    reported_md: serde_json::Value,
) -> bool {
    let payload = json!({
        "subject": { "id": "bosspipeline", "subject_kind": "custom" },
        "job": { "metadata": { "packet": "p", "step": "build" } },
        "steps": {
            "claimed": { "done": true, "metadata": {} },
            "briefed": { "done": true, "metadata": { "prompt_bytes": "1" } },
            "building": { "done": true, "metadata": building_md },
            "reported": { "done": reported_done, "metadata": reported_md },
        },
    });
    let expr = boss_expr::parse(&step(spec, title).ready_when).expect("ready_when parses");
    boss_expr::eval(
        &expr,
        &boss_expr::Context {
            payload: &payload,
            helpers: &boss_expr::NoHelpers,
        },
    )
    .expect("ready_when evaluates")
    .as_bool()
    .expect("ready_when is boolean")
}

#[test]
fn the_run_has_the_decided_steps_in_order() {
    let run = bundled("agent-run");
    let titles: Vec<&str> = run.steps.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(
        titles,
        vec![
            "claimed",
            "briefed",
            "building",
            "reported",
            "landed",
            "unreported",
            "refused",
            "died"
        ]
    );
    assert_eq!(step(&run, "claimed").kind, "trigger");
    assert_eq!(step(&run, "briefed").ready_when, "steps.claimed.done");
    assert_eq!(step(&run, "building").ready_when, "steps.briefed.done");
    // The report opens on the green OR on delivered work, and the run
    // lands on the report either way.
    assert_eq!(
        step(&run, "reported").ready_when,
        "steps.building.done AND (steps.building.metadata.result = \"gated\" OR steps.building.metadata.result = \"delivered\")"
    );
    assert_eq!(
        step(&run, "landed").ready_when,
        "steps.reported.done AND steps.reported.metadata.handback != \"absent\""
    );
}

/// The fork is one required enum: a run cannot close without saying
/// which way it went, and every value has exactly one branch.
#[test]
fn building_says_how_it_ended_and_every_value_routes_once() {
    let run = bundled("agent-run");
    let field = step(&run, "building")
        .fields
        .iter()
        .find(|f| f.name == "result")
        .expect("building declares `result`");
    assert!(field.required);
    assert_eq!(field.field_type, "gated|delivered|refused|died");

    let outcomes: Vec<(&str, &str)> = run
        .steps
        .iter()
        .filter_map(|s| {
            s.terminal
                .as_ref()
                .map(|t| (s.title.as_str(), t.outcome.as_str()))
        })
        .collect();
    assert_eq!(
        outcomes,
        vec![
            ("landed", "landed"),
            ("unreported", "unreported"),
            ("refused", "refused"),
            ("died", "died")
        ]
    );

    for value in ["gated", "delivered", "refused", "died"] {
        let md = json!({ "result": value });
        let reached: Vec<&str> = ["reported", "refused", "died"]
            .into_iter()
            .filter(|t| reaches(&run, t, md.clone(), false))
            .collect();
        let want = if value == "gated" || value == "delivered" {
            "reported"
        } else {
            value
        };
        assert_eq!(reached, vec![want], "result = {value}");
    }
}

/// THE SECOND FORK, AND THE SLOT IT RELEASES (backlog 6a5f4214).
/// A run whose work is evidenced but whose agent never came back had
/// NO reachable terminal: `building` is completed, so the silence
/// clock that watches `building` correctly finds nothing, every hour,
/// forever, and the run holds one of the six concurrent slots the
/// claim door enforces. `reported` now forks: a handback the agent
/// gave lands the run, and an absence the clock recorded ends it
/// `unreported` — which is what is true. Not `died`: the work SHIPPED
/// (backlog c8703bee — successful work must not be recorded as a
/// failure), and `landed` would claim a report nobody gave.
#[test]
fn an_absent_handback_ends_the_run_unreported_and_never_landed() {
    let run = bundled("agent-run");
    let gated = json!({ "result": "gated" });
    let absent = json!({ "handback": "absent" });
    // The clock's ending: `unreported`, and NOT `landed`.
    assert!(reaches_with_report(
        &run,
        "unreported",
        gated.clone(),
        true,
        absent.clone()
    ));
    assert!(!reaches_with_report(
        &run,
        "landed",
        gated.clone(),
        true,
        absent.clone()
    ));
    // An analyst's delivered work reaches it the same way.
    assert!(reaches_with_report(
        &run,
        "unreported",
        json!({ "result": "delivered" }),
        true,
        absent
    ));
    // A handback that arrived lands, whether or not it says so, and
    // never reaches `unreported`. The agent's own report writes no
    // `handback` key at all (`boss dispatch --report`), so the absent
    // key must read as a report that came.
    for md in [json!({}), json!({ "handback": "recorded" })] {
        assert!(
            reaches_with_report(&run, "landed", gated.clone(), true, md.clone()),
            "a report on the record lands the run: {md}"
        );
        assert!(
            !reaches_with_report(&run, "unreported", gated.clone(), true, md.clone()),
            "a report on the record is not an absence: {md}"
        );
    }
    // Neither terminal is reachable while the report step is open.
    for t in ["landed", "unreported"] {
        assert!(!reaches_with_report(
            &run,
            t,
            gated.clone(),
            false,
            json!({})
        ));
    }
    assert_eq!(
        step(&run, "unreported").ready_when,
        "steps.reported.done AND steps.reported.metadata.handback = \"absent\""
    );
}

/// THE SECOND FACT THAT LIVES TWICE. The clock rule that ends a run
/// whose handback never arrived declares its bound as a rule arg, and
/// the protocol authors `reported`'s duration here; they are held
/// equal at 2x, exactly as `building` and its silence rule are.
///
/// The duration is measured, not chosen: over the 116 agent-runs that
/// had landed on 2026-09-22, the gap from `building` completing to
/// `reported` completing was a median of 2 minutes and a p90 of 49
/// minutes, with the slowest genuine handback at 1.15h — and then a
/// hole, with the next nine at 6.08h and up, every one of them a run
/// an operator noticed by hand.
#[test]
fn the_unreported_bound_is_twice_the_reports_duration() {
    let run = bundled("agent-run");
    let duration = step(&run, "reported")
        .duration_hours
        .expect("reported authors duration_hours");
    let rule_file = boss_testing::repo_root().join(
        "infra/dispatcher/rules/agent-run-ends-unreported-when-the-handback-never-arrives.toml",
    );
    let rule: toml::Value = toml::from_str(
        &std::fs::read_to_string(&rule_file).expect("the unreported clock rule is authored"),
    )
    .expect("the rule file parses");
    let action = &rule["rule"][0]["do"][0];
    assert_eq!(action["handler"].as_str(), Some("jobs.age_out_step"));
    assert_eq!(action["args"]["kind"].as_str(), Some("\"agent-run\""));
    assert_eq!(action["args"]["step"].as_str(), Some("\"reported\""));
    let hours: f64 = action["args"]["hours"]
        .as_str()
        .expect("hours is an expression string")
        .trim_matches('"')
        .parse()
        .expect("hours is a number");
    assert_eq!(hours, 2.0 * duration, "the bound is 2x the step's duration");
    assert_eq!(
        rule["rule"][0]["schedule"]["cadence"].as_str(),
        Some("hourly"),
        "jobs.age_out_step reads the tick's `_at`, which only a sub-day cadence carries"
    );
    // What it writes has to satisfy the step it completes: `summary` is
    // required at done, and `handback = absent` is the fork the
    // `unreported` terminal reads. The clock invents no account of the
    // work — it records that none arrived, and says so in the summary.
    let done = action["args"]["done_metadata"]
        .as_str()
        .expect("done_metadata is an expression string");
    assert!(done.contains(r#"\"handback\": \"absent\""#), "{done}");
    assert!(done.contains("No handback"), "{done}");
}

/// A green gate alone does not land a run: the report must be on the
/// record too. No rule writes the report, so a landing without one
/// would be a summary nobody gave.
#[test]
fn landed_needs_the_gate_and_the_report() {
    let run = bundled("agent-run");
    assert!(!reaches(
        &run,
        "landed",
        json!({ "result": "gated" }),
        false
    ));
    assert!(reaches(&run, "landed", json!({ "result": "gated" }), true));
    // The analyst's ending is held to the same bar: work delivered
    // without a handback is not a landing either.
    assert!(!reaches(
        &run,
        "landed",
        json!({ "result": "delivered" }),
        false
    ));
    assert!(reaches(
        &run,
        "landed",
        json!({ "result": "delivered" }),
        true
    ));
    // And never on a refusal or a death, whatever the report says.
    assert!(!reaches(
        &run,
        "reported",
        json!({ "result": "died" }),
        false
    ));
    // The report requires a summary, and the machine's evidence keys
    // are not one.
    let summary = step(&run, "reported")
        .fields
        .iter()
        .find(|f| f.name == "summary")
        .expect("reported declares `summary`");
    assert!(summary.required);
}

/// THE FACT THAT LIVES TWICE. The clock rule that ages a silent run
/// out declares its bound as a rule arg (the packet does not carry the
/// step's duration), and the protocol authors the duration here. They
/// are held equal at 2x — the design's own words — because a bound
/// that drifted from the duration would age runs out on a number
/// nobody decided.
#[test]
fn the_silence_bound_is_twice_the_build_steps_duration() {
    let run = bundled("agent-run");
    let duration = step(&run, "building")
        .duration_hours
        .expect("building authors duration_hours");
    let rule_file = boss_testing::repo_root()
        .join("infra/dispatcher/rules/agent-run-dies-when-building-is-silent.toml");
    let rule: toml::Value = toml::from_str(
        &std::fs::read_to_string(&rule_file).expect("the died clock rule is authored"),
    )
    .expect("the rule file parses");
    let action = &rule["rule"][0]["do"][0];
    assert_eq!(action["handler"].as_str(), Some("jobs.age_out_step"));
    assert_eq!(action["args"]["kind"].as_str(), Some("\"agent-run\""));
    assert_eq!(action["args"]["step"].as_str(), Some("\"building\""));
    let hours: f64 = action["args"]["hours"]
        .as_str()
        .expect("hours is an expression string")
        .trim_matches('"')
        .parse()
        .expect("hours is a number");
    assert_eq!(hours, 2.0 * duration, "the bound is 2x the step's duration");
    assert_eq!(
        rule["rule"][0]["schedule"]["cadence"].as_str(),
        Some("hourly"),
        "jobs.age_out_step reads the tick's `_at`, which only a sub-day cadence carries"
    );
}

/// The landing rule names the run's `building` step through the edge
/// `boss gate` writes on the gate-run.
#[test]
fn the_landing_rule_follows_the_gates_agent_run_edge() {
    let rule_file =
        boss_testing::repo_root().join("infra/dispatcher/rules/agent-run-lands-on-gate-green.toml");
    let rule: toml::Value =
        toml::from_str(&std::fs::read_to_string(&rule_file).expect("the landing rule is authored"))
            .expect("the rule file parses");
    let r = &rule["rule"][0];
    assert_eq!(r["on_event"].as_str(), Some("jobs.job.closed"));
    assert_eq!(
        r["when"].as_str(),
        Some("kind = \"gate-run\" AND outcome = \"completed\"")
    );
    let action = &r["do"][0];
    assert_eq!(
        action["handler"].as_str(),
        Some("jobs.complete_linked_step")
    );
    assert_eq!(action["args"]["link"].as_str(), Some("\"agent_run\""));
    assert_eq!(action["args"]["steps"].as_str(), Some("\"building\""));
    // Green is the gate-run's own terminal label for green.
    let gate = bundled("gate-run");
    assert_eq!(
        step(&gate, "green")
            .terminal
            .as_ref()
            .map(|t| t.outcome.as_str()),
        Some("completed")
    );
}

/// THE CAR'S ARRIVAL LANDS THE RUN TOO (car 3, backlog cb78818d — car
/// 2's first loose end). Auto-park copies the gate-run's `agent_run`
/// onto the car; this rule follows it from the car's `merged` terminal
/// and completes the SAME step with the SAME result the gate-green rule
/// writes, so whichever fires second finds it done. Pinned equal to the
/// gate rule on every arg but the evidence key, because two rules that
/// drifted on `result` would route one run to two terminals.
#[test]
fn the_cars_arrival_follows_the_same_agent_run_edge_as_the_gates_green() {
    let rules = boss_testing::repo_root().join("infra/dispatcher/rules");
    let read = |name: &str| -> toml::Value {
        toml::from_str(
            &std::fs::read_to_string(rules.join(name))
                .unwrap_or_else(|e| panic!("{name} is authored: {e}")),
        )
        .expect("the rule file parses")
    };
    let on_car = read("agent-run-lands-on-car-merged.toml");
    let on_gate = read("agent-run-lands-on-gate-green.toml");
    let car = &on_car["rule"][0];
    let gate = &on_gate["rule"][0];
    assert_eq!(car["on_event"].as_str(), Some("jobs.job.closed"));
    assert_eq!(
        car["when"].as_str(),
        Some("kind = \"ship-a-change\" AND outcome = \"merged\"")
    );
    let (car_do, gate_do) = (&car["do"][0], &gate["do"][0]);
    assert_eq!(car_do["handler"], gate_do["handler"]);
    for arg in ["link", "steps", "done_metadata"] {
        assert_eq!(
            car_do["args"][arg], gate_do["args"][arg],
            "the two landing rules must agree on `{arg}`"
        );
    }
    assert_eq!(car_do["args"]["evidence_key"].as_str(), Some("\"car\""));
    assert_eq!(
        gate_do["args"]["evidence_key"].as_str(),
        Some("\"gate_run\"")
    );
    // `merged` is the car's own terminal label for arrival.
    let ship = bundled("ship-a-change");
    assert!(
        ship.steps
            .iter()
            .any(|s| s.terminal.as_ref().is_some_and(|t| t.outcome == "merged")),
        "ship-a-change has a `merged` terminal"
    );
}

/// What `boss dispatch` writes is what the schema requires, and nothing
/// a later hand adds is.
#[test]
fn the_schema_requires_the_dispatch_time_keys_only() {
    let run = bundled("agent-run");
    let required: Vec<&str> = run.metadata_schema["required"]
        .as_array()
        .expect("required is a list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        required,
        vec!["packet", "step", "agent", "model", "budget_usd", "effort"]
    );
    for later in ["worktree", "host", "gate_run", "car", "spend_usd", "tokens"] {
        assert!(
            run.metadata_schema["properties"].get(later).is_some(),
            "{later} is declared"
        );
    }
}
