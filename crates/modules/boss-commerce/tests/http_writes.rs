//! HTTP-level write path tests for the commerce service.

mod common;

use axum::http::StatusCode;
use boss_testing::TestRequest;
use common::{CommerceTestApp, invoice_fixture};

// ---------------------------------------------------------------------------
// POST /api/commerce/invoices/create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_invoice_returns_201_on_valid_input() {
    let app = CommerceTestApp::new();
    let inv = invoice_fixture("inv-create-1");

    let resp = TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_invoice_emits_commerce_invoice_created_event() {
    let app = CommerceTestApp::new();
    let inv = invoice_fixture("inv-event-1");

    TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    let event = app.assert_recorded("commerce.invoice.created");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("inv-event-1"),
    );
}

/// Backlog 9d2af748: a second POST under one id — a retried click, a
/// redelivered issue — writes nothing, records no second
/// `commerce.invoice.created` (the event that drives the finished-goods
/// consume), and answers 200 saying so rather than 201.
#[tokio::test]
async fn post_invoice_twice_answers_the_invoice_once_and_records_one_event() {
    let app = CommerceTestApp::new();
    let inv = invoice_fixture("inv-twice-1");

    TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);
    let again = TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await;
    again.assert_status(StatusCode::OK);
    let body: serde_json::Value = again.assert_json();
    assert_eq!(body["already_created"], true, "{body}");
    assert_eq!(app.recorded_of_kind("commerce.invoice.created").len(), 1);

    // A different invoice under the same id is refused, naming why.
    let mut other = inv.clone();
    other.amount_cents = 1;
    other.line_items[0].amount_cents = 1;
    let refused = TestRequest::post("/api/commerce/invoices/create")
        .json(&other)
        .send(&app.router)
        .await;
    refused.assert_status(StatusCode::CONFLICT);
    assert_eq!(app.recorded_of_kind("commerce.invoice.created").len(), 1);
}

/// The batch the dispatcher's `commerce.invoice.issue` posts: a
/// redelivered issue writes nothing, so `inserted` reads 0, and
/// `already_created` names the invoice that was answered rather than
/// written — the handler converges on it instead of NAKing.
#[tokio::test]
async fn a_redelivered_batch_names_the_invoice_it_did_not_write() {
    let app = CommerceTestApp::new();
    let inv = invoice_fixture("inv-batch-twice-1");

    let first = TestRequest::post("/api/commerce/invoices/batch")
        .json(&vec![inv.clone()])
        .send(&app.router)
        .await;
    first.assert_status(StatusCode::OK);
    let first: serde_json::Value = first.assert_json();
    assert_eq!(first["inserted"], 1, "{first}");
    assert_eq!(first["already_created"], serde_json::json!([]), "{first}");

    let again = TestRequest::post("/api/commerce/invoices/batch")
        .json(&vec![inv])
        .send(&app.router)
        .await;
    again.assert_status(StatusCode::OK);
    let again: serde_json::Value = again.assert_json();
    assert_eq!(again["inserted"], 0, "nothing was written: {again}");
    assert_eq!(
        again["already_created"],
        serde_json::json!(["inv-batch-twice-1"]),
        "{again}"
    );
    assert_eq!(again["skipped"], serde_json::json!([]), "{again}");
    assert_eq!(app.recorded_of_kind("commerce.invoice.created").len(), 1);
}

#[tokio::test]
async fn post_invoice_with_invalid_json_returns_4xx() {
    let app = CommerceTestApp::new();

    let resp = TestRequest::post("/api/commerce/invoices/create")
        .raw_body("{not valid json")
        .send(&app.router)
        .await;

    assert!(
        resp.status.is_client_error(),
        "expected 4xx for malformed JSON, got {}",
        resp.status,
    );
}

