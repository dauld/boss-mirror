//! End-to-end tests for graduated excise rates as registry data.
//!
//! The contract under test (docs/design/brewery-fidelity.md Q4,
//! decided 2026-08-22): excise rates live in the
//! `excise_rate_schedules` registry, and `POST /api/ledger/tax-accruals`
//! resolves the rate from it — splitting a batch across tiers by the
//! jurisdiction's year-to-date taxed barrels — with a loud flat-rate
//! fallback to the caller's `fallback_rate_cents_per_bbl` when the
//! jurisdiction has no registry row, so pre-registry rules.toml
//! deployments keep accruing exactly as before.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_ledger::http::{LedgerApiState, router};
use boss_testing::{RecordingEventBus, TestDb};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Events land on the transactional outbox (same tx as the row); the
/// relay moves them to audit_log. Tests drain explicitly.
async fn drain_outbox(db: &TestDb) {
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 500)
        .await
        .expect("relay drain");
}

fn make_router(db: &TestDb) -> axum::Router {
    router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    })
}

async fn send(
    router: axum::Router,
    method: &str,
    path: &str,
    body: Value,
    role: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json");
    if let Some(role) = role {
        // Matches what the gateway builds for real sessions — the
        // ledger trusts the header because only the gateway speaks to
        // it on the prod loopback.
        req = req.header(
            "x-boss-user",
            json!({
                "id": "emp-test",
                "role": role,
                "access_tier": role,
                "territory_account_ids": [],
                "direct_report_ids": [],
                "department": null,
            })
            .to_string(),
        );
    }
    let resp = router
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, parsed)
}

async fn post_json(router: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(router, "POST", path, body, None).await
}

async fn put_json(router: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(router, "PUT", path, body, None).await
}

/// The TTB small-brewer curve (26 USC 5051) — the row the brewery
/// seeds ship for US-FEDERAL.
fn ttb_tiers() -> Value {
    json!([
        {"up_to_bbl": 60000, "rate_cents_per_bbl": 350},
        {"up_to_bbl": 6000000, "rate_cents_per_bbl": 1600},
    ])
}

