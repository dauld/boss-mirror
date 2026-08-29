//! The job_edges registry — job-to-job links as data
//! (department-flow-dashboards Q1, decided 2026-08-09: registry).
//!
//! Contracts pinned:
//! 1. **The registry seeds the four real edges** (backlog_item,
//!    train, boarded_jobs, and the every-kind '*' waiting_on) — instruments derive topology from rows,
//!    never from hardcoded key names.
//! 2. **Resolution is prefix-aware**: an exact Job id resolves; an
//!    unambiguous prefix of length >= 8 resolves (the folklore's
//!    dominant shape, measured live); a garbage value does not.
//! 3. **The dial is real in both directions**: shipped default is
//!    'abort' since migration 105 (the folklore was cleaned first);
//!    dialing an edge back to 'warn' permits the dirty write again.
//!    Both directions pinned by flipping one edge.

use boss_testing::TestDb;

async fn seed_job(pool: &sqlx::PgPool, kind: &str) -> uuid::Uuid {
    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on) \
         VALUES ($1, $2, 'custom', 'main', 'T', 'emp-o', 'standard', 'open', CURRENT_DATE)",
    )
    .bind(job_id)
    .bind(kind)
    .execute(pool)
    .await
    .expect("job");
    job_id
}

/// Writes go through a connection with the ref-check hatch RE-ENABLED:
/// TestDb sets `audit_log.ref_check = 'off'` database-wide (its
/// restore hatch, which this trigger honors), so exercising the guard
/// requires turning it back on for the session — otherwise every
/// assertion here passes vacuously against a disabled trigger.
async fn guarded_conn(pool: &sqlx::PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    let mut conn = pool.acquire().await.expect("conn");
    sqlx::query("SET audit_log.ref_check = 'on'")
        .execute(&mut *conn)
        .await
        .expect("re-enable ref check");
    conn
}

async fn set_meta(
    conn: &mut sqlx::PgConnection,
    id: uuid::Uuid,
    metadata: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE jobs SET metadata = $2 WHERE id = $1")
        .bind(id)
        .bind(metadata)
        .execute(conn)
        .await
        .map(|_| ())
}

/// The roster of declared edges, pinned exactly.
///
/// The count is deliberately NOT in the name any more. It read
/// `registry_seeds_the_four_real_edges`, so adding a fifth edge failed
/// a test whose name then also had to be corrected — the name was a
/// second copy of the assertion, and it drifted the moment the
/// assertion did.
#[tokio::test]
async fn registry_seeds_exactly_the_declared_edges() {
    let db = TestDb::new().await;
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT source_kind, field_path, field_kind FROM job_edges ORDER BY source_kind, field_path",
    )
    .fetch_all(&db.pool)
    .await
    .expect("registry rows");
    assert_eq!(
        rows,
        vec![
            // '*' applies to every kind (migration 110, waiting_on).
            ("*".into(), "waiting_on".into(), "job_id".into()),
            // A design doc's revision chain (87f5bc84 Q5).
            (
                "design-doc".into(),
                "translated_from".into(),
                "job_id".into()
            ),
            (
                "pr-train".into(),
                "boarded_jobs".into(),
                "job_id_list".into()
            ),
            (
                "ship-a-change".into(),
                "backlog_item".into(),
                "job_id".into()
            ),
            ("ship-a-change".into(), "train".into(), "job_id".into()),
        ]
    );
}

#[tokio::test]
async fn warn_permits_dirty_links_and_prefixes_resolve() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    // Shipped default is abort (105); this test pins the warn
    // direction of the dial explicitly.
    sqlx::query(
        "UPDATE job_edges SET on_missing = 'warn'          WHERE source_kind = 'ship-a-change' AND field_path = 'backlog_item'",
    )
    .execute(pool)
    .await
    .expect("dial back to warn");

    let target = seed_job(pool, "pr-train").await;
    let src = seed_job(pool, "ship-a-change").await;

    let mut conn = guarded_conn(pool).await;
    // Exact id: clean.
    set_meta(
        &mut conn,
        src,
        serde_json::json!({ "train": target.to_string() }),
    )
    .await
    .expect("exact id resolves");

    // Unambiguous 8-char prefix: the folklore's shape, resolves.
    let prefix = target.to_string()[..8].to_string();
    set_meta(
        &mut conn,
        src,
        serde_json::json!({ "backlog_item": prefix }),
    )
    .await
    .expect("unambiguous >=8 prefix resolves");

    // Garbage under on_missing=warn: warns, still lands.
    set_meta(
        &mut conn,
        src,
        serde_json::json!({ "backlog_item": "not-a-job-anywhere" }),
    )
    .await
    .expect("warn must not break the writer");

    // List field on the train side, mixed clean + prefix.
    let p1 = seed_job(pool, "ship-a-change").await;
    set_meta(
        &mut conn,
        target,
        serde_json::json!({ "boarded_jobs": [p1.to_string(), src.to_string()[..8]] }),
    )
    .await
    .expect("list edges resolve per element");
}

#[tokio::test]
async fn the_abort_dial_refuses_what_warn_permits() {
    let db = TestDb::new().await;
    let pool = &db.pool;
    sqlx::query(
        "UPDATE job_edges SET on_missing = 'abort' \
         WHERE source_kind = 'ship-a-change' AND field_path = 'backlog_item'",
    )
    .execute(pool)
    .await
    .expect("dial to abort");

    let src = seed_job(pool, "ship-a-change").await;
    let mut conn = guarded_conn(pool).await;
    let err = set_meta(
        &mut conn,
        src,
        serde_json::json!({ "backlog_item": "not-a-job-anywhere" }),
    )
    .await
    .expect_err("abort must refuse the unresolvable link");
    let msg = err.to_string();
    assert!(
        msg.contains("unresolvable Job"),
        "refusal names the disease: {msg}"
    );
}

/// The guard's refusal reaches the caller as a 400 CARRYING ITS
/// MESSAGE, not a bare 500 (`8424fb8d` — it took the guard's own
/// builder two attempts to understand the blank 500). Exercised at
/// the classification seam the HTTP handlers share.
#[tokio::test(flavor = "multi_thread")]
async fn the_trigger_text_classifies_as_a_caller_error() {
    let db = TestDb::new().await;
    let mut conn = guarded_conn(&db.pool).await;
    let job_id = uuid::Uuid::new_v4();
    let err = sqlx::query(
        "INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, priority, status, opened_on, metadata) \
         VALUES ($1, 'ship-a-change', 'custom', 'x', 'T', 'emp-o', 'standard', 'open', CURRENT_DATE, \
                 '{\"backlog_item\": \"totally-not-a-job\"}')",
    )
    .bind(job_id)
    .execute(&mut *conn)
    .await
    .expect_err("the guard must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("job edge") && msg.contains("unresolvable"),
        "the classification signature the HTTP mapping keys on must hold: {msg}"
    );
}