#[tokio::test]
async fn post_invoice_with_multiple_line_items_accepts_and_sum_matches() {
    use boss_commerce::types::{Invoice, InvoiceLineItem, InvoiceStatus, RevenueCategory};

    let app = CommerceTestApp::new();
    let inv = Invoice {
        id: "inv-multi-1".to_string(),
        account_id: "account-001".to_string(),
        issued_on: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        due_on: chrono::NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: 6_000_000,
        currency: "USD".to_string(),
        tax_cents: 0,
        tax_jurisdiction: None,
        payment_method: None,
        line_items: vec![
            InvoiceLineItem {
                id: "inv-multi-1-L1".to_string(),
                invoice_id: "inv-multi-1".to_string(),
                revenue_category: RevenueCategory::from("wholesale"),
                amount_cents: 4_500_000,
                currency: "USD".to_string(),
                description: "New device sale".to_string(),
                ref_id: Some("opp-001".to_string()),
                sku: None,
                qty: None,
                cost_basis_cents: None,
                cost_total_cents: None,
            },
            InvoiceLineItem {
                id: "inv-multi-1-L2".to_string(),
                invoice_id: "inv-multi-1".to_string(),
                revenue_category: RevenueCategory::from("distribution"),
                amount_cents: 1_200_000,
                currency: "USD".to_string(),
                description: "1-year service agreement".to_string(),
                ref_id: Some("sa-001".to_string()),
                sku: None,
                qty: None,
                cost_basis_cents: None,
                cost_total_cents: None,
            },
            InvoiceLineItem {
                id: "inv-multi-1-L3".to_string(),
                invoice_id: "inv-multi-1".to_string(),
                revenue_category: RevenueCategory::from("taproom"),
                amount_cents: 300_000,
                currency: "USD".to_string(),
                description: "Installation + training".to_string(),
                ref_id: Some("wo-001".to_string()),
                sku: None,
                qty: None,
                cost_basis_cents: None,
                cost_total_cents: None,
            },
        ],
    };

    let resp = TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_invoice_with_mismatched_sum_rejected() {
    use boss_commerce::types::{Invoice, InvoiceLineItem, InvoiceStatus, RevenueCategory};

    let app = CommerceTestApp::new();
    let inv = Invoice {
        id: "inv-bad-sum".to_string(),
        account_id: "account-001".to_string(),
        issued_on: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        due_on: chrono::NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        paid_on: None,
        status: InvoiceStatus::OUTSTANDING.into(),
        amount_cents: 1_000_000, // header claims 10k
        currency: "USD".to_string(),
        tax_cents: 0,
        tax_jurisdiction: None,
        payment_method: None,
        line_items: vec![InvoiceLineItem {
            id: "inv-bad-sum-L1".to_string(),
            invoice_id: "inv-bad-sum".to_string(),
            revenue_category: RevenueCategory::from("wholesale"),
            amount_cents: 500_000, // line item only has 5k
            currency: "USD".to_string(),
            description: "mismatch".to_string(),
            ref_id: None,
            sku: None,
            qty: None,
            cost_basis_cents: None,
            cost_total_cents: None,
        }],
    };

    let resp = TestRequest::post("/api/commerce/invoices/create")
        .json(&inv)
        .send(&app.router)
        .await;

    assert!(
        resp.status.is_server_error() || resp.status.is_client_error(),
        "expected failure status for mismatched sum, got {}",
        resp.status,
    );
}

// ---------------------------------------------------------------------------
// PUT /api/commerce/invoices/{id}/paid
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_invoice_paid_returns_204() {
    let inv = invoice_fixture("inv-paid-1");
    let app = CommerceTestApp::with_invoices(vec![inv]);

    let resp = TestRequest::put("/api/commerce/invoices/inv-paid-1/paid")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn put_invoice_paid_emits_commerce_invoice_paid_event() {
    let inv = invoice_fixture("inv-paid-2");
    let app = CommerceTestApp::with_invoices(vec![inv]);

    TestRequest::put("/api/commerce/invoices/inv-paid-2/paid")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.assert_recorded("commerce.invoice.paid");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("inv-paid-2"),
    );
}

#[tokio::test]
async fn put_invoice_paid_returns_404_for_unknown_id() {
    let app = CommerceTestApp::new();

    let resp = TestRequest::put("/api/commerce/invoices/inv-nope/paid")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_invoice_after_seeded_returns_same_data() {
    let inv = invoice_fixture("inv-roundtrip-1");
    let app = CommerceTestApp::with_invoices(vec![inv.clone()]);

    let resp = TestRequest::get("/api/commerce/invoices/inv-roundtrip-1")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let fetched: boss_commerce::types::Invoice = resp.assert_json();
    assert_eq!(fetched.id, inv.id);
    assert_eq!(fetched.amount_cents, inv.amount_cents);
}

// ---------------------------------------------------------------------------
// POST /api/commerce/invoices/write-off/from-past-due
//
// The bad-debt-writeoff counterparty drive. Fires once per
// `commerce.invoice.past_due` copy the sim receives — ar-aging's
// internal emission (trigger = the billing step payload) AND the
// system's webhook copy (the enriched invoice row) — so the adapter
// resolves either shape and the double delivery converges on one
// flip + one emitted event. Mirrors from-paid-invoice (#102).
// ---------------------------------------------------------------------------

fn past_due_fixture(id: &str) -> boss_commerce::types::Invoice {
    let mut inv = invoice_fixture(id);
    inv.status = boss_commerce::types::InvoiceStatus::PAST_DUE.into();
    inv
}

#[tokio::test]
async fn write_off_from_past_due_resolves_the_counterparty_chain_shape() {
    let app = CommerceTestApp::with_invoices(vec![past_due_fixture("inv-step-chain-1")]);

    let resp = TestRequest::post("/api/commerce/invoices/write-off/from-past-due")
        .json(&serde_json::json!({
            "trigger": {
                "trigger": { "step_id": "chain-1", "job_id": "job-77" },
                "channel": "collections",
            },
        }))
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::OK);
    let event = app.assert_recorded("commerce.invoice.written_off");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("inv-step-chain-1"),
    );
}

