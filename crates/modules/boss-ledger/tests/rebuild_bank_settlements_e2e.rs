//! End-to-end tests for `boss-ledger::rebuild_bank_settlements`.
//!
//! Proves `bank_settlements` is a pure projection of `audit_log` — the
//! third table brought under the log-rooted guarantee, after
//! `payroll_runs` (`rebuild_payroll`) and `tax_filings`
//! (`rebuild_tax_filings`).
//!
//! Why this exists: `bank_settlements` was live state written by the
//! create/settle handlers and owned by no rebuilder. To survive the
//! commerce rebuilder's `TRUNCATE invoices`, that rebuilder detached
//! every row (`UPDATE bank_settlements SET invoice_id = NULL`), replayed
//! invoices, then re-attached by the deterministic `inv-step-{step_id}`
//! key. After a demo epoch trim the prior lap's invoices are gone
//! forever, so those rows never re-attached — they accumulated with a
//! NULL `invoice_id` (1,131,311 of them on the playground, ~99.2% of the
//! table) and were never deleted.
//!
//! That was not merely bloat. `row_to_settlement` decodes `invoice_id`
//! into a non-`Option<String>`, so once the sim date passed the point
//! where the previous lap's orphaned pending rows started maturing,
//! every `POST /api/ledger/bank-settlements/sweep` panicked on
//! `UnexpectedNullError` → 500, and AR collections stopped settling for
//! the rest of the lap.
//!
//! Making the table a projection fixes both: a TRUNCATE-then-replay
//! reproduces exactly the rows the log backs, and the detach/re-attach
//! dance (the thing that manufactured the NULLs) is deleted — it was
//! guarding a foreign key that does not exist.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_ledger::http::{LedgerApiState, router};
use boss_ledger::rebuild_bank_settlements;
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::Row;
use tower::ServiceExt;
use uuid::Uuid;

async fn insert_audit_event(db: &TestDb, kind: &str, timestamp: DateTime<Utc>, payload: &Value) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, $2, 'ledger', $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(timestamp)
    .bind(kind)
    .bind(payload)
    .execute(&db.pool)
    .await
    .unwrap();
}

/// A `ledger.payment.received` payload as `create_bank_settlement`
/// emits it. `expected_settle_on` + `bank_provider` ride along so the
/// row is reconstructable without re-deriving the settle window — a
/// caller-supplied `settle_in_days` override would make any
/// re-derivation silently disagree with what actually happened.
fn received_payload(settlement_id: &str, invoice_id: &str) -> Value {
    serde_json::json!({
        "settlement_id": settlement_id,
        "invoice_id": invoice_id,
        "account_id": "acc-1",
        "received_on": "2026-05-06",
        "expected_settle_on": "2026-05-20",
        "amount_cents": 610_000i64,
        "currency": "USD",
        "bank_provider": "chase",
        "payment_method": "ach",
    })
}

fn settled_payload(settlement_id: &str, invoice_id: &str) -> Value {
    serde_json::json!({
        "settlement_id": settlement_id,
        "invoice_id": invoice_id,
        "settled_on": "2026-05-20",
        "amount_cents": 610_000i64,
        "bank_provider": "chase",
        "payment_method": "ach",
    })
}

