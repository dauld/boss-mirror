//! The platform delivery-policy bundle (`infra/platform/delivery-policy/`)
//! declares exactly the ACTIVE row the migrations produce — every
//! column — and can be the registry's only home.
//!
//! MEASURED 2026-09-18 on origin/main 478231fb (backlog 393d3234,
//! consolidation H4, car 4 of 4 — the last registry). Two migrations
//! were the only place the delivery policy was declared: 202608242117
//! created the table and seeded `train-conductor` v1 as the constants
//! train.rs had compiled in, verbatim; 202609050500 retired v1 and
//! published v2 by copying it with `ci_host_floor_gb` at 40. Three more
//! touched the table without declaring a row — 202609030800 and
//! 202609031000 each added a column with a default (the floor at 90,
//! the gate bound at 3), and 20260918102236 dropped
//! `consist_excluded_lints` (H9). One active row comes out of that, at
//! v2, and `boss-cli`'s `the_seeded_policy_equals_the_compiled_fallback`
//! already held it equal to the conductor's compiled fallback. The
//! conductor reads the LIVE row over `/api/delivery/policy/<name>` and
//! is untouched by this move; what changes is where the row is
//! DECLARED.
//!
//! TWO PINS, in the order the move needs them:
//!
//!   1. The bundle is COMPLETE before it becomes the home: the active
//!      rows a TestDb holds after the migrations equal the rows the
//!      bundle declares, name for name and column for column
//!      (`created_at` excepted — it is when the deployment was built,
//!      not part of the declaration).
//!   2. The bundle can be the ONLY home: with the `delivery_policy`
//!      table emptied, the seed's publish function recreates the
//!      migration row exactly — same version, same columns, active.
//!
//! And the three edges of the decision table: a bundle row that
//! differs from the live active row of the same (name, version) is
//! refused by name and field, a version bump publishes and retires the
//! live row, and a lineage an operator retired stays retired — a boot
//! never re-activates a policy someone switched off (the conductor runs
//! on its compiled fallback and says so).

use boss_core::actor::ActorId;
use boss_jobs::delivery::{DeliveryPolicyRegistry, DeliveryPolicySpec, PgDeliveryPolicy};
use boss_jobs::delivery_policy_seed::{
    DeliveryPolicySeedError, SeedOutcome, platform_delivery_policy_path, seed_delivery_policies,
};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::seed_loader::load_delivery_policies;
use boss_testing::TestDb;
use std::collections::BTreeMap;

const POLICY: &str = "train-conductor";

fn actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

/// A row as a declaration: everything but `created_at`, keyed by name.
fn declarations(rows: &[DeliveryPolicySpec]) -> BTreeMap<String, serde_json::Value> {
    rows.iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).expect("a policy serializes");
            v.as_object_mut()
                .expect("a policy is an object")
                .remove("created_at");
            (s.name().to_string(), v)
        })
        .collect()
}

fn bundle() -> Vec<DeliveryPolicySpec> {
    load_delivery_policies(platform_delivery_policy_path())
        .expect("the platform delivery-policy bundle parses")
}

/// Every ACTIVE row, name-ordered, read the way the seed reads a
/// lineage — one name at a time — so the pin never grows a second
/// SELECT that could drift from the port's.
async fn active_rows(registry: &PgDeliveryPolicy, names: &[String]) -> Vec<DeliveryPolicySpec> {
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
    sqlx::query_scalar("SELECT DISTINCT name FROM delivery_policy ORDER BY name")
        .fetch_all(&db.pool)
        .await
        .expect("names")
}

/// PIN 1 — every active row the migrations produce is declared in the
/// bundle, column for column, and the bundle declares nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_declares_every_active_row_the_migrations_produce() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let live = active_rows(&registry, &every_name(&db).await).await;
    assert!(!live.is_empty(), "the migrations seed at least one policy");

    let from_migrations = declarations(&live);
    let from_bundle = declarations(&bundle());

    let migration_names: Vec<&String> = from_migrations.keys().collect();
    let bundle_names: Vec<&String> = from_bundle.keys().collect();
    assert_eq!(
        migration_names, bundle_names,
        "the bundle's names must be exactly the migrations' active names (a policy \
         the migrations seed and the bundle does not declare has no home once \
         migrations declare schema only)"
    );
    for (name, migrated) in &from_migrations {
        let declared = &from_bundle[name];
        assert_eq!(
            declared, migrated,
            "infra/platform/delivery-policy/{name}.toml must equal the active row the \
             migrations produce, every column (left = bundle, right = migrations)"
        );
    }
}

/// PIN 2 — with the table emptied, the seed alone rebuilds exactly the
/// migration row: same version, same columns, active.
#[tokio::test(flavor = "multi_thread")]
async fn an_emptied_registry_is_rebuilt_from_the_bundle_alone() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let names = every_name(&db).await;
    let expected = declarations(&active_rows(&registry, &names).await);

    sqlx::query("DELETE FROM delivery_policy")
        .execute(&db.pool)
        .await
        .expect("empty the delivery_policy table");
    assert!(
        active_rows(&registry, &names).await.is_empty(),
        "the table is empty before the seed runs"
    );

    let report = seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the seed publishes into an empty registry");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Inserted)),
        expected.len(),
        "every bundle policy is inserted into an empty registry: {report}"
    );

    let after = active_rows(&registry, &names).await;
    assert_eq!(
        declarations(&after),
        expected,
        "the seed must recreate the migration row exactly — the bundle can be the only home"
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
            versions.iter().map(|v| v.version()).collect::<Vec<_>>()
        );
    }
    let policy = after
        .iter()
        .find(|r| r.name() == POLICY)
        .expect("the conductor's policy is active");
    assert_eq!(
        policy.version(),
        2,
        "the policy lands at the version two migrations produced, not at v1"
    );
}

