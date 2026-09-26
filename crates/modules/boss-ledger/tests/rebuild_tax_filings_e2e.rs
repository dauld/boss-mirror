//! End-to-end tests for `boss-ledger::rebuild_tax_filings`.
//!
//! Proves `tax_filings` is a pure projection of `audit_log` — the same
//! property `rebuild_payroll` established for `payroll_runs`.
//!
//! Why this exists: `tax_filings` was written directly by the
//! create/remit handlers and owned by no rebuilder, so it survived the
//! demo epoch reset (which trims `audit_log` then runs
//! `boss-rebuild-all`) carrying prior-lap rows. Because
//! `remit_tax_filing` short-circuits on an already-`paid` filing and
//! the `(kind, jurisdiction, period)` unique index collides across
//! laps, the new lap's remittance silently never posted:
//! `finance.tax.remitted` was never emitted and 2150/2300/2310/2320
//! accrued forever while cash stayed overstated. Rebuilding the table
//! from the log puts it back under the audit-log-rooted guarantee.
//!
//! Unlike payroll, the projection is rooted in `audit_log` rather than
//! `financial_facts`: a filing for a non-accruing kind (sales,
//! payroll_941/940, excise — the liability was already credited by its
//! source facts) posts no journal entry, so it has no fact to rebuild
//! from. `ledger.tax.filing.created` carries the row shape instead.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_ledger::http::{LedgerApiState, router};
use boss_ledger::rebuild_tax_filings;
use boss_testing::{RecordingEventBus, TestDb};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::Row;
use tower::ServiceExt;
use uuid::Uuid;

async fn insert_audit_event(
    db: &TestDb,
    kind: &str,
    timestamp: DateTime<Utc>,
    payload: &Value,
) -> Uuid {
    let event_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, $2, 'ledger', $3, $4)",
    )
    .bind(event_id)
    .bind(timestamp)
    .bind(kind)
    .bind(payload)
    .execute(&db.pool)
    .await
    .unwrap();
    event_id
}

/// A `ledger.tax.filing.created` payload exactly as `create_tax_filing`
/// emits it — every column the row needs to be reconstructed.
fn created_payload(filing_id: &str, kind: &str, liability_account: &str) -> Value {
    serde_json::json!({
        "filing_id": filing_id,
        "kind": kind,
        "jurisdiction": "US-FEDERAL",
        "period_start": "2026-04-01",
        "period_end": "2026-06-30",
        "due_on": "2026-07-20",
        "amount_cents": 250_000i64,
        "liability_account": liability_account,
        "provider": "self",
    })
}

/// A `ledger.tax.remitted` payload as `remit_tax_filing` emits it.
fn remitted_payload(filing_id: &str, kind: &str, liability_account: &str) -> Value {
    serde_json::json!({
        "filing_id": filing_id,
        "kind": kind,
        "jurisdiction": "US-FEDERAL",
        "filed_on": "2026-07-15",
        "liability_account": liability_account,
        "amount_cents": 250_000i64,
        "period_start": "2026-04-01",
        "period_end": "2026-06-30",
    })
}

/// The bug, expressed as a test: a prior-lap `paid` filing with no
/// backing event must not survive a log-rooted rebuild. If it does, the
/// new lap's remit short-circuits and the liability never drains.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_wipes_stale_prior_epoch_filings_and_replays_from_the_log() {
    let db = TestDb::new().await;

    // A stale directly-written filing from a "prior epoch": already
    // `paid`, filed_on in the future relative to the new lap, and
    // occupying the (kind, jurisdiction, period) unique key the new
    // lap's filing needs. No backing audit event.
    sqlx::query(
        "INSERT INTO tax_filings \
            (id, kind, jurisdiction, period_start, period_end, due_on, filed_on, \
             amount_cents, liability_account, status, provider) \
         VALUES ('tax-stale','excise','US-FEDERAL','2026-04-01','2026-06-30','2026-07-20', \
                 '2026-07-15', 99, '2320', 'paid', 'self')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // The current lap's filing, created but not yet remitted.
    insert_audit_event(
        &db,
        "ledger.tax.filing.created",
        "2026-07-01T09:00:00Z".parse().unwrap(),
        &created_payload("tax-excise-2026Q2", "excise", "2320"),
    )
    .await;

    let report = rebuild_tax_filings(&db.pool).await.unwrap();
    assert_eq!(report.filings_written, 1, "one filing rebuilt from the log");
    assert_eq!(report.remittances_applied, 0);

    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM tax_filings ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        ids,
        vec!["tax-excise-2026Q2".to_string()],
        "the stale prior-epoch filing is gone — tax_filings is exactly the log's set"
    );

    // Rebuilt as `accrued` with no filed_on: the new lap has not
    // remitted yet, so remit must NOT short-circuit.
    let row = sqlx::query(
        "SELECT status, filed_on, amount_cents, liability_account, jurisdiction, provider \
         FROM tax_filings WHERE id = 'tax-excise-2026Q2'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "accrued");
    assert_eq!(row.get::<Option<chrono::NaiveDate>, _>("filed_on"), None);
    assert_eq!(row.get::<i64, _>("amount_cents"), 250_000);
    assert_eq!(row.get::<String, _>("liability_account"), "2320");
    assert_eq!(row.get::<String, _>("jurisdiction"), "US-FEDERAL");
    assert_eq!(row.get::<String, _>("provider"), "self");
}

