//! An annotate write resolves the id a reader actually holds.
//!
//! THE DEFECT (packet cd7b0054), and it is a door that teaches the
//! wrong move rather than an operator being careless. Every surface in
//! this system prints EIGHT characters of an id — `boss orient`, the
//! yard, `boss triage`'s own output, a packet's references, a packet
//! title. Every verb takes eight (`boss triage`, `boss step complete`,
//! `boss prove`, `boss rerail` each document "8+ characters of its
//! id"). `GET /api/jobs/{id}` has resolved eight since c2e6a703.
//!
//! `PATCH /api/jobs/{id}/metadata` — the door CLAUDE.md names as THE
//! way to annotate a packet — demanded thirty-six, and answered
//! `invalid job id`.
//!
//! WHAT THE REFUSAL TAUGHT, measured three times in one day
//! (2026-09-21, on packets dd8120ba, 445c1494 and 5994de6d): a reader
//! holding eight characters, refused, CONSTRUCTED a thirty-six
//! character id by padding — `5994de6d-9f3a-…` — and sent that. All
//! three 404'd, which is the lucky outcome, because a padded uuid
//! cannot collide with a real one. The habit is the unlucky part, and
//! this project already carries the rule it breaks: never write a sha
//! you did not read. A refusal that says only "invalid" names neither
//! the shape it wants nor the route one door over that would have
//! answered.
//!
//! THIS REVERSES A DELIBERATE EARLIER DECISION, and says so out loud.
//! `get_job_by_prefix.rs` recorded: "WRITES ARE DELIBERATELY NOT
//! WIDENED. A prefix that resolves to the wrong job on a PUT/PATCH is
//! a silently-corrupted packet; a write holds the full id it just read
//! back." Both halves turned out not to hold.
//!
//!   - "resolves to the wrong job" CANNOT happen. `resolve_path_job_id`
//!     refuses an ambiguous prefix with 409 rather than picking one —
//!     `an_ambiguous_prefix_is_refused_on_a_write_too` below pins that
//!     on the write path. The silent corruption the decision guarded
//!     against has no route to occur.
//!   - "a write holds the full id it just read back" is false of the
//!     actual readers. They hold what the surface printed, which is
//!     eight. Measured, three times.
//!
//! So the decision traded a failure that cannot happen for one that
//! did, three times in a day, to the same person who had just read the
//! packet describing the trap.
//!
//! WHAT IS NOT WIDENED, and this time for a reason that survives
//! measurement: the step doors under `/api/jobs/{id}/steps/{step_id}`.
//! Nothing prints a step id short — a caller gets one from a step
//! listing, which carries it in full — so there is no eight-character
//! currency there to accommodate, and no measured slip.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

/// Two packets share the prefix `aaaaaaaa`; one owns `bcbcbcbc`.
/// Controlled ids make ambiguity deterministic — random v4 ids never
/// collide at eight characters in a test-sized set.
const AMBIG_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const AMBIG_B: &str = "aaaaaaaa-2222-4222-8222-222222222222";
const UNIQUE: &str = "bcbcbcbc-3333-4333-8333-333333333333";

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-op",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn job_at(id: &str, title: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: title.into(),
        owner_id: "emp-op".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({ "area": "boss-jobs" }),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

async fn seed() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    for (id, title) in [
        (AMBIG_A, "first sharer"),
        (AMBIG_B, "second sharer"),
        (UNIQUE, "the unique one"),
    ] {
        jobs.create_job(&job_at(id, title)).await.unwrap();
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );
    (router(state), jobs)
}

async fn patch_metadata(
    app: &axum::Router,
    id: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{id}/metadata"))
                .header("content-type", "application/json")
                .header("x-boss-user", admin_header())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn metadata_of(jobs: &InMemoryJobs, id: &str) -> serde_json::Value {
    jobs.get_job(&JobId::from_uuid(Uuid::parse_str(id).unwrap()))
        .await
        .unwrap()
        .expect("the packet is there")
        .metadata
}

/// THE BUG: the annotate door refused the id the reader was holding.
#[tokio::test]
async fn the_annotate_door_takes_the_eight_characters_a_surface_printed() {
    let (app, jobs) = seed().await;

    let (status, body) = patch_metadata(&app, "bcbcbcbc", r#"{"note":"annotated"}"#).await;
    assert!(
        status.is_success(),
        "the prefix every surface prints must reach the door CLAUDE.md names for annotating; \
         {status} {body}"
    );

    let md = metadata_of(&jobs, UNIQUE).await;
    assert_eq!(
        md["note"], "annotated",
        "the write landed on the packet the prefix names"
    );
    assert_eq!(
        md["area"], "boss-jobs",
        "and it MERGED — the existing keys survive, which is the whole contract of this door"
    );
}

/// A hyphenated prefix is still a prefix, the way a reader copies one
/// out of a longer id.
#[tokio::test]
async fn a_longer_hyphenated_prefix_reaches_the_door_too() {
    let (app, jobs) = seed().await;
    let (status, body) = patch_metadata(&app, "bcbcbcbc-3333", r#"{"note":"longer"}"#).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(metadata_of(&jobs, UNIQUE).await["note"], "longer");
}

/// THE HALF THAT MAKES THE WIDENING SAFE, and the reason the earlier
/// decision's stated risk cannot occur: two packets matching means the
/// caller asked a two-answer question, and a write is the last place
/// to guess. Neither packet may move.
#[tokio::test]
async fn an_ambiguous_prefix_is_refused_on_a_write_too() {
    let (app, jobs) = seed().await;

    let (status, _) = patch_metadata(&app, "aaaaaaaa", r#"{"note":"which one?"}"#).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an ambiguous prefix is refused, not resolved to whichever row came back first"
    );

    for id in [AMBIG_A, AMBIG_B] {
        let md = metadata_of(&jobs, id).await;
        assert!(
            md.get("note").is_none(),
            "{id} must not have been written to by a refused, ambiguous write"
        );
    }
}

/// A prefix matching nothing is absent, not malformed — the distinction
/// a caller needs to tell "I mistyped it" from "it is gone".
#[tokio::test]
async fn a_prefix_matching_nothing_is_absent_not_malformed() {
    let (app, _) = seed().await;
    let (status, _) = patch_metadata(&app, "ffffffff", r#"{"note":"nobody"}"#).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// THE REFUSAL THAT REMAINS MUST TEACH. Too short to resolve safely is
/// still a 400 — but "invalid job id" is what sent three writes off to
/// construct an id nobody had read. It has to name the shape and the
/// route.
#[tokio::test]
async fn what_is_still_refused_says_what_would_work() {
    let (app, _) = seed().await;

    for garbage in ["not-a-real-id-xyz", "bcb"] {
        let (status, body) = patch_metadata(&app, garbage, r#"{"note":"x"}"#).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{garbage} is not an id or a resolvable prefix"
        );
        let text = body.as_str().unwrap_or_default();
        assert!(
            text.contains('8'),
            "the refusal must name the shape that works — 8 or more characters. \
             `{garbage}` got: {text}"
        );
    }
}

/// THE CONTROL. A full uuid resolves exactly as it always did, on the
/// fast path, with no lookup — without this the tests above would pass
/// against a door that had merely stopped parsing ids.
#[tokio::test]
async fn a_full_uuid_still_takes_the_fast_path() {
    let (app, jobs) = seed().await;
    let (status, body) = patch_metadata(&app, UNIQUE, r#"{"note":"by full id"}"#).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(metadata_of(&jobs, UNIQUE).await["note"], "by full id");
}
