//! `GET /api/commerce/open-ar` — open receivables per account, summed
//! by the service (backlog 5257bfa9).
//!
//! /ux/accounts used to compute Open AR by reading
//! `/api/commerce/invoices?limit=10000` and summing the unpaid rows in
//! the browser. The list clamps `limit` to 1,000, so past a thousand
//! invoices the money figure was short, and nothing on the page said
//! so. The figure is an aggregate; the service computes it over every
//! row, and the page reads the answer, not the rows.

mod common;

use boss_commerce::types::*;
use boss_testing::TestRequest;
use common::{CommerceTestApp, invoice_fixture};

fn inv(id: &str, account: &str, cents: i64, status: &str) -> Invoice {
    let mut i = invoice_fixture(id);
    i.account_id = account.to_string();
    i.amount_cents = cents;
    i.line_items[0].amount_cents = cents;
    i.status = status.into();
    if status == InvoiceStatus::PAID {
        i.paid_on = chrono::NaiveDate::from_ymd_opt(2025, 4, 1);
    }
    i
}

async fn open_ar(app: &CommerceTestApp) -> serde_json::Value {
    let resp = TestRequest::get("/api/commerce/open-ar")
        .send(&app.router)
        .await;
    assert_eq!(
        resp.status,
        200,
        "{}",
        String::from_utf8_lossy(&resp.body_bytes)
    );
    serde_json::from_slice(&resp.body_bytes).unwrap()
}

/// Outstanding and past-due are owed; paid is not, and neither is
/// written-off — the write-off credits 1100 A/R (InvoicesTab's
/// "outstanding" is the same two statuses out). An account with
/// nothing open has no row.
#[tokio::test]
async fn open_ar_sums_outstanding_and_past_due_per_account() {
    let app = CommerceTestApp::with_invoices(vec![
        inv("inv-1", "acct-zed", 125_000, InvoiceStatus::OUTSTANDING),
        inv("inv-2", "acct-zed", 2_550, InvoiceStatus::PAST_DUE),
        inv("inv-3", "acct-zed", 5_000, InvoiceStatus::PAID),
        inv("inv-4", "acct-zed", 9_900, InvoiceStatus::WRITTEN_OFF),
        inv("inv-5", "acct-anchor", 40_000, InvoiceStatus::PAID),
        inv("inv-6", "acct-bay", 700, InvoiceStatus::OUTSTANDING),
    ]);

    let body = open_ar(&app).await;
    assert_eq!(
        body["data"],
        serde_json::json!([
            {"account_id": "acct-bay", "open_ar_cents": 700, "open_count": 1},
            {"account_id": "acct-zed", "open_ar_cents": 127_550, "open_count": 2},
        ])
    );
    assert_eq!(body["total"], 2, "total counts the accounts with open AR");
}

/// The defect itself: 1,500 open invoices on one account, more than
/// the invoices list will hand over in one page. The sum is exact.
#[tokio::test]
async fn open_ar_is_exact_past_the_list_endpoints_page_cap() {
    let invoices: Vec<Invoice> = (0..1_500)
        .map(|n| {
            inv(
                &format!("inv-{n}"),
                "acct-big",
                100,
                InvoiceStatus::OUTSTANDING,
            )
        })
        .collect();
    let app = CommerceTestApp::with_invoices(invoices);

    let list = TestRequest::get("/api/commerce/invoices?limit=10000")
        .send(&app.router)
        .await;
    let list: serde_json::Value = serde_json::from_slice(&list.body_bytes).unwrap();
    assert_eq!(list["total"], 1_500);
    assert_eq!(
        list["data"].as_array().map(Vec::len),
        Some(1_000),
        "the list clamps its page — which is why the page cannot sum it"
    );

    let body = open_ar(&app).await;
    assert_eq!(
        body["data"],
        serde_json::json!([
            {"account_id": "acct-big", "open_ar_cents": 150_000, "open_count": 1_500},
        ])
    );
}

#[tokio::test]
async fn open_ar_with_no_invoices_is_an_empty_envelope() {
    let app = CommerceTestApp::new();
    let body = open_ar(&app).await;
    assert_eq!(body["data"], serde_json::json!([]));
    assert_eq!(body["total"], 0);
}
