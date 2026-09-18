//! The platform cadence bundle (`infra/platform/cadence/`) declares
//! exactly the ACTIVE rows the migrations produce — every column — and
//! can be the registry's only home.
//!
//! MEASURED 2026-09-18 on origin/main 07daca42 (backlog 393d3234,
//! consolidation H4, car 3 of 4). Nine migrations were the only place a
//! cadence rule was declared: 114 created the table and seeded
//! `train-reconcile`, `train-window` and `train-board-on-dock-depth`;
//! 123 / 131 / 147 / 202609031515 / 202609032030 / 202609042110 each
//! re-versioned the boarding rule (depth 8, 12, 4, 4 again as a no-op,
//! 3, then 1 with a 45-minute cooldown); 134 moved the window off the
//! grid to 06:05 / 18:05; 202608282140 added `protocol-retro-daily` on
//! the calendar basis 202608282135 had just taught the table. Four
//! active rows come out of that — and the live system of record served
//! the same four, column for column, over `/api/cadence/rules` the same
//! day. The conductor's loop reads the LIVE table through that door and
//! is untouched by this move; what changes is where a rule is DECLARED.
//!
//! THE BUNDLE DECLARES THREE OF THE FOUR, BY DECISION. The weekly
//! department retro (feat/department-retro-kind-weekly-rule-and-
//! readiness-read, the same day) opens IT's protocol-retro through a
//! dispatcher clock rule, so `protocol-retro-daily` is redundant and
//! the operator retires it by name the moment that car lands. A bundle
//! must not carry a row the operator is retiring — a retired lineage
//! stays retired, and a file here would be the seed arguing with that
//! decision on every boot. The migrations still insert it (they are
//! history and still run on a fresh database); the exclusion is
//! written down in [`RETIRED_BY_DECISION`], so the pin below compares
//! the bundle to the migrations' active rows MINUS that list, and a
//! name that leaves the list without a file appearing here fails the
//! pin by name.
//!
//! TWO PINS, in the order the move needs them:
//!
//!   1. The bundle is COMPLETE before it becomes the home: the active
//!      rows a TestDb holds after the migrations, less the retired-by-
//!      decision names, equal the rows the bundle declares, name for
//!      name and column for column (`created_at` excepted — it is when
//!      the deployment was built, not part of the declaration).
//!   2. The bundle can be the ONLY home: with the `cadence_rules` table
//!      emptied, the seed's publish function recreates the migration
//!      rows exactly — same versions, same columns, active.
//!
//! And the three edges of the decision table this registry leans on:
//! a bundle row that differs from the live active row of the same
//! (name, version) is refused by name and field, a version bump
//! publishes and retires the live row, and a lineage an operator
//! retired stays retired — a boot never re-activates a rule someone
//! switched off.

use boss_core::actor::ActorId;
use boss_jobs::cadence::{CadenceRegistry, CadenceRuleSpec, PgCadence};
use boss_jobs::cadence_seed::{
    CadenceSeedError, SeedOutcome, platform_cadence_path, seed_cadence_rules,
};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::seed_loader::load_cadence_rules;
use boss_testing::TestDb;
use std::collections::BTreeMap;

/// Rules the migrations still seed that the bundle deliberately does
/// NOT declare, each with the decision that retired it. A name here is
/// one the operator retires live; the seed then sees a lineage with no
/// active row and leaves it alone. Remove a name from this list only
/// with a file for it in `infra/platform/cadence/` — the pin refuses
/// the other combination.
const RETIRED_BY_DECISION: &[(&str, &str)] = &[(
    "protocol-retro-daily",
    "2026-09-18: the weekly department retro opens IT's protocol-retro through a \
     dispatcher clock rule (retro.open); the daily cadence row is redundant and is \
     retired by name once that car lands",
)];

fn actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

/// The migrations' active rows the bundle is held equal to: every
/// active row the migrations produce, minus the names retired by
/// decision.
fn expected_from(migrated: Vec<CadenceRuleSpec>) -> Vec<CadenceRuleSpec> {
    migrated
        .into_iter()
        .filter(|r| !RETIRED_BY_DECISION.iter().any(|(n, _)| *n == r.name()))
        .collect()
}

/// A row as a declaration: everything but `created_at`, keyed by name.
fn declarations(rows: &[CadenceRuleSpec]) -> BTreeMap<String, serde_json::Value> {
    rows.iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).expect("a rule serializes");
            v.as_object_mut()
                .expect("a rule is an object")
                .remove("created_at");
            (s.name().to_string(), v)
        })
        .collect()
}

fn bundle() -> Vec<CadenceRuleSpec> {
    load_cadence_rules(platform_cadence_path()).expect("the platform cadence bundle parses")
}

