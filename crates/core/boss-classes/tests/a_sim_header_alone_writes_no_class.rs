//! A sim header alone writes no Class (backlog 85e7f10f, 2026-09-25).
//!
//! The operator-tier doors — this crate's batch, update and retire, and
//! the same shape in locations, calendar and the ledger's chart and tax
//! registry — each wrote `if !(is_in_sim_chain() || tier_ok) { 403 }`,
//! so a caller that reached :7800 directly and sent `x-sim-origin: true`
//! wrote the registry with no identity at all. Each now asks the one
//! predicate, `boss_policy_client::sim_bypass_allowed`.
//!
//! Driven through the real request-context middleware, with the header
//! on the wire, on both instances:
//!
//!  - SIM OFF (prod): the anonymous caller with the header is refused
//!    and nothing lands; an operator-tier seed caller — the identity
//!    the reset and validate scripts under infra/postgres/ and the
//!    engines' prepare all sign with, header included — still writes.
//!  - SIM ON (the playground): the anonymous caller is still refused;
//!    the sim's `put_as` identity (an employee id, role `system-sim`,
//!    USER tier — the one shape the tier check alone would refuse)
//!    still writes, so the bypass remains exactly for the sim.
//!
//! ONE test on a runtime built after the variable is set: the
//! environment is process-wide, and this file is its own process.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_classes::InMemoryClasses;
use boss_classes::http::{ClassesApiState, router};
use boss_classes::port::ClassRepository;
use boss_policy_client::SIM_ENABLED_ENV;
use serde_json::json;
use tower::ServiceExt;

const SEED: &str = r#"{"id":"automation:classes-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;
const SIM_AS_EMPLOYEE: &str = r#"{"id":"emp-042","role":"system-sim","access_tier":"user","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

async fn batch(repo: &Arc<InMemoryClasses>, user: Option<&str>, code: &str) -> StatusCode {
    let app = router(ClassesApiState {
        classes: repo.clone(),
    })
    .layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ));
    let mut b = Request::builder()
        .method("POST")
        .uri("/api/classes/batch")
        .header("content-type", "application/json")
        .header("x-sim-origin", "true");
    if let Some(u) = user {
        b = b.header("x-boss-user", u);
    }
    let body = json!([{
        "subject_kind": "employee", "code": code, "display_name": code,
        "member_attribute": "role", "sort_order": 1,
    }]);
    let req = b.body(Body::from(body.to_string())).unwrap();
    app.oneshot(req).await.unwrap().status()
}

async fn held(repo: &Arc<InMemoryClasses>) -> Vec<String> {
    let mut codes: Vec<String> = repo
        .list_for_subject_kind("employee")
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.code)
        .collect();
    codes.sort();
    codes
}

async fn with_the_sim_off() {
    let repo = Arc::new(InMemoryClasses::new(vec![]));
    assert_eq!(batch(&repo, None, "forged").await, StatusCode::FORBIDDEN);
    assert_eq!(
        batch(&repo, Some(SIM_AS_EMPLOYEE), "sim-off").await,
        StatusCode::FORBIDDEN,
        "no sim instance, no bypass"
    );
    assert_eq!(batch(&repo, Some(SEED), "seeded").await, StatusCode::OK);
    assert_eq!(held(&repo).await, vec!["seeded"]);
}

async fn with_the_sim_on() {
    let repo = Arc::new(InMemoryClasses::new(vec![]));
    assert_eq!(
        batch(&repo, None, "forged").await,
        StatusCode::FORBIDDEN,
        "a header is not a caller, even on a sim instance"
    );
    assert_eq!(batch(&repo, Some(SEED), "seeded").await, StatusCode::OK);
    assert_eq!(
        batch(&repo, Some(SIM_AS_EMPLOYEE), "by-the-sim").await,
        StatusCode::OK,
        "the sim still writes on a sim instance"
    );
    assert_eq!(held(&repo).await, vec!["by-the-sim", "seeded"]);
}

#[test]
fn a_sim_header_alone_writes_no_class_with_the_sim_off_or_on() {
    let rt = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    };
    // SAFETY: the only test in this binary; nothing else reads the
    // environment while it is written.
    unsafe { std::env::remove_var(SIM_ENABLED_ENV) };
    rt().block_on(with_the_sim_off());
    unsafe { std::env::set_var(SIM_ENABLED_ENV, "true") };
    rt().block_on(with_the_sim_on());
    unsafe { std::env::remove_var(SIM_ENABLED_ENV) };
}
