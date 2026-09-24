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
//!
//! THE ROSTER CARRIES THE SETTINGS, NOT JUST THE NAMES (backlog
//! 4a1b307c, 2026-09-19). Until page-audit this file held the profile
//! and a second test held the claim that the profile FIXED the rest —
//! a builder $5/high, an analyst $2/medium, "two settings, not two
//! spellings". That was true while effort was a label. Car e720dd00
//! made the declared effort select the definition the step actually
//! runs under, and page-audit is the first kind to spend that: four
//! steps of one march at three different efforts, argued step by step
//! in `infra/platform/workflows/page-audit.toml`. A test asserting
//! profile ⇒ effort would forbid exactly the control e720dd00 built,
//! so the pairing pin is gone and the roster below holds the whole
//! tuple instead — ONE home for every block's numbers, rather than a
//! rule here and an exception per kind. The profile now says only
//! which rules document the run is briefed with
//! (`infra/platform/documents/<profile>-rules.md`).

use boss_core::job::{JobId, StepId, Subject};
use boss_jobs::agent_spec::KEYS;
use boss_jobs::registry::{WorkflowSpec, materialize_steps, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use std::collections::BTreeSet;

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(platform_bundle_path()).expect("the platform bundle parses")
}

/// Every block in the bundle, whole, one line each:
/// `kind/step profile $budget effort`. A line rather than a tuple so
/// the roster below reads as a table and a drifted row names itself in
/// the diff. The model is checked separately — every block names the
/// one priced model, so repeating it per row would be noise.
fn declared(bundle: &[WorkflowSpec]) -> BTreeSet<String> {
    bundle
        .iter()
        .flat_map(|w| {
            w.steps.iter().filter_map(|s| {
                s.agent.as_ref().map(|a| {
                    format!(
                        "{}/{} {} ${:.2} {}",
                        w.kind,
                        s.title,
                        a.profile,
                        a.budget_usd,
                        a.effort.as_str()
                    )
                })
            })
        })
        .collect()
}

#[test]
fn the_steps_agents_execute_today_declare_their_agent_block() {
    let want: BTreeSet<String> = [
        "backlog-item/build builder $5.00 high",
        "backlog-item/draft-design analyst $2.00 medium",
        "user-feedback/build builder $5.00 high",
        "user-feedback/draft-design analyst $2.00 medium",
        "protocol-retro/collect analyst $2.00 medium",
        "protocol-retro/analyze analyst $2.00 medium",
        "protocol-retro/gaps analyst $2.00 medium",
        "protocol-retro/report analyst $2.00 medium",
        "department-retro/collect analyst $2.00 medium",
        "department-retro/analyze analyst $2.00 medium",
        "department-retro/gaps analyst $2.00 medium",
        "department-retro/report analyst $2.00 medium",
        // v7's judge step (backlog 321f1409, 2026-09-19): a disposition
        // per code-scanning rule off the reading on the packet — the
        // analyst setting, the same as the retros' work steps.
        "publish-to-github/judge-checks analyst $2.00 medium",
        // v8's work steps (backlog d4bfe548, David 2026-09-24): the
        // daily packet sat at `measure` from 00:00Z until the operator
        // worked it after 03:50Z, because no step before the sign-off
        // declared an agent. The same analyst setting as judge-checks;
        // `approve` stays David's and declares none.
        "publish-to-github/measure analyst $2.00 medium",
        "publish-to-github/review analyst $2.00 medium",
        // The page march (backlog 4a1b307c, 2026-09-19). Three efforts
        // across four steps of ONE kind, each argued in the TOML beside
        // the step: `measure` subtracts a department's needs from a
        // component tree it walked, `file` transcribes the numbered
        // list `measure` already produced into packets, `test` writes
        // the mocked spec AND the only thing the founder reads, and
        // `revise` applies edits David has already decided.
        "page-audit/measure analyst $3.00 high",
        "page-audit/file analyst $1.00 low",
        "page-audit/test builder $5.00 high",
        "page-audit/revise builder $3.00 medium",
        // The fold (2026-09-23): a car writing the settled paragraphs
        // into docs/architecture-decisions.md, so the builder setting.
        // Without it `boss dispatch` refused every answered design by
        // name and 34 waited at `fold` with no way to hand them out.
        "design-doc/fold builder $5.00 high",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(declared(&bundle()), want);
}

/// What the profile still fixes, now that it no longer fixes the
/// effort: the rules document the run is briefed with, and so the two
/// names a claim door knows. Plus the one priced model on every block
/// — a step naming a model `agent_rate_card` cannot price would run
/// unpriced — and a budget inside the cap a builder's car carries, so
/// a typo'd `50` is a red test rather than an hour of the agent's
/// budget reserved against one page.
#[test]
fn every_block_names_a_known_profile_a_priced_model_and_a_bounded_budget() {
    for w in bundle() {
        for s in &w.steps {
            let Some(a) = &s.agent else { continue };
            let at = format!("{}/{}", w.kind, s.title);
            assert_eq!(a.model, "opus-5[1m]", "{at}");
            assert!(
                matches!(a.profile.as_str(), "builder" | "analyst"),
                "{at}: unexpected profile {}",
                a.profile
            );
            assert!(
                a.budget_usd > 0.0 && a.budget_usd <= 5.0,
                "{at}: budget {} is outside (0, 5]",
                a.budget_usd
            );
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
                .any(|line| line.starts_with(&format!("{}/{} ", w.kind, spec.title)));
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
    assert_eq!(
        seen,
        declared.len(),
        "every declared block was checked on a packet"
    );
}
