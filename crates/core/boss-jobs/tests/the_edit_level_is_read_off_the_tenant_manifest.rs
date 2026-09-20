//! `GET /api/tenant/edit-level` — the instance's hosting edit level,
//! read off the tenant manifest the launcher hands every service
//! (`BOSS_TENANT_MANIFEST_TOML`), answered to the two doors that judge
//! a change against it: `boss dispatch` and the gate's
//! `a-car-stays-under-the-edit-level` lint (a479faf7; design 01c3cc3f
//! reader 3).
//!
//! WHY THE JOBS API. Both doors already speak to exactly one address —
//! `BOSS_JOBS_URL`, spelled once in infra/estate/estate.toml — and the
//! level is an INSTANCE fact (the tenant's, delivered with the tenant),
//! not a fact of the checkout being gated: the product tree has no
//! tenant.toml at its root, and the instance's tenant (the tenant repo)
//! is not in any checkout a gate sees. The gateway serves the manifest
//! too, but nothing off the pod knows its address.
//!
//! Three properties: a declared level is answered verbatim with the
//! file it came from; a manifest that declares none answers `null`
//! (no level: the doors enforce nothing — the operator's own instance);
//! a manifest that exists but does not parse is a 500 carrying toml's
//! own words, so the gate refuses rather than reads a level that is not
//! there. No env var is set in these tests: the pure half takes the
//! path, and the route is exercised with the env unset — which on this
//! runner is the no-manifest case.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::tenant::{EditLevelAnswer, edit_level_answer};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use tower::ServiceExt;

fn app() -> axum::Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::builder().build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        jobs,
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: None,
        plugin_registry: None,
        job_edges: None,
        stations: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
        dispatcher_firings: None,
        agent_budget: None,
    };
    router(state)
}

fn manifest(tag: &str, text: &str) -> std::path::PathBuf {
    let dir = boss_testing::scratch_dir(&format!("edit-level-{tag}"));
    let path = dir.join("tenant.toml");
    boss_testing::write_file(&path, text);
    path
}

#[test]
fn a_declared_level_is_answered_with_the_file_it_came_from() {
    let path = manifest(
        "declared",
        "[meta]\ntenant_id = \"acme\"\nedit_level = \"tenants\"\n",
    );
    let answer = edit_level_answer(Some(&path)).expect("a manifest that parses");
    assert_eq!(
        answer,
        EditLevelAnswer {
            edit_level: Some("tenants".into()),
            manifest: Some(path.display().to_string()),
        }
    );
}

#[test]
fn a_manifest_that_declares_none_and_no_manifest_at_all_both_answer_null() {
    let path = manifest("undeclared", "[meta]\ntenant_id = \"acme\"\n");
    let answer = edit_level_answer(Some(&path)).unwrap();
    assert_eq!(answer.edit_level, None);
    assert_eq!(answer.manifest.as_deref(), Some(path.to_str().unwrap()));

    let none = edit_level_answer(None).unwrap();
    assert_eq!(
        none,
        EditLevelAnswer {
            edit_level: None,
            manifest: None,
        }
    );
}

#[test]
fn a_manifest_that_does_not_parse_is_an_error_in_tomls_own_words_not_a_level() {
    let path = manifest("broken", "[meta\ntenant_id = \"acme\"\n");
    let err = edit_level_answer(Some(&path)).expect_err("does not parse");
    assert!(err.contains(&path.display().to_string()), "{err}");
    assert!(err.contains("line 1"), "toml's own words: {err}");
}

#[test]
fn a_manifest_that_is_named_but_absent_is_an_error_naming_the_path() {
    let path = boss_testing::scratch_dir("edit-level-absent").join("nowhere.toml");
    let err = edit_level_answer(Some(&path)).expect_err("absent");
    assert!(err.contains("nowhere.toml"), "{err}");
}

/// The route, unauthenticated as the gate's curl is: with no manifest
/// named in this process's environment the answer is `null`, not a
/// refusal — the level door is guest-readable because a gate has no
/// session and the level is one word.
#[tokio::test]
async fn the_route_answers_without_a_session() {
    assert!(
        std::env::var_os("BOSS_TENANT_MANIFEST_TOML").is_none(),
        "this test reads the route with no manifest named; the runner set one"
    );
    let resp = app()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/tenant/edit-level")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "edit_level": null, "manifest": null })
    );
}
