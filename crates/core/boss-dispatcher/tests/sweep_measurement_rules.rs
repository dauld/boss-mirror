//! A sweep's measurement is filed the moment the sweep opens, so its
//! inspector reads a number instead of fetching one (backlog 8e8311f5:
//! six inspections a day each began with a hand-fetched measurement;
//! David: "love it").
//!
//! The two forge-measured daily sweeps — disk-headroom and
//! image-freshness — each carry a rule that fires on THEIR OWN
//! `Inspect` checklist becoming ready and spawns an ops-request for
//! the read verb that answers ITS question (`disk-report`,
//! `ci-image-report`) to the forge, linked back by
//! `metadata.for_sweep`. Pinned over the shipped rule directory, not a
//! copy: each rule fires for exactly its sweep's subject, spawns its
//! own verb, and carries the link the inspector follows.
//!
//! Until 2026-09-18 (backlog 18df96c4) image-freshness and a third
//! sweep, stale-build-caches, filed `disk-report` too — a verb that
//! cannot judge image age or a cargo target dir on the dev pod. The
//! first now files `ci-image-report`; the second was retired, its
//! question answered hourly by maintenance-dev-scratch-reclaim on the
//! host where the caches live.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

/// The whole authored directory, read the way the seed reads it.
fn shipped_rules() -> Registry {
    common::authored_registry()
}

/// The `step.ready.checklist` event the jobs API publishes when a
/// sweep's Inspect step becomes ready — the sweep's subject is its
/// target (maintenance-sweep-*-daily spawns with subject = target).
fn inspect_ready(target: &str) -> serde_json::Value {
    serde_json::json!({
        "job_id": format!("sweep-{target}"), "step_id": "s-inspect", "kind": "checklist",
        "subject_kind": "custom", "subject_id": target,
        "assignee_id": null,
        "metadata": { "authority_role": "platform-admin" }
    })
}

fn spawns_of(
    hits: &[boss_dispatcher::rules::registry::MatchedRule],
) -> Vec<(&str, &[(String, Value)])> {
    hits.iter()
        .flat_map(|h| {
            h.invocations
                .iter()
                .filter(|i| i.handler == "jobs.spawn")
                .map(move |i| (h.rule_name.as_str(), i.args.as_slice()))
        })
        .collect()
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    args.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[test]
fn each_forge_measured_sweep_files_its_own_verb_for_itself_when_its_inspect_becomes_ready() {
    let reg = shipped_rules();
    for (target, verb) in [
        ("disk-headroom", "disk-report"),
        ("image-freshness", "ci-image-report"),
    ] {
        let hits = match_event(
            &reg,
            "step.ready.checklist",
            &inspect_ready(target),
            &NoHelpers,
        )
        .matched;
        let spawns = spawns_of(&hits);
        let ours: Vec<_> = spawns
            .iter()
            .filter(|(_, a)| arg(a, "kind") == Some(&Value::String("ops-request".into())))
            .collect();
        assert_eq!(
            ours.len(),
            1,
            "{target}: exactly one ops-request spawn; got {spawns:?}"
        );
        let (_, a) = ours[0];
        assert_eq!(
            arg(a, "subject"),
            Some(&Value::String("forge".into())),
            "{target}"
        );
        let vals: Vec<&Value> = a.iter().map(|(_, v)| v).collect();
        assert!(
            vals.contains(&&Value::String(verb.into())),
            "{target}: the verb `{verb}` rides the spawn: {a:?}"
        );
        assert!(
            vals.contains(&&Value::String(format!("sweep-{target}"))),
            "{target}: the sweep's job id rides as metadata.for_sweep so the inspector can find its number: {a:?}"
        );
    }
}

#[test]
fn a_sweep_without_a_disk_measurement_files_no_disk_report() {
    let reg = shipped_rules();
    for target in [
        "deploy-convergence",
        "empty-decisions",
        "cluster-conformance",
        // Its own verb since 18df96c4: a clean disk said nothing about it.
        "image-freshness",
        // Retired by 18df96c4: nothing measures it any more.
        "stale-build-caches",
    ] {
        let hits = match_event(
            &reg,
            "step.ready.checklist",
            &inspect_ready(target),
            &NoHelpers,
        )
        .matched;
        let disk: Vec<_> = spawns_of(&hits)
            .into_iter()
            .filter(|(_, a)| {
                a.iter()
                    .any(|(_, v)| v == &Value::String("disk-report".into()))
            })
            .collect();
        assert!(disk.is_empty(), "{target} is not a disk question: {disk:?}");
    }
    // Nor does any other checklist becoming ready.
    let hits = match_event(
        &reg,
        "step.ready.checklist",
        &inspect_ready("some-other-packet"),
        &NoHelpers,
    )
    .matched;
    assert!(spawns_of(&hits).iter().all(|(_, a)| {
        !a.iter()
            .any(|(_, v)| v == &Value::String("disk-report".into()))
    }));
}
