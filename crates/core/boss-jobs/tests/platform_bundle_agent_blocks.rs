//! The steps agents execute today declare HOW (design c87fb59b car 1,
//! backlog 028891cf), and the declaration reaches the packet.
//!
//! Measured 2026-09-18 on the system of record before the bundle was
//! edited: over 155 backlog-items read, `build` was completed 74 times
//! by the car-merged rule on a builder's behalf and 6 times by
//! agent-claude, `draft-design` 4 times by agent-claude; the one live
//! protocol-retro had collect / analyze / gaps / report all completed
//! by agent-claude and `review` by David. Those steps — and their twins
//! on user-feedback and department-retro, which are the same steps by
//! decision — carry the block; every other step in the bundle carries
//! none. The set is pinned here so a step that starts or stops
//! declaring is a deliberate edit to this file, not drift.

use boss_core::job::{JobId, StepId, Subject};
use boss_jobs::agent_spec::{Effort, KEYS};
use boss_jobs::registry::{WorkflowSpec, materialize_steps, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use std::collections::BTreeSet;

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(platform_bundle_path()).expect("the platform bundle parses")
}

/// `(kind, step, profile)` for every step that declares an agent block.
fn declared(bundle: &[WorkflowSpec]) -> BTreeSet<(String, String, String)> {
    bundle
        .iter()
        .flat_map(|w| {
            w.steps.iter().filter_map(|s| {
                s.agent
                    .as_ref()
                    .map(|a| (w.kind.clone(), s.title.clone(), a.profile.clone()))
            })
        })
        .collect()
}

#[test]
fn the_steps_agents_execute_today_declare_their_agent_block() {
    let want: BTreeSet<(String, String, String)> = [
        ("backlog-item", "build", "builder"),
        ("backlog-item", "draft-design", "analyst"),
        ("user-feedback", "build", "builder"),
        ("user-feedback", "draft-design", "analyst"),
        ("protocol-retro", "collect", "analyst"),
        ("protocol-retro", "analyze", "analyst"),
        ("protocol-retro", "gaps", "analyst"),
        ("protocol-retro", "report", "analyst"),
        ("department-retro", "collect", "analyst"),
        ("department-retro", "analyze", "analyst"),
        ("department-retro", "gaps", "analyst"),
        ("department-retro", "report", "analyst"),
        // v7's judge step (backlog 321f1409, 2026-09-19): a disposition
        // per code-scanning rule off the reading on the packet — the
        // analyst setting, the same as the retros' work steps.
        ("publish-to-github", "judge-checks", "analyst"),
    ]
    .into_iter()
    .map(|(k, s, p)| (k.to_string(), s.to_string(), p.to_string()))
    .collect();
    assert_eq!(declared(&bundle()), want);
}

/// The two profiles are two settings, not two spellings: a builder
/// runs high effort under $5, an analyst medium under $2, both on the
/// pod's own priced model. Every block in the bundle is one of the two.
#[test]
fn a_builder_and_an_analyst_are_the_two_settings_in_use() {
    for w in bundle() {
        for s in &w.steps {
            let Some(a) = &s.agent else { continue };
            assert_eq!(a.model, "opus-5[1m]", "{}/{}", w.kind, s.title);
            match a.profile.as_str() {
                "builder" => {
                    assert_eq!((a.budget_usd, a.effort), (5.0, Effort::High), "{}", s.title)
                }
                "analyst" => {
                    assert_eq!(
                        (a.budget_usd, a.effort),
                        (2.0, Effort::Medium),
                        "{}",
                        s.title
                    )
                }
                other => panic!("{}/{}: unexpected profile {other}", w.kind, s.title),
            }
        }
    }
}

/// The declaration reaches the PACKET: a job admitted under each kind
/// carries the four projected keys on exactly the steps that declare,
/// and none of them on the steps that do not — which is what a station
/// predicate and the claim door will read.
#[test]
fn every_declared_block_projects_onto_the_materialised_step() {
    let bundle = bundle();
    let declared = declared(&bundle);
    let mut seen = 0;
    for w in &bundle {
        let subject = Subject::new(w.subject_kinds[0].as_str(), "probe");
        let job_metadata = serde_json::json!({});
        let steps = materialize_steps(w, &subject, JobId::new(), &job_metadata, StepId::new);
        for (spec, step) in w.steps.iter().zip(&steps) {
            let declares = declared
                .iter()
                .any(|(k, s, _)| k == &w.kind && s == &spec.title);
            match (&spec.agent, declares) {
                (Some(a), true) => {
                    seen += 1;
                    assert_eq!(step.metadata["agent_profile"], a.profile, "{}", spec.title);
                    assert_eq!(step.metadata["agent_model"], a.model, "{}", spec.title);
                    assert_eq!(
                        step.metadata["agent_budget_usd"], a.budget_usd,
                        "{}",
                        spec.title
                    );
                    assert_eq!(
                        step.metadata["agent_effort"],
                        a.effort.as_str(),
                        "{}",
                        spec.title
                    );
                }
                (None, false) => {
                    for key in KEYS {
                        assert!(
                            step.metadata.get(key).is_none(),
                            "{}/{} declares no block but carries `{key}`",
                            w.kind,
                            spec.title
                        );
                    }
                }
                _ => unreachable!("declared() is derived from the same specs"),
            }
        }
    }
    assert_eq!(seen, 13, "every declared block was checked on a packet");
}