/// A remitted filing replays to `paid` + its `filed_on` — the status
/// flip is carried by `ledger.tax.remitted`, applied in log order.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_replays_the_remittance_status_flip() {
    let db = TestDb::new().await;

    insert_audit_event(
        &db,
        "ledger.tax.filing.created",
        "2026-07-01T09:00:00Z".parse().unwrap(),
        &created_payload("tax-sales-2026Q2", "sales", "2300"),
    )
    .await;
    insert_audit_event(
        &db,
        "ledger.tax.remitted",
        "2026-07-15T09:00:00Z".parse().unwrap(),
        &remitted_payload("tax-sales-2026Q2", "sales", "2300"),
    )
    .await;

    let report = rebuild_tax_filings(&db.pool).await.unwrap();
    assert_eq!(report.filings_written, 1);
    assert_eq!(report.remittances_applied, 1);

    let row = sqlx::query("SELECT status, filed_on FROM tax_filings WHERE id = 'tax-sales-2026Q2'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("status"), "paid");
    assert_eq!(
        row.get::<Option<chrono::NaiveDate>, _>("filed_on"),
        Some("2026-07-15".parse().unwrap())
    );
}

/// TRUNCATE-then-replay: a second pass over the same log re-derives the
/// identical set, no duplication.
#[tokio::test(flavor = "multi_thread")]
async fn rebuild_tax_filings_is_idempotent() {
    let db = TestDb::new().await;

    insert_audit_event(
        &db,
        "ledger.tax.filing.created",
        "2026-07-01T09:00:00Z".parse().unwrap(),
        &created_payload("tax-941-2026Q2", "payroll_941", "2150"),
    )
    .await;
    insert_audit_event(
        &db,
        "ledger.tax.remitted",
        "2026-07-15T09:00:00Z".parse().unwrap(),
        &remitted_payload("tax-941-2026Q2", "payroll_941", "2150"),
    )
    .await;

    rebuild_tax_filings(&db.pool).await.unwrap();
    let report = rebuild_tax_filings(&db.pool).await.unwrap();
    assert_eq!(report.filings_written, 1);
    assert_eq!(report.remittances_applied, 1);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tax_filings")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

// --- the live write path emits what the projection reads -------------------

/// No publisher: the handlers record on the transactional outbox and
/// the drain below moves it to `audit_log`, so these tests only pass
/// through the real outbox → relay → audit_log pipe.
fn build_router(db: &TestDb) -> Router {
    router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        // The read gate is not this test's subject (tests/the_ledger_read_gate.rs).
        policy: std::sync::Arc::new(boss_policy_client::PermissivePolicyClient),
    })
}

/// Returns the body alongside the status so a failed assertion shows
/// the server's reason instead of a bare `400 != 200`.
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

