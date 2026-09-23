//! `GET /api/people/accounts` answers the paged envelope.
//!
//! Backlog 2d1d298e (found by the /ux/support page audit, 2026-09-19):
//! this read returned a bare array from an unbounded `SELECT … ORDER BY
//! id`, so no caller could tell a capped list from a complete one, and
//! the whole table came back on every render. Every other boss-* list
//! endpoint answers `{data, total, limit, offset}` with a real LIMIT
//! and a DB-wide total, which is what the SPA's `fetchPaged` +
//! `isCapped` consume. These tests pin that shape here.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_accounts::accounts::accounts_router;
use boss_assets_client::FakeAssetsClient;
use boss_testing::{TestDb, TestRequest};
use sqlx::PgPool;

fn build_app(pool: PgPool) -> Router {
    accounts_router(
        pool,
        None,
        Arc::new(FakeAssetsClient::with_count(0)),
        Arc::new(boss_clock_client::WallClockClient),
        None,
    )
}

/// Identity-first accounts: only `id` is required, so the read is
/// exercised without the write path's Class-registry validation.
async fn seed_accounts(pool: &PgPool, n: usize) {
    for i in 0..n {
        sqlx::query("INSERT INTO accounts (id, name) VALUES ($1, $2)")
            .bind(format!("acct-{i:03}"))
            .bind(format!("Account {i}"))
            .execute(pool)
            .await
            .unwrap();
    }
}

fn ids(body: &serde_json::Value) -> Vec<String> {
    body["data"]
        .as_array()
        .expect("data is an array")
        .iter()
        .map(|r| r["id"].as_str().expect("id").to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_is_the_paged_envelope_with_a_db_wide_total() {
    let db = TestDb::new().await;
    seed_accounts(&db.pool, 3).await;
    let app = build_app(db.pool.clone());

    let resp = TestRequest::get("/api/people/accounts").send(&app).await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json();

    assert!(body.is_object(), "a bare array is the defect: {body}");
    assert_eq!(ids(&body), ["acct-000", "acct-001", "acct-002"]);
    assert_eq!(body["total"], 3);
    assert_eq!(body["offset"], 0);
    assert!(body["limit"].as_i64().is_some_and(|l| l >= 3), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_capped_page_says_there_is_more_than_it_carries() {
    let db = TestDb::new().await;
    seed_accounts(&db.pool, 5).await;
    let app = build_app(db.pool.clone());

    let resp = TestRequest::get("/api/people/accounts?limit=2&offset=1")
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json();

    // The page is bounded, ordered by id, and the total counts the
    // whole table — the difference is what an overflow banner reads.
    assert_eq!(ids(&body), ["acct-001", "acct-002"]);
    assert_eq!(body["total"], 5);
    assert_eq!(body["limit"], 2);
    assert_eq!(body["offset"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_absurd_limit_is_clamped_rather_than_unbounded() {
    let db = TestDb::new().await;
    seed_accounts(&db.pool, 1).await;
    let app = build_app(db.pool.clone());

    let resp = TestRequest::get("/api/people/accounts?limit=999999999&offset=-4")
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json();

    assert_eq!(body["limit"], boss_accounts::accounts::MAX_LIST_LIMIT);
    assert_eq!(body["offset"], 0);
    assert_eq!(body["total"], 1);
}