/// On a registry the migrations already filled, the seed inserts
/// nothing and says every row is present — the insert-if-missing
/// posture every boot relies on.
#[tokio::test(flavor = "multi_thread")]
async fn a_present_registry_is_left_untouched() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let names = every_name(&db).await;
    let before = declarations(&active_rows(&registry, &names).await);
    let rows_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_policy")
        .fetch_one(&db.pool)
        .await
        .expect("count rows");

    let report = seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a present registry is not a failure");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Present)),
        bundle().len(),
        "every bundle row is already present: {report}"
    );
    assert_eq!(report.count(|o| matches!(o, SeedOutcome::Inserted)), 0);

    let rows_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_policy")
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
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let names = every_name(&db).await;
    let before = declarations(&active_rows(&registry, &names).await);

    let mut specs = bundle();
    let edited = specs
        .iter_mut()
        .find(|s| s.name() == POLICY)
        .expect("the bundle declares the conductor's policy");
    edited.row.gate_max_concurrent = 4;

    let err = seed_delivery_policies(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect_err("a drifted bundle row is refused");
    match &err {
        DeliveryPolicySeedError::Refused { rows: refusals, .. } => {
            assert_eq!(refusals.len(), 1, "{err}");
            assert_eq!(refusals[0].name, POLICY);
            assert_eq!(refusals[0].version, 2);
            assert_eq!(
                refusals[0].fields,
                vec!["gate_max_concurrent".to_string()],
                "the refusal names every differing column"
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }
    let text = err.to_string();
    assert!(
        text.contains(POLICY)
            && text.contains("version")
            && text.contains("gate_max_concurrent")
            && text.contains("infra/platform/delivery-policy/"),
        "the refusal must name the policy, the edit path, the field and the bundle: {text}"
    );
    assert_eq!(
        declarations(&active_rows(&registry, &names).await),
        before,
        "a refused seed writes nothing"
    );
}

/// A version bump IS the edit path: a bundle row one version ahead of
/// the live active row is published, retiring the live one, and the
/// declared version is the one that lands — retire-by-name, then
/// insert, the order the one-active-per-name partial index demands.
#[tokio::test(flavor = "multi_thread")]
async fn a_version_bump_publishes_and_retires_the_live_row() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let live = registry
        .live_versions(POLICY)
        .await
        .expect("lineage")
        .into_iter()
        .find(|r| r.status == WorkflowStatus::Active)
        .expect("the policy is active");

    let mut specs = bundle();
    let bumped = specs
        .iter_mut()
        .find(|s| s.name() == POLICY)
        .expect("the bundle declares the conductor's policy");
    bumped.row.version = live.version() + 1;
    bumped.row.gate_max_concurrent = 4;

    let report = seed_delivery_policies(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect("a version bump publishes");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Published { .. })),
        1,
        "{report}"
    );

    let lineage = registry.live_versions(POLICY).await.expect("lineage");
    let now_active = lineage
        .iter()
        .find(|r| r.status == WorkflowStatus::Active)
        .expect("the policy is active");
    assert_eq!(now_active.version(), live.version() + 1);
    assert_eq!(now_active.row.gate_max_concurrent, 4);
    let old = lineage
        .iter()
        .find(|r| r.version() == live.version())
        .expect("the prior version is history");
    assert_eq!(old.status, WorkflowStatus::Retired);

    // What the conductor reads is what the bundle now says — and the
    // version a train departed under is still readable, retired.
    let served: Vec<(i32, i32)> = sqlx::query_as(
        "SELECT version, gate_max_concurrent FROM delivery_policy \
         WHERE status = 'active' AND name = $1",
    )
    .bind(POLICY)
    .fetch_all(&db.pool)
    .await
    .expect("the active row");
    assert_eq!(served, vec![(live.version() + 1, 4)]);
}

/// A lineage with NO active row is one an operator retired, and a
/// boot leaves it that way — the seed reports it and writes nothing.
/// Re-activation is an explicit publish (a version bump in the bundle
/// would NOT do it either: the lineage is judged before any version
/// is compared).
#[tokio::test(flavor = "multi_thread")]
async fn a_retired_lineage_stays_retired() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    sqlx::query("UPDATE delivery_policy SET status = 'retired' WHERE name = $1")
        .bind(POLICY)
        .execute(&db.pool)
        .await
        .expect("the operator retires the policy");

    let report = seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a retired lineage is not a failure");
    let policy = report
        .rows
        .iter()
        .find(|r| r.name == POLICY)
        .expect("the report names the policy");
    assert!(
        matches!(policy.outcome, SeedOutcome::Retired { newest: 2 }),
        "the policy is reported retired, not re-inserted: {report}"
    );

    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM delivery_policy WHERE name = $1 AND status = 'active'",
    )
    .bind(POLICY)
    .fetch_one(&db.pool)
    .await
    .expect("count active policy rows");
    assert_eq!(
        active, 0,
        "a boot never re-activates a policy an operator retired"
    );
}
