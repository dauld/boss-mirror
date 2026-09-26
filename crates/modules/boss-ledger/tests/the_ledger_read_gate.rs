//! The ledger's read gate has no open configuration (backlog 7048afa8,
//! 2026-09-26).
//!
//! `LedgerApiState.policy` was an `Option`, and `None` let every
//! `/api/ledger/*` read through — fail-open by configuration: an
//! instance or harness built without a client served the company's
//! books to anyone. The field is now a required client, so a surface
//! cannot be built without one, and every test wires one explicitly.
//! These pin what the gate does with the client it is always handed:
//! a denial is 403, an unreachable engine is 503, `/health` is exempt.
//!
//! No database is needed — the gate answers before any handler runs —
//! so the pool is a lazy one that is never connected.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_ledger::http::{LedgerApiState, router};
use boss_policy_client::{
    Action, Decision, FakePolicyClient, PolicyClient, PolicyClientError, Predicate, Resource, User,
};
use tower::ServiceExt;

/// A policy engine that cannot be reached.
struct UnreachablePolicy;

#[async_trait]
impl PolicyClient for UnreachablePolicy {
    async fn check(
        &self,
        _user: &User,
        _action: Action,
        _resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        Err(PolicyClientError::Unreachable("connection refused".into()))
    }

    async fn scope_predicate(
        &self,
        _user: &User,
        _resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        Err(PolicyClientError::Unreachable("connection refused".into()))
    }
}

fn surface(policy: Arc<dyn PolicyClient>) -> axum::Router {
    // Never connected: the gate refuses before a handler touches it.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    router(LedgerApiState {
        pool,
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        policy,
    })
}

async fn status_of(app: axum::Router, path: &str) -> StatusCode {
    app.oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_without_a_ledger_grant_is_refused() {
    let app = surface(Arc::new(FakePolicyClient::deny_all()));
    for path in [
        "/api/ledger/accounts",
        "/api/ledger/trial-balance",
        "/api/ledger/entries",
    ] {
        assert_eq!(
            status_of(app.clone(), path).await,
            StatusCode::FORBIDDEN,
            "{path} answered without a ledger read grant"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_policy_engine_closes_the_books() {
    let app = surface(Arc::new(UnreachablePolicy));
    assert_eq!(
        status_of(app, "/api/ledger/accounts").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn health_answers_without_a_grant() {
    let app = surface(Arc::new(FakePolicyClient::deny_all()));
    assert_eq!(status_of(app, "/api/ledger/health").await, StatusCode::OK);
}
