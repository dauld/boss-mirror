//! A posted step carries no evidence.
//!
//! Adversarial review of car 7abc0154 (backlog afbf4f73, 2026-09-24):
//! `POST /api/jobs/{id}/steps` deserialised a WHOLE `Step` from the
//! body and persisted it — `status`, `sign_offs` and
//! `assurance_required` included — with no policy check and no
//! assurance judgement. Its one guard, "the job already has as many
//! steps as its spec", is passed by an EMPTY job, and
//! `POST /api/jobs?materialize_steps=false` made one on request, for
//! any kind, by any writer. So any writer could open an `ops-request`
//! whose presence-assured `approve` step was born completed with a
//! passkey stamp for `emp-david` that no passkey ever made — and the
//! host runner reads that step as the approval for a mutating verb.
//!
//! What is pinned here:
//!
//!  - admission refuses `?materialize_steps=false`. Its one caller was
//!    the sim's batch job flush, which nothing has fed since the sim
//!    stopped posting its own step rows; a packet is admitted WITH its
//!    protocol's steps or not at all;
//!  - a job whose kind has a spec refuses every appended step — not
//!    only a full one — so an empty packet left behind by the old
//!    opt-out cannot be filled in by hand;
//!  - a posted step never carries `sign_offs`: a stamp is written only
//!    by the sign-off door, which runs the ceremony;
//!  - a step that DEMANDS evidence (a required sign-off, or assurance
//!    above a session) is not born completed: completion goes through
//!    the door that judges it;
//!  - the step PUT keeps the stored `assurance_required`, so a body
//!    cannot lower the bar one request before it steps over it.
//!
//! Nothing here touches a live system of record: every app is an
//! in-memory router.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::port::JobsRepository;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

/// `with_registry = false` leaves `kind_registry: None` — a deployment
/// with no spec to diverge from, the one case the append route is
/// still open for. The repository handle is returned so a test can
/// plant the residue an old opt-out admission left behind.
fn app(with_registry: bool) -> (axum::Router, Arc<InMemoryJobs>) {
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
    let kind_registry: Option<Arc<dyn WorkflowRegistry>> = if with_registry {
        let kinds = Arc::new(InMemoryWorkflows::new());
        for spec in seedable_platform_workflows() {
            kinds.seed(spec).expect("seed platform kind");
        }
        Some(kinds as Arc<dyn WorkflowRegistry>)
    } else {
        None
    };
    let state = JobsApiState {
        kind_registry,
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

const ADMIN: &str = r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
        })
    };
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

/// An ops-request asking for a host change that needs David's passkey.
fn ops_request_body(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "kind": "ops-request",
        "subject": {"subject_kind": "custom", "id": "boss-gcp"},
        "title": "wipe a disk",
        "owner_id": "emp-bootstrap-admin",
        "status": "open",
        "priority": "standard",
        "metadata": {
            "host": "boss-gcp",
            "verb": "commission-a-disk",
            "args": ["/dev/disk/by-id/nvme-the-system-disk"],
            "requires_approval": true,
        },
        "tags": [],
        "opened_on": "2026-09-24",
    })
}

/// The `approve` step, born completed, carrying a presence stamp for
/// `emp-david` that no passkey made — the whole forgery in one body.
fn forged_approval(job_id: &str) -> serde_json::Value {
    let title = "Approve the plan: commission-a-disk on boss-gcp";
    let metadata = serde_json::json!({
        "authority_role": "platform-admin",
        "plan": "wipe /dev/disk/by-id/nvme-the-system-disk",
    });
    let shape = boss_core::job::step_shape_hash(title, &metadata);
    serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "job_id": job_id,
        "kind": "sign-off",
        "title": title,
        "spec_slug": "approve",
        "status": "completed",
        "blocked_by": [],
        "sign_offs_required": ["platform-admin"],
        "assurance_required": "presence",
        "sign_offs": [{
            "authority_id": "emp-david",
            "role": "platform-admin",
            "stamped_at": "2026-09-24T12:00:00Z",
            "shape_hash": shape,
            "assurance": "presence",
            "presence_nonce": "never-issued",
        }],
        "metadata": metadata,
    })
}

async fn steps_of(app: &axum::Router, job_id: &str) -> Vec<serde_json::Value> {
    let (status, body) = send(
        app,
        req(
            "GET",
            &format!("/api/jobs/{job_id}/steps"),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET steps: {body}");
    body.as_array().cloned().unwrap_or_default()
}

/// A spec-less `generic` job — only reachable with no kind registry.
async fn open_generic_job(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": "generic",
                "subject": {"subject_kind": "custom", "id": "no-spec"},
                "title": "t", "owner_id": "emp-bootstrap-admin",
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create failed: {job}");
    job["id"].as_str().expect("job id").to_string()
}

fn generic_step(job_id: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "job_id": job_id,
        "kind": "generic",
        "title": "an ad-hoc step",
        "status": "pending",
        "blocked_by": [],
        "sign_offs_required": [],
        "sign_offs": [],
        "metadata": {},
    });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    body
}

