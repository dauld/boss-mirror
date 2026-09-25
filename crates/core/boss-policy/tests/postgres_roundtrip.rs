//! Postgres adapter round-trip: rule CRUD + audit log integrity.

use std::sync::Mutex;

use boss_policy::port::PolicyRepository;
use boss_policy::postgres::PgPolicy;
use boss_policy::{Action, PolicyError, PolicyRule, Resource, Scope, UserOverride};
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn rule_upsert_and_lookup_round_trip() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());

    let rule = PolicyRule::new("sales-rep", Resource::job(), Action::Read, Scope::Territory);
    policy.upsert_rule(&rule, "test").await.expect("upsert");

    let back = policy
        .rule_for(&rule.id)
        .await
        .expect("rule_for")
        .expect("present");
    assert_eq!(back.role, "sales-rep");
    assert_eq!(back.scope, Scope::Territory);
    assert!(back.active);

    // Upserting again updates scope + leaves id stable.
    let rule2 = PolicyRule::new("sales-rep", Resource::job(), Action::Read, Scope::All);
    policy
        .upsert_rule(&rule2, "test2")
        .await
        .expect("re-upsert");
    let back2 = policy.rule_for(&rule.id).await.unwrap().unwrap();
    assert_eq!(back2.scope, Scope::All);

    // Two audit rows exist (both upserts).
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM policy_rule_audit WHERE target_id = $1")
            .bind(&rule.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count.0, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn deactivate_rule_hides_from_list() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());

    let r = PolicyRule::new("service-tech", Resource::job(), Action::Close, Scope::Self_);
    policy.upsert_rule(&r, "t").await.unwrap();
    assert_eq!(policy.list_rules().await.unwrap().len(), 1);

    policy.deactivate_rule(&r.id, "t").await.unwrap();
    assert_eq!(policy.list_rules().await.unwrap().len(), 0);

    // rule_for still returns the row (with active=false) for admin inspection.
    let back = policy.rule_for(&r.id).await.unwrap().unwrap();
    assert!(!back.active);

    // One upsert + one deactivate in audit.
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM policy_rule_audit WHERE target_id = $1")
            .bind(&r.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count.0, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_override_respects_expiry() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());

    let active = UserOverride {
        id: "ov-active".into(),
        user_id: "emp-1".into(),
        resource: Resource::employee(),
        action: Action::Read,
        scope: Scope::All,
        reason: "covering HR".into(),
        expires_at: None,
    };
    let expired = UserOverride {
        id: "ov-expired".into(),
        user_id: "emp-1".into(),
        resource: Resource::job(),
        action: Action::Close,
        scope: Scope::All,
        reason: "old".into(),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
    };
    policy.upsert_user_override(&active, "t").await.unwrap();
    policy.upsert_user_override(&expired, "t").await.unwrap();

    let list = policy.list_user_overrides("emp-1").await.unwrap();
    assert_eq!(list.len(), 1, "expired override should be filtered out");
    assert_eq!(list[0].id, "ov-active");
}

// ----- backlog b8e75382 (F5): the judgement sees what the write changes ----
//
// The write doors used to read the row through one port call and write
// through another: an expired override on the conflict key was judged a
// Create and then revived by the upsert. The judge now runs inside the
// write's transaction, on the locked row, expired or inactive included —
// and a refusal there writes nothing, audit row included.