/// Remitting drains the liability to 1000 Cash, and the posting rules
/// refuse to overdraw it — so the books need opening capital before a
/// remit can post. Seeded as a manual entry, the same way the http_api
/// suite does.
async fn seed_opening_cash(db: &TestDb, cents: i64) {
    let id = Uuid::new_v4();
    let happened_on = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let payload = serde_json::json!({
        "lines": [
            {"account_code": "1000", "debit_cents": cents, "memo": "opening cash"},
            {"account_code": "3000", "credit_cents": cents, "memo": "opening capital"},
        ]
    });
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts \
            (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, 'finance.manual.entry', $2, $3, 'invoices', 'opening-cash', 'test')",
    )
    .bind(id)
    .bind(happened_on)
    .bind(&payload)
    .execute(&mut *tx)
    .await
    .unwrap();
    boss_ledger::post_fact_in_tx(
        &mut tx,
        &boss_ledger::FactRef {
            id,
            kind: "finance.manual.entry",
            happened_on,
            payload: &payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn drain(db: &TestDb) {
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
}

fn excise_filing_body() -> Value {
    serde_json::json!({
        "id": "tax-excise-US-FEDERAL-2026-04-01-2026-06-30",
        "kind": "excise",
        "jurisdiction": "US-FEDERAL",
        "period_start": "2026-04-01",
        "period_end": "2026-06-30",
        "due_on": "2026-07-20",
        "amount_cents": 250_000i64,
        "provider": "self",
    })
}

/// The whole guarantee in one test: drive the live create + remit
/// endpoints, then rebuild from the log alone and get the same row
/// back. `excise` is deliberate — it is one of the four non-accruing
/// kinds that post no journal entry and therefore have no
/// `financial_facts` row to rebuild from, so this is exactly the case
/// `ledger.tax.filing.created` exists to cover.
#[tokio::test(flavor = "multi_thread")]
async fn live_written_filing_survives_a_log_rooted_rebuild() {
    let db = TestDb::new().await;
    let id = "tax-excise-US-FEDERAL-2026-04-01-2026-06-30";
    seed_opening_cash(&db, 1_000_000).await;

    let (status, body) = post(
        build_router(&db),
        "/api/ledger/tax-filings",
        excise_filing_body(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {body}");
    let (status, body) = post(
        build_router(&db),
        &format!("/api/ledger/tax-filings/{id}/remit"),
        serde_json::json!({"filed_on": "2026-07-15"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "remit: {body}");

    let live: (String, Option<chrono::NaiveDate>, i64, String) = sqlx::query_as(
        "SELECT status, filed_on, amount_cents, liability_account \
         FROM tax_filings WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(live.0, "paid", "live remit flipped the status");

    // Everything the handlers wrote is on the outbox; move it to
    // audit_log the way the relay does, then rebuild from the log.
    drain(&db).await;
    let report = rebuild_tax_filings(&db.pool).await.unwrap();
    assert_eq!(report.filings_written, 1);
    assert_eq!(report.remittances_applied, 1);
    assert_eq!(report.remittances_orphaned, 0);

    let rebuilt: (String, Option<chrono::NaiveDate>, i64, String) = sqlx::query_as(
        "SELECT status, filed_on, amount_cents, liability_account \
         FROM tax_filings WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        rebuilt, live,
        "rebuilt row is byte-identical to the live one"
    );
}

/// The idempotency guard doubles as the event gate: a repeat create
/// resolves to the existing row and must NOT append a second creation
/// to the log (which would make the rebuild's insert count lie).
#[tokio::test(flavor = "multi_thread")]
async fn repeat_create_emits_exactly_one_creation_event() {
    let db = TestDb::new().await;

    for _ in 0..3 {
        let (status, body) = post(
            build_router(&db),
            "/api/ledger/tax-filings",
            excise_filing_body(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "create: {body}");
    }
    drain(&db).await;

    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE kind = 'ledger.tax.filing.created'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(events, 1, "three POSTs, one creation in the log");

    let report = rebuild_tax_filings(&db.pool).await.unwrap();
    assert_eq!(report.filings_written, 1);
}

/// A remittance whose filing never made it into the log is skipped, not
/// fatal — the same skip-don't-fail leniency `rebuild_facts` applies to
/// a malformed payload. A trimmed log can legitimately hold a remit
/// whose create fell before the baseline.
#[tokio::test(flavor = "multi_thread")]
async fn orphan_remittance_is_skipped_not_fatal() {
    let db = TestDb::new().await;

    insert_audit_event(
        &db,
        "ledger.tax.remitted",
        "2026-07-15T09:00:00Z".parse().unwrap(),
        &remitted_payload("tax-ghost-2026Q2", "sales", "2300"),
    )
    .await;

    let report = rebuild_tax_filings(&db.pool).await.unwrap();
    assert_eq!(report.filings_written, 0);
    assert_eq!(report.remittances_applied, 0);
    assert_eq!(report.remittances_orphaned, 1);
}
