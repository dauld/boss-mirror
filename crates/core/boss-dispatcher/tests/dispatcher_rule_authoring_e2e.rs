//! Authoring writes for `dispatcher_rules`: the versioning + activation
//! invariants behind the rule-authoring UX (mirrors the step_plugins
//! registry tests). draft → publish (activates, retires prior active) →
//! retire; `validate` rejects un-loadable rules before they persist.

use std::collections::HashMap;

use boss_dispatcher::rules::authoring::{
    create_draft, get_active, list_versions, publish, retire, validate,
};
use boss_dispatcher::rules::registry::{RawDoStep, RawRule};
use boss_testing::TestDb;

fn rule(name: &str, on_event: &str, when: Option<&str>) -> RawRule {
    RawRule {
        name: name.to_string(),
        on_event: Some(on_event.to_string()),
        schedule: None,
        when: when.map(|s| s.to_string()),
        do_steps: vec![RawDoStep {
            handler: "jobs.spawn".into(),
            args: HashMap::new(),
        }],
        delay: None,
        version: 1,
    }
}

async fn status_of(db: &TestDb, name: &str, version: i32) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM dispatcher_rules WHERE name = $1 AND version = $2")
        .bind(name)
        .bind(version)
        .fetch_optional(&db.pool)
        .await
        .unwrap()
}