/// The bug, expressed as a test: a prior-lap row that no event backs —
/// still `pending`, its invoice long gone with the trimmed epoch — must
/// not survive a log-rooted rebuild. While such rows survived, they
/// accumulated every lap and the sweep eventually panicked on them.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_wipes_orphaned_prior_epoch_settlements_and_replays_from_the_log() {
    let db = TestDb::new().await;

    sqlx::query(
        "INSERT INTO bank_settlements \
            (id, invoice_id, received_on, expected_settle_on, amount_cents, \
             bank_provider, payment_method, status) \
         VALUES ('bs-orphan', 'inv-prior-lap', '2026-01-02', '2026-01-03', 500, \
                 'chase', 'ach', 'pending')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    insert_audit_event(
        &db,
        "ledger.payment.received",
        "2026-05-06T09:00:00Z".parse().unwrap(),
        &received_payload("bs-inv-1", "inv-1"),
    )
    .await;

    let report = rebuild_bank_settlements(&db.pool).await.unwrap();
    assert_eq!(report.settlements_written, 1);
    assert_eq!(report.settlements_marked, 0);

    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM bank_settlements ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        ids,
        vec!["bs-inv-1".to_string()],
        "the orphaned prior-lap row is gone — the table is exactly the log's set"
    );

    let row = sqlx::query(
        "SELECT invoice_id, received_on, expected_settle_on, settled_on, amount_cents, \
                bank_provider, payment_method, status \
         FROM bank_settlements WHERE id = 'bs-inv-1'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("invoice_id"), "inv-1");
    assert_eq!(row.get::<String, _>("status"), "pending");
    assert_eq!(row.get::<Option<chrono::NaiveDate>, _>("settled_on"), None);
    assert_eq!(row.get::<String, _>("bank_provider"), "chase");
    assert_eq!(row.get::<i64, _>("amount_cents"), 610_000);
    // Carried, not re-derived: ach's default float is 1 day, so a
    // re-derivation would have produced 2026-05-07.
    assert_eq!(
        row.get::<chrono::NaiveDate, _>("expected_settle_on"),
        "2026-05-20".parse::<chrono::NaiveDate>().unwrap()
    );
}

/// The crash class is closed by construction, not just by cleanup:
/// `invoice_id` is NOT NULL, so the value `row_to_settlement` used to
/// panic decoding can no longer be written at all. The detach dance
/// that produced it is deleted; this constraint is what keeps it gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_settlement_cannot_be_written_without_its_invoice() {
    let db = TestDb::new().await;

    let err = sqlx::query(
        "INSERT INTO bank_settlements \
            (id, invoice_id, received_on, expected_settle_on, amount_cents, \
             bank_provider, payment_method, status) \
         VALUES ('bs-null', NULL, '2026-01-02', '2026-01-03', 500, \
                 'chase', 'ach', 'pending')",
    )
    .execute(&db.pool)
    .await
    .expect_err("a NULL invoice_id must be rejected by the schema");
    assert!(
        err.to_string().contains("invoice_id"),
        "expected a NOT NULL violation on invoice_id, got: {err}"
    );
}

/// The settle flip replays from `ledger.payment.settled`, in log order.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_replays_the_settle_flip() {
    let db = TestDb::new().await;

    insert_audit_event(
        &db,
        "ledger.payment.received",
        "2026-05-06T09:00:00Z".parse().unwrap(),
        &received_payload("bs-inv-2", "inv-2"),
    )
    .await;
    insert_audit_event(
        &db,
        "ledger.payment.settled",
        "2026-05-20T09:00:00Z".parse().unwrap(),
        &settled_payload("bs-inv-2", "inv-2"),
    )
    .await;

    let report = rebuild_bank_settlements(&db.pool).await.unwrap();
    assert_eq!(report.settlements_written, 1);
    assert_eq!(report.settlements_marked, 1);

    let row = sqlx::query("SELECT status, settled_on FROM bank_settlements WHERE id = 'bs-inv-2'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("status"), "settled");
    assert_eq!(
        row.get::<Option<chrono::NaiveDate>, _>("settled_on"),
        Some("2026-05-20".parse().unwrap())
    );
}

/// TRUNCATE-then-replay: a second pass re-derives the identical set.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_bank_settlements_is_idempotent() {
    let db = TestDb::new().await;

    insert_audit_event(
        &db,
        "ledger.payment.received",
        "2026-05-06T09:00:00Z".parse().unwrap(),
        &received_payload("bs-inv-3", "inv-3"),
    )
    .await;
    insert_audit_event(
        &db,
        "ledger.payment.settled",
        "2026-05-20T09:00:00Z".parse().unwrap(),
        &settled_payload("bs-inv-3", "inv-3"),
    )
    .await;

    rebuild_bank_settlements(&db.pool).await.unwrap();
    let report = rebuild_bank_settlements(&db.pool).await.unwrap();
    assert_eq!(report.settlements_written, 1);
    assert_eq!(report.settlements_marked, 1);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bank_settlements")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