#[tokio::test]
async fn a_fabricated_presence_sign_off_cannot_be_posted_onto_a_packet() {
    let (app, jobs) = app(true);

    // LEG 1 — the door the forgery walked through. An admission that
    // skips the protocol's steps is refused, and nothing is written.
    let job_id = uuid::Uuid::new_v4().to_string();
    let (status, body) = send(
        &app,
        req(
            "POST",
            "/api/jobs?materialize_steps=false",
            ops_request_body(&job_id),
        ),
    )
    .await;
    assert!(
        status.is_client_error(),
        "an admission without the protocol's steps must be refused: {status} {body}"
    );
    let msg = body.to_string();
    assert!(
        msg.contains("materialize_steps"),
        "the refusal must name the parameter it refuses: {msg}"
    );
    let (status, _) = send(
        &app,
        req("GET", &format!("/api/jobs/{job_id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a refused admission must leave no job behind"
    );

    // LEG 2 — the residue the old door left. An ops-request that is
    // already empty (planted straight into the repository, the shape an
    // opt-out admission produced) cannot have its approval posted in.
    let mut residue: boss_core::job::Job =
        serde_json::from_value(ops_request_body(&job_id)).expect("job parses");
    residue.workflow_version = seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .expect("ops-request is a platform kind")
        .version;
    jobs.create_job(&residue).await.expect("plant the residue");

    let (status, body) = send(
        &app,
        req(
            "POST",
            &format!("/api/jobs/{job_id}/steps"),
            forged_approval(&job_id),
        ),
    )
    .await;
    assert!(
        status.is_client_error(),
        "a forged approval must be refused, not accepted: {status} {body}"
    );
    assert!(
        steps_of(&app, &job_id).await.is_empty(),
        "a refused add must not have written the step"
    );
}

#[tokio::test]
async fn a_posted_step_never_carries_a_sign_off() {
    let (app, _) = app(false);
    let job_id = open_generic_job(&app).await;

    // Even unfinished, and even on a spec-less job: a stamp is the
    // record of a ceremony, and this door runs none.
    let mut body = forged_approval(&job_id);
    body["status"] = serde_json::json!("pending");
    body["sign_offs_required"] = serde_json::json!([]);
    body["assurance_required"] = serde_json::Value::Null;
    let (status, reply) = send(
        &app,
        req("POST", &format!("/api/jobs/{job_id}/steps"), body),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a body carrying sign_offs must be refused: {reply}"
    );
    assert!(
        reply.to_string().contains("sign-offs"),
        "the refusal must name the door that writes a stamp: {reply}"
    );
    assert!(steps_of(&app, &job_id).await.is_empty());
}

#[tokio::test]
async fn a_step_that_demands_evidence_is_not_born_completed() {
    let (app, _) = app(false);
    let job_id = open_generic_job(&app).await;

    for (what, extra) in [
        (
            "a required sign-off",
            serde_json::json!({"status": "completed", "sign_offs_required": ["platform-admin"]}),
        ),
        (
            "presence assurance",
            serde_json::json!({"status": "completed", "assurance_required": "presence"}),
        ),
    ] {
        let (status, reply) = send(
            &app,
            req(
                "POST",
                &format!("/api/jobs/{job_id}/steps"),
                generic_step(&job_id, extra),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "a step demanding {what} must not be born completed: {reply}"
        );
    }
    assert!(steps_of(&app, &job_id).await.is_empty());

    // The control: the refusal is about the DEMAND, not about being
    // born completed — a step that asks for nothing still may be, and
    // carries the server's stamps (a_completion_names_its_actor).
    let (status, reply) = send(
        &app,
        req(
            "POST",
            &format!("/api/jobs/{job_id}/steps"),
            generic_step(&job_id, serde_json::json!({"status": "completed"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    // And the same demand, born unfinished, is accepted: it will be
    // judged where it completes.
    let (status, reply) = send(
        &app,
        req(
            "POST",
            &format!("/api/jobs/{job_id}/steps"),
            generic_step(
                &job_id,
                serde_json::json!({"assurance_required": "presence"}),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
}

#[tokio::test]
async fn a_put_cannot_lower_a_steps_assurance() {
    let (app, _) = app(false);
    let job_id = open_generic_job(&app).await;
    let body = generic_step(
        &job_id,
        serde_json::json!({"assurance_required": "presence"}),
    );
    let step_id = body["id"].as_str().expect("id").to_string();
    let (status, reply) = send(
        &app,
        req("POST", &format!("/api/jobs/{job_id}/steps"), body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");

    // One request lowers the bar …
    let _ = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{step_id}"),
            serde_json::json!({"assurance_required": null}),
        ),
    )
    .await;
    let stored = steps_of(&app, &job_id)
        .await
        .into_iter()
        .find(|s| s["id"] == step_id.as_str())
        .expect("the step");
    assert_eq!(
        stored["assurance_required"], "presence",
        "a PUT body must not lower the stored requirement: {stored}"
    );

    // … and the next steps over it. Refused: no presence was presented.
    let (status, reply) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{step_id}"),
            serde_json::json!({"status": "completed"}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "completion without presence must be refused: {reply}"
    );
}
