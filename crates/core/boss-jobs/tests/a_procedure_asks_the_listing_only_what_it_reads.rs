//! Every `GET /api/jobs?…` a platform procedure tells an actor to send
//! uses only parameters the listing reads — judged by the listing
//! itself, not by a copy of its parameter list.
//!
//! WHY THIS FILE EXISTS. A procedure is the text an agent executes, and
//! the department-retro collect procedure told all thirteen W39 retros
//! to add `closed_since=<week start>` to their listing read. The
//! listing has no such parameter; it dropped it without a word, and
//! five retros collected all-time counts as their week (backlog
//! 7f3e871a, measured 2026-09-23: `closed_since=2099-01-01` still
//! returned a 09-17 close). The listing now refuses an unknown
//! parameter with a 400 — which turns the next such procedure into a
//! failed read at run time, a week after the car that wrote it. This
//! moves the refusal to the gate.
//!
//! HOW. Load the bundle with the seed's own loader, walk every string
//! in every row, and on each line that names `/api/jobs?` collect every
//! `name=` token between that mention and the next `/api/` door — the
//! query itself AND the prose around it ("add status=open … and
//! closed_since=<week start>" was prose, not the query literal). Each
//! name is then sent to the real router, alone, and a 400 that says
//! `unknown field` fails the test naming the kind and the parameter.
//! The listing's query struct stays the one definition (CLAUDE.md §9a).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::platform_bundle_path;
use boss_jobs::seed_loader::load_workflows;
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

const DOOR: &str = "/api/jobs?";

/// Every string anywhere in a JSON value — procedures live in step
/// `metadata_defaults`, but a description or a field doc is an
/// instruction too.
fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// The `name=` tokens a line asks the listing for: from each mention of
/// the door to the next `/api/` mention (another door's parameters are
/// not this one's) or the end of the line.
fn asked(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let mut rest = line;
        while let Some(at) = rest.find(DOOR) {
            let after = &rest[at + DOOR.len()..];
            let end = after.find("/api/").unwrap_or(after.len());
            let span = &after[..end];
            let bytes = span.as_bytes();
            for (i, _) in span.match_indices('=') {
                let start = span[..i]
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .map_or(0, |p| p + 1);
                let name = &span[start..i];
                // A JSON value's `"k":"v"` or an `==` is not a
                // parameter; a parameter name starts with a letter.
                let leads = name.chars().next().is_some_and(|c| c.is_ascii_lowercase());
                let quoted = start > 0 && bytes[start - 1] == b'"';
                if leads && !quoted {
                    names.insert(name.to_string());
                }
            }
            rest = &after[end..];
        }
    }
    names
}

fn app() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: chrono::DateTime::from_timestamp(1_790_000_000, 0).expect("instant"),
        simulated: false,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    router(JobsApiState::minimal(jobs, bus, publisher, policy, clock))
}

/// Does the listing refuse `name` as a parameter it does not read?
async fn refused(name: &str) -> Option<String> {
    let user = User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };
    let resp = app()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs?{name}=x"))
                .header("x-boss-user", serde_json::to_string(&user).expect("user"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let body = String::from_utf8_lossy(&body).into_owned();
    (status == StatusCode::BAD_REQUEST && body.contains("unknown field")).then_some(body)
}

#[tokio::test]
async fn every_listing_read_a_procedure_names_is_one_the_listing_accepts() {
    let rows = load_workflows(platform_bundle_path()).expect("the platform bundle parses");
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for row in &rows {
        let mut texts = Vec::new();
        strings(
            &serde_json::to_value(row).expect("a row serialises"),
            &mut texts,
        );
        let names: BTreeSet<String> = texts.iter().flat_map(|t| asked(t)).collect();
        for name in names {
            checked += 1;
            if let Some(body) = refused(&name).await {
                failures.push(format!("{}: `{name}=` — {body}", row.kind));
            }
        }
    }
    assert!(
        checked > 0,
        "no procedure names a listing read — the scan proves nothing"
    );
    assert!(
        failures.is_empty(),
        "a procedure tells its actor to send a parameter GET /api/jobs does not read:\n{}",
        failures.join("\n")
    );
}

/// The scan itself, on the measured text: the query literal's names and
/// the prose's, but not a JSON key inside `metadata=` and not another
/// door's parameters.
#[test]
fn the_scan_reads_the_query_and_its_prose_and_nothing_else() {
    let text = "  - GET /api/jobs?department={department} - the packets. Add status=open \
                and closed_since=<week start>.\n  - GET /api/sensors?unstamped=true\n  - \
                GET /api/jobs?kind=ops-request&metadata={\"department\":\"{department}\"}";
    let got: Vec<String> = asked(text).into_iter().collect();
    assert_eq!(
        got,
        ["closed_since", "department", "kind", "metadata", "status"]
    );
}
