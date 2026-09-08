//! Postgres-backed coverage for the delivery-policy registry.
//!
//! What is pinned here is what only a real database can answer: the
//! CHECK constraints and the one-active-per-name partial index actually
//! refuse the rows they claim to refuse, and the seeded policy is
//! readable through the port the conductor reads it through.
//!
//! The VALUES are pinned elsewhere, deliberately. Asserting `2` and `6`
//! and `1200` here would be a third copy of numbers that already live in
//! the migration and in the conductor's compiled fallback, and CLAUDE.md
//! §9a is explicit that a fact living twice gets collapsed rather than
//! re-typed. `boss-cli`'s `delivery_policy::db_tests` compares the
//! seeded row to `DeliveryPolicy::compiled()` directly, so the numbers
//! are written down once on each side and equality is the test.

use boss_jobs::delivery::{DeliveryPolicyRepository, PgDeliveryPolicy};
use boss_testing::TestDb;

const POLICY: &str = "train-conductor";

#[tokio::test(flavor = "multi_thread")]
async fn the_seeded_policy_is_active_and_readable_through_the_port() {
    let db = TestDb::new().await;
    let repo = PgDeliveryPolicy::new(db.pool.clone());
    let policy = repo
        .active_policy(POLICY)
        .await
        .unwrap()
        .expect("the seed migration leaves exactly one active policy");
    assert_eq!(policy.name, POLICY);
    // The seed inserts v1; 202609050500-the-ci-host-floor-is-forty then
    // retires it and inserts its copy as version + 1 with the floor at
    // 40 — so a fresh schema is in force at v2 (a live registry that had
    // already moved to v2 lands at v3: the migration copies whatever was
    // active, it does not hard-code a number). The floor is the point.
    assert_eq!(policy.version, 2);
    assert_eq!(
        policy.ci_host_floor_gb, 40,
        "policy v3's one change (approval d99b198d)"
    );
    assert!(
        policy
            .consist_excluded_lints
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "the consist exclusions are a JSON array of {{script, reason}}: {:?}",
        policy.consist_excluded_lints
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retired_version_stops_being_the_policy_in_force() {
    let db = TestDb::new().await;
    let repo = PgDeliveryPolicy::new(db.pool.clone());
    sqlx::query("UPDATE delivery_policy SET status = 'retired' WHERE name = $1")
        .bind(POLICY)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        repo.active_policy(POLICY).await.unwrap().is_none(),
        "retirement is how a policy version goes out of force; an empty \
         answer sends the conductor to its compiled fallback, which is a \
         safe place to land"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_version_stays_readable_after_it_is_retired() {
    // The case pinning exists for. A policy edit lands while a train is
    // in flight; the version that train departed under is retired the
    // same second. Reconcile must still be able to read it, or the pin
    // on the train's job metadata buys nothing.
    let db = TestDb::new().await;
    let repo = PgDeliveryPolicy::new(db.pool.clone());
    sqlx::query("UPDATE delivery_policy SET status = 'retired' WHERE name = $1")
        .bind(POLICY)
        .execute(&db.pool)
        .await
        .unwrap();
    let pinned = repo.policy_version(POLICY, 1).await.unwrap();
    assert!(
        pinned.is_some(),
        "a retired version is history, and history is exactly what an \
         in-flight train needs to read"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_active_versions_of_one_policy_cannot_coexist() {
    // `delivery_policy_one_active_per_name` is a plain partial unique
    // index, enforced per STATEMENT — the same shape that reddened two
    // trains on 2026-08-15 when a migration inserted before it retired
    // (infra/lint/registry-bump-retires-first.sh).
    let db = TestDb::new().await;
    let inserted = sqlx::query(
        "INSERT INTO delivery_policy (name, version, status, max_red_trains, stall_hours, \
         consist_excluded_lints, consist_budget_secs, consist_output_budget, \
         consist_files_named, skip_reason_file_budget, blip_cause_budget) \
         VALUES ($1, 2, 'active', 2, 6, '[]'::jsonb, 60, 1200, 6, 96, 80)",
    )
    .bind(POLICY)
    .execute(&db.pool)
    .await;
    assert!(
        inserted.is_err(),
        "a second active row must be refused — 'what was the policy when \
         this train departed?' is only answerable while one version is in \
         force at a time"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_budget_of_zero_is_refused_by_the_schema() {
    // The registry is editable data, so the database holds the floor: a
    // zero budget would silently truncate every reason string to nothing
    // and a zero hold would strike a car out on its first red.
    let db = TestDb::new().await;
    let inserted = sqlx::query(
        "INSERT INTO delivery_policy (name, version, status, max_red_trains, stall_hours, \
         consist_excluded_lints, consist_budget_secs, consist_output_budget, \
         consist_files_named, skip_reason_file_budget, blip_cause_budget) \
         VALUES ('draft-policy', 1, 'draft', 0, 6, '[]'::jsonb, 60, 1200, 6, 96, 80)",
    )
    .execute(&db.pool)
    .await;
    assert!(
        inserted.is_err(),
        "max_red_trains = 0 must not be storable: a hold at zero strikes \
         every car out of the queue on its first red train"
    );
}