/// Every ACTIVE row, name-ordered, read the way the seed reads a
/// lineage — one name at a time — so the pin never grows a second
/// SELECT that could drift from the port's.
async fn active_rows(registry: &PgCadence, names: &[String]) -> Vec<CadenceRuleSpec> {
    let mut out = Vec::new();
    for name in names {
        out.extend(
            registry
                .live_versions(name)
                .await
                .expect("live_versions")
                .into_iter()
                .filter(|r| r.status == WorkflowStatus::Active),
        );
    }
    out
}

/// The names the migrations left in the table, active or not.
async fn every_name(db: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT DISTINCT name FROM cadence_rules ORDER BY name")
        .fetch_all(&db.pool)
        .await
        .expect("names")
}

/// PIN 1 — every active row the migrations produce is declared in the
/// bundle, column for column, and the bundle declares nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_declares_every_active_row_the_migrations_produce() {
    let db = TestDb::new().await;
    let registry = PgCadence::new(db.pool.clone());
    let live = active_rows(&registry, &every_name(&db).await).await;
    assert!(!live.is_empty(), "the migrations seed at least one rule");
    for (name, why) in RETIRED_BY_DECISION {
        assert!(
            live.iter().any(|r| r.name() == *name),
            "`{name}` is listed as retired by decision but no migration seeds it \
             active — the list has rotted ({why})"
        );
    }

    let from_migrations = declarations(&expected_from(live));
    let from_bundle = declarations(&bundle());

    let migration_names: Vec<&String> = from_migrations.keys().collect();
    let bundle_names: Vec<&String> = from_bundle.keys().collect();
    assert_eq!(
        migration_names, bundle_names,
        "the bundle's names must be exactly the migrations' active names less \
         RETIRED_BY_DECISION (a rule the migrations seed and the bundle does not \
         declare has no home once migrations declare schema only — unless its \
         retirement is written down in that list)"
    );
    for (name, migrated) in &from_migrations {
        let declared = &from_bundle[name];
        assert_eq!(
            declared, migrated,
            "infra/platform/cadence/{name}.toml must equal the active row the \
             migrations produce, every column (left = bundle, right = migrations)"
        );
    }
}

/// PIN 2 — with the table emptied, the seed alone rebuilds exactly the
/// migration rows (less the retired-by-decision ones): same versions,
/// same columns, all active.
#[tokio::test(flavor = "multi_thread")]
async fn an_emptied_registry_is_rebuilt_from_the_bundle_alone() {
    let db = TestDb::new().await;
    let registry = PgCadence::new(db.pool.clone());
    let names = every_name(&db).await;
    let expected = declarations(&expected_from(active_rows(&registry, &names).await));

    sqlx::query("DELETE FROM cadence_rules")
        .execute(&db.pool)
        .await
        .expect("empty the cadence_rules table");
    assert!(
        active_rows(&registry, &names).await.is_empty(),
        "the table is empty before the seed runs"
    );

    let report = seed_cadence_rules(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the seed publishes into an empty registry");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Inserted)),
        expected.len(),
        "every bundle rule is inserted into an empty registry: {report}"
    );

    let after = active_rows(&registry, &names).await;
    assert_eq!(
        declarations(&after),
        expected,
        "the seed must recreate the migration rows exactly — the bundle can be the only home"
    );
    for row in &after {
        let versions = registry
            .live_versions(row.name())
            .await
            .expect("live_versions");
        assert_eq!(
            versions.len(),
            1,
            "{} has exactly the declared version and no synthetic history: {:?}",
            row.name(),
            versions.iter().map(|v| v.version).collect::<Vec<_>>()
        );
    }
    let board = after
        .iter()
        .find(|r| r.name() == "train-board-on-dock-depth")
        .expect("the boarding rule is active");
    assert_eq!(
        board.version, 6,
        "the boarding rule lands at the version six migrations produced, not at v1"
    );
}

/// On a registry the migrations already filled, the seed inserts
/// nothing and says every row is present — the insert-if-missing
/// posture every boot relies on.
#[tokio::test(flavor = "multi_thread")]
async fn a_present_registry_is_left_untouched() {
    let db = TestDb::new().await;
    let registry = PgCadence::new(db.pool.clone());
    let names = every_name(&db).await;
    let before = declarations(&active_rows(&registry, &names).await);
    let rows_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cadence_rules")
        .fetch_one(&db.pool)
        .await
        .expect("count rows");

    let report = seed_cadence_rules(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a present registry is not a failure");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Present)),
        bundle().len(),
        "every bundle row is already present: {report}"
    );
    assert_eq!(report.count(|o| matches!(o, SeedOutcome::Inserted)), 0);

    let rows_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cadence_rules")
        .fetch_one(&db.pool)
        .await
        .expect("count rows");
    assert_eq!(rows_after, rows_before, "a no-op seed writes no row");
    assert_eq!(declarations(&active_rows(&registry, &names).await), before);
}

