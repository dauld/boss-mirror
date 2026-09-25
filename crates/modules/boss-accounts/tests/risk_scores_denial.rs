//! `GET /api/people/accounts/risk-scores` answers a caller it will not
//! show the watchlist to with 403, never with `200 {accounts: []}`
//! (backlog 3f0cdca8; page audit 08b0c4f8 GAP 5, 2026-09-23).
//!
//! The handler used to hand a role without broad account access an
//! empty list "so the panel degrades cleanly", and /watchlist then
//! painted "No accounts match those filters." — the page could not tell
//! "you may not see this" from "nothing is at risk". A denial that
//! answers as an empty result is the false-empty class; these two tests
//! pin that the refusal is a refusal and that an admitted caller with
//! nothing scored still gets the honest empty 200.

use axum::http::StatusCode;
use boss_accounts::account_risk_scores::risk_scores_router;
use boss_testing::{TestDb, TestRequest};

const PATH: &str = "/api/people/accounts/risk-scores?limit=200&min_score=0";

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_without_broad_account_access_is_refused_not_handed_an_empty_list() {
    let db = TestDb::new().await;
    let app = risk_scores_router(db.pool.clone());

    // A field role: no broad account access, `user` access tier.
    let resp = TestRequest::get(PATH)
        .as_user("emp-field", "service-tech")
        .send(&app)
        .await;
    resp.assert_status(StatusCode::FORBIDDEN);
    let body = String::from_utf8_lossy(&resp.body_bytes);
    assert!(
        !body.contains("\"accounts\""),
        "a refusal must not carry an accounts list a reader could take for data: {body}"
    );
}

/// A request with no `x-boss-user` is nobody, and nobody sees the
/// watchlist (backlog 2f4be936 / e84de48e, 2026-09-25). It used to be
/// the one caller admitted by name — `role=guest` — while the field
/// role above was refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_without_the_identity_header_is_refused() {
    let db = TestDb::new().await;
    let app = risk_scores_router(db.pool.clone());
    let resp = TestRequest::get(PATH).send(&app).await;
    resp.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admitted_caller_with_nothing_scored_gets_an_empty_200() {
    let db = TestDb::new().await;
    let app = risk_scores_router(db.pool.clone());

    // Operator tier is admitted whatever the role lists hold, and a
    // fresh database has no churn predictions — so this is the true
    // empty the 403 above must stay distinguishable from.
    let resp = TestRequest::get(PATH).as_smoke().send(&app).await;
    resp.assert_status(StatusCode::OK);
    let list: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert_eq!(list["accounts"], serde_json::json!([]));
    assert_eq!(list["total_scored"], 0);
}
