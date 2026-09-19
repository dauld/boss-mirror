//! The `subjects` identity table — R1 of
//! docs/design/subject-identity-and-relationships.md (approved
//! 2026-07-15).
//!
//! Contract under test:
//! - `record_subject_in_tx` joins the caller's transaction (the Q1
//!   write-through half): commit lands the identity row, rollback
//!   removes it, and an UNREGISTERED kind is rejected by the FK —
//!   the vocabulary gate enforced at the identity layer.
//! - Upserts are idempotent; a later label wins, a NULL label never
//!   erases an earlier one.
//! - `subject_exists` is the uniform existence probe the jobs gate
//!   uses for every kind.

use boss_subject_kinds::subjects::{record_subject_in_tx, subject_exists, upsert_subject};
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn record_in_tx_commit_lands_rollback_does_not() {
    let db = TestDb::new().await;

    let mut tx = db.pool.begin().await.unwrap();
    record_subject_in_tx(&mut tx, "account", "acc-r1-001", Some("R1 Test Account"))
        .await
        .expect("record");
    tx.commit().await.unwrap();
    assert!(
        subject_exists(&db.pool, "account", "acc-r1-001")
            .await
            .unwrap()
    );

    let mut tx = db.pool.begin().await.unwrap();
    record_subject_in_tx(&mut tx, "account", "acc-r1-002", None)
        .await
        .expect("record");
    tx.rollback().await.unwrap();
    assert!(
        !subject_exists(&db.pool, "account", "acc-r1-002")
            .await
            .unwrap(),
        "rollback must remove the identity row"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unregistered_kind_is_rejected() {
    // The subjects FK onto subject_kinds is the vocabulary gate made
    // structural: an identity row cannot exist for a kind the
    // registry never declared (the `workflow`-as-subject audit
    // finding becomes impossible to repeat silently).
    let db = TestDb::new().await;
    let mut tx = db.pool.begin().await.unwrap();
    let err = record_subject_in_tx(&mut tx, "not-a-kind", "x-1", None)
        .await
        .expect_err("unregistered kind must be rejected");
    assert!(
        err.contains("subjects_kind_fkey") || err.contains("foreign key"),
        "error should be the FK rejection: {err}"
    );
    tx.rollback().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn upsert_is_idempotent_and_label_never_regresses() {
    let db = TestDb::new().await;
    upsert_subject(&db.pool, "vendor", "vnd-r1-001", Some("Vendor One"))
        .await
        .unwrap();
    // Re-upsert with no label: keeps the old one.
    upsert_subject(&db.pool, "vendor", "vnd-r1-001", None)
        .await
        .unwrap();
    let label: Option<String> =
        sqlx::query_scalar("SELECT label FROM subjects WHERE kind='vendor' AND id='vnd-r1-001'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(label.as_deref(), Some("Vendor One"));
    // A newer non-null label wins.
    upsert_subject(&db.pool, "vendor", "vnd-r1-001", Some("Vendor One Renamed"))
        .await
        .unwrap();
    let label: Option<String> =
        sqlx::query_scalar("SELECT label FROM subjects WHERE kind='vendor' AND id='vnd-r1-001'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(label.as_deref(), Some("Vendor One Renamed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn exists_is_false_for_unknown_and_kind_scoped() {
    let db = TestDb::new().await;
    upsert_subject(&db.pool, "account", "shared-id", None)
        .await
        .unwrap();
    assert!(
        subject_exists(&db.pool, "account", "shared-id")
            .await
            .unwrap()
    );
    // Identity is (kind, id) — the same id under another kind is a
    // different subject.
    assert!(
        !subject_exists(&db.pool, "vendor", "shared-id")
            .await
            .unwrap()
    );
    assert!(
        !subject_exists(&db.pool, "account", "missing")
            .await
            .unwrap()
    );
}

/// The rebuild's event passes must tolerate the log's real shape:
/// `*.upserted` kinds emit MANY events per subject id (a purchase
/// order upserts on every lifecycle change). A single
/// `INSERT … ON CONFLICT DO UPDATE` statement that hits the same
/// (kind, id) twice aborts with "cannot affect row a second time",
/// rolling back the whole rebuild — the 2026-07-16 playground
/// backfill failure. Dedup inside the statement; the LATEST event
/// (highest audit id) owns the label.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_tolerates_repeated_events_per_id_latest_label_wins() {
    let db = TestDb::new().await;
    for (kind, payload) in [
        (
            "inventory.purchase_order.upserted",
            serde_json::json!({"id": "po-1", "status": "draft"}),
        ),
        (
            "inventory.purchase_order.upserted",
            serde_json::json!({"id": "po-1", "status": "sent"}),
        ),
        (
            "inventory.vendor.created",
            serde_json::json!({"id": "vnd-1", "name": "Vendor v1"}),
        ),
        (
            "inventory.vendor.created",
            serde_json::json!({"id": "vnd-1", "name": "Vendor v2"}),
        ),
    ] {
        sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             VALUES (gen_random_uuid(), '2026-07-01T00:00:00Z'::timestamptz, 'test', $1, $2)",
        )
        .bind(kind)
        .bind(payload)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    boss_subject_kinds::subjects::rebuild_subjects(&db.pool)
        .await
        .expect("rebuild must not abort on repeated events per id");

    assert!(
        subject_exists(&db.pool, "purchase_order", "po-1")
            .await
            .unwrap()
    );
    let label: Option<String> =
        sqlx::query_scalar("SELECT label FROM subjects WHERE kind = 'vendor' AND id = 'vnd-1'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        label.as_deref(),
        Some("Vendor v2"),
        "the latest event's label must win"
    );
}

/// The 2026-07-17 rollover incident: `company` identities are minted
/// at prepare time (a live write-through), NOT event-sourced. An
/// epoch rollover trims `audit_log` to its baseline and reprojects
/// `subjects` from what remains — so a prepare-only company vanishes
/// and every org-level Job then fails the existence gate. The
/// `companies` reference table (schema-seeded, like `locations`) is
/// the durable source the rebuild reads, so the identity survives.
///
/// This mirrors the rollover exactly: an audit_log with NO company
/// events, and rebuild must still reproduce the tenant company.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_company_from_reference_table_after_a_trim() {
    let db = TestDb::new().await;
    // The schema seeds companies (brewery, used-device-shop); the
    // log carries nothing about them — exactly the post-trim state.
    boss_subject_kinds::subjects::rebuild_subjects(&db.pool)
        .await
        .unwrap();
    assert!(
        subject_exists(&db.pool, "company", "brewery")
            .await
            .unwrap(),
        "the tenant company must be reproducible from the reference table alone"
    );
    let label: Option<String> =
        sqlx::query_scalar("SELECT label FROM subjects WHERE kind='company' AND id='brewery'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(label.as_deref(), Some("Algedonic Ales"));
}

/// The jobs.job.created pass must read the subject from the payload's
/// NESTED `subject` object — `{subject: {id, subject_kind}}` — not
/// top-level `subject_kind`/`subject_id` keys that never existed.
/// Reading the wrong path silently matched zero rows since #123, so
/// birth-by-job subjects (workflow, custom) — minted live by
/// create_job_at but not carried by any TOML event source — vanished
/// on every log-only rebuild (an epoch rollover), reddening invariant
/// X on the 25 workflow-design meta-jobs (task #18, 2026-07-18).
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_homes_job_subjects_from_the_nested_payload() {
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), '2026-07-21T00:00:00Z'::timestamptz, 'test', \
                 'jobs.job.created', $1)",
    )
    .bind(serde_json::json!({
        "id": "job-1",
        "kind": "workflow-design",
        "subject": {"id": "ad-hoc", "subject_kind": "workflow"}
    }))
    .execute(&db.pool)
    .await
    .unwrap();

    boss_subject_kinds::subjects::rebuild_subjects(&db.pool)
        .await
        .unwrap();

    assert!(
        subject_exists(&db.pool, "workflow", "ad-hoc")
            .await
            .unwrap(),
        "rebuild must home a job's subject from payload.subject (nested)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_asset_and_message_subjects_from_the_log() {
    // The R2 PR2 identity-source gap: assets and messages were minted
    // by live write-throughs but had no rebuild source, so every
    // epoch rollover's reprojection dropped them (found as 170 assets
    // / 0 asset subjects on the playground). The TOML sources close
    // it: asset identity from registered OR received (the seeded
    // brewhouse fleet lands received-first, no prior registered);
    // message identity from sent.
    let db = TestDb::new().await;
    for (kind, payload) in [
        (
            "asset.registered",
            serde_json::json!({"asset_id": "SYS-REG-1", "kind": {"kind": "Registered"}}),
        ),
        (
            "asset.received",
            serde_json::json!({"asset_id": "SYS-RCV-1", "kind": {"kind": "Received"}}),
        ),
        (
            "messages.message.sent",
            serde_json::json!({"id": "msg-1", "subject": "Hello", "recipient_id": "emp-1"}),
        ),
    ] {
        sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             VALUES (gen_random_uuid(), '2026-07-29T00:00:00Z'::timestamptz, 'test', $1, $2)",
        )
        .bind(kind)
        .bind(payload)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    boss_subject_kinds::subjects::rebuild_subjects(&db.pool)
        .await
        .unwrap();

    for (kind, id) in [
        ("asset", "SYS-REG-1"),
        ("asset", "SYS-RCV-1"),
        ("message", "msg-1"),
    ] {
        assert!(
            subject_exists(&db.pool, kind, id).await.unwrap(),
            "rebuild must reproduce {kind}:{id} from the log"
        );
    }
}

/// THE INSTANCE IS THE TRUTH (design e187198f, David 2026-09-18). The
/// mint door — `POST /api/subjects/{kind}`, the one the company
/// identity comes through at every tenant publish — keeps a held
/// row's label by default and answers what differs; only
/// `?mode=take` overwrites it, naming the change. Measured that day:
/// the door's `COALESCE(EXCLUDED.label, subjects.label)` overwrote the
/// company's label with tenant.toml's display name at every boot.
#[tokio::test(flavor = "multi_thread")]
async fn the_mint_door_keeps_a_held_label_by_default_and_overwrites_only_under_take() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let db = TestDb::new().await;
    let app = boss_subject_kinds::subjects::subjects_router(db.pool.clone());
    let post = |uri: &str, label: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"id": "acme", "label": label}).to_string(),
            ))
            .unwrap()
    };
    let read = |resp: axum::response::Response| async move {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, v)
    };
    let label = || async {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT label FROM subjects WHERE kind='company' AND id='acme'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap()
    };

    // Absent: inserted, 201.
    let resp = app
        .clone()
        .oneshot(post("/api/subjects/company", "Acme, LLC"))
        .await
        .unwrap();
    let (status, v) = read(resp).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["inserted"], 1);
    assert_eq!(label().await.as_deref(), Some("Acme, LLC"));

    // An operator renamed the company in the instance; the file's
    // label differs: kept, named, 200.
    sqlx::query("UPDATE subjects SET label = 'Acme Holdings' WHERE kind='company' AND id='acme'")
        .execute(&db.pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(post("/api/subjects/company", "Acme, LLC"))
        .await
        .unwrap();
    let (status, v) = read(resp).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["inserted"], 0);
    assert_eq!(v["kept"][0]["id"], "acme");
    assert_eq!(v["kept"][0]["differs"], serde_json::json!(["label"]));
    assert_eq!(v["updated"], serde_json::json!([]));
    assert_eq!(
        label().await.as_deref(),
        Some("Acme Holdings"),
        "the instance's label is kept under the default"
    );

    // Identical: nothing kept-differing, nothing updated.
    let resp = app
        .clone()
        .oneshot(post("/api/subjects/company", "Acme Holdings"))
        .await
        .unwrap();
    let (_, v) = read(resp).await;
    assert_eq!(v["kept"], serde_json::json!([]));
    assert_eq!(v["unchanged"], 1);

    // Under take: overwritten, the change named from → to.
    let resp = app
        .clone()
        .oneshot(post("/api/subjects/company?mode=take", "Acme, LLC"))
        .await
        .unwrap();
    let (status, v) = read(resp).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["kept"], serde_json::json!([]));
    assert_eq!(v["updated"][0]["id"], "acme");
    assert_eq!(v["updated"][0]["changes"][0]["field"], "label");
    assert_eq!(v["updated"][0]["changes"][0]["from"], "Acme Holdings");
    assert_eq!(v["updated"][0]["changes"][0]["to"], "Acme, LLC");
    assert_eq!(label().await.as_deref(), Some("Acme, LLC"));
}

