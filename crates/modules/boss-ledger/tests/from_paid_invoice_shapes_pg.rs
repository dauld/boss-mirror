//! Both drive shapes of `/api/ledger/bank-settlements/from-paid-invoice`
//! must settle — and their double delivery must converge on ONE
//! settlement (the deterministic `bs-{invoice_id}`).
//!
//! Shape 1 (sim-internal counterparty chain): `trigger.trigger.step_id`
//! — ar-aging chains the billing step payload, bank-ach chains
//! ar-aging's emit.
//! Shape 2 (system webhook copy): #100 put `commerce.>` into the
//! durable stream and woke the previously dead-air
//! forward-invoice-paid-to-webhook rule — the counterparty now also
//! receives the enriched invoice row itself (top-level `id`, no
//! trigger lineage). The endpoint rejecting that shape hard-failed the
//! first post-#100 year run at sim-day 37.

use axum::body::Body;
use axum::http::Request;
use boss_testing::TestDb;
use http_body_util::BodyExt;
use tower::ServiceExt;

const STEP_ID: &str = "e5760000-0000-0000-0000-000000000001";

async fn seed_invoice(db: &TestDb) -> String {
    let invoice_id = format!("inv-step-{STEP_ID}");
    sqlx::query(
        "INSERT INTO invoices (id, account_id, issued_on, due_on, status, amount_cents, currency) \
         VALUES ($1, 'acct-1', '2025-04-01', '2025-05-01', 'paid', 250000, 'USD')",
    )
    .bind(&invoice_id)
    .execute(&db.pool)
    .await
    .unwrap();
    invoice_id
}

async fn post_from_paid(
    app: &axum::Router,
    trigger: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ledger/bank-settlements/from-paid-invoice")
                .header("content-type", "application/json")
                .header(
                    "x-boss-user",
                    r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
                )
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "trigger": trigger,
                        "day": "2025-04-03",
                        "bank": "first-national",
                        "channel": "ach",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::String(
        String::from_utf8_lossy(&bytes).into(),
    ));
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn both_trigger_shapes_settle_and_converge_on_one_row() {
    let db = TestDb::new().await;
    let invoice_id = seed_invoice(&db).await;
    let app = boss_ledger::http::router(boss_ledger::http::LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        // The read gate is not this test's subject (tests/the_ledger_read_gate.rs).
        policy: std::sync::Arc::new(boss_policy_client::PermissivePolicyClient),
    });

    // Shape 1: the counterparty chain.
    let (status, body) = post_from_paid(
        &app,
        serde_json::json!({ "trigger": { "step_id": STEP_ID } }),
    )
    .await;
    assert!(
        status.is_success(),
        "chain shape must settle: {status} {body}"
    );

    // Shape 2: the webhook copy of the invoice row — same invoice.
    let (status, body) = post_from_paid(&app, serde_json::json!({ "id": invoice_id })).await;
    assert!(
        status.is_success(),
        "webhook shape must settle (was the 400 that killed the year run): {status} {body}"
    );

    // Double delivery converged on ONE deterministic settlement.
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM bank_settlements WHERE invoice_id = $1")
            .bind(&invoice_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 1, "bs-{{invoice_id}} must dedupe the double drive");

    // A triggerless garbage body still 400s.
    let (status, _) = post_from_paid(&app, serde_json::json!({ "whatever": 1 })).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
}