async fn active_count(db: &TestDb, name: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM dispatcher_rules WHERE name = $1 AND status = 'active'",
    )
    .bind(name)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_publish_retire_lifecycle() {
    let db = TestDb::new().await;

    // create draft v1
    let d1 = create_draft(&db.pool, &rule("r1", "step.done.x", None), None)
        .await
        .unwrap();
    assert_eq!(d1.version, 1);
    assert_eq!(d1.status, "draft");
    assert!(
        get_active(&db.pool, "r1").await.is_err(),
        "draft is not active"
    );

    // publish → v1 active
    let a1 = publish(&db.pool, "r1").await.unwrap();
    assert_eq!((a1.version, a1.status.as_str()), (1, "active"));
    assert_eq!(get_active(&db.pool, "r1").await.unwrap().version, 1);

    // create draft v2 + publish → v2 active, v1 retired, still exactly one active
    create_draft(&db.pool, &rule("r1", "step.done.y", None), None)
        .await
        .unwrap();
    let a2 = publish(&db.pool, "r1").await.unwrap();
    assert_eq!((a2.version, a2.status.as_str()), (2, "active"));
    assert_eq!(status_of(&db, "r1", 1).await.as_deref(), Some("retired"));
    assert_eq!(get_active(&db.pool, "r1").await.unwrap().version, 2);
    assert_eq!(active_count(&db, "r1").await, 1, "one active per name");

    // list_versions: both, oldest first
    let versions: Vec<i32> = list_versions(&db.pool, "r1")
        .await
        .unwrap()
        .iter()
        .map(|v| v.version)
        .collect();
    assert_eq!(versions, vec![1, 2]);

    // retire → no active
    retire(&db.pool, "r1").await.unwrap();
    assert_eq!(status_of(&db, "r1", 2).await.as_deref(), Some("retired"));
    assert!(get_active(&db.pool, "r1").await.is_err());
    assert_eq!(active_count(&db, "r1").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_rejects_unloadable_rule_and_create_draft_persists_nothing() {
    // bad `when` expr
    assert!(validate(&rule("bad", "step.done.x", Some("a AND"))).is_err());
    // bad topic (empty segment)
    assert!(validate(&rule("bad", "step..x", None)).is_err());
    // a clean rule validates
    assert!(validate(&rule("ok", "step.done.x", Some("a > 1"))).is_ok());

    // create_draft rejects an invalid rule and writes no row
    let db = TestDb::new().await;
    assert!(
        create_draft(&db.pool, &rule("bad", "step..x", None), None)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM dispatcher_rules WHERE name = 'bad'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "invalid draft must not persist");
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_without_draft_errors() {
    let db = TestDb::new().await;
    assert!(publish(&db.pool, "nonexistent").await.is_err());
}

/// A DRAFT LANDS AT THE VERSION ITS BODY DECLARES when that is above
/// the registry's (backlog 458971ef). `create_draft` assigned
/// `MAX(version) + 1` and ignored the body's `version` outright, so a
/// tenant file saying `version = 3` over a live v1 would have landed a
/// v2 the file never named — and the next publish would compare the
/// file's 3 against a registry at 2 and publish AGAIN. The seed already
/// honours the file's version for product rules ("insert what is
/// absent, at the version the FILE declares"); the API door now does
/// the same. The SPA's editor sends no version (the default, 1), so
/// its drafts still take `MAX + 1` — `max(declared, MAX + 1)` never
/// walks a version back.
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_lands_at_the_declared_version_when_it_is_ahead_of_the_registry() {
    let db = TestDb::new().await;
    let mut v1 = rule("declared", "step.done.x", None);
    v1.version = 1;
    create_draft(&db.pool, &v1, Some("tenant:acme"))
        .await
        .unwrap();
    publish(&db.pool, "declared").await.unwrap();

    let mut v3 = rule("declared", "step.done.y", None);
    v3.version = 3;
    let d = create_draft(&db.pool, &v3, Some("tenant:acme"))
        .await
        .unwrap();
    assert_eq!(d.version, 3, "the declared version, not MAX + 1");
    assert_eq!(d.source.as_deref(), Some("tenant:acme"));
    let a = publish(&db.pool, "declared").await.unwrap();
    assert_eq!((a.version, a.status.as_str()), (3, "active"));
    assert_eq!(
        status_of(&db, "declared", 1).await.as_deref(),
        Some("retired")
    );

    // The SPA's shape: no version in the body (the default is 1),
    // which is BELOW the registry — the draft takes MAX + 1 as before.
    let unversioned = rule("declared", "step.done.z", None);
    assert_eq!(unversioned.version, 1, "the default the editor sends");
    let d = create_draft(&db.pool, &unversioned, Some("tenant:acme"))
        .await
        .unwrap();
    assert_eq!(d.version, 4, "MAX + 1 when the body declares nothing newer");
}

/// A RULE NAME BELONGS TO WHOEVER PUBLISHED IT FIRST. `dispatcher_rules`
/// is one namespace with one active row per name; a tenant drafting
/// `auto-park-on-gate-green` v2 and publishing it would retire the
/// product's auto-park and put the tenant's reaction in its slot. The
/// door refuses a draft whose `source` differs from the name's existing
/// rows — by name, naming the owner — and writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_name_another_source_owns_is_refused_at_the_draft() {
    let db = TestDb::new().await;
    // The product's row: published with no source, as the authored
    // directory's seed and the SPA's editor both do.
    create_draft(&db.pool, &rule("owned", "step.done.x", None), None)
        .await
        .unwrap();
    publish(&db.pool, "owned").await.unwrap();

    let mut hijack = rule("owned", "step.done.y", None);
    hijack.version = 2;
    let err = create_draft(&db.pool, &hijack, Some("tenant:acme"))
        .await
        .expect_err("a tenant cannot draft over a product-owned name");
    let msg = err.to_string();
    assert!(msg.contains("owned"), "names the rule: {msg}");
    assert!(
        msg.contains("product") && msg.contains("tenant:acme"),
        "names both owners: {msg}"
    );
    assert_eq!(
        list_versions(&db.pool, "owned").await.unwrap().len(),
        1,
        "the refusal wrote no row"
    );
    // And the other direction: the product (no source) cannot take
    // over a tenant's name either.
    create_draft(
        &db.pool,
        &rule("theirs", "step.done.x", None),
        Some("tenant:acme"),
    )
    .await
    .unwrap();
    let err = create_draft(&db.pool, &rule("theirs", "step.done.y", None), None)
        .await
        .expect_err("the product cannot draft over a tenant-owned name");
    assert!(err.to_string().contains("tenant:acme"), "{err}");
}
