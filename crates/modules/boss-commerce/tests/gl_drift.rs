//! Drift test: a manual revenue credit posted through the ledger's
//! `POST /api/ledger/journal-entries` endpoint must surface in both
//! `revenue_ttm` and `revenue_by_month` of `GET /api/commerce/summary`.
//!
//! This is the regression guard for sourcing the commerce summary from
//! the GL rather than from `invoice_line_items`. A non-invoice posting
//! (what this test simulates) would be invisible if the query ever
//! walked back to the invoice table.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use boss_commerce::PgCommerce;
use boss_commerce::http::{CommerceApiState, router as commerce_router};
use boss_commerce::types::InvoiceSummary;
use boss_ledger::http::{LedgerApiState, router as ledger_router};
use boss_people_client::{FakePeopleClient, PeopleClient};
use boss_policy_client::{PermissivePolicyClient, PolicyClient};
use boss_testing::{TestDb, TestRequest};
use chrono::{Datelike, Utc};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn manual_revenue_credit_flows_into_commerce_summary() {
    let db = TestDb::new().await;

    let ledger = ledger_router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // The ledger read gate is not this test's subject (boss-ledger's
        // tests/the_ledger_read_gate.rs pins it).
        policy: Arc::new(PermissivePolicyClient),
    });

    let posted_on = Utc::now().date_naive();
    let amount_cents: i64 = 777_000;

    TestRequest::post("/api/ledger/journal-entries")
        .json(&json!({
            "posted_on": posted_on.format("%Y-%m-%d").to_string(),
            "memo": "drift test: manual revenue credit",
            "created_by": "drift-test",
            "lines": [
                {"account_code": "1100", "debit_cents":  amount_cents, "memo": "A/R"},
                {"account_code": "4100", "credit_cents": amount_cents, "memo": "wholesale"},
            ],
        }))
        .send(&ledger)
        .await
        .assert_status(StatusCode::OK);

    // Commerce summary on the same DB. `invoices` is empty, so every cent
    // of revenue the summary reports has to come from the GL — which is
    // the whole point of sourcing from `gl_journal_lines`.
    let commerce = Arc::new(PgCommerce::new(db.pool.clone()));
    let people: Arc<dyn PeopleClient> = Arc::new(FakePeopleClient::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(PermissivePolicyClient);
    let commerce_app = commerce_router(CommerceApiState {
        commerce,
        publisher: None,
        people_client: people,
        policy: Some(policy),
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    });

    let resp = TestRequest::get("/api/commerce/summary")
        .send(&commerce_app)
        .await;
    resp.assert_status(StatusCode::OK);
    let summary: InvoiceSummary = resp.assert_json();

    // category surface is the gl_accounts.name column (free-text
    // tenant-flavoured). The platform schema seeds 4100 with the
    // brewery name "Revenue — Wholesale Beer Sales"; this test
    // only cares that THE row for 4100 carries the manual credit.
    let row_4100 = summary
        .revenue_ttm
        .iter()
        .find(|c| c.category.contains("Wholesale") || c.category == "wholesale")
        .expect("revenue_ttm missing 4100 category row");
    assert_eq!(
        row_4100.revenue_cents, amount_cents,
        "revenue_ttm row for 4100 should reflect the manual credit"
    );
    assert_eq!(summary.total_revenue_ttm_cents, amount_cents);

    let month_label = format!("{:04}-{:02}-01", posted_on.year(), posted_on.month());
    let month_row = summary
        .revenue_by_month
        .iter()
        .find(|m| m.month == month_label)
        .unwrap_or_else(|| panic!("revenue_by_month missing entry for {month_label}"));
    assert_eq!(
        month_row.revenue_cents, amount_cents,
        "revenue_by_month[{month_label}] should reflect the manual credit"
    );
}