/// The list read `boss tenant export` writes `tenant.toml`'s
/// display_name from (design e187198f car 3, backlog e618f3ac).
/// Measured 2026-09-18: the company Subject lives in `subjects` (kind
/// `company`, the label the mint door lands), and the only read was
/// the per-id exists probe — an export could confirm a company it
/// already knew and never learn its label. `GET /api/subjects/{kind}`
/// is every identity row of the kind, sorted by id, with its label.
#[tokio::test(flavor = "multi_thread")]
async fn a_kind_lists_every_identity_row_sorted_with_its_label() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let db = TestDb::new().await;
    upsert_subject(&db.pool, "company", "zeta", Some("Zeta, LLC"))
        .await
        .unwrap();
    upsert_subject(&db.pool, "company", "acme", None)
        .await
        .unwrap();
    upsert_subject(&db.pool, "vendor", "vnd-1", Some("not a company"))
        .await
        .unwrap();

    let rows = boss_subject_kinds::subjects::list_subjects(&db.pool, "company")
        .await
        .unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["acme", "zeta"], "sorted by id, the kind's rows only");
    assert_eq!(rows[0].label, None, "a row minted without a label says so");
    assert_eq!(rows[1].label.as_deref(), Some("Zeta, LLC"));

    let app = boss_subject_kinds::subjects::subjects_router(db.pool.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/subjects/company")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        v,
        serde_json::json!([
            {"kind": "company", "id": "acme", "label": null},
            {"kind": "company", "id": "zeta", "label": "Zeta, LLC"},
        ])
    );
}