async fn put_ttb_schedule(db: &TestDb, jurisdiction: &str, effective_from: &str) {
    let (status, body) = put_json(
        make_router(db),
        "/api/ledger/excise-rate-schedules",
        json!({
            "jurisdiction": jurisdiction,
            "effective_from": effective_from,
            "tiers": ttb_tiers(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PUT schedule failed: {body}");
}

/// POST an excise accrual by quantity, return (status, body).
async fn accrue(
    db: &TestDb,
    id: &str,
    posted_on: &str,
    excise_bbl: i64,
    fallback_rate: Option<i64>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "id": id,
        "kind": "excise",
        "posted_on": posted_on,
        "jurisdiction": "US-FEDERAL",
        "excise_bbl": excise_bbl,
    });
    if let Some(rate) = fallback_rate {
        body["fallback_rate_cents_per_bbl"] = json!(rate);
    }
    post_json(make_router(db), "/api/ledger/tax-accruals", body).await
}

/// Net credit balance on an account across all journal lines.
async fn gl_credit_balance(db: &TestDb, code: &str) -> i64 {
    let (v,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(l.credit_cents - l.debit_cents), 0)::bigint \
         FROM gl_journal_lines l \
         JOIN gl_accounts a ON l.account_id = a.id \
         WHERE a.code = $1",
    )
    .bind(code)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    v
}

// --- flat fallback ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn no_registry_row_falls_back_to_the_flat_rate() {
    let db = TestDb::new().await;
    // No schedule seeded: the pre-registry behavior must hold exactly.
    let (status, body) = accrue(&db, "excise-flat-1", "2026-03-01", 105, Some(350)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["amount_cents"], json!(105 * 350));
    assert_eq!(body["rate_source"], "flat-fallback");
    assert_eq!(gl_credit_balance(&db, "2320").await, 105 * 350);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_registry_row_and_no_fallback_rate_is_a_400() {
    let db = TestDb::new().await;
    let (status, body) = accrue(&db, "excise-nofall-1", "2026-03-01", 105, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

// --- graduated resolution --------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn graduated_single_tier_uses_the_registry_rate() {
    let db = TestDb::new().await;
    put_ttb_schedule(&db, "US-FEDERAL", "2025-01-01").await;
    let (status, body) = accrue(&db, "excise-t1", "2026-03-01", 105, Some(350)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["amount_cents"], json!(105 * 350));
    assert_eq!(body["rate_source"], "registry");
    assert_eq!(body["ytd_bbl_before"], json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_above_the_boundary_accrues_at_the_upper_rate() {
    let db = TestDb::new().await;
    put_ttb_schedule(&db, "US-FEDERAL", "2025-01-01").await;
    // Push YTD to exactly the 60k boundary, then accrue past it.
    let (s1, b1) = accrue(&db, "excise-fill", "2026-02-01", 60_000, None).await;
    assert_eq!(s1, StatusCode::OK, "{b1}");
    assert_eq!(b1["amount_cents"], json!(60_000 * 350));
    let (s2, b2) = accrue(&db, "excise-upper", "2026-03-01", 158, None).await;
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b2["amount_cents"], json!(158 * 1600));
    assert_eq!(b2["ytd_bbl_before"], json!(60_000));
    assert_eq!(
        gl_credit_balance(&db, "2320").await,
        60_000 * 350 + 158 * 1600
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tier_crossing_batch_splits_its_barrels_across_both_rates() {
    let db = TestDb::new().await;
    put_ttb_schedule(&db, "US-FEDERAL", "2025-01-01").await;
    let (s1, b1) = accrue(&db, "excise-near", "2026-02-01", 59_900, None).await;
    assert_eq!(s1, StatusCode::OK, "{b1}");
    // The straddling batch: 100 bbl left at $3.50, 100 bbl at $16.00.
    let (s2, b2) = accrue(&db, "excise-straddle", "2026-03-01", 200, None).await;
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b2["amount_cents"], json!(100 * 350 + 100 * 1600));
    assert_eq!(b2["ytd_bbl_before"], json!(59_900));
    assert_eq!(
        gl_credit_balance(&db, "2320").await,
        59_900 * 350 + 100 * 350 + 100 * 1600
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ytd_barrels_reset_each_calendar_year() {
    let db = TestDb::new().await;
    put_ttb_schedule(&db, "US-FEDERAL", "2025-01-01").await;
    let (s1, _) = accrue(&db, "excise-dec", "2025-12-30", 59_900, None).await;
    assert_eq!(s1, StatusCode::OK);
    // New calendar year: the counter is back at zero, so a batch that
    // would have straddled in December is all first-tier in January.
    let (s2, b2) = accrue(&db, "excise-jan", "2026-01-05", 200, None).await;
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b2["amount_cents"], json!(200 * 350));
    assert_eq!(b2["ytd_bbl_before"], json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn schedule_versions_resolve_by_posted_on() {
    let db = TestDb::new().await;
    // v1: flat 350 forever. v2 (effective 2026-01-01): the TTB curve.
    let (s, b) = put_json(
        make_router(&db),
        "/api/ledger/excise-rate-schedules",
        json!({
            "jurisdiction": "US-FEDERAL",
            "effective_from": "2025-01-01",
            "tiers": [{"rate_cents_per_bbl": 100}],
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    put_ttb_schedule(&db, "US-FEDERAL", "2026-01-01").await;
    // 2025 accrual resolves the old row; 2026 resolves the new one.
    let (_, b25) = accrue(&db, "excise-2025", "2025-06-01", 100, None).await;
    assert_eq!(b25["amount_cents"], json!(100 * 100));
    let (_, b26) = accrue(&db, "excise-2026", "2026-06-01", 100, None).await;
    assert_eq!(b26["amount_cents"], json!(100 * 350));
}

// --- replay determinism ----------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_post_is_idempotent_and_amount_is_stable() {
    let db = TestDb::new().await;
    put_ttb_schedule(&db, "US-FEDERAL", "2025-01-01").await;
    let (s1, b1) = accrue(&db, "excise-dup", "2026-03-01", 105, None).await;
    assert_eq!(s1, StatusCode::OK, "{b1}");
    // Redelivery: same id, same inputs. The YTD sum must NOT count the
    // first write's own fact (self-exclusion), so the recomputed amount
    // is identical — and nothing double-books.
    let (s2, b2) = accrue(&db, "excise-dup", "2026-03-01", 105, None).await;
    assert_eq!(s2, StatusCode::OK, "{b2}");
    assert_eq!(b2["amount_cents"], b1["amount_cents"]);
    assert_eq!(b2["ytd_bbl_before"], json!(0));
    assert_eq!(gl_credit_balance(&db, "2320").await, 105 * 350);
    let (facts,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM financial_facts WHERE source_id = 'excise-dup'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts, 1);
    // The audit event is gated on the fact insert — a redelivery
    // appends nothing to the log.
    drain_outbox(&db).await;
    let (events,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM audit_log \
         WHERE kind = 'ledger.tax.accrual.recorded' \
           AND payload->>'accrual_id' = 'excise-dup'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_from_audit_log_reproduces_the_graduated_facts() {
    let db = TestDb::new().await;
    put_ttb_schedule(&db, "US-FEDERAL", "2025-01-01").await;
    accrue(&db, "excise-r1", "2026-02-01", 59_900, None).await;
    accrue(&db, "excise-r2", "2026-03-01", 200, None).await;

    let before: Vec<(String, Value)> = sqlx::query_as(
        "SELECT source_id, payload FROM financial_facts \
         WHERE kind = 'finance.tax.accrued' ORDER BY source_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(before.len(), 2);

    // Same events → same accruals: wipe the projection, replay from the
    // log, compare payload-for-payload. The projection rule passes the
    // event payload through verbatim, so the graduated amounts (and the
    // excise_bbl the YTD sum reads) survive a TRUNCATE-then-replay.
    drain_outbox(&db).await;
    sqlx::query("TRUNCATE gl_journal_lines, gl_journal_entries, financial_facts")
        .execute(&db.pool)
        .await
        .unwrap();
    boss_ledger::rebuild_facts(&db.pool).await.unwrap();

    let after: Vec<(String, Value)> = sqlx::query_as(
        "SELECT source_id, payload FROM financial_facts \
         WHERE kind = 'finance.tax.accrued' ORDER BY source_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        after[1].1["amount_cents"],
        json!(100 * 350 + 100 * 1600),
        "the tier-split amount must survive replay byte-for-byte"
    );
}

// --- legacy + validation ---------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn legacy_amount_cents_body_still_posts() {
    let db = TestDb::new().await;
    let (status, body) = post_json(
        make_router(&db),
        "/api/ledger/tax-accruals",
        json!({
            "id": "excise-legacy",
            "kind": "excise",
            "amount_cents": 36750,
            "posted_on": "2026-03-01",
            "jurisdiction": "US-FEDERAL",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(gl_credit_balance(&db, "2320").await, 36750);
}

#[tokio::test(flavor = "multi_thread")]
async fn neither_quantity_nor_amount_is_a_400() {
    let db = TestDb::new().await;
    let (status, _) = post_json(
        make_router(&db),
        "/api/ledger/tax-accruals",
        json!({
            "id": "excise-empty",
            "kind": "excise",
            "posted_on": "2026-03-01",
            "jurisdiction": "US-FEDERAL",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn put_schedule_rejects_malformed_tiers() {
    let db = TestDb::new().await;
    // Unbounded tier before the last — the shape validate_tiers names.
    let (status, body) = put_json(
        make_router(&db),
        "/api/ledger/excise-rate-schedules",
        json!({
            "jurisdiction": "US-FEDERAL",
            "effective_from": "2025-01-01",
            "tiers": [
                {"rate_cents_per_bbl": 350},
                {"up_to_bbl": 60000, "rate_cents_per_bbl": 1600},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn schedule_writes_are_auditor_gated_and_listable() {
    let db = TestDb::new().await;
    let (status, _) = send(
        make_router(&db),
        "PUT",
        "/api/ledger/excise-rate-schedules",
        json!({
            "jurisdiction": "US-FEDERAL",
            "effective_from": "2025-01-01",
            "tiers": ttb_tiers(),
        }),
        Some("auditor"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    put_ttb_schedule(&db, "US-FEDERAL", "2025-01-01").await;
    let resp = make_router(&db)
        .oneshot(
            Request::builder()
                .uri("/api/ledger/excise-rate-schedules?jurisdiction=US-FEDERAL")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let listed: Value = serde_json::from_slice(&bytes).unwrap();
    let rows = listed.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["jurisdiction"], "US-FEDERAL");
    assert_eq!(rows[0]["tiers"], ttb_tiers());
}

// --- the accounts are the kind's (backlog c0b83e13) ------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_accrual_posts_to_the_accounts_the_kinds_row_names() {
    // The body names only the kind; the door reads 2320 / 6550 off the
    // `excise` tax_kinds row and stamps both onto the fact, so the
    // payload a rebuild replays carries them.
    let db = TestDb::new().await;
    let (status, _) = accrue(&db, "excise-row-1", "2026-03-01", 100, Some(350)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gl_credit_balance(&db, "2320").await, 100 * 350);
    assert_eq!(gl_credit_balance(&db, "6550").await, -(100 * 350));
    let (payload,): (Value,) = sqlx::query_as(
        "SELECT payload FROM financial_facts WHERE source_table = 'tax_accruals' \
         AND source_id = 'excise-row-1'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(payload["kind"], json!("excise"));
    assert_eq!(payload["liability_account"], json!("2320"));
    assert_eq!(payload["expense_account"], json!("6550"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_with_no_row_is_refused_naming_it() {
    let db = TestDb::new().await;
    let (status, body) = post_json(
        make_router(&db),
        "/api/ledger/tax-accruals",
        json!({
            "id": "excise-unregistered",
            "kind": "wheat-levy",
            "excise_bbl": 10,
            "posted_on": "2026-03-01",
            "jurisdiction": "US-FEDERAL",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let text = body.to_string();
    assert!(
        text.contains("wheat-levy") && text.contains("tax_kinds"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_that_drains_rather_than_accrues_is_refused_naming_it() {
    // `sales` builds its liability up per invoice line: its row names no
    // expense account, so it cannot be the target of a standalone accrual.
    let db = TestDb::new().await;
    let (status, body) = post_json(
        make_router(&db),
        "/api/ledger/tax-accruals",
        json!({
            "id": "excise-sales",
            "kind": "sales",
            "excise_bbl": 10,
            "posted_on": "2026-03-01",
            "jurisdiction": "US-CA",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let text = body.to_string();
    assert!(
        text.contains("sales") && text.contains("expense_account"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_still_naming_the_accounts_is_refused_rather_than_ignored() {
    // A caller on the pre-c0b83e13 contract must hear about it: unknown
    // fields were dropped in silence before, which would have accrued
    // against whatever the kind's row said while the caller believed
    // its own codes were in force.
    let db = TestDb::new().await;
    let (status, _) = post_json(
        make_router(&db),
        "/api/ledger/tax-accruals",
        json!({
            "id": "excise-old-contract",
            "kind": "excise",
            "expense_account": "6550",
            "liability_account": "2320",
            "excise_bbl": 10,
            "posted_on": "2026-03-01",
            "jurisdiction": "US-FEDERAL",
        }),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "the old body shape must not post");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_excise_filing_drains_the_liability_and_never_accrues_it_again() {
    // c0b83e13 gave the `excise` tax_kinds row an expense account, and
    // the filing door used to read "this kind accrues at filing time"
    // off exactly that field. Excise is accrued per brew batch, so a
    // filing that accrued again would double the liability: the period
    // derivation reads the 2320 balance the batches already credited.
    let db = TestDb::new().await;
    put_ttb_schedule(&db, "US-FEDERAL", "2026-01-01").await;
    let (status, _) = accrue(&db, "excise-batch-1", "2026-01-15", 100, None).await;
    assert_eq!(status, StatusCode::OK);
    let accrued = gl_credit_balance(&db, "2320").await;
    let expensed = gl_credit_balance(&db, "6550").await;
    assert_eq!(accrued, 100 * 350);

    let (status, body) = post_json(
        make_router(&db),
        "/api/ledger/tax-filings",
        json!({
            "id": "tf-excise-US-FEDERAL-2026-Q1",
            "kind": "excise",
            "jurisdiction": "US-FEDERAL",
            "period_start": "2026-01-01",
            "period_end": "2026-03-31",
            "due_on": "2026-04-15",
            "amount_cents": 1,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["amount_cents"],
        json!(accrued),
        "the filing derives from 2320"
    );
    assert_eq!(
        gl_credit_balance(&db, "2320").await,
        accrued,
        "creating the filing must not credit 2320 a second time"
    );
    assert_eq!(
        gl_credit_balance(&db, "6550").await,
        expensed,
        "creating the filing must not debit 6550 a second time"
    );
}
