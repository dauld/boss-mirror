//! Every `infra/platform/workflows/maintenance-*.toml` chore's `run`
//! step is its automation's, and says so. One pin file per kind file
//! (see `platform_bundle.rs`) is the rule; this one ranges over a
//! FAMILY because the defect it pins was the same line in every file.
//!
//! WHY (backlog 4f909642, precedent af796788 for the pr-train).
//! Measured 2026-09-18 across every closed `maintenance-*` packet the
//! system of record would list (backup, cluster-converge,
//! forge-converge, estate-observe-units, views-catchup, search-reindex,
//! dev-scratch-reclaim, …): every `run` step carried `assignee_id =
//! claude@algedonic.dev` and `completed_by = automation:boss-step` —
//! or, for the dev pod's reclaim sidecar, `automation:dev-scratch-
//! reclaim`. The bundle declared each `run` as `authority_role =
//! platform-admin` with no assignee, which is exactly the shape the
//! dispatcher's executes-lane nominates
//! (`BOSS_DISPATCH_EXECUTOR_ID=claude@algedonic.dev`, roles
//! `platform-admin`; `task` is not decision-shaped), so every chore —
//! some every five minutes — put a step in the agent alias's MY WORK
//! that the timer, CronJob or sidecar then completed over its head.
//!
//! A bare role with no assignee is not the fix either: the assignment
//! query's role arm lists such a step for EVERY holder of the role, so
//! the same chores would have reached David's queue as claimable work.
//! The honest declaration is the actor that completes the step —
//! `audience = { individual = "<the actor boss-step.sh signs as>" }`
//! (design f5ebd2e1) — so the step materialises born assigned to it,
//! the dispatcher's assignee-already-set guard passes it over, neither
//! arm of the assignment query lists it, and the no-orphan-steps check
//! counts it covered. Authority is unchanged: the jobs API gates a
//! step's completion on policy's `(Update, step)` decision by role,
//! never on the assignee, so `automation:dev-scratch-reclaim` closing
//! a step born `automation:boss-step`'s would still land — it just
//! does not happen, because each chore's script names one actor.

use boss_core::job::{JobId, StepId, Subject};
use boss_jobs::audience::Audience;
use boss_jobs::registry::{WorkflowSpec, materialize_steps, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

/// The actor `infra/boss-step.sh` signs as when nothing overrides
/// `BOSS_STEP_ACTOR` — every systemd timer (boss-gcp, the forge) and
/// every in-cluster CronJob's `run` completion, measured as
/// `completed_by` on the 2026-09-18 closed packets of twenty-one kinds.
const BOSS_STEP: &str = "automation:boss-step";

/// The chores whose script signs as something else. Measured the same
/// way; the override is the `BOSS_STEP_ACTOR=` line in the script named.
const OVERRIDES: [(&str, &str); 1] = [
    // infra/cluster/dev-scratch-reclaim.sh — the sidecar files and
    // completes its own packet, as itself.
    (
        "maintenance-dev-scratch-reclaim",
        "automation:dev-scratch-reclaim",
    ),
];

/// The actor that completes `kind`'s `run` step.
fn completing_actor(kind: &str) -> &'static str {
    OVERRIDES
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, actor)| *actor)
        .unwrap_or(BOSS_STEP)
}

/// Every `maintenance-*` kind in the bundle that has a `run` task step —
/// the chore shape (`maintenance-sweep` is an inspection with no `run`,
/// so it is not one). Derived from the directory, not listed: a new
/// chore is pinned the day it is authored.
fn bundled_chores() -> Vec<WorkflowSpec> {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .filter(|w| w.kind.starts_with("maintenance-"))
        .filter(|w| w.steps.iter().any(|s| s.title == "run" && s.kind == "task"))
        .collect()
}

/// Each chore's `run` step declares the actor that completes it as its
/// audience — one declaration, the `individual` shape — and nothing
/// else about who it is for.
#[test]
fn every_chores_run_step_declares_its_own_automation_as_its_audience() {
    let chores = bundled_chores();
    // Twenty-two on 2026-09-18; a floor keeps this from passing over a
    // bundle the filter emptied.
    boss_testing::assert_roster_floor!(
        chores,
        15,
        "maintenance-* chores with a `run` task step in {}",
        platform_bundle_path()
    );
    for chore in &chores {
        let run = chore
            .steps
            .iter()
            .find(|s| s.title == "run")
            .expect("filtered on a `run` step");
        let actor = completing_actor(&chore.kind);
        assert_eq!(
            run.audience,
            Some(Audience::Individual(actor.into())),
            "{}: `run` is {actor}'s step and declares so as its audience",
            chore.kind
        );
        assert_eq!(
            run.authority_role, None,
            "{}: `run` declares who it is for ONCE — the legacy `authority_role` would put it \
             in every platform-admin holder's role queue",
            chore.kind
        );
    }
}

/// Materialised, each chore's `run` is born its automation's: the
/// dispatcher's assignee-already-set guard passes it over, and neither
/// arm of the assignment query lists it for a person or an agent — no
/// `assignee_id` a login matches, no `authority_role`.
#[test]
fn a_materialised_chore_is_born_its_automations() {
    for chore in bundled_chores() {
        let subject = Subject::new("custom", format!("{}/2026-09-18", chore.kind));
        let steps = materialize_steps(
            &chore,
            &subject,
            JobId::new(),
            &serde_json::Value::Object(Default::default()),
            StepId::new,
        );
        let run = steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some("run"))
            .unwrap_or_else(|| panic!("{}: `run` materialised", chore.kind));
        assert_eq!(
            run.assignee_id.as_deref(),
            Some(completing_actor(&chore.kind)),
            "{}: `run` is born assigned to the actor that completes it",
            chore.kind
        );
        assert!(
            run.metadata.get("authority_role").is_none(),
            "{}: `run` carries no authority_role for the role arm to list",
            chore.kind
        );
        // The chore's markers are untouched: the trigger and both
        // outcomes are nobody's, exactly as before.
        for marker in steps
            .iter()
            .filter(|s| s.spec_slug.as_deref() != Some("run"))
        {
            assert_eq!(
                marker.assignee_id,
                None,
                "{}: `{}` is a marker, nobody's",
                chore.kind,
                marker.spec_slug.as_deref().unwrap_or("?")
            );
        }
    }
}
