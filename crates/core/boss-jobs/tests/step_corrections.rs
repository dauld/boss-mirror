//! `POST /api/jobs/{id}/steps/{step_id}/corrections` — a correction
//! names what it corrects (design 4105b020, backlog 56727f95).
//!
//! A completed step is a fact and stays one; what was missing was a
//! TIE between it and the correction written beside it. On 2026-09-19
//! packet f3e091f0's triage evidence read "Ordering trap confirmed:  is
//! required of every rule" and the only route was a job-metadata key
//! of the author's invention, which no reader of the step ever saw.
//! These tests pin the door's three refusals (an open step, a field the
//! step does not hold, an excerpt not in the stored text), the one list
//! it appends to and the `jobs.step.corrected` event it records, the
//! withdrawal-by-appending rule, the two other writers that must not be
//! able to rewrite the list (the metadata PATCH refuses the key; the
//! job PUT carries it forward), the job GET handing each step its own
//! entries, and the terminal-freeze 409 naming the door.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const DAMAGED: &str = "Ordering trap confirmed:  is required of every rule";

fn tech(id: &str) -> User {
    User {
        id: id.to_string(),
        role: "service-tech".to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("service".into()),
    }
}

fn open_job(id: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/it/backlog"),
        title: "A packet whose completed step says something wrong".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: json!({ "area": "boss-jobs" }),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn build_app() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("service-tech", Action::Update, Resource::job(), Scope::All)
            .allow("service-tech", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        publisher,
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs)
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header(
                    "x-boss-user",
                    serde_json::to_string(&tech("emp-1")).unwrap(),
                )
                .body(match body {
                    Some(b) => Body::from(b.to_string()),
                    None => Body::empty(),
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, v)
}

/// A job with a completed triage step holding the damaged evidence,
/// and a second, still-active step.
async fn seed(jobs: &Arc<InMemoryJobs>, id: &str) -> (Job, Step, Step) {
    let job = open_job(id);
    jobs.create_job(&job).await.unwrap();
    let mut triage = Step::new(job.id, "task", "Measure the claim, choose a route", 0);
    triage.status = StepStatus::Completed;
    triage.metadata = json!({ "evidence": DAMAGED, "disposition": "build" });
    jobs.add_step(&triage).await.unwrap();
    let mut build = Step::new(job.id, "task", "Build the change", 1);
    build.status = StepStatus::Active;
    build.metadata = json!({ "summary": "in progress" });
    jobs.add_step(&build).await.unwrap();
    (job, triage, build)
}

fn corrections_uri(job: &Job, step: &Step) -> String {
    format!("/api/jobs/{}/steps/{}/corrections", job.id, step.id)
}

fn the_fix() -> Value {
    json!({
        "field": "evidence",
        "reads": "confirmed:  is required",
        "should_read": "confirmed: `why` is required",
        "why": "the backticked word was run by the shell (2376b89e)",
    })
}

fn events_of(jobs: &Arc<InMemoryJobs>, kind: &str) -> Vec<boss_core::event::Event> {
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

#[tokio::test]
async fn a_correction_is_appended_beside_the_step_which_stays_as_it_was() {
    let (app, jobs) = build_app();
    let (job, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c001").await;

    let (status, body) = send(
        &app,
        "POST",
        &corrections_uri(&job, &triage),
        Some(the_fix()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["index"], 0, "{body}");
    assert_eq!(body["correction"]["field"], "evidence", "{body}");

    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    let list = after.metadata["corrections"].as_array().expect("a list");
    assert_eq!(list.len(), 1);
    let e = &list[0];
    assert_eq!(e["step"], triage.id.to_string());
    assert_eq!(e["field"], "evidence");
    assert_eq!(e["reads"], "confirmed:  is required");
    assert_eq!(e["should_read"], "confirmed: `why` is required");
    assert_eq!(e["by"], "emp-1", "signed by the caller: {e}");
    assert!(e["at"].as_str().is_some_and(|t| !t.is_empty()), "{e}");
    assert_eq!(after.metadata["area"], "boss-jobs", "other keys survive");

    // The step — the record of what happened — is untouched.
    let step = jobs.get_step(&triage.id).await.unwrap().unwrap();
    assert_eq!(step.metadata["evidence"], DAMAGED);
    assert_eq!(step.status, StepStatus::Completed);

    // One declared fact, naming the step and the index it landed at.
    let corrected = events_of(&jobs, "jobs.step.corrected");
    assert_eq!(corrected.len(), 1);
    let p = &corrected[0].payload;
    assert_eq!(p["job_id"], job.id.to_string());
    assert_eq!(p["step_id"], triage.id.to_string());
    assert_eq!(p["index"], 0);
    assert_eq!(p["correction"]["reads"], "confirmed:  is required");
    // And the job's row state, which is what the rebuild replays.
    let updated = events_of(&jobs, "jobs.job.updated");
    assert!(
        updated
            .last()
            .is_some_and(|e| e.payload["metadata"]["corrections"][0]["field"] == "evidence"),
        "the JOB_UPDATED row state carries the list"
    );
}

#[tokio::test]
async fn a_second_correction_appends_at_the_next_index() {
    let (app, jobs) = build_app();
    let (job, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c002").await;
    let uri = corrections_uri(&job, &triage);
    send(&app, "POST", &uri, Some(the_fix())).await;
    let (status, body) = send(
        &app,
        "POST",
        &uri,
        Some(
            json!({ "field": "evidence", "reads": "every rule", "should_read": "every rule file" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["index"], 1);
    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(after.metadata["corrections"].as_array().unwrap().len(), 2);
    assert_eq!(
        after.metadata["corrections"][0]["reads"],
        "confirmed:  is required"
    );
}

#[tokio::test]
async fn an_open_step_is_refused_because_it_can_still_be_edited() {
    let (app, jobs) = build_app();
    let (job, _, build) = seed(&jobs, "00000000-0000-0000-0000-00000000c003").await;
    let (status, body) = send(
        &app,
        "POST",
        &corrections_uri(&job, &build),
        Some(json!({ "field": "summary", "reads": "in progress", "should_read": "done" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().is_some_and(|e| e.contains("active")),
        "{body}"
    );
    assert!(
        jobs.get_job(&job.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("corrections")
            .is_none()
    );
    assert!(events_of(&jobs, "jobs.step.corrected").is_empty());
}

#[tokio::test]
async fn a_field_the_step_does_not_hold_is_refused_naming_what_it_holds() {
    let (app, jobs) = build_app();
    let (job, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c004").await;
    let (status, body) = send(
        &app,
        "POST",
        &corrections_uri(&job, &triage),
        Some(json!({ "field": "finding", "reads": "trap", "should_read": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let e = body["error"].as_str().unwrap_or_default();
    assert!(e.contains("`finding`") && e.contains("evidence"), "{body}");
    assert!(events_of(&jobs, "jobs.step.corrected").is_empty());
}

#[tokio::test]
async fn an_excerpt_that_is_not_in_the_stored_text_is_refused() {
    let (app, jobs) = build_app();
    let (job, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c005").await;
    // What the author MEANT to have written is not what is stored —
    // quoting it proves nothing about the sentence that is there.
    let (status, body) = send(
        &app,
        "POST",
        &corrections_uri(&job, &triage),
        Some(json!({ "field": "evidence", "reads": "confirmed: `why` is", "should_read": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("does not appear")),
        "{body}"
    );
    assert!(
        jobs.get_job(&job.id)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .get("corrections")
            .is_none()
    );
}

#[tokio::test]
async fn a_step_addressed_through_another_job_is_not_found_there() {
    let (app, jobs) = build_app();
    let (_, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c006").await;
    let (other, _, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c007").await;
    let (status, _) = send(
        &app,
        "POST",
        &corrections_uri(&other, &triage),
        Some(the_fix()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_correction_is_withdrawn_only_by_appending() {
    let (app, jobs) = build_app();
    let (job, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c008").await;
    let uri = corrections_uri(&job, &triage);
    send(&app, "POST", &uri, Some(the_fix())).await;
    let (status, body) = send(
        &app,
        "POST",
        &uri,
        Some(json!({ "withdraws": 0, "why": "the original was right after all" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["index"], 1);
    let list = jobs.get_job(&job.id).await.unwrap().unwrap().metadata["corrections"].clone();
    assert_eq!(
        list[0]["should_read"], "confirmed: `why` is required",
        "entry 0 is never edited"
    );
    assert_eq!(list[1]["withdraws"], 0);
    assert_eq!(list[1]["field"], "evidence");

    let (status, body) = send(
        &app,
        "POST",
        &uri,
        Some(json!({ "withdraws": 0, "why": "again" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "already withdrawn: {body}"
    );
}

#[tokio::test]
async fn the_job_get_hands_each_step_its_own_corrections() {
    let (app, jobs) = build_app();
    let (job, triage, build) = seed(&jobs, "00000000-0000-0000-0000-00000000c009").await;
    send(
        &app,
        "POST",
        &corrections_uri(&job, &triage),
        Some(the_fix()),
    )
    .await;

    let (status, body) = send(&app, "GET", &format!("/api/jobs/{}", job.id), None).await;
    assert_eq!(status, StatusCode::OK);
    let steps = body["steps"].as_array().expect("steps");
    let find = |id: &str| steps.iter().find(|s| s["id"] == id).expect("step").clone();
    let t = find(&triage.id.to_string());
    let c = t["corrections"]
        .as_array()
        .expect("the corrected step carries its list");
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["index"], 0);
    assert_eq!(c[0]["field"], "evidence");
    assert_eq!(
        t["metadata"]["evidence"], DAMAGED,
        "the original is still shown"
    );
    let b = find(&build.id.to_string());
    assert!(
        b.get("corrections")
            .is_none_or(|c| c.as_array().is_some_and(Vec::is_empty)),
        "a step nobody corrected carries none: {b}"
    );
}

#[tokio::test]
async fn the_metadata_patch_refuses_the_reserved_key_naming_the_door() {
    let (app, jobs) = build_app();
    let (job, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c00a").await;
    send(
        &app,
        "POST",
        &corrections_uri(&job, &triage),
        Some(the_fix()),
    )
    .await;
    for body in [json!({ "corrections": [] }), json!({ "corrections": null })] {
        let (status, resp) = send(
            &app,
            "PATCH",
            &format!("/api/jobs/{}/metadata", job.id),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{resp}");
        assert!(
            resp.as_str().is_some_and(|e| e.contains("/corrections")),
            "the refusal names the door: {resp}"
        );
    }
    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(after.metadata["corrections"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn the_job_put_cannot_rewrite_or_erase_the_list() {
    let (app, jobs) = build_app();
    let (job, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c00b").await;
    send(
        &app,
        "POST",
        &corrections_uri(&job, &triage),
        Some(the_fix()),
    )
    .await;

    // A full-replacement body built without the list — the PUT's
    // ordinary shape for a caller that never read it.
    let mut body = serde_json::to_value(&job).unwrap();
    body["metadata"] = json!({ "area": "boss-jobs", "note": "put" });
    let (status, resp) = send(&app, "PUT", &format!("/api/jobs/{}", job.id), Some(body)).await;
    assert!(status.is_success(), "{status}: {resp}");
    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(after.metadata["note"], "put");
    assert_eq!(
        after.metadata["corrections"][0]["field"], "evidence",
        "the stored list is carried forward: {}",
        after.metadata
    );
}

#[tokio::test]
async fn the_terminal_freeze_names_the_corrections_door_at_both_sites() {
    let (app, jobs) = build_app();
    let (job, triage, _) = seed(&jobs, "00000000-0000-0000-0000-00000000c00c").await;
    let step_uri = format!("/api/jobs/{}/steps/{}", job.id, triage.id);

    let (status, put) = send(
        &app,
        "PUT",
        &step_uri,
        Some(json!({ "metadata": { "evidence": "rewritten", "disposition": "build" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{put}");
    let (status, patch) = send(
        &app,
        "PATCH",
        &format!("{step_uri}/metadata"),
        Some(json!({ "evidence": "rewritten" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{patch}");
    for body in [put, patch] {
        let hint = body["hint"].as_str().unwrap_or_default();
        assert!(
            hint.contains("POST /api/jobs/{id}/steps/{step_id}/corrections")
                && hint.contains("boss correct"),
            "the 409 names the door a correction goes through: {body}"
        );
    }
}