/// The refusal: a bundle row edited WITHOUT a version bump differs
/// from the live active row of the same (name, version), and the seed
/// refuses it by name and field rather than overwriting or ignoring.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_row_that_differs_from_the_live_row_is_refused_by_field() {
    let db = TestDb::new().await;
    let registry = PgCadence::new(db.pool.clone());
    let names = every_name(&db).await;
    let before = declarations(&active_rows(&registry, &names).await);

    let mut specs = bundle();
    let edited = specs
        .iter_mut()
        .find(|s| s.name() == "train-reconcile")
        .expect("the bundle declares the reconcile rule");
    edited.row.every_minutes = Some(5);

    let err = seed_cadence_rules(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect_err("a drifted bundle row is refused");
    match &err {
        CadenceSeedError::Refused { rows: refusals, .. } => {
            assert_eq!(refusals.len(), 1, "{err}");
            assert_eq!(refusals[0].name, "train-reconcile");
            assert_eq!(refusals[0].version, 1);
            assert_eq!(
                refusals[0].fields,
                vec!["every_minutes".to_string()],
                "the refusal names every differing column"
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }
    let text = err.to_string();
    assert!(
        text.contains("train-reconcile")
            && text.contains("version")
            && text.contains("every_minutes")
            && text.contains("infra/platform/cadence/"),
        "the refusal must name the rule, the edit path, the field and the bundle: {text}"
    );
    assert_eq!(
        declarations(&active_rows(&registry, &names).await),
        before,
        "a refused seed writes nothing"
    );
}

/// A version bump IS the edit path: a bundle row one version ahead of
/// the live active row is published, retiring the live one, and the
/// declared version is the one that lands — the same retire-by-name-
/// then-insert the safe migration idiom (202609032030) established.
#[tokio::test(flavor = "multi_thread")]
async fn a_version_bump_publishes_and_retires_the_live_row() {
    let db = TestDb::new().await;
    let registry = PgCadence::new(db.pool.clone());
    let live = registry
        .live_versions("train-reconcile")
        .await
        .expect("lineage")
        .into_iter()
        .find(|r| r.status == WorkflowStatus::Active)
        .expect("train-reconcile is active");

    let mut specs = bundle();
    let bumped = specs
        .iter_mut()
        .find(|s| s.name() == "train-reconcile")
        .expect("the bundle declares the reconcile rule");
    bumped.version = live.version + 1;
    bumped.row.every_minutes = Some(15);

    let report = seed_cadence_rules(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect("a version bump publishes");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Published { .. })),
        1,
        "{report}"
    );

    let lineage = registry
        .live_versions("train-reconcile")
        .await
        .expect("lineage");
    let now_active = lineage
        .iter()
        .find(|r| r.status == WorkflowStatus::Active)
        .expect("train-reconcile is active");
    assert_eq!(now_active.version, live.version + 1);
    assert_eq!(now_active.row.every_minutes, Some(15));
    let old = lineage
        .iter()
        .find(|r| r.version == live.version)
        .expect("the prior version is history");
    assert_eq!(old.status, WorkflowStatus::Retired);

    // What the conductor reads is what the bundle now says.
    let served: Vec<(String, Option<i32>)> = sqlx::query_as(
        "SELECT name, every_minutes FROM cadence_rules WHERE status = 'active' AND name = $1",
    )
    .bind("train-reconcile")
    .fetch_all(&db.pool)
    .await
    .expect("the active row");
    assert_eq!(served, vec![("train-reconcile".to_string(), Some(15))]);
}

/// A lineage with NO active row is one an operator retired, and a
/// boot leaves it that way — the seed reports it and writes nothing.
/// Re-activation is an explicit publish (a version bump in the bundle
/// would NOT do it either: the lineage is judged before any version
/// is compared).
#[tokio::test(flavor = "multi_thread")]
async fn a_retired_lineage_stays_retired() {
    let db = TestDb::new().await;
    let registry = PgCadence::new(db.pool.clone());
    sqlx::query("UPDATE cadence_rules SET status = 'retired' WHERE name = 'train-window'")
        .execute(&db.pool)
        .await
        .expect("the operator retires the window rule");

    let report = seed_cadence_rules(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a retired lineage is not a failure");
    let window = report
        .rows
        .iter()
        .find(|r| r.name == "train-window")
        .expect("the report names the window rule");
    assert!(
        matches!(window.outcome, SeedOutcome::Retired { newest: 2 }),
        "the window rule is reported retired, not re-inserted: {report}"
    );

    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cadence_rules WHERE name = 'train-window' AND status = 'active'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("count active window rows");
    assert_eq!(
        active, 0,
        "a boot never re-activates a rule an operator retired"
    );
}
