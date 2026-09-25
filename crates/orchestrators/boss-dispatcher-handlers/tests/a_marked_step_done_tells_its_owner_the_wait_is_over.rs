//! A step marked `notify_on_done` that completes sends a `done:` signal
//! to a person — end to end, from the step PUT that completes it to the
//! message the messages API is asked to send (backlog 58f0b536).
//!
//! THE DEFECT. The wait-is-over signal (`notify-on-step-done-marked`,
//! David 2026-08-09) addressed itself through the completed step's
//! `authority_role`, resolved to the role's on-call member. pr-train v2
//! (backlog af796788, published live 2026-09-18T12:53Z) moved every
//! conductor step from `authority_role = "platform-admin"` to
//! `audience = { individual = "automation:train-conductor" }` — correct
//! for WHO EXECUTES the step, and it took the notice's only recipient
//! with it. `step.done` carries no assignee, so `messages.notify` found
//! neither an assignee nor a role and returned `Ok(())`: no error, no
//! dead letter, no firing a reader would notice. Measured in the audit
//! log: the last `done:` send was train/20260918-1226's three at
//! 12:40Z (the last train pinned to v1), and none in the window read on
//! 2026-09-25 (05:19-08:01Z, 500 sends) although train 0372de3b
//! completed ci / merged / deployed inside it, each `step.done.task`
//! carrying `notify_on_done: true`.
//!
//! Every piece in isolation was green: the rule's predicate matched
//! (notify_on_done_rule.rs), the handler sent for a payload with a role
//! (the unit tests beside it), and the workflow pinned its new audience
//! (platform_bundle_pr_train.rs). The seam between them — what a REAL
//! train's step.done looks like to the handler — was pinned nowhere,
//! so this test walks the shipped pr-train bundle through the jobs API,
//! matches the step.done it records against the rule FILE, and drives
//! the handler with the rule's own args.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::handler::{Handler, InvocationContext};
use boss_dispatcher::rules::registry::{Registry, match_event};
use boss_dispatcher_handlers::handlers::messages_notify::MessagesNotify;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, dispatcher_rules_dir};
use http_body_util::BodyExt;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const RULE: &str = "notify-on-step-done-marked";
/// The person the train belongs to — the live trains' `owner_id` is
/// `emp-david`; any person makes the point.
const OWNER: &str = "emp-owner";
const ADMIN: &str = r#"{"id":"emp-owner","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

struct Roster;

#[async_trait::async_trait]
impl RosterLookup for Roster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec![OWNER.to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == OWNER)
    }
}

fn jobs_app() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(Roster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string()));
    (status, json)
}

fn req(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", ADMIN)
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// Complete the step with `slug` the way the conductor does: read it,
/// lay the evidence over its stored metadata, PUT it completed.
async fn complete(app: &axum::Router, job_id: &str, slug: &str, evidence: serde_json::Value) {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{job_id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    let step = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("`{slug}` materialised"))
        .clone();
    let mut metadata = step["metadata"].clone();
    for (k, v) in evidence.as_object().expect("evidence is an object") {
        metadata[k] = v.clone();
    }
    let step_id = step["id"].as_str().expect("step id");
    let (status, body) = send(
        app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{step_id}"),
            serde_json::json!({"status": "completed", "metadata": metadata}),
        ),
    )
    .await;
    assert!(status.is_success(), "completing `{slug}`: {status}: {body}");
}

/// A stand-in `boss-people` (the role arm's lookup) and `boss-messages`
/// that records every message it is asked to send.
async fn mock_services() -> (String, String, Arc<Mutex<Vec<serde_json::Value>>>) {
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    let people = Router::new().route(
        "/api/people",
        get(|| async { Json(serde_json::json!([{ "id": OWNER }])) }),
    );
    let people_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let people_addr = people_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(people_listener, people).await.unwrap() });

    let sent = Arc::new(Mutex::new(Vec::new()));
    let cap = sent.clone();
    let messages = Router::new().route(
        "/api/messages/send",
        post(move |Json(body): Json<serde_json::Value>| {
            let cap = cap.clone();
            async move {
                cap.lock().unwrap().push(body);
                StatusCode::CREATED
            }
        }),
    );
    let messages_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let messages_addr = messages_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(messages_listener, messages).await.unwrap() });

    (
        format!("http://{people_addr}"),
        format!("http://{messages_addr}"),
        sent,
    )
}