/// A settle whose receive fell before a trim baseline is counted and
/// skipped, not fatal — the same leniency `rebuild_facts` applies.
#[tokio::test(flavor = "multi_thread")]
async fn orphan_settle_is_skipped_not_fatal() {
    let db = TestDb::new().await;

    insert_audit_event(
        &db,
        "ledger.payment.settled",
        "2026-05-20T09:00:00Z".parse().unwrap(),
        &settled_payload("bs-ghost", "inv-ghost"),
    )
    .await;

    let report = rebuild_bank_settlements(&db.pool).await.unwrap();
    assert_eq!(report.settlements_written, 0);
    assert_eq!(report.settlements_marked, 0);
    assert_eq!(report.settles_orphaned, 1);
}

// --- the live write path emits what the projection reads -------------------

fn build_router(db: &TestDb) -> Router {
    router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        // The read gate is not this test's subject (tests/the_ledger_read_gate.rs).
        policy: std::sync::Arc::new(boss_policy_client::PermissivePolicyClient),
    })
}

async fn post(app: Router, path: &str, body: Value) -> (StatusCode, String) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn drain(db: &TestDb) {
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
}

/// The whole guarantee: drive the live create + settle endpoints, then
/// rebuild from the log alone and get the same row back. `settle_in_days`
/// is deliberately NOT ach's default (1) — if the rebuild re-derived the
/// settle window instead of reading it off the event, this fails.
#[tokio::test(flavor = "multi_thread")]
async fn live_written_settlement_survives_a_log_rooted_rebuild() {
    let db = TestDb::new().await;

    let (status, body) = post(
        build_router(&db),
        "/api/ledger/bank-settlements",
        serde_json::json!({
            "id": "bs-live-1",
            "invoice_id": "inv-live-1",
            "account_id": "acc-1",
            "amount_cents": 610_000i64,
            "currency": "USD",
            "received_on": "2026-05-06",
            "bank_provider": "chase",
            "payment_method": "ach",
            "settle_in_days": 14,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {body}");

    let (status, body) = post(
        build_router(&db),
        "/api/ledger/bank-settlements/bs-live-1/settle",
        serde_json::json!({"settled_on": "2026-05-20"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settle: {body}");

    let live: (
        String,
        String,
        chrono::NaiveDate,
        Option<chrono::NaiveDate>,
        i64,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT invoice_id, status, expected_settle_on, settled_on, amount_cents, \
                    bank_provider, payment_method \
             FROM bank_settlements WHERE id = 'bs-live-1'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(live.1, "settled");
    assert_eq!(
        live.2,
        "2026-05-20".parse::<chrono::NaiveDate>().unwrap(),
        "the 14-day override, not ach's 1-day default"
    );

    drain(&db).await;
    let report = rebuild_bank_settlements(&db.pool).await.unwrap();
    assert_eq!(report.settlements_written, 1);
    assert_eq!(report.settlements_marked, 1);
    assert_eq!(report.settles_orphaned, 0);

    let rebuilt: (
        String,
        String,
        chrono::NaiveDate,
        Option<chrono::NaiveDate>,
        i64,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT invoice_id, status, expected_settle_on, settled_on, amount_cents, \
                    bank_provider, payment_method \
             FROM bank_settlements WHERE id = 'bs-live-1'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(rebuilt, live, "rebuilt row is identical to the live one");
}

/// The idempotency guard doubles as the event gate: a repeat create
/// resolves to the existing row and must not append a second receive.
#[tokio::test(flavor = "multi_thread")]
async fn repeat_create_emits_exactly_one_receive_event() {
    let db = TestDb::new().await;
    let body = serde_json::json!({
        "id": "bs-live-2",
        "invoice_id": "inv-live-2",
        "account_id": "acc-1",
        "amount_cents": 500_00i64,
        "currency": "USD",
        "received_on": "2026-05-06",
        "bank_provider": "chase",
        "payment_method": "ach",
    });
    for _ in 0..3 {
        let (status, b) = post(
            build_router(&db),
            "/api/ledger/bank-settlements",
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "create: {b}");
    }
    drain(&db).await;

    let events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE kind = 'ledger.payment.received'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(events, 1, "three POSTs, one receive in the log");
}