async fn audit_rows(db: &TestDb, target: &str) -> i64 {
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM policy_rule_audit WHERE target_id = $1")
            .bind(target)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    count.0
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_write_is_judged_on_its_row_inactive_or_not() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());
    let r = PolicyRule::new("service-tech", Resource::job(), Action::Close, Scope::Self_);
    policy.upsert_rule(&r, "t").await.unwrap();
    policy.deactivate_rule(&r.id, "t").await.unwrap();

    let seen = Mutex::new(None);
    let widen = PolicyRule::new("service-tech", Resource::job(), Action::Close, Scope::All);
    let refuse = |existing: Option<&PolicyRule>| {
        *seen.lock().unwrap() = Some(existing.cloned());
        Err("refused by the test".to_string())
    };
    let err = policy
        .upsert_rule_judged(&widen, "t", &refuse)
        .await
        .expect_err("the judge refused");
    assert!(matches!(err, PolicyError::Refused(_)), "{err:?}");
    let seen = seen.into_inner().unwrap().expect("the judge ran");
    assert_eq!(
        seen.map(|r| (r.active, r.scope)),
        Some((false, Scope::Self_))
    );

    let back = policy.rule_for(&r.id).await.unwrap().unwrap();
    assert!(!back.active, "a refused write changed nothing");
    assert_eq!(back.scope, Scope::Self_);
    assert_eq!(audit_rows(&db, &r.id).await, 2, "and audited nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_override_write_is_judged_on_the_expired_row_it_rewrites() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());
    let lapsed = UserOverride {
        id: "ov-lapsed".into(),
        user_id: "emp-1".into(),
        resource: Resource::job(),
        action: Action::Close,
        scope: Scope::All,
        reason: "last quarter".into(),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
    };
    policy.upsert_user_override(&lapsed, "t").await.unwrap();

    let revive = UserOverride {
        id: "ov-revive".into(),
        expires_at: None,
        reason: "back again".into(),
        ..lapsed.clone()
    };
    let seen = Mutex::new(None);
    let refuse = |existing: Option<&UserOverride>| {
        *seen.lock().unwrap() = Some(existing.map(|o| o.id.clone()));
        Err("refused by the test".to_string())
    };
    let err = policy
        .upsert_user_override_judged(&revive, "t", &refuse)
        .await
        .expect_err("the judge refused");
    assert!(matches!(err, PolicyError::Refused(_)), "{err:?}");
    assert_eq!(
        seen.into_inner().unwrap(),
        Some(Some("ov-lapsed".to_string())),
        "the judge saw the expired row on the conflict key"
    );
    assert_eq!(policy.list_user_overrides("emp-1").await.unwrap(), vec![]);
    assert_eq!(audit_rows(&db, "ov-revive").await, 0);

    // Accepted, the write rewrites that row rather than adding one.
    policy
        .upsert_user_override_judged(&revive, "t", &|_| Ok(()))
        .await
        .unwrap();
    let live = policy.list_user_overrides("emp-1").await.unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, "ov-lapsed");
    assert_eq!(live[0].reason, "back again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retirement_is_judged_on_the_row_it_retires() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());
    let deny = UserOverride {
        id: "ov-deny".into(),
        user_id: "emp-2".into(),
        resource: Resource::ledger(),
        action: Action::Read,
        scope: Scope::None,
        reason: "suspended".into(),
        expires_at: None,
    };
    policy.upsert_user_override(&deny, "t").await.unwrap();

    let seen = Mutex::new(None);
    let refuse = |existing: Option<&UserOverride>| {
        *seen.lock().unwrap() = Some(existing.map(|o| o.scope.clone()));
        Err("refused by the test".to_string())
    };
    let err = policy
        .deactivate_user_override_judged("ov-deny", "t", &refuse)
        .await
        .expect_err("the judge refused");
    assert!(matches!(err, PolicyError::Refused(_)), "{err:?}");
    assert_eq!(seen.into_inner().unwrap(), Some(Some(Scope::None)));
    assert_eq!(
        policy.list_user_overrides("emp-2").await.unwrap(),
        vec![deny],
        "the deny still stands"
    );
    assert_eq!(
        policy.user_override("ov-deny").await.unwrap().map(|o| o.id),
        Some("ov-deny".to_string())
    );
    assert!(matches!(
        policy
            .deactivate_user_override_judged("ov-none", "t", &|_| Ok(()))
            .await,
        Err(PolicyError::NotFound(_))
    ));
}

/// S2 of the hold review of car a8becd52: a new override whose id
/// another (user, resource, action) already owns is refused as a
/// Conflict — the in-memory stand-in used to replace that row, and this
/// adapter answered the primary key's violation as a storage failure.
/// Both rows stand and nothing is audited.
#[tokio::test(flavor = "multi_thread")]
async fn an_override_id_owned_by_another_key_is_a_conflict() {
    let db = TestDb::new().await;
    let policy = PgPolicy::new(db.pool.clone());
    let deny = UserOverride {
        id: "ov-1".into(),
        user_id: "emp-1".into(),
        resource: Resource::ledger(),
        action: Action::Read,
        scope: Scope::None,
        reason: "suspended".into(),
        expires_at: None,
    };
    policy.upsert_user_override(&deny, "t").await.unwrap();

    let squatter = UserOverride {
        user_id: "emp-2".into(),
        scope: Scope::All,
        reason: "same id, another user".into(),
        ..deny.clone()
    };
    let err = policy
        .upsert_user_override(&squatter, "t")
        .await
        .expect_err("the id is taken");
    assert!(matches!(err, PolicyError::Conflict(_)), "{err:?}");
    assert_eq!(
        policy.list_user_overrides("emp-1").await.unwrap(),
        vec![deny]
    );
    assert_eq!(policy.list_user_overrides("emp-2").await.unwrap(), vec![]);
    assert_eq!(audit_rows(&db, "ov-1").await, 1, "only the first write");
}