/// A pr-train's `ci` step completes and its owner is told: the shipped
/// bundle's shape, the rule's file, the handler's own code.
#[tokio::test]
async fn a_trains_ci_verdict_tells_the_trains_owner_the_wait_is_over() {
    let (app, jobs) = jobs_app();
    let (status, job) = send(
        &app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": "pr-train",
                "subject": {"subject_kind": "custom", "id": "train/20260925-0717"},
                "title": "PR train", "owner_id": OWNER,
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": ["train"],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "train open: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

    complete(&app, &job_id, "scheduled", serde_json::json!({})).await;
    complete(
        &app,
        &job_id,
        "collect",
        serde_json::json!({"boarded": "1"}),
    )
    .await;
    complete(
        &app,
        &job_id,
        "assemble",
        serde_json::json!({"train_ref": "train/20260925-0717"}),
    )
    .await;
    complete(
        &app,
        &job_id,
        "pr",
        serde_json::json!({"pr_url": "http://forge.invalid/pulls/1"}),
    )
    .await;
    complete(&app, &job_id, "ci", serde_json::json!({"result": "green"})).await;

    let recorded = jobs.recorded_events();
    let ci_done = recorded
        .iter()
        .find(|e| e.kind == "step.done.task" && e.payload["spec_slug"] == "ci")
        .unwrap_or_else(|| {
            panic!(
                "completing ci records a step.done.task; recorded: {:?}",
                recorded.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>()
            )
        });

    // The rule as the dispatcher boots it: its file, not a copy.
    let path = dispatcher_rules_dir().join(format!("{RULE}.toml"));
    let toml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read rule file {}: {e}", path.display()));
    let registry = Registry::from_toml(&toml).expect("the rule file parses");
    let outcome = match_event(&registry, "step.done.task", &ci_done.payload, &NoHelpers);
    assert_eq!(
        outcome.matched.len(),
        1,
        "the ci step is marked notify_on_done, so the rule matches its step.done: {}",
        ci_done.payload
    );
    let invocation = &outcome.matched[0].invocations[0];
    assert_eq!(invocation.handler, "messages.notify");

    let (people, messages, sent) = mock_services().await;
    let handler = MessagesNotify::with_client(reqwest::Client::new(), people, messages);
    handler
        .invoke(
            &invocation.args,
            &InvocationContext {
                rule_name: RULE.into(),
                triggering_event_id: ci_done.id.to_string(),
                triggering_topic: "step.done.task".into(),
                event_payload: ci_done.payload.clone(),
            },
        )
        .await
        .expect("messages.notify");

    let sent = sent.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        1,
        "a marked step's completion sends exactly one wait-is-over signal — none means the \
         operator waiting on this train is never told (backlog 58f0b536); step.done payload: {}",
        ci_done.payload
    );
    let step_id = ci_done.payload["step_id"].as_str().expect("step_id");
    assert_eq!(sent[0]["id"], format!("done:{step_id}:{OWNER}"));
    assert_eq!(
        sent[0]["recipient_id"], OWNER,
        "the train's owner is the one waiting on it — never the conductor that completed it"
    );
    assert_eq!(
        sent[0]["kind"], "signal",
        "a signal, so expire-signals-on-job-closed retires it when the train closes"
    );
    assert!(
        sent[0]["subject"]
            .as_str()
            .unwrap_or_default()
            .starts_with("Done:"),
        "{}",
        sent[0]
    );
}
