//! The no-orphan-steps check, run over the registries a deployment
//! actually ships — design f5ebd2e1 resolution `no-orphan-steps`, car 1
//! (backlog 67a58840): a WARN with the count PINNED.
//!
//! "Every ready step in an open packet is selected by at least one
//! station predicate or carries an assignee ... computable from
//! registry data alone." So this reads registry data alone: the
//! platform Workflow bundle (`infra/platform/workflows/*.toml`, through
//! the same loader the seed uses), the station rows THE SCHEMA SHIPS
//! (`infra/postgres/schema/*.sql` applied to a scratch database and
//! read back through `PgStations::list_active`, never a hand-copied
//! predicate — CLAUDE.md 9a), and the constraint queues the projection
//! derives from the bundle — the same union `GET /api/stations` serves.
//!
//! WARN, NOT REFUSE, in this car. The design's open sub-question was
//! "warn or refuse": a refusal at publish needs the station set to be
//! complete first, and the measurement below says it is not. So the
//! list is PRINTED (an operator reading the test output sees every
//! orphan by protocol/step/kind) and the COUNT is pinned: a car that
//! adds an orphan reds this test and must say so; a car that covers
//! one moves the number DOWN here, which is the ratchet toward the
//! refusal. The next car flips `assert_eq!` to `assert!(is_empty())`
//! at the publish gate once every row below is covered.
//!
//! Measured 2026-09-15 against origin/main 82be5b64 (48 platform kinds,
//! 268 steps, 117 of them a person's work, 14 stations authored +
//! derived): the count below. Notably
//! `approval/decide` — an `answer-question` decision with no authority,
//! no station and no assignee — is exactly the class the design's own
//! instance belonged to; and several `task` steps here are in practice
//! completed by the ops runner (`ops_verb` in their defaults) or the
//! gate runner, which is a declaration the registry cannot yet make
//! (an `individual` audience naming the automation is the shape for
//! it, once those actors are registered).

use boss_jobs::orphan_steps::orphan_steps;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::station_projection::derived_stations;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{PgStations, StationRegistry};
use boss_testing::TestDb;

/// The orphan count on the day this landed. Move it DOWN when a car
/// covers a step; a car that makes it go UP has added a person's step
/// that no queue holds and nobody is named for, and must say why.
const ORPHANS_PINNED: usize = 28;

#[tokio::test(flavor = "multi_thread")]
async fn every_persons_step_no_station_holds_is_named_and_the_count_is_pinned() {
    let db = TestDb::new().await;
    let authored = PgStations::new(db.pool.clone())
        .list_active()
        .await
        .expect("the schema ships the station rows");
    assert!(
        !authored.is_empty(),
        "no authored station rows: the schema did not apply, and an empty set proves nothing"
    );
    let workflows = seedable_platform_workflows();
    assert!(!workflows.is_empty(), "an empty bundle proves nothing");

    let names: Vec<String> = authored.iter().map(|s| s.name.clone()).collect();
    let mut stations = authored;
    stations.extend(derived_stations(&workflows, &names, chrono::Utc::now()));

    let registry = StepRegistry::v1();
    let orphans = orphan_steps(&workflows, &stations, &registry);

    // WARN: the list, every time, so the output names each one.
    println!(
        "no-orphan-steps: {} of the platform bundle's steps are a person's work that no \
         station holds and nobody is named for ({} kinds, {} stations authored+derived):",
        orphans.len(),
        workflows.len(),
        stations.len()
    );
    for o in &orphans {
        println!("  orphan: {o}");
    }

    let listed: Vec<String> = orphans.iter().map(ToString::to_string).collect();
    assert_eq!(
        orphans.len(),
        ORPHANS_PINNED,
        "the platform bundle's orphan count moved (pinned {ORPHANS_PINNED}, now {}). DOWN: a \
         step is now covered — move the pin. UP: a new step a person must do that no station \
         holds — declare its `audience` (design f5ebd2e1) or author the station. The list:\n  {}",
        orphans.len(),
        listed.join("\n  ")
    );
}

/// The design's own instance is covered, and the check can tell: the
/// backlog-item `triage` and `design-review` steps declare a role
/// audience, project the `q.platform-admin.<kind>` queue, and are held
/// by it. And the derivation is what makes them held — strip the
/// projected keys and the same two steps are orphans.
#[tokio::test(flavor = "multi_thread")]
async fn the_backlog_items_decision_steps_declare_their_audience_and_are_held() {
    let workflows = seedable_platform_workflows();
    let item = workflows
        .iter()
        .find(|w| w.kind == "backlog-item")
        .expect("the bundle ships backlog-item");
    for slug in ["triage", "design-review"] {
        let step = item
            .steps
            .iter()
            .find(|s| s.title == slug)
            .unwrap_or_else(|| panic!("backlog-item has a `{slug}` step"));
        assert_eq!(
            step.audience,
            Some(boss_jobs::audience::Audience::Role("platform-admin".into())),
            "`{slug}` declares its audience once"
        );
        assert_eq!(
            step.selectors().authority_role.as_deref(),
            Some("platform-admin"),
            "`{slug}` derives the role arm from it"
        );
    }

    let registry = StepRegistry::v1();
    let derived = derived_stations(std::slice::from_ref(item), &[], chrono::Utc::now());
    let held = orphan_steps(std::slice::from_ref(item), &derived, &registry);
    assert!(
        held.iter()
            .all(|o| o.step != "triage" && o.step != "design-review"),
        "{held:?}"
    );

    // Contrast: the same protocol with the declarations removed.
    let mut stripped = item.clone();
    for step in stripped.steps.iter_mut() {
        if step.title == "triage" || step.title == "design-review" {
            step.audience = None;
            step.authority_role = None;
        }
    }
    let derived = derived_stations(std::slice::from_ref(&stripped), &[], chrono::Utc::now());
    let orphans = orphan_steps(std::slice::from_ref(&stripped), &derived, &registry);
    let slugs: Vec<&str> = orphans.iter().map(|o| o.step.as_str()).collect();
    assert!(
        slugs.contains(&"triage") && slugs.contains(&"design-review"),
        "{slugs:?}"
    );
}
