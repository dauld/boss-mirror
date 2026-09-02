//! HTTP-layer tests for the read-only ledger API.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_ledger::http::{LedgerApiState, router};
use boss_ledger::{FactRef, post_fact_in_tx};
use boss_testing::TestDb;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn seed_entry(
    db: &TestDb,
    kind: &str,
    happened_on: NaiveDate,
    payload: &Value,
    source_id: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'invoices', $5, 'test')",
    )
    .bind(id)
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    post_fact_in_tx(
        &mut tx,
        &FactRef {
            id,
            kind,
            happened_on,
            payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

/// Seed opening cash so cash-crediting posts (payroll, bill payments,
/// tax remittance) don't trip the "1000 Cash must not go negative"
/// guard in `post_fact_in_tx`. Posts a balanced manual entry
/// DR 1000 Cash / CR 3000 Retained Earnings — the books' equivalent
/// of an opening capital injection on a fresh ledger.
async fn seed_opening_cash(db: &TestDb, cents: i64) {
    seed_entry(
        db,
        "finance.manual.entry",
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        &json!({
            "lines": [
                {"account_code": "1000", "debit_cents": cents, "memo": "opening cash"},
                {"account_code": "3000", "credit_cents": cents, "memo": "opening capital"},
            ]
        }),
        "opening-cash",
    )
    .await;
}

async fn get(router: axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = router
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn health_is_ok() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_accounts_returns_seeded_chart() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/accounts").await;
    assert_eq!(status, StatusCode::OK);
    let accounts = body.as_array().unwrap();
    // The starter chart ships 31 active accounts. Recent additions:
    //   1010 Cash in Transit (two-phase bank settlement),
    //   2150 Payroll Liability + 6400 Payroll Taxes & Benefits (payroll),
    //   2200 Deferred Revenue (ASC 606 step 1),
    //   2300 Sales Tax Payable + 2310 Income Tax Payable (tax authorities),
    //   2320 Excise Tax Payable + 6550 Excise Tax Expense (federal beer excise),
    //   6500 Income Tax Expense (income-tax accrual),
    //   4140 Revenue — Distribution Contracts,
    //   5200 COGS — Packaging,
    //   6700 Bad Debt Expense,
    //   6900 Depreciation Expense,
    //   2110 Goods Received Not Invoiced (GR-IR capitalize-at-receive),
    //   2400 Keg Deposits Payable + 4150 Keg Deposit Forfeitures
    //     (93f936b9: full balance-sheet keg model).
    // When adding a new account, bump this count + add a presence
    // check below.
    assert_eq!(accounts.len(), 33);
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "2110" && a["name"] == "Goods Received Not Invoiced"),
        "2110 GR-IR should be in the chart for capitalize-at-receive",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "1010" && a["name"] == "Cash in Transit"),
        "1010 Cash in Transit should be in the chart for the bank clearing flow",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "2150" && a["name"] == "Payroll Liability"),
        "2150 Payroll Liability should be in the chart for the payroll flow",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "2320" && a["name"] == "Excise Tax Payable"),
        "2320 Excise Tax Payable should be in the chart for the federal excise flow",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "6550" && a["name"] == "Excise Tax Expense"),
        "6550 Excise Tax Expense should be in the chart for the federal excise flow",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "2300" && a["name"] == "Sales Tax Payable"),
        "2300 Sales Tax Payable should be in the chart for the tax-authorities flow",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "2310" && a["name"] == "Income Tax Payable"),
        "2310 Income Tax Payable should be in the chart for the tax-authorities flow",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "6400" && a["name"] == "Payroll Taxes & Benefits"),
        "6400 Payroll Taxes & Benefits should be in the chart for the payroll flow",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "1100" && a["name"] == "Accounts Receivable"),
        "1100 A/R should be in the chart",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "2400" && a["name"] == "Keg Deposits Payable"),
        "2400 Keg Deposits Payable should be in the chart for the keg-deposit flow",
    );
    assert!(
        accounts
            .iter()
            .any(|a| a["code"] == "4150" && a["name"] == "Keg Deposit Forfeitures"),
        "4150 Keg Deposit Forfeitures should be in the chart for the keg-deposit flow",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trial_balance_reflects_posted_entries() {
    let db = TestDb::new().await;
    // Issue an invoice + pay it. Trial balance should show:
    //  A/R: 1000 debit, 1000 credit → balance 0
    //  Cash: 1000 debit → balance 1000
    //  Revenue Service: 1000 credit → balance 1000
    let p_issue = json!({"invoice_id": "i1", "amount_cents": 1_000, "line_items": [{"category": "service", "amount_cents": 1_000}]});
    let p_paid = json!({"invoice_id": "i1", "amount_cents": 1_000});
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p_issue,
        "i1",
    )
    .await;
    seed_entry(
        &db,
        "finance.invoice.paid",
        NaiveDate::from_ymd_opt(2026, 3, 20).unwrap(),
        &p_paid,
        "i1",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/trial-balance").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["balanced"], true);
    assert_eq!(body["total_debits_cents"], 2000);
    assert_eq!(body["total_credits_cents"], 2000);

    let rows = body["rows"].as_array().unwrap();
    let cash = rows.iter().find(|r| r["account_code"] == "1000").unwrap();
    assert_eq!(cash["debit_total_cents"], 1000);
    assert_eq!(cash["balance_cents"], 1000);
    let ar = rows.iter().find(|r| r["account_code"] == "1100").unwrap();
    assert_eq!(ar["debit_total_cents"], 1000);
    assert_eq!(ar["credit_total_cents"], 1000);
    assert_eq!(ar["balance_cents"], 0);
    let rev = rows.iter().find(|r| r["account_code"] == "4120").unwrap();
    assert_eq!(rev["credit_total_cents"], 1000);
    assert_eq!(rev["balance_cents"], 1000);
}

#[tokio::test(flavor = "multi_thread")]
async fn trial_balance_as_of_filters_by_date() {
    let db = TestDb::new().await;
    let p1 = json!({"invoice_id": "i1", "amount_cents": 100, "line_items": [{"category": "service", "amount_cents": 100}]});
    let p2 = json!({"invoice_id": "i2", "amount_cents": 900, "line_items": [{"category": "service", "amount_cents": 900}]});
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p1,
        "i1",
    )
    .await;
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 4, 10).unwrap(),
        &p2,
        "i2",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    // As of end-of-March: only the $100 invoice is in scope.
    let (status, body) = get(r, "/api/ledger/trial-balance?as_of=2026-03-31").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total_debits_cents"], 100);
    assert_eq!(body["total_credits_cents"], 100);
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_lookup_by_account_code() {
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i1", "amount_cents": 500, "line_items": [{"category": "new-sales", "amount_cents": 500}]});
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p,
        "i1",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/entries?account_code=4100").await;
    assert_eq!(status, StatusCode::OK);
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["fact_kind"], "finance.invoice.issued");
    assert_eq!(entries[0]["fact_source_id"], "i1");
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_requires_account_or_fact_filter() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let resp = r
        .oneshot(
            Request::builder()
                .uri("/api/ledger/entries")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_lookup_by_source_pair() {
    // IT panel's activity tabs use this path: given a projection id
    // (here source_table='invoices', source_id='i-src-1'), fetch the
    // one journal entry it produced in a single round-trip.
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i-src-1", "amount_cents": 500, "line_items": [{"category": "new-sales", "amount_cents": 500}]});
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p,
        "i-src-1",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(
        r,
        "/api/ledger/entries?source_table=invoices&source_id=i-src-1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["fact_source_id"], "i-src-1");
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_source_filter_requires_both_halves() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = get(r, "/api/ledger/entries?source_table=invoices").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn get_entry_detail_returns_lines() {
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i1", "amount_cents": 500, "line_items": [{"category": "new-sales", "amount_cents": 500}]});
    let fact_id = seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p,
        "i1",
    )
    .await;

    // Resolve the entry id via the list endpoint, then fetch detail.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, list) = get(r, &format!("/api/ledger/entries?fact_id={fact_id}")).await;
    let entry_id = list[0]["id"].as_str().unwrap();

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, &format!("/api/ledger/entries/{entry_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["fact_kind"], "finance.invoice.issued");
    assert_eq!(body["fact_source_id"], "i1");
    assert_eq!(body["fact_payload"]["invoice_id"], "i1");
    let lines = body["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .any(|l| l["account_code"] == "1100" && l["debit_cents"] == 500)
    );
    assert!(
        lines
            .iter()
            .any(|l| l["account_code"] == "4100" && l["credit_cents"] == 500)
    );
}

async fn post_json(router: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
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

/// Same shape as `post_json` but synthesises an auditor-role caller
/// via `X-Boss-User`. Matches what the gateway builds for real
/// sessions — the ledger service trusts the header because only
/// the gateway speaks to it on the prod loopback.
async fn post_as_auditor(router: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let user_json = json!({
        "id": "emp-auditor",
        "role": "auditor",
        "access_tier": "auditor",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string();
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("Content-Type", "application/json")
                .header("x-boss-user", user_json)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
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

#[tokio::test(flavor = "multi_thread")]
async fn post_manual_entry_happy_path() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r,
        "/api/ledger/journal-entries",
        json!({
            "posted_on": "2026-03-15",
            "memo": "Q1 rent accrual",
            "created_by": "cfo",
            "lines": [
                {"account_code": "6200", "debit_cents": 250_000, "memo": "April rent"},
                {"account_code": "2100", "credit_cents": 250_000},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body["fact_id"].as_str().is_some());
    assert!(body["entry_id"].as_str().is_some());
    assert_eq!(body["posted_on"], "2026-03-15");

    // The fact landed as kind=finance.manual.entry with the recorded author.
    let (kind, created_by): (String, String) = sqlx::query_as(
        "SELECT kind, created_by FROM financial_facts \
         WHERE id = $1::uuid",
    )
    .bind(body["fact_id"].as_str().unwrap())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(kind, "finance.manual.entry");
    assert_eq!(created_by, "cfo");

    // And the lines projected into the GL with the right codes + amounts.
    let lines: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT a.code, l.debit_cents, l.credit_cents \
         FROM gl_journal_lines l \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE l.journal_entry_id = $1::uuid \
         ORDER BY l.sort_order",
    )
    .bind(body["entry_id"].as_str().unwrap())
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], ("6200".to_string(), 250_000i64, 0i64));
    assert_eq!(lines[1], ("2100".to_string(), 0i64, 250_000i64));
}

#[tokio::test(flavor = "multi_thread")]
async fn post_manual_entry_rejects_unbalanced() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _body) = post_json(
        r,
        "/api/ledger/journal-entries",
        json!({
            "lines": [
                {"account_code": "6200", "debit_cents": 100},
                {"account_code": "2100", "credit_cents": 99},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_manual_entry_rejects_unknown_account() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _body) = post_json(
        r,
        "/api/ledger/journal-entries",
        json!({
            "lines": [
                {"account_code": "9999", "debit_cents": 100},
                {"account_code": "2100", "credit_cents": 100},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_manual_entry_rejects_locked_period() {
    let db = TestDb::new().await;

    // Lock January 2026 up-front so the manual entry dated inside it
    // must be rejected. boss_ledger::periods exposes the lock helper.
    let jan_period_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO gl_periods (id, kind, starts_on, ends_on, status) \
         VALUES ($1, 'month', '2026-01-01', '2026-01-31', 'open')",
    )
    .bind(jan_period_id)
    .execute(&db.pool)
    .await
    .unwrap();
    let stamp = boss_core::publisher::EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(chrono::Utc::now());
    boss_ledger::periods::lock_period(&db.pool, jan_period_id, "test", &stamp, "test")
        .await
        .unwrap();

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _body) = post_json(
        r,
        "/api/ledger/journal-entries",
        json!({
            "posted_on": "2026-01-15",
            "lines": [
                {"account_code": "6200", "debit_cents": 100},
                {"account_code": "2100", "credit_cents": 100},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// -------------------------------------------------------------------------
// Cash flow statement (indirect method)
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cash_flow_unpaid_invoice_produces_no_operating_cash() {
    let db = TestDb::new().await;
    // Issue a service invoice but don't collect it. Revenue is
    // recognized (net_income = +1000), but no cash moves (the JE is
    // DR 1100 AR / CR revenue — nothing touches the cash pool), so
    // the cash-movement statement reports cash_from_operations = 0
    // and the Cash balance is unchanged → reconciliation_gap = 0.
    let payload = json!({
        "invoice_id": "cfo-1",
        "amount_cents": 1_000,
        "line_items": [{"category": "service", "amount_cents": 1_000}],
    });
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &payload,
        "cfo-1",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/cash-flow?from=2026-03-01&to=2026-03-31").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["net_income_cents"], 1000);
    // No cash JE for an uncollected invoice → zero operating cash.
    assert_eq!(body["cash_from_operations_cents"], 0);
    assert_eq!(body["net_change_in_cash_cents"], 0);
    assert_eq!(body["reconciled"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn cash_flow_paid_invoice_shows_cash_delta() {
    let db = TestDb::new().await;
    let p_issue = json!({
        "invoice_id": "cfo-2",
        "amount_cents": 1_000,
        "line_items": [{"category": "service", "amount_cents": 1_000}],
    });
    let p_paid = json!({"invoice_id": "cfo-2", "amount_cents": 1_000});
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p_issue,
        "cfo-2",
    )
    .await;
    seed_entry(
        &db,
        "finance.invoice.paid",
        NaiveDate::from_ymd_opt(2026, 3, 20).unwrap(),
        &p_paid,
        "cfo-2",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/cash-flow?from=2026-03-01&to=2026-03-31").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["net_income_cents"], 1000);
    assert_eq!(body["cash_from_operations_cents"], 1000);
    assert_eq!(body["cash_from_investing_cents"], 0);
    assert_eq!(body["cash_from_financing_cents"], 0);
    assert_eq!(body["net_change_in_cash_cents"], 1000);
    assert_eq!(
        body["cash_end_cents"].as_i64().unwrap() - body["cash_start_cents"].as_i64().unwrap(),
        1000
    );
    assert_eq!(body["reconciled"], true);
}

// -------------------------------------------------------------------------
// Cash flow statement (direct method, ?method=direct)
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cash_flow_direct_sums_buckets_and_reconciles_against_cash_pool() {
    // Seed one fact per cash-event kind, then assert the direct
    // statement's four buckets AND the structural invariant: the
    // direct method's net_change_in_cash reconciles against the GL
    // cash-pool (1000 + 1010) delta over the same window.
    //
    //   payment.received  amount 10_000  → DR 1010 / CR 1100   pool +10_000
    //   payment.settled   amount 10_000  → DR 1000 / CR 1010   pool   0 (internal sweep)
    //   bill.paid         amount  3_000  → DR 2100 / CR 1000   pool  -3_000
    //   payroll.run       net     4_000  → CR 1000 (net)       pool  -4_000
    //   tax.remitted      amount  2_000  → DR 2300 / CR 1000   pool  -2_000
    //
    // Direct net change = 10_000 − 3_000 − 4_000 − 2_000 = +1_000.
    // GL cash-pool delta = +10_000 − 3_000 − 4_000 − 2_000 = +1_000.
    // → reconciled, gap = 0.
    let db = TestDb::new().await;
    let d = |day: u32| NaiveDate::from_ymd_opt(2026, 4, day).unwrap();

    seed_entry(
        &db,
        "finance.payment.received",
        d(5),
        &json!({"invoice_id": "dcf-1", "amount_cents": 10_000}),
        "dcf-recv-1",
    )
    .await;
    seed_entry(
        &db,
        "finance.payment.settled",
        d(7),
        &json!({"settlement_id": "dcf-set-1", "amount_cents": 10_000}),
        "dcf-set-1",
    )
    .await;
    seed_entry(
        &db,
        "finance.bill.paid",
        d(10),
        &json!({"vendor_invoice_id": "dcf-bill-1", "amount_cents": 3_000}),
        "dcf-bill-1",
    )
    .await;
    seed_entry(
        &db,
        "finance.payroll.run",
        d(12),
        &json!({
            "run_id": "dcf-pr-1",
            "gross_cents": 5_000,
            "withheld_cents": 1_000,
            "employer_tax_cents": 0,
            "net_cents": 4_000,
            "employee_count": 1,
        }),
        "dcf-pr-1",
    )
    .await;
    seed_entry(
        &db,
        "finance.tax.remitted",
        d(15),
        &json!({
            "filing_id": "dcf-tf-1",
            "kind": "sales",
            "jurisdiction": "US-CA",
            "liability_account": "2300",
            "amount_cents": 2_000,
        }),
        "dcf-tf-1",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(
        r,
        "/api/ledger/cash-flow?method=direct&from=2026-04-01&to=2026-04-30",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["method"], "direct");
    assert_eq!(body["cash_in_from_customers_cents"], 10_000);
    assert_eq!(body["cash_out_to_vendors_cents"], 3_000);
    assert_eq!(body["cash_out_to_employees_cents"], 4_000);
    assert_eq!(body["cash_out_to_authorities_cents"], 2_000);
    assert_eq!(body["net_change_in_cash_cents"], 1_000);

    // Structural invariant: direct net change reconciles against the
    // GL cash-pool (1000 + 1010) net delta over the same window.
    assert_eq!(body["gl_cash_pool_delta_cents"], 1_000);
    assert_eq!(body["reconciliation_gap_cents"], 0);
    assert_eq!(body["reconciled"], true);
    // 1010 fully swept to 1000 → 1000-only delta equals the pool delta.
    assert_eq!(body["gl_cash_1000_delta_cents"], 1_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn cash_flow_direct_empty_period_returns_zeros() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(
        r,
        "/api/ledger/cash-flow?method=direct&from=2026-01-01&to=2026-01-31",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["method"], "direct");
    assert_eq!(body["cash_in_from_customers_cents"], 0);
    assert_eq!(body["cash_out_to_vendors_cents"], 0);
    assert_eq!(body["cash_out_to_employees_cents"], 0);
    assert_eq!(body["cash_out_to_authorities_cents"], 0);
    assert_eq!(body["net_change_in_cash_cents"], 0);
    assert_eq!(body["reconciled"], true);
}

async fn seed_invoice(db: &TestDb, invoice_id: &str, amount_cents: i64) {
    sqlx::query(
        "INSERT INTO invoices (id, account_id, issued_on, due_on, status, amount_cents, currency) \
         VALUES ($1, 'prac-1', '2026-03-15', '2026-04-14', 'outstanding', $2, 'USD') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(invoice_id)
    .bind(amount_cents)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn bank_settlement_create_and_settle_round_trip() {
    // The two-phase bank clearing path: POST a settlement to post
    // finance.payment.received (DR 1010 / CR 1100), then POST /settle
    // to post finance.payment.settled (DR 1000 / CR 1010). After both,
    // the trial balance shows Cash +amount, A/R -amount, Cash in Transit 0.
    let db = TestDb::new().await;
    // A/R needs a prior credit so it can drop — seed an invoice-issued fact.
    let p_issue = json!({
        "invoice_id": "inv-bank-1",
        "amount_cents": 5_000,
        "line_items": [{"category": "service", "amount_cents": 5_000}],
    });
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p_issue,
        "inv-bank-1",
    )
    .await;
    seed_invoice(&db, "inv-bank-1", 5_000).await;

    let r1 = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r1,
        "/api/ledger/bank-settlements",
        json!({
            "id": "set-1",
            "invoice_id": "inv-bank-1",
            "account_id": "prac-1",
            "amount_cents": 5_000,
            "currency": "USD",
            "received_on": "2026-03-20",
            "bank_provider": "chase",
            "payment_method": "ach",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "pending");
    assert_eq!(body["expected_settle_on"], "2026-03-21");

    // Trial balance: A/R drained into Cash in Transit, Cash untouched.
    let r2 = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r2, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(row("1010")["balance_cents"], 5_000);
    assert_eq!(row("1100")["balance_cents"], 0);
    assert_eq!(row("1000")["balance_cents"], 0);

    // Now settle.
    let r3 = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r3,
        "/api/ledger/bank-settlements/set-1/settle",
        json!({"settled_on": "2026-03-22"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "settled");
    assert_eq!(body["settled_on"], "2026-03-22");

    // Trial balance: Cash in Transit zeros out, Cash sees the gain.
    let r4 = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r4, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(row("1010")["balance_cents"], 0);
    assert_eq!(row("1000")["balance_cents"], 5_000);
    assert_eq!(row("1100")["balance_cents"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn bank_settlement_sweep_settles_only_due_rows() {
    // Two pending rows: one due today, one due tomorrow. Sweep with
    // as_of=today picks up only the first.
    let db = TestDb::new().await;
    for (inv_id, set_id) in [("inv-s1", "set-today"), ("inv-s2", "set-tomorrow")] {
        let p_issue = json!({
            "invoice_id": inv_id,
            "amount_cents": 1_000,
            "line_items": [{"category": "service", "amount_cents": 1_000}],
        });
        seed_entry(
            &db,
            "finance.invoice.issued",
            NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
            &p_issue,
            inv_id,
        )
        .await;
        seed_invoice(&db, inv_id, 1_000).await;
        let r = router(LedgerApiState {
            pool: db.pool.clone(),
            publisher: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
            // No read gate in tests; production wires one.
            policy: None,
        });
        // set-today → expected_settle_on = 2026-03-20 (ach default = +1 day)
        // set-tomorrow → expected_settle_on = 2026-03-21
        let received = if set_id == "set-today" {
            "2026-03-19"
        } else {
            "2026-03-20"
        };
        let (status, _) = post_json(
            r,
            "/api/ledger/bank-settlements",
            json!({
                "id": set_id,
                "invoice_id": inv_id,
                "account_id": "prac-1",
                "amount_cents": 1_000,
                "currency": "USD",
                "received_on": received,
                "bank_provider": "chase",
                "payment_method": "ach",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r,
        "/api/ledger/bank-settlements/sweep?as_of=2026-03-20",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["swept"], 1);
    assert_eq!(body["ids"][0], "set-today");

    // Only the first row flipped.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, list) = get(r, "/api/ledger/bank-settlements").await;
    let rows = list.as_array().unwrap();
    let by_id = |id: &str| rows.iter().find(|r| r["id"] == id).cloned().unwrap();
    assert_eq!(by_id("set-today")["status"], "settled");
    assert_eq!(by_id("set-tomorrow")["status"], "pending");
}

#[tokio::test(flavor = "multi_thread")]
async fn bank_settlement_create_is_idempotent_on_id() {
    // Re-POST with the same id must not create a second fact or a second
    // journal entry.
    let db = TestDb::new().await;
    let p_issue = json!({
        "invoice_id": "inv-idem",
        "amount_cents": 2_000,
        "line_items": [{"category": "service", "amount_cents": 2_000}],
    });
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p_issue,
        "inv-idem",
    )
    .await;
    seed_invoice(&db, "inv-idem", 2_000).await;

    for _ in 0..2 {
        let r = router(LedgerApiState {
            pool: db.pool.clone(),
            publisher: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
            // No read gate in tests; production wires one.
            policy: None,
        });
        let (status, _) = post_json(
            r,
            "/api/ledger/bank-settlements",
            json!({
                "id": "set-dup",
                "invoice_id": "inv-idem",
                "account_id": "prac-1",
                "amount_cents": 2_000,
                "currency": "USD",
                "received_on": "2026-03-20",
                "bank_provider": "chase",
                "payment_method": "ach",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (n_facts,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = 'finance.payment.received' AND source_id = 'set-dup'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n_facts, 1, "duplicate POSTs must collapse to one fact");

    // Trial balance unchanged from single post — only $20 moved into 1010.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let cash_in_transit = rows.iter().find(|r| r["account_code"] == "1010").unwrap();
    assert_eq!(cash_in_transit["balance_cents"], 2_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn cash_flow_empty_period_returns_zeros() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/cash-flow?from=2026-01-01&to=2026-01-31").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["net_income_cents"], 0);
    assert_eq!(body["cash_from_operations_cents"], 0);
    assert_eq!(body["net_change_in_cash_cents"], 0);
    assert_eq!(body["reconciled"], true);
    assert!(
        body["working_capital_adjustments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

async fn seed_employee(db: &TestDb, id: &str, department: &str, role: &str) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone) \
         VALUES ('loc-hq', 'HQ', 'hq', 'UTC') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO employees \
            (id, name, email, role, department, hire_date, location, \
             employment_type, status) \
         VALUES ($1, $2, $3, $4, $5, '2025-01-01', 'loc-hq', 'full-time', 'active') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(format!("Test {id}"))
    .bind(format!("{id}@example.com"))
    .bind(role)
    .bind(department)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn payroll_run_posts_compound_journal_entry() {
    // Three employees, gross = 12,000 / withholding = 2,600 / employer
    // tax = 1,100. Net = 9,400. Journal entry should post:
    //   DR 6100  12,000 (gross)
    //   DR 6400   1,100 (employer tax)
    //   CR 2150   3,700 (withheld + employer tax)
    //   CR 1000   9,400 (net)
    let db = TestDb::new().await;
    seed_opening_cash(&db, 1_000_000).await;
    for (id, dept, role) in [
        ("emp-pay-1", "sales", "sales-rep"),
        ("emp-pay-2", "service", "service-tech"),
        ("emp-pay-3", "finance", "controller"),
    ] {
        seed_employee(&db, id, dept, role).await;
    }

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r,
        "/api/ledger/payroll-runs",
        json!({
            "id": "pr-2026-04-15",
            "run_date": "2026-04-15",
            "period_start": "2026-04-01",
            "period_end": "2026-04-14",
            "employer_tax_cents": 110_000,
            "provider": "adp",
            "lines": [
                {"employee_id": "emp-pay-1", "gross_cents": 500_000, "withheld_cents": 110_000, "net_cents": 390_000, "department": "sales", "role": "sales-rep"},
                {"employee_id": "emp-pay-2", "gross_cents": 400_000, "withheld_cents":  85_000, "net_cents": 315_000, "department": "service", "role": "service-tech"},
                {"employee_id": "emp-pay-3", "gross_cents": 300_000, "withheld_cents":  65_000, "net_cents": 235_000, "department": "finance", "role": "controller"},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["status"], "posted");
    assert_eq!(body["gross_cents"], 1_200_000);
    assert_eq!(body["withheld_cents"], 260_000);
    assert_eq!(body["employer_tax_cents"], 110_000);
    assert_eq!(body["net_cents"], 940_000);
    assert_eq!(body["employee_count"], 3);

    // Trial balance: the compound entry landed against all four accounts.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(row("6100")["balance_cents"], 1_200_000);
    assert_eq!(row("6400")["balance_cents"], 110_000);
    assert_eq!(row("2150")["balance_cents"], 370_000);
    // Cash: 1,000,000 opening − 940,000 net payroll credit = 60,000.
    // (1000 is debit-normal; the payroll net is a credit reducing cash.)
    assert_eq!(row("1000")["balance_cents"], 60_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn payroll_run_is_idempotent_on_id() {
    // Re-POST the same run id; the second call returns the existing
    // row without posting a second journal entry or inserting a
    // duplicate fact.
    let db = TestDb::new().await;
    seed_opening_cash(&db, 1_000_000).await;
    seed_employee(&db, "emp-idem-1", "finance", "controller").await;
    let body = json!({
        "id": "pr-idem",
        "run_date": "2026-04-15",
        "period_start": "2026-04-01",
        "period_end": "2026-04-14",
        "employer_tax_cents": 10_000,
        "provider": "adp",
        "lines": [
            {"employee_id": "emp-idem-1", "gross_cents": 100_000, "withheld_cents": 25_000, "net_cents": 75_000, "department": "finance", "role": "controller"},
        ],
    });

    for _ in 0..2 {
        let r = router(LedgerApiState {
            pool: db.pool.clone(),
            publisher: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
            // No read gate in tests; production wires one.
            policy: None,
        });
        let (status, _) = post_json(r, "/api/ledger/payroll-runs", body.clone()).await;
        assert_eq!(status, StatusCode::OK);
    }

    let (n_facts,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = 'finance.payroll.run' AND source_id = 'pr-idem'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n_facts, 1);

    let (n_lines,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM payroll_run_lines WHERE run_id = 'pr-idem'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n_lines, 1);

    // Gross on 6100 should reflect a single $1,000 posting, not $2,000.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let gross = rows.iter().find(|r| r["account_code"] == "6100").unwrap();
    assert_eq!(gross["balance_cents"], 100_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn payroll_run_detail_returns_header_plus_lines() {
    let db = TestDb::new().await;
    seed_opening_cash(&db, 1_000_000).await;
    seed_employee(&db, "emp-det-1", "sales", "sales-rep").await;
    seed_employee(&db, "emp-det-2", "sales", "sales-rep").await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_json(
        r,
        "/api/ledger/payroll-runs",
        json!({
            "id": "pr-det",
            "run_date": "2026-04-15",
            "period_start": "2026-04-01",
            "period_end": "2026-04-14",
            "employer_tax_cents": 20_000,
            "lines": [
                {"employee_id": "emp-det-1", "gross_cents": 200_000, "withheld_cents": 45_000, "net_cents": 155_000, "department": "sales", "role": "sales-rep"},
                {"employee_id": "emp-det-2", "gross_cents": 210_000, "withheld_cents": 47_000, "net_cents": 163_000, "department": "sales", "role": "sales-rep"},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/payroll-runs/pr-det").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "pr-det");
    assert_eq!(body["employee_count"], 2);
    let lines = body["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    // Ordered by employee_id ASC.
    assert_eq!(lines[0]["employee_id"], "emp-det-1");
    assert_eq!(lines[1]["employee_id"], "emp-det-2");
}

#[tokio::test(flavor = "multi_thread")]
async fn payroll_run_rejects_line_arithmetic_mismatch() {
    let db = TestDb::new().await;
    seed_employee(&db, "emp-bad", "sales", "sales-rep").await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_json(
        r,
        "/api/ledger/payroll-runs",
        json!({
            "id": "pr-bad",
            "run_date": "2026-04-15",
            "period_start": "2026-04-01",
            "period_end": "2026-04-14",
            "employer_tax_cents": 0,
            "lines": [
                // net should be gross - withheld = 75,000, but we wrote 90,000.
                {"employee_id": "emp-bad", "gross_cents": 100_000, "withheld_cents": 25_000, "net_cents": 90_000, "department": "sales", "role": "sales-rep"},
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// --- tax filings ----------------------------------------------------------

/// Accrue a sales-tax invoice directly (no commerce-side integration
/// test harness in this crate) so we can then remit it through
/// /api/ledger/tax-filings/{id}/remit and verify the journal entry.
async fn seed_sales_tax_accrual(db: &TestDb, source_id: &str, tax_cents: i64) {
    let payload = json!({
        "invoice_id": source_id,
        "amount_cents": 100_000 + tax_cents,
        "currency": "USD",
        "line_items": [
            {"category": "service", "amount_cents": 100_000, "currency": "USD"},
        ],
        "tax_lines": [
            {"account": "2300", "jurisdiction": "US-CA", "amount_cents": tax_cents},
        ],
    });
    seed_entry(
        db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &payload,
        source_id,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sales_tax_accrual_credits_2300() {
    // Issue an invoice with a sales-tax line; the ledger should credit
    // 2300 Sales Tax Payable for the tax amount + A/R for revenue+tax.
    let db = TestDb::new().await;
    seed_sales_tax_accrual(&db, "inv-tax-1", 7_250).await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(row("1100")["balance_cents"], 107_250);
    assert_eq!(row("4120")["balance_cents"], 100_000);
    assert_eq!(row("2300")["balance_cents"], 7_250);
}

#[tokio::test(flavor = "multi_thread")]
async fn tax_filing_remit_posts_finance_tax_remitted_and_drains_2300() {
    // Accrue → create filing → remit. Cash should drop by the tax
    // amount, 2300 should net to zero, and a `finance.tax.remitted`
    // fact should exist.
    let db = TestDb::new().await;
    seed_opening_cash(&db, 100_000).await;
    seed_sales_tax_accrual(&db, "inv-rem-1", 10_000).await;
    seed_sales_tax_accrual(&db, "inv-rem-2", 5_000).await;

    // Create the filing row.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_json(
        r,
        "/api/ledger/tax-filings",
        json!({
            "id": "tf-sales-US-CA-2026-03",
            "kind": "sales",
            "jurisdiction": "US-CA",
            "period_start": "2026-03-01",
            "period_end": "2026-03-31",
            "due_on": "2026-04-20",
            "amount_cents": 15_000,
            "liability_account": "2300",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Remit.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r,
        "/api/ledger/tax-filings/tf-sales-US-CA-2026-03/remit",
        json!({"filed_on": "2026-04-20"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["status"], "paid");

    // Trial balance.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .cloned()
            .unwrap_or(Value::Null)
    };
    // 2300 credited 15k, debited 15k → net zero.
    assert_eq!(row("2300")["balance_cents"], 0);
    // Cash: 100,000 opening − 15,000 tax remittance = 85,000.
    assert_eq!(row("1000")["balance_cents"], 85_000);

    // Exactly one finance.tax.remitted fact exists.
    let (n_facts,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = 'finance.tax.remitted' \
           AND source_id = 'tf-sales-US-CA-2026-03'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n_facts, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn tax_filing_remit_is_idempotent() {
    let db = TestDb::new().await;
    seed_opening_cash(&db, 100_000).await;
    seed_sales_tax_accrual(&db, "inv-idem-1", 12_000).await;

    let body = json!({
        "id": "tf-idem",
        "kind": "sales",
        "jurisdiction": "US-CA",
        "period_start": "2026-03-01",
        "period_end": "2026-03-31",
        "due_on": "2026-04-20",
        "amount_cents": 12_000,
        "liability_account": "2300",
    });
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_json(r, "/api/ledger/tax-filings", body).await;
    assert_eq!(status, StatusCode::OK);

    // Remit twice — second call should short-circuit.
    for _ in 0..2 {
        let r = router(LedgerApiState {
            pool: db.pool.clone(),
            publisher: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
            // No read gate in tests; production wires one.
            policy: None,
        });
        let (status, _) = post_json(
            r,
            "/api/ledger/tax-filings/tf-idem/remit",
            json!({"filed_on": "2026-04-20"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (n_facts,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE kind = 'finance.tax.remitted' AND source_id = 'tf-idem'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n_facts, 1);

    // 2300 still nets to zero (drained once, not twice).
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| rows.iter().find(|r| r["account_code"] == code).unwrap();
    assert_eq!(row("2300")["balance_cents"], 0);
    // Cash: 100,000 opening − 12,000 remittance = 88,000.
    assert_eq!(row("1000")["balance_cents"], 88_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn tax_filing_upsert_is_idempotent_on_period() {
    // Two POSTs with different PK ids but the same
    // (kind, jurisdiction, period) must collapse onto one row.
    let db = TestDb::new().await;
    let body1 = json!({
        "id": "tf-a",
        "kind": "sales",
        "jurisdiction": "US-TX",
        "period_start": "2026-03-01",
        "period_end": "2026-03-31",
        "due_on": "2026-04-20",
        "amount_cents": 1_000,
        "liability_account": "2300",
    });
    let body2 = json!({
        "id": "tf-b",
        "kind": "sales",
        "jurisdiction": "US-TX",
        "period_start": "2026-03-01",
        "period_end": "2026-03-31",
        "due_on": "2026-04-20",
        "amount_cents": 1_000,
        "liability_account": "2300",
    });

    for b in [body1, body2] {
        let r = router(LedgerApiState {
            pool: db.pool.clone(),
            publisher: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
            // No read gate in tests; production wires one.
            policy: None,
        });
        let (status, _) = post_json(r, "/api/ledger/tax-filings", b).await;
        assert_eq!(status, StatusCode::OK);
    }

    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM tax_filings \
         WHERE kind = 'sales' AND jurisdiction = 'US-TX' \
           AND period_start = '2026-03-01'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn income_tax_accrue_plus_remit_nets_2310_to_zero_and_lands_expense() {
    // Income-tax path: POST with accrue=true posts DR 6500 / CR 2310
    // alongside the tax_filings insert. Remit drains 2310 → Cash.
    // After both legs: 6500 debit-positive, 2310 zero, and Cash =
    // 1,000,000 opening − 500,000 remittance = 500,000.
    let db = TestDb::new().await;
    seed_opening_cash(&db, 1_000_000).await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_json(
        r,
        "/api/ledger/tax-filings",
        json!({
            "id": "tf-income-US-FEDERAL-2026-Q1",
            "kind": "income",
            "jurisdiction": "US-FEDERAL",
            "period_start": "2026-01-01",
            "period_end": "2026-03-31",
            "due_on": "2026-04-15",
            "amount_cents": 500_000,
            "liability_account": "2310",
            "accrue": true,
            "expense_account": "6500",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // After accrual: 2310 credit = 500k, 6500 debit = 500k.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(row("2310")["balance_cents"], 500_000);
    assert_eq!(row("6500")["balance_cents"], 500_000);

    // Remit — drains 2310 to Cash.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_json(
        r,
        "/api/ledger/tax-filings/tf-income-US-FEDERAL-2026-Q1/remit",
        json!({"filed_on": "2026-04-15"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance").await;
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| rows.iter().find(|r| r["account_code"] == code).unwrap();
    assert_eq!(row("2310")["balance_cents"], 0);
    assert_eq!(row("6500")["balance_cents"], 500_000);
    assert_eq!(row("1000")["balance_cents"], 500_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn tax_liability_summary_includes_accrued_and_next_due() {
    let db = TestDb::new().await;
    seed_sales_tax_accrual(&db, "inv-sum-1", 3_000).await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_json(
        r,
        "/api/ledger/tax-filings",
        json!({
            "id": "tf-sum-CA",
            "kind": "sales",
            "jurisdiction": "US-CA",
            "period_start": "2026-03-01",
            "period_end": "2026-03-31",
            "due_on": "2026-04-20",
            "amount_cents": 3_000,
            "liability_account": "2300",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(r, "/api/ledger/tax-liability").await;
    assert_eq!(status, StatusCode::OK);
    // 2300 shows the 3k credit balance.
    let libs = body["liabilities"].as_array().unwrap();
    let sales = libs
        .iter()
        .find(|l| l["account_code"] == "2300")
        .expect("2300 row");
    assert_eq!(sales["balance_cents"], 3_000);
    // Accrued filings list carries the row we just created; next_due is it.
    let accrued = body["accrued_filings"].as_array().unwrap();
    assert_eq!(accrued.len(), 1);
    assert_eq!(accrued[0]["id"], "tf-sum-CA");
    assert_eq!(body["next_due"]["id"], "tf-sum-CA");
}

// --- deferred revenue runoff ---------------------------------------------

async fn seed_runoff_account(db: &TestDb, id: &str) {
    sqlx::query(
        "INSERT INTO accounts \
            (id, name, director, city, state, tier, customer_since, territory_rep_id) \
         VALUES ($1, 'Runoff Clinic', 'Dr. Test', 'Austin', 'TX', 'silver', '2024-01-01', 'emp-001') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn seed_revenue_schedule(
    db: &TestDb,
    id: &str,
    account_id: &str,
    total_cents: i64,
    start: NaiveDate,
    end: NaiveDate,
    next_recognition_date: NaiveDate,
) {
    sqlx::query(
        "INSERT INTO revenue_schedules \
             (id, source_kind, source_id, account_id, revenue_category, \
              revenue_account, deferred_account, total_cents, start_date, \
              end_date, frequency, recognized_to_date_cents, \
              next_recognition_date, status) \
         VALUES ($1, 'service_agreement', $1, $2, 'contracts', \
                 '4140', '2200', $3, $4, $5, 'monthly', 0, $6, 'active')",
    )
    .bind(id)
    .bind(account_id)
    .bind(total_cents)
    .bind(start)
    .bind(end)
    .bind(next_recognition_date)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn deferred_revenue_runoff_projects_active_schedules() {
    let db = TestDb::new().await;
    seed_runoff_account(&db, "prac-runoff").await;
    // Two active schedules: a $1,200/12-month contract starting May 2026,
    // and a $600/6-month contract starting June 2026 with cursor pre-
    // advanced by 2 periods (so only 4 periods remain).
    seed_revenue_schedule(
        &db,
        "rs-a",
        "prac-runoff",
        120_000,
        NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        NaiveDate::from_ymd_opt(2027, 4, 30).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 31).unwrap(),
    )
    .await;
    seed_revenue_schedule(
        &db,
        "rs-b",
        "prac-runoff",
        60_000,
        NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
        NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(),
    )
    .await;
    // Update rs-b's recognized_to_date to reflect the 2-period advance.
    sqlx::query("UPDATE revenue_schedules SET recognized_to_date_cents = $1 WHERE id = 'rs-b'")
        .bind(20_000i64)
        .execute(&db.pool)
        .await
        .unwrap();

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = get(
        r,
        "/api/ledger/deferred-revenue-runoff?as_of=2026-05-15&months=12",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(body["as_of"], "2026-05-15");
    assert_eq!(body["horizon_months"], 12);
    // rs-a contributes $100/mo × 12 = $1,200. rs-b contributes $100/mo
    // across 4 remaining periods (Sep, Oct, Nov + cursor starting Aug).
    // Wait — next_recognition_date above is Aug 31, and we advanced 2
    // periods (Jun, Jul), so remaining periods post Aug/Sep/Oct/Nov.
    // Total schedule-remaining = 120_000 + 40_000 = 160_000.
    assert_eq!(body["schedules_remaining_cents"], 160_000);
    let months = body["months"].as_array().unwrap();
    assert_eq!(months.len(), 12);
    // First bucket is 2026-05.
    assert_eq!(months[0]["month"], "2026-05-01");
    // Every bucket from May onward carries rs-a's $100; Aug–Nov adds
    // rs-b's $100 on top.
    let within: i64 = months
        .iter()
        .map(|m| m["amount_cents"].as_i64().unwrap())
        .sum();
    assert_eq!(within, 160_000);
    assert_eq!(body["beyond_horizon_cents"], 0);
    // No ledger activity means the deferred balance is zero and the
    // drift is exactly the negative of schedules_remaining_cents.
    assert_eq!(body["deferred_account_balance_cents"], 0);
    assert_eq!(body["drift_cents"], -160_000);
}

// --- auditor write-gate ---------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn auditor_role_is_rejected_from_every_ledger_write() {
    // Sweeps every POST endpoint on the ledger router. An auditor
    // session calling any write should get a 403 before any DB work
    // runs — parallel to the UI-level `role === 'auditor'` hiding in
    // `apps/web/src/finance/FinancePage.svelte`.
    let db = TestDb::new().await;
    let mk = || {
        router(LedgerApiState {
            pool: db.pool.clone(),
            publisher: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
            // No read gate in tests; production wires one.
            policy: None,
        })
    };

    // Use a valid-shape payload where possible; when it's wrong the
    // handler ought to 400 *for non-auditor* callers, but we never
    // get that far — the auditor reject fires before validation.
    let cases: Vec<(&str, Value)> = vec![
        (
            "/api/ledger/periods/00000000-0000-0000-0000-000000000000/lock",
            json!({"locked_by": "emp-auditor"}),
        ),
        (
            "/api/ledger/periods/00000000-0000-0000-0000-000000000000/unlock",
            json!({}),
        ),
        (
            "/api/ledger/journal-entries",
            json!({
                "posted_on": "2026-04-01",
                "lines": [
                    {"account_code": "1000", "debit_cents": 1, "credit_cents": 0, "memo": null},
                    {"account_code": "4100", "debit_cents": 0, "credit_cents": 1, "memo": null},
                ],
            }),
        ),
        (
            "/api/ledger/bank-settlements",
            json!({
                "id": "no-write",
                "invoice_id": "inv-1",
                "account_id": "p-1",
                "amount_cents": 100,
                "currency": "USD",
                "received_on": "2026-04-01",
                "bank_provider": "chase",
                "payment_method": "ach",
            }),
        ),
        (
            "/api/ledger/bank-settlements/no-write/settle",
            json!({"settled_on": "2026-04-02"}),
        ),
        ("/api/ledger/bank-settlements/sweep", json!({})),
        (
            "/api/ledger/payroll-runs",
            json!({
                "id": "no-write",
                "run_date": "2026-04-01",
                "period_start": "2026-04-01",
                "period_end": "2026-04-15",
                "employer_tax_cents": 0,
                "provider": "gusto",
                "lines": [],
            }),
        ),
        (
            "/api/ledger/tax-filings",
            json!({
                "id": "no-write",
                "kind": "sales",
                "jurisdiction": "CA",
                "period_start": "2026-04-01",
                "period_end": "2026-04-30",
                "due_on": "2026-05-15",
                "amount_cents": 100,
                "liability_account": "2300",
                "provider": "avalara",
                "accrue": false,
                "expense_account": null,
            }),
        ),
        (
            "/api/ledger/tax-filings/no-write/remit",
            json!({"filed_on": "2026-04-10"}),
        ),
        (
            "/api/ledger/revenue-schedules",
            json!({
                "id": "rs-no-write",
                "source_kind": "service_agreement",
                "source_id": "sa-no-write",
                "account_id": "p-1",
                "revenue_category": "contracts",
                "revenue_account": "4140",
                "deferred_account": "2200",
                "total_cents": 1200,
                "start_date": "2026-05-01",
                "end_date": "2027-04-30",
                "frequency": "monthly",
                "next_recognition_date": "2026-05-31",
            }),
        ),
    ];

    for (path, body) in cases {
        let (status, _) = post_as_auditor(mk(), path, body).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "POST {path} should 403 for auditor role",
        );
    }
}

// --- year-end close ------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn close_yearly_period_posts_closing_entries_and_locks() {
    let db = TestDb::new().await;

    // Seed some FY-2026 activity: $5,000 revenue + $2,000 expense.
    // Two revenue lines ($3k service + $2k new-sales) so we exercise
    // the multi-account path. Expense side uses the payroll rule
    // (DR 6100 gross + CR 2150 + 1000), but since that mingles cash
    // and payroll-liability, use manual entries for the expense so
    // we can control exactly which expense accounts light up.
    let invoice_payload = json!({
        "invoice_id": "inv-close-1",
        "amount_cents": 5_000,
        "line_items": [
            { "category": "service", "amount_cents": 3_000 },
            { "category": "new-sales", "amount_cents": 2_000 },
        ],
    });
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 6, 15).unwrap(),
        &invoice_payload,
        "inv-close-1",
    )
    .await;
    // Collect the invoice so Cash is funded before the rent JE below
    // credits it — the negative-cash guard refuses an overdraft. This
    // moves AR → Cash without touching revenue/expense/RE, so the
    // closing assertions below are unaffected.
    seed_entry(
        &db,
        "finance.invoice.paid",
        NaiveDate::from_ymd_opt(2026, 6, 15).unwrap(),
        &json!({ "invoice_id": "inv-close-1", "amount_cents": 5_000 }),
        "inv-close-1",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r,
        "/api/ledger/journal-entries",
        json!({
            "posted_on": "2026-06-15",
            "memo": "Rent expense test",
            "lines": [
                { "account_code": "6200", "debit_cents": 2_000, "credit_cents": 0, "memo": "rent" },
                { "account_code": "1000", "debit_cents": 0, "credit_cents": 2_000, "memo": "cash out" },
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manual entry body={body:?}");

    // Create the yearly period for FY 2026.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(r, "/api/ledger/periods", json!({"year": 2026})).await;
    assert_eq!(status, StatusCode::OK, "create period body={body:?}");
    let period_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["kind"], "year");

    // Close the period.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let path = format!("/api/ledger/periods/{period_id}/close");
    let (status, body) = post_json(r, &path, json!({"closed_by": "emp-close-test"})).await;
    assert_eq!(status, StatusCode::OK, "close body={body:?}");
    assert_eq!(body["status"], "locked");
    assert_eq!(body["revenue_closed_cents"], 5_000);
    assert_eq!(body["expense_closed_cents"], 2_000);
    assert_eq!(body["net_income_cents"], 3_000);
    assert!(!body["checksum"].as_str().unwrap().is_empty());

    // Trial balance as-of Dec 31 2026 must show revenue + expense
    // accounts zeroed and RE increased by net income.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance?as_of=2026-12-31").await;
    let rows = tb["rows"].as_array().unwrap();
    let balance = |code: &str| -> i64 {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .and_then(|r| r["balance_cents"].as_i64())
            .unwrap_or(0)
    };
    assert_eq!(balance("4100"), 0, "new-sales revenue closed");
    assert_eq!(balance("4120"), 0, "service revenue closed");
    assert_eq!(balance("6200"), 0, "rent expense closed");
    assert_eq!(balance("3000"), 3_000, "RE absorbed net income");

    // Idempotent — re-closing returns the existing checksum.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status2, body2) = post_json(r, &path, json!({})).await;
    assert_eq!(status2, StatusCode::OK);
    assert_eq!(body2["status"], "locked");
    assert_eq!(body2["checksum"], body["checksum"]);

    // No WIP activity was seeded, so the close reports a clean WIP.
    assert_eq!(body["wip_variance_cents"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn close_yearly_period_writes_off_wip_variance() {
    // The capstone-year shape in miniature: the year's production
    // flow leaves a residual balance on WIP (1310) that the close
    // must write off to retained earnings — 1310 ends the year at
    // zero, and the drained COGS account is NOT re-inflated.
    let db = TestDb::new().await;

    // Revenue $5,000 (issued + collected so cash is funded).
    let invoice_payload = json!({
        "invoice_id": "inv-wip-close-1",
        "amount_cents": 5_000,
        "line_items": [{ "category": "new-sales", "amount_cents": 5_000 }],
    });
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 6, 15).unwrap(),
        &invoice_payload,
        "inv-wip-close-1",
    )
    .await;
    seed_entry(
        &db,
        "finance.invoice.paid",
        NaiveDate::from_ymd_opt(2026, 6, 15).unwrap(),
        &json!({ "invoice_id": "inv-wip-close-1", "amount_cents": 5_000 }),
        "inv-wip-close-1",
    )
    .await;

    // Expense $2,000 (manual rent JE against the funded cash).
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r,
        "/api/ledger/journal-entries",
        json!({
            "posted_on": "2026-06-15",
            "memo": "Rent expense",
            "lines": [
                { "account_code": "6200", "debit_cents": 2_000, "credit_cents": 0, "memo": "rent" },
                { "account_code": "1000", "debit_cents": 0, "credit_cents": 2_000, "memo": "cash out" },
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rent entry body={body:?}");

    // Residual WIP: $250 sitting on 1310 at year-end (opening-WIP
    // shape — DR 1310 / CR 3000, no cash guard involved).
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(
        r,
        "/api/ledger/journal-entries",
        json!({
            "posted_on": "2026-11-30",
            "memo": "Opening WIP",
            "lines": [
                { "account_code": "1310", "debit_cents": 250, "credit_cents": 0, "memo": "wip residual" },
                { "account_code": "3000", "debit_cents": 0, "credit_cents": 250, "memo": "equity seed" },
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wip entry body={body:?}");

    // Create + close FY 2026 (default body → wip_account 1310).
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, body) = post_json(r, "/api/ledger/periods", json!({"year": 2026})).await;
    assert_eq!(status, StatusCode::OK, "create period body={body:?}");
    let period_id = body["id"].as_str().unwrap().to_string();

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let path = format!("/api/ledger/periods/{period_id}/close");
    let (status, body) = post_json(r, &path, json!({"closed_by": "emp-wip-close"})).await;
    assert_eq!(status, StatusCode::OK, "close body={body:?}");
    assert_eq!(body["status"], "locked");
    assert_eq!(body["net_income_cents"], 3_000);
    assert_eq!(body["wip_variance_cents"], 250);

    // Trial balance as-of Dec 31: WIP zeroed, P&L accounts zeroed,
    // RE = opening 250 + net income 3,000 − write-off 250 = 3,000.
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (_, tb) = get(r, "/api/ledger/trial-balance?as_of=2026-12-31").await;
    let rows = tb["rows"].as_array().unwrap();
    let balance = |code: &str| -> i64 {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .and_then(|r| r["balance_cents"].as_i64())
            .unwrap_or(0)
    };
    assert_eq!(balance("1310"), 0, "WIP written off at close");
    assert_eq!(balance("4100"), 0, "revenue closed");
    assert_eq!(balance("6200"), 0, "expense closed");
    assert_eq!(
        balance("5100"),
        0,
        "COGS not re-inflated by the WIP write-off"
    );
    assert_eq!(
        balance("3000"),
        3_000,
        "RE = NI; the WIP residual moved through, not into, RE"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn close_monthly_period_is_rejected() {
    // Only yearly periods can be closed via /close — monthly use /lock.
    let db = TestDb::new().await;
    let p = json!({"invoice_id": "i-monthly", "amount_cents": 100, "line_items": [{"category": "service", "amount_cents": 100}]});
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        &p,
        "i-monthly",
    )
    .await;
    let monthly_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = '2026-03-01'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let path = format!("/api/ledger/periods/{monthly_id}/close");
    let (status, _) = post_json(r, &path, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn deferred_revenue_runoff_clamps_horizon_and_handles_empty_db() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    // months=0 should clamp up to 1; months=999 should clamp down to 60.
    let (status, body) = get(r, "/api/ledger/deferred-revenue-runoff?months=999").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["horizon_months"], 60);
    assert_eq!(body["schedules_remaining_cents"], 0);
    assert_eq!(body["beyond_horizon_cents"], 0);
    let months = body["months"].as_array().unwrap();
    assert_eq!(months.len(), 60);
    assert!(
        months
            .iter()
            .all(|m| m["amount_cents"].as_i64().unwrap() == 0)
    );
}

/// Property test for /api/ledger/balance-sheet: for ANY valid
/// set of journal entries (where trial balance is balanced by
/// construction), the BS endpoint's response must satisfy
/// A = L + E. Closes the 2026-05-29 bug class structurally —
/// the previous hand-coded "current-fiscal-year start" date
/// filter on the YTD-net-income calc silently excluded
/// pre-Jan-1 activity, leaving the BS off by months of
/// revenue + expense on any tenant whose first year hadn't
/// closed yet. The kind-bucketed single-query refactor (no
/// date filter on revenue/expense — period close zeroes them
/// at the source) makes the property hold by construction;
/// this test locks it in.
#[tokio::test(flavor = "multi_thread")]
async fn balance_sheet_holds_across_periods() {
    let db = TestDb::new().await;

    // Seed activity in TWO different calendar years so the
    // previous calendar-year-start filter would have orphaned
    // the older half. With the fix, both periods contribute
    // to unclosed net income.
    let p_issue_old = json!({
        "invoice_id": "old", "amount_cents": 5_000,
        "line_items": [{"category": "service", "amount_cents": 5_000}]
    });
    let p_paid_old = json!({"invoice_id": "old", "amount_cents": 5_000});
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2025, 6, 15).unwrap(),
        &p_issue_old,
        "old",
    )
    .await;
    seed_entry(
        &db,
        "finance.invoice.paid",
        NaiveDate::from_ymd_opt(2025, 7, 20).unwrap(),
        &p_paid_old,
        "old",
    )
    .await;

    let p_issue_new = json!({
        "invoice_id": "new", "amount_cents": 3_000,
        "line_items": [{"category": "service", "amount_cents": 3_000}]
    });
    let p_paid_new = json!({"invoice_id": "new", "amount_cents": 3_000});
    seed_entry(
        &db,
        "finance.invoice.issued",
        NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
        &p_issue_new,
        "new",
    )
    .await;
    seed_entry(
        &db,
        "finance.invoice.paid",
        NaiveDate::from_ymd_opt(2026, 3, 12).unwrap(),
        &p_paid_new,
        "new",
    )
    .await;

    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });

    // Trial balance: every JE balances by construction (the
    // db-side trigger enforces it). So total_debits ==
    // total_credits across both periods.
    let (tb_status, tb_body) = get(r.clone(), "/api/ledger/trial-balance").await;
    assert_eq!(tb_status, StatusCode::OK);
    assert_eq!(tb_body["balanced"], true);

    // Balance sheet against an as_of that includes both periods
    // — and a couple of intermediate as_of points — must each
    // satisfy A = L + E. Pre-fix, the as_of=2026-05-29 case
    // would have computed YTD net income from 2026-01-01 only,
    // orphaning the 2025-06-15 revenue and 2025-07-20 cash
    // receipt's expense-side legs.
    for as_of in [
        "2025-08-01", // mid-prior-period
        "2026-01-15", // just after calendar rollover
        "2026-05-29", // matches the bug-finding date
    ] {
        let path = format!("/api/ledger/balance-sheet?as_of={as_of}");
        let (status, body) = get(r.clone(), &path).await;
        assert_eq!(status, StatusCode::OK, "as_of={as_of}");
        let a = body["total_assets_cents"].as_i64().unwrap();
        let l = body["total_liabilities_cents"].as_i64().unwrap();
        let e = body["total_equity_cents"].as_i64().unwrap();
        assert_eq!(
            a - (l + e),
            0,
            "BS endpoint A != L + E at as_of={as_of}: A={a}, L={l}, E={e}"
        );
    }
}

// --- ledger bills (general AP / OpEx) -------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn bills_approve_routes_by_category_then_pay_run_drains_ap() {
    let db = TestDb::new().await;
    seed_opening_cash(&db, 1_000_000).await;

    let state = || LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    };

    // Approve a rent bill (→ 6200) and a utilities bill (→ 6300). The free
    // `bill_category` routes the debit via bill_accounts.toml.
    let (s1, b1) = post_json(
        router(state()),
        "/api/ledger/bills",
        json!({"id": "bill-rent", "vendor": "Acme Realty", "bill_category": "rent", "amount_cents": 120_000}),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED, "body: {b1}");
    let (s2, _) = post_json(
        router(state()),
        "/api/ledger/bills",
        json!({"id": "bill-util", "vendor": "City Power", "bill_category": "utilities", "amount_cents": 80_000}),
    )
    .await;
    assert_eq!(s2, StatusCode::CREATED);

    // After approval: 6200 + 6300 carry the expense; 2100 A/P = 200_000.
    let (_, tb) = get(router(state()), "/api/ledger/trial-balance").await;
    assert_eq!(tb["balanced"], true);
    let rows = tb["rows"].as_array().unwrap();
    let row = |code: &str| {
        rows.iter()
            .find(|r| r["account_code"] == code)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(row("6200")["debit_total_cents"], 120_000);
    assert_eq!(row("6300")["debit_total_cents"], 80_000);
    assert_eq!(row("2100")["credit_total_cents"], 200_000);
    assert_eq!(row("2100")["balance_cents"], 200_000);

    // Pay-run settles every approved bill: A/P drains, Cash drops.
    let (s3, payrun) = post_json(router(state()), "/api/ledger/bills/pay-run", json!({})).await;
    assert_eq!(s3, StatusCode::OK);
    assert_eq!(payrun["paid_count"], 2);
    assert_eq!(payrun["total_paid_cents"], 200_000);

    let (_, tb2) = get(router(state()), "/api/ledger/trial-balance").await;
    assert_eq!(tb2["balanced"], true);
    let rows2 = tb2["rows"].as_array().unwrap();
    let row2 = |code: &str| {
        rows2
            .iter()
            .find(|r| r["account_code"] == code)
            .cloned()
            .unwrap_or(Value::Null)
    };
    // A/P fully drained back to zero; Cash = opening 1_000_000 − 200_000.
    assert_eq!(row2("2100")["balance_cents"], 0);
    assert_eq!(row2("1000")["balance_cents"], 800_000);

    // The subledger now lists both, paid.
    let (_, listed) = get(router(state()), "/api/ledger/bills?status=paid").await;
    assert_eq!(listed.as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn bills_approve_is_idempotent_on_id() {
    let db = TestDb::new().await;
    let state = || LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    };
    let body =
        json!({"id": "bill-x", "vendor": "V", "bill_category": "rent", "amount_cents": 50_000});
    let (s1, _) = post_json(router(state()), "/api/ledger/bills", body.clone()).await;
    assert_eq!(s1, StatusCode::CREATED);
    // Re-POST the same id: the bill is returned without a second posting.
    let (_, _) = post_json(router(state()), "/api/ledger/bills", body).await;

    let (_, tb) = get(router(state()), "/api/ledger/trial-balance").await;
    assert_eq!(tb["balanced"], true);
    let rows = tb["rows"].as_array().unwrap();
    let rent = rows.iter().find(|r| r["account_code"] == "6200").unwrap();
    assert_eq!(rent["debit_total_cents"], 50_000); // not double-posted to 100_000
}

#[tokio::test(flavor = "multi_thread")]
async fn bills_reject_auditor_writes() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_as_auditor(
        r,
        "/api/ledger/bills",
        json!({"id": "b", "vendor": "V", "bill_category": "rent", "amount_cents": 1000}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// --- keg deposit settlements (93f936b9: full balance-sheet keg model) ------

#[tokio::test(flavor = "multi_thread")]
async fn keg_deposit_settlement_books_both_legs_and_drains_the_liability() {
    let db = TestDb::new().await;
    let state = || LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    };
    let body = json!({
        "job_id": "job-keg-1",
        "account_id": "account-00042",
        "kegs_out": 10,
        "kegs_returned": 7,
        "kegs_lost": 3,
        "deposit_cents": 30_000,
        "shipped_on": "2026-03-01",
        "returned_on": "2026-03-15",
    });
    let (status, resp) = post_json(
        router(state()),
        "/api/ledger/keg-deposit-settlements",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");

    // Charge leg: DR 1000 / CR 2400 at the fleet-out date.
    let charge_lines: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT a.code, l.debit_cents, l.credit_cents \
         FROM gl_journal_lines l \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE e.fact_id = $1::uuid ORDER BY l.sort_order",
    )
    .bind(resp["charge_fact_id"].as_str().unwrap())
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        charge_lines,
        vec![
            ("1000".to_string(), 30_000i64, 0i64),
            ("2400".to_string(), 0i64, 30_000i64),
        ]
    );
    let charge_date: chrono::NaiveDate =
        sqlx::query_scalar("SELECT posted_on FROM gl_journal_entries WHERE fact_id = $1::uuid")
            .bind(resp["charge_fact_id"].as_str().unwrap())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(charge_date, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());

    // Release leg: DR 2400 full deposit / CR 1000 refund + CR 4150
    // forfeiture, at the return date.
    let release_lines: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT a.code, l.debit_cents, l.credit_cents \
         FROM gl_journal_lines l \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE e.fact_id = $1::uuid ORDER BY l.sort_order",
    )
    .bind(resp["release_fact_id"].as_str().unwrap())
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        release_lines,
        vec![
            ("2400".to_string(), 30_000i64, 0i64),
            ("1000".to_string(), 0i64, 21_000i64),
            ("4150".to_string(), 0i64, 9_000i64),
        ]
    );

    // The liability drained exactly: 2400 nets to zero for this fleet.
    let (tb_status, tb) = get(router(state()), "/api/ledger/trial-balance").await;
    assert_eq!(tb_status, StatusCode::OK);
    let rows = tb["rows"].as_array().unwrap();
    let keg = rows.iter().find(|r| r["account_code"] == "2400").unwrap();
    assert_eq!(keg["debit_total_cents"], 30_000);
    assert_eq!(keg["credit_total_cents"], 30_000);

    // Redelivery: the same settlement re-POSTs as a no-op 200 — the
    // facts key on the job id, so nothing double-books.
    let (status2, resp2) =
        post_json(router(state()), "/api/ledger/keg-deposit-settlements", body).await;
    assert_eq!(status2, StatusCode::OK, "body: {resp2}");
    let fact_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM financial_facts WHERE kind LIKE 'finance.keg_deposit.%'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(fact_count, 2, "redelivery must not mint new facts");
}

#[tokio::test(flavor = "multi_thread")]
async fn keg_deposit_settlement_rejects_non_conserving_counts_as_422() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    // 1 out, 1 returned AND 1 lost — the shape the free-field faker
    // used to produce (feedback 52f49cc7). Deterministic data error →
    // 422 so the dispatcher Terms instead of NAK-retrying.
    let (status, body) = post_json(
        r,
        "/api/ledger/keg-deposit-settlements",
        json!({
            "job_id": "job-keg-bad",
            "kegs_out": 1,
            "kegs_returned": 1,
            "kegs_lost": 1,
            "deposit_cents": 3_000,
            "shipped_on": "2026-03-01",
            "returned_on": "2026-03-15",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    let nothing: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM financial_facts WHERE kind LIKE 'finance.keg_deposit.%'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(nothing, 0, "a rejected settlement must book nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn keg_deposit_settlement_rejects_auditor_writes() {
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, _) = post_as_auditor(
        r,
        "/api/ledger/keg-deposit-settlements",
        json!({
            "job_id": "job-keg-aud",
            "kegs_out": 2,
            "kegs_returned": 2,
            "kegs_lost": 0,
            "deposit_cents": 6_000,
            "shipped_on": "2026-03-01",
            "returned_on": "2026-03-15",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn keg_deposit_same_day_settlement_survives_a_rebuild() {
    // The nasty edge: a fleet reconciled the same day it shipped means
    // charge and release share `happened_on`, and both facts were
    // written in ONE transaction. The `clock_timestamp()` stamp on
    // `recorded_at` (keg_deposits.rs) is what pins the replay order —
    // without it the release could re-post before the charge it
    // drains, and the "1000 Cash must not go negative" guard would
    // abort the rebuild on an empty ledger.
    let db = TestDb::new().await;
    let r = router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // No read gate in tests; production wires one.
        policy: None,
    });
    let (status, resp) = post_json(
        r,
        "/api/ledger/keg-deposit-settlements",
        json!({
            "job_id": "job-keg-sameday",
            "kegs_out": 4,
            "kegs_returned": 3,
            "kegs_lost": 1,
            "deposit_cents": 12_000,
            "shipped_on": "2026-03-10",
            "returned_on": "2026-03-10",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");

    // Replay from facts alone: drop the projected entries and rebuild.
    let report = boss_ledger::rebuild(&db.pool).await.unwrap();
    assert!(report.is_balanced(), "{report:?}");
    let (debits, credits): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(l.debit_cents), 0)::bigint, COALESCE(SUM(l.credit_cents), 0)::bigint \
         FROM gl_journal_lines l JOIN gl_accounts a ON a.id = l.account_id \
         WHERE a.code = '2400'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(debits, 12_000, "liability drains on rebuild too");
    assert_eq!(credits, 12_000);
}