#[tokio::test]
async fn write_off_from_past_due_resolves_the_webhook_copy_shape() {
    let app = CommerceTestApp::with_invoices(vec![past_due_fixture("inv-step-web-1")]);

    let resp = TestRequest::post("/api/commerce/invoices/write-off/from-past-due")
        .json(&serde_json::json!({
            "trigger": {
                "id": "inv-step-web-1",
                "status": "past-due",
                "amount_cents": 1_200_000,
            },
        }))
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::OK);
    app.assert_recorded("commerce.invoice.written_off");
}

#[tokio::test]
async fn write_off_from_past_due_double_delivery_emits_one_event() {
    let app = CommerceTestApp::with_invoices(vec![past_due_fixture("inv-step-dd-1")]);

    TestRequest::post("/api/commerce/invoices/write-off/from-past-due")
        .json(&serde_json::json!({
            "trigger": { "trigger": { "step_id": "dd-1" } },
        }))
        .send(&app.router)
        .await
        .assert_status(StatusCode::OK);

    TestRequest::post("/api/commerce/invoices/write-off/from-past-due")
        .json(&serde_json::json!({
            "trigger": { "id": "inv-step-dd-1", "status": "past-due" },
        }))
        .send(&app.router)
        .await
        .assert_status(StatusCode::OK);

    assert_eq!(
        app.recorded_of_kind("commerce.invoice.written_off").len(),
        1,
        "the second drive converges without a duplicate event"
    );
}

#[tokio::test]
async fn write_off_from_past_due_unresolvable_shape_400s() {
    let app = CommerceTestApp::with_invoices(vec![past_due_fixture("inv-step-x-1")]);

    let resp = TestRequest::post("/api/commerce/invoices/write-off/from-past-due")
        .json(&serde_json::json!({
            "trigger": { "note": "no lineage, no id" },
        }))
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_write_off_second_call_converges_without_duplicate_event() {
    let app = CommerceTestApp::with_invoices(vec![past_due_fixture("inv-step-put-1")]);

    TestRequest::put("/api/commerce/invoices/inv-step-put-1/write-off")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    TestRequest::put("/api/commerce/invoices/inv-step-put-1/write-off")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    assert_eq!(
        app.recorded_of_kind("commerce.invoice.written_off").len(),
        1,
        "repeat PUT is a converged no-op"
    );
}

/// A written-off invoice cannot be marked past-due: the receivable is
/// already gone, and the flip would count it owed again. The API says
/// so by name — 409 naming both statuses — and records nothing
/// (backlog 203ef806; the whole table is `invoice_transitions.rs`).
#[tokio::test]
async fn put_past_due_on_a_written_off_invoice_is_refused_by_name() {
    let app = CommerceTestApp::with_invoices(vec![past_due_fixture("inv-step-wo-pd")]);
    TestRequest::put("/api/commerce/invoices/inv-step-wo-pd/write-off")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let resp = TestRequest::put("/api/commerce/invoices/inv-step-wo-pd/past-due")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CONFLICT);
    let body = String::from_utf8_lossy(&resp.body_bytes);
    assert!(
        body.contains("'written-off' -> 'past-due'"),
        "the refusal names the transition: {body}"
    );
    assert!(app.recorded_of_kind("commerce.invoice.past_due").is_empty());
}
