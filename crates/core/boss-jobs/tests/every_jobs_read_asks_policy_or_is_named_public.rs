//! Every read the jobs API serves asks policy, or is named public with
//! a reason (backlog e5f7b51e, 2026-09-26).
//!
//! WHY IT EXISTS. `/ics/{*rest}` is mounted sessionless onto the jobs
//! upstream on every instance, so until the gateway's dot-segment fix
//! (1d9b7db7) `GET /ics/%2e%2e/api/<path>` reached this service with no
//! `x-boss-user` — and so does anything reaching the jobs port directly
//! (2710c8fc). The triage of this item measured 64 GET routes on
//! origin/main 9df57797 and found 24 that consulted nothing: full
//! packets, their audit slices, the assignments queue, employee
//! schedules. Those were fixed one door at a time (046832d3, e84de48e,
//! a621d091, 7ae9ccec, 19f08bd6); nothing held the NEXT door to the
//! same rule. This pin does.
//!
//! THE ROUTE LIST IS DERIVED, NOT TYPED (CLAUDE.md §9a). It is read out
//! of the router source the service binary assembles: the binary's
//! `router_shared` and every `boss_jobs::<module>::http::router` it
//! merges, and within each, every `.route(` whose method router carries
//! a `get(`. A GET added to any of them is in this test's list the
//! moment it is written. A router the binary merges that this test does
//! not also mount answers the test's fallback (418), and fails naming
//! the route — so a new module cannot slip past by not being assembled
//! here.
//!
//! THE JUDGEMENT IS MEASURED, NOT READ FROM THE HANDLER. Each route is
//! requested exactly as the anonymous caller arrives — no headers at
//! all — against a router with every optional port wired (an unwired
//! port answers 503 before most handlers reach their check, which would
//! say nothing about the door). A route passes when it REFUSES that
//! caller (401 / 403), or when it ASKED the policy engine while
//! answering (a recording client counts every `check` and
//! `scope_predicate`; the engine behind it denies everything, so a
//! scoped read answers from an empty scope). Anything else must sit on
//! one of the two lists below:
//!
//! - [`PUBLIC`]: sessionless by design, each with its reason. A row
//!   that starts asking policy fails the test — it is not public any
//!   more, so the list must not say it is.
//! - [`PENDING`]: a RATCHET. The reads that still answer an anonymous
//!   caller unasked, measured at this car, each naming why it is not
//!   yet fixed. It may only shrink: a pending route that now asks fails
//!   the test until its row is deleted, and a new route cannot be added
//!   without a reviewer reading the row that adds it.
//!
//! Both lists are held to the derived set: a row naming a route the
//! router no longer serves as a GET fails, so neither list outlives its
//! routes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::scheduling::{
    CalendarTokenSha256, NewScheduledAssignment, NewTechAvailability, ScheduledAssignment,
    SchedulingError, SchedulingRepository, TechAvailability, TechShiftPattern, WeekGridRow,
};
use boss_policy_client::{
    Decision, FakePolicyClient, PolicyClient, PolicyClientError, Predicate, Resource, User,
};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use tower::ServiceExt;
use uuid::Uuid;

/// Sessionless by design. Each reason must hold for EVERY instance: a
/// read a stranger may make on only some instances is the gateway's
/// per-tenant `PUBLISHABLE` table, not a reason for this door to skip
/// policy.
const PUBLIC: &[(&str, &str)] = &[
    (
        "/api/jobs/health",
        "liveness and build; the off-cluster watchdog reads it with no identity, and the machine gate exempts it for the same reason",
    ),
    (
        "/api/jobs/live",
        "the public landing window (gateway PUBLISHABLE): open counts per kind and twelve recent titles, deliberately unscoped (19f08bd6 left it so)",
    ),
    (
        "/api/jobs/sim-clock/stream",
        "the simulated date the SPA badge shows; a clock reading, nothing of any packet",
    ),
    (
        "/api/jobs/step-types",
        "the StepType registry: the alphabet of legal transitions, platform code, no tenant data",
    ),
    (
        "/api/jobs/job-edges",
        "the declared job-to-job link fields: schema, no rows",
    ),
    (
        "/api/tenant/edit-level",
        "the instance's hosting edit level word off the tenant manifest; the dispatch door and a lint read it",
    ),
    (
        "/api/flights/mine",
        "answers only the flight codes whose audience includes the CALLER; an anonymous caller is in no audience a packet names (design c4c2a607)",
    ),
    (
        "/ics/{token}/calendar.ics",
        "the calendar feed: the token in the path IS the credential, and it is the gateway's one PUBLIC_BY_DESIGN jobs route",
    ),
    (
        "/api/scheduling/calendar-tokens/logged-raw",
        "one integer, how many feeds still open with a token the log holds in the clear; any caller by design (4aaff4dc), and the proof of that design reads it to zero",
    ),
];

/// THE RATCHET: reads that still answer an anonymous caller without
/// asking policy, measured on this car. Delete a row when its route is
/// fixed — the test fails until you do. Never add one without a reason
/// a reviewer can argue with.
const PENDING: &[(&str, &str)] = &[
    (
        "/api/estate/nodes",
        "the estate registry: every host's LAN address, roles and capacity. Not refusable yet: infra/estate/node-roles.sh reads it with a bare curl on every host converge, and a refusal there installs the cached roles or [always] only — the converge must sign first",
    ),
    (
        "/api/estate/observations",
        "the estate loop's observation series (disk, units, evictions); commented guest-readable since d471a8ce, and its shell readers are not all signed — the same decision as /api/estate/nodes",
    ),
    (
        "/api/estate/comparisons",
        "the estate loop's comparison series; the same decision as /api/estate/nodes",
    ),
];

/// A query string a route cannot be reached without: its extractor
/// refuses the request (400) before the handler runs, which would say
/// nothing about whether the handler asks. Held to the derived set.
const QUERY: &[(&str, &str)] = &[
    ("/api/agent-runs/profiles", "since=2020-01-01T00:00:00Z"),
    ("/api/surface-opens/rollup", "since=2020-01-01T00:00:00Z"),
];

/// A station the queue route can find, so it reaches its check rather
/// than answering 404 for a name no registry holds.
const STATION: &str = "pin-probe";

// ---------------------------------------------------------------------------
// The route list, derived from the router source the binary assembles
// ---------------------------------------------------------------------------

fn crate_file(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The body of `fn <name>` in `src`: from its signature to the first
/// line that closes a top-level item. Router fns are flat builder
/// chains, so the first `\n}` after the signature is their end.
fn fn_body<'a>(src: &'a str, signature: &str, file: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("{file}: no `{signature}` — did the router move?"));
    let rest = &src[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{file}: `{signature}` never closes"));
    &rest[..end]
}

/// Whether `text` calls `get(` as a method router — `get(handler)` or
/// `.get(handler)` — rather than naming a handler that starts `get_`.
fn has_get(text: &str) -> bool {
    text.match_indices("get(").any(|(i, _)| {
        text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// Every `(path, has GET)` a router fn body declares.
fn routes_in(body: &str, file: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(".route(") {
        rest = &rest[i + ".route(".len()..];
        let trimmed = rest.trim_start();
        let lit = trimmed.strip_prefix('"').unwrap_or_else(|| {
            panic!(
                "{file}: a `.route(` whose path is not a string literal — this pin reads paths \
                 from the source; spell it as a literal or teach the pin: {}",
                &trimmed[..trimmed.len().min(80)]
            )
        });
        let close = lit.find('"').expect("unterminated path literal");
        let path = lit[..close].to_string();
        // The method router runs to the `.route(` call's closing paren.
        let after = &lit[close + 1..];
        let mut depth = 1usize;
        let mut end = after.len();
        for (j, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = j;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push((path, has_get(&after[..end])));
        rest = &after[end..];
    }
    out
}

/// Every GET path the service binary serves, read from the source it
/// assembles.
fn derived_get_routes() -> BTreeSet<String> {
    let bin = crate_file("src/bin/boss_jobs_api.rs");
    assert!(
        bin.contains("router_shared(state)"),
        "the binary no longer builds its app from router_shared — re-derive this pin's source"
    );
    let mut sources: Vec<(String, String)> = vec![(
        "src/http/mod.rs".to_string(),
        "pub fn router_shared<".to_string(),
    )];
    for (i, _) in bin.match_indices("boss_jobs::") {
        let tail = &bin[i + "boss_jobs::".len()..];
        if let Some(module) = tail.split("::http::router(").next()
            && tail.starts_with(&format!("{module}::http::router("))
            && module.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        {
            sources.push((
                format!("src/{module}/http.rs"),
                "pub fn router(".to_string(),
            ));
        }
    }
    assert!(
        sources.len() >= 10,
        "read only {} router sources out of the binary; the pin's reader is broken, not the routes",
        sources.len()
    );
    let mut gets = BTreeSet::new();
    for (file, signature) in &sources {
        let src = crate_file(file);
        let body = fn_body(&src, signature, file);
        let routes = routes_in(body, file);
        assert!(
            !routes.is_empty(),
            "{file}: read no routes from `{signature}`"
        );
        gets.extend(routes.into_iter().filter(|(_, get)| *get).map(|(p, _)| p));
    }
    gets
}

// ---------------------------------------------------------------------------
// The assembled router, every optional port wired
// ---------------------------------------------------------------------------

/// Denies everything and counts every question it is asked.
struct Recording {
    inner: FakePolicyClient,
    asked: Arc<AtomicUsize>,
}

#[async_trait]
impl PolicyClient for Recording {
    async fn check(
        &self,
        user: &User,
        action: boss_policy_client::Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.inner.check(user, action, resource).await
    }
    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.inner.scope_predicate(user, resource).await
    }
}

/// A scheduling repository that holds nothing: the reads under test
/// must refuse before they reach storage, and one that does not answers
/// an error the test still judges by whether policy was asked.
struct NoSchedules;

fn empty<T>() -> Result<T, SchedulingError> {
    Err(SchedulingError::Storage("this fake holds nothing".into()))
}

#[async_trait]
impl SchedulingRepository for NoSchedules {
    async fn create_availability(
        &self,
        _: NewTechAvailability,
        _: &EventStamp,
    ) -> Result<TechAvailability, SchedulingError> {
        empty()
    }
    async fn list_availability(
        &self,
        _: Option<&str>,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<TechAvailability>, SchedulingError> {
        empty()
    }
    async fn delete_availability(
        &self,
        _: Uuid,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        empty()
    }
    async fn create_assignment(
        &self,
        _: NewScheduledAssignment,
        _: &EventStamp,
    ) -> Result<ScheduledAssignment, SchedulingError> {
        empty()
    }
    async fn get_assignment(
        &self,
        _: Uuid,
    ) -> Result<Option<ScheduledAssignment>, SchedulingError> {
        empty()
    }
    async fn list_assignments(
        &self,
        _: Option<&str>,
        _: Option<Uuid>,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<ScheduledAssignment>, SchedulingError> {
        empty()
    }
    async fn update_assignment_status(
        &self,
        _: Uuid,
        _: boss_jobs::scheduling::AssignmentStatus,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        empty()
    }
    async fn delete_assignment(
        &self,
        _: Uuid,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        empty()
    }
    #[allow(clippy::too_many_arguments)]
    async fn upsert_shift_pattern(
        &self,
        _: &str,
        _: i16,
        _: NaiveTime,
        _: NaiveTime,
        _: &str,
        _: NaiveDate,
        _: &EventStamp,
    ) -> Result<TechShiftPattern, SchedulingError> {
        empty()
    }
    async fn list_shift_patterns(
        &self,
        _: Option<&str>,
    ) -> Result<Vec<TechShiftPattern>, SchedulingError> {
        empty()
    }
    async fn materialize_shift_patterns(
        &self,
        _: NaiveDate,
        _: NaiveDate,
    ) -> Result<i64, SchedulingError> {
        empty()
    }
    async fn week_grid(
        &self,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
        _: Option<&[String]>,
    ) -> Result<Vec<WeekGridRow>, SchedulingError> {
        empty()
    }
    async fn calendar_feed_created_at(
        &self,
        _: &str,
    ) -> Result<Option<DateTime<Utc>>, SchedulingError> {
        empty()
    }
    async fn rotate_calendar_token(
        &self,
        _: &str,
        _: &CalendarTokenSha256,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        empty()
    }
    async fn revoke_calendar_token(
        &self,
        _: &str,
        _: &CalendarTokenSha256,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        empty()
    }
    async fn count_calendar_tokens_logged_raw(&self) -> Result<i64, SchedulingError> {
        empty()
    }
    async fn revoke_calendar_tokens_logged_raw(
        &self,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<u64, SchedulingError> {
        empty()
    }
    async fn employee_by_calendar_token(
        &self,
        _: &CalendarTokenSha256,
    ) -> Result<Option<String>, SchedulingError> {
        empty()
    }
}

/// The status a route answers when THIS assembly does not mount it: a
/// router the binary merges and the test does not.
const UNMOUNTED: StatusCode = StatusCode::IM_A_TEAPOT;

/// The same routers the binary merges, each over in-memory adapters,
/// with every optional port wired.
fn assembled(asked: Arc<AtomicUsize>) -> Router {
    let policy: Arc<dyn PolicyClient> = Arc::new(Recording {
        inner: FakePolicyClient::deny_all(),
        asked,
    });
    let clock: Arc<dyn boss_clock_client::ClockClient> =
        Arc::new(boss_clock_client::WallClockClient);
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let kinds: Arc<dyn boss_jobs::registry::WorkflowRegistry> =
        Arc::new(boss_jobs::registry::InMemoryWorkflows::new());
    let cadence = Arc::new(boss_jobs::cadence::InMemoryCadence::new(Vec::new()));
    let delivery = Arc::new(boss_jobs::delivery::InMemoryDeliveryPolicy::new(Vec::new()));
    let sensors = Arc::new(boss_jobs::sensors::InMemorySensors::new());
    let stations = boss_jobs::InMemoryStations::new();
    let mut station = boss_jobs::StationSpec::draft(
        STATION,
        "The pin's station",
        boss_jobs::StationKind::Batch,
        boss_jobs::station_queue::StationPredicate::default(),
        Utc::now(),
    );
    station.status = boss_jobs::registry::WorkflowStatus::Active;
    stations.seed(station).expect("seed the pin's station");
    let state = JobsApiState {
        kind_registry: Some(kinds.clone()),
        plugin_registry: Some(Arc::new(boss_jobs::step_plugins::InMemoryStepPlugins::new())),
        job_edges: Some(Arc::new(boss_jobs::job_edges::InMemoryJobEdges)),
        stations: Some(Arc::new(stations)),
        cadence: Some(cadence.clone()),
        dispatcher_firings: Some(Arc::new(
            boss_jobs::dispatcher_firings::InMemoryDispatcherFirings::new(Vec::new()),
        )),
        delivery: Some(delivery.clone()),
        yard_moves: Some(Arc::new(boss_jobs::moves::MovesFeed::new(Arc::new(
            boss_jobs::moves::InMemoryMoves::new(),
        )))),
        ..JobsApiState::minimal(jobs.clone(), bus, publisher, policy.clone(), clock.clone())
    };
    router(state)
        .merge(boss_jobs::scheduling::http::router(
            boss_jobs::scheduling::http::SchedulingApiState {
                repo: Arc::new(NoSchedules),
                publisher: None,
                clock: clock.clone(),
                policy: policy.clone(),
            },
        ))
        .merge(boss_jobs::cadence::http::router(
            boss_jobs::cadence::http::CadenceApiState {
                repo: cadence.clone(),
                registry: cadence,
                policy: policy.clone(),
                clock: clock.clone(),
            },
        ))
        .merge(boss_jobs::delivery::http::router(
            boss_jobs::delivery::http::DeliveryPolicyApiState { repo: delivery },
        ))
        .merge(boss_jobs::credentials::http::router(
            boss_jobs::credentials::http::CredentialsApiState {
                registry: Arc::new(boss_jobs::credentials::InMemoryCredentials::new(Vec::new())),
            },
        ))
        .merge(boss_jobs::agent_runs::http::router(
            boss_jobs::agent_runs::http::AgentRunsApiState {
                log: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(Vec::new())),
            },
        ))
        .merge(boss_jobs::surface_opens::http::router(
            boss_jobs::surface_opens::http::SurfaceOpensApiState {
                repo: Arc::new(boss_jobs::surface_opens::InMemorySurfaceOpens::new()),
            },
        ))
        .merge(boss_jobs::sensors::http::router(
            boss_jobs::sensors::http::SensorsApiState {
                repo: sensors.clone(),
            },
        ))
        .merge(boss_jobs::agents::http::router(
            boss_jobs::agents::http::AgentsApiState {
                registry: Arc::new(boss_jobs::agents::InMemoryAgents::new()),
                classes: None,
            },
        ))
        .merge(boss_jobs::department::http::router(
            boss_jobs::department::http::DepartmentsApiState {
                departments: Some(Arc::new(
                    boss_jobs::department::registry::InMemoryDepartments::new(),
                )),
                kinds: Some(kinds),
                jobs,
                sensors: Some(sensors),
                rules: Some(Arc::new(boss_jobs::department::rules::FakeDispatcherRules(
                    Vec::new(),
                ))),
                classes: None,
            },
        ))
        .fallback(|| async { (UNMOUNTED, "this pin does not mount the route") })
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ))
}

/// A concrete path for a route pattern: a uuid for an id, a version
/// number for a version, a word for any other segment.
fn concrete(pattern: &str) -> String {
    let path = pattern
        .split('/')
        .map(
            |seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some("id") | Some("step_id") => Uuid::nil().to_string(),
                Some("version") | Some("v") => "1".to_string(),
                Some(_) => STATION.to_string(),
                None => seg.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join("/");
    match QUERY.iter().find(|(route, _)| *route == pattern) {
        Some((_, query)) => format!("{path}?{query}"),
        None => path,
    }
}

/// How a route answered a caller with no identity.
#[derive(Debug)]
struct Answer {
    status: StatusCode,
    asked: usize,
}

impl Answer {
    fn checked(&self) -> bool {
        matches!(
            self.status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) || self.asked > 0
    }
}

async fn measure(routes: &BTreeSet<String>) -> BTreeMap<String, Answer> {
    let mut out = BTreeMap::new();
    for route in routes {
        let asked = Arc::new(AtomicUsize::new(0));
        let app = assembled(asked.clone());
        // No header of any kind: exactly what the gateway forwards for a
        // sessionless route, and what a direct caller of the port sends.
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(concrete(route))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        out.insert(
            route.clone(),
            Answer {
                status: response.status(),
                asked: asked.load(Ordering::SeqCst),
            },
        );
    }
    out
}

#[test]
fn the_reader_finds_the_routes_the_binary_serves() {
    let gets = derived_get_routes();
    // Controls whose answer is known: one from router_shared, one from a
    // merged module, one that is a POST only and must NOT be read as a GET.
    for known in ["/api/jobs/{id}", "/api/agents", "/api/scheduling/week-grid"] {
        assert!(gets.contains(known), "derived list lost {known}: {gets:#?}");
    }
    for post_only in ["/api/jobs/{id}/steps/{step_id}/claim", "/api/sensors/batch"] {
        assert!(
            !gets.contains(post_only),
            "{post_only} is a POST, read as a GET"
        );
    }
    assert!(gets.len() >= 60, "only {} GET routes derived", gets.len());
}

#[tokio::test]
async fn every_jobs_read_asks_policy_or_is_named_public() {
    let gets = derived_get_routes();
    let answers = measure(&gets).await;

    let public: BTreeMap<&str, &str> = PUBLIC.iter().copied().collect();
    let pending: BTreeMap<&str, &str> = PENDING.iter().copied().collect();
    let mut failures = Vec::new();

    for name in public
        .keys()
        .chain(pending.keys())
        .chain(QUERY.iter().map(|(route, _)| route))
    {
        if !gets.contains(*name) {
            failures.push(format!(
                "{name}: listed, but the router serves no such GET — delete its row"
            ));
        }
    }
    for name in public.keys() {
        if pending.contains_key(name) {
            failures.push(format!("{name}: on both lists — it is one or the other"));
        }
    }
    for (route, answer) in &answers {
        let route = route.as_str();
        if answer.status == UNMOUNTED {
            failures.push(format!(
                "{route}: the binary serves it, but this pin's assembly does not mount its \
                 router — merge that router in `assembled`"
            ));
            continue;
        }
        match (answer.checked(), public.get(route), pending.get(route)) {
            (true, None, None) | (false, Some(_), None) | (false, None, Some(_)) => {}
            (true, Some(_), _) => failures.push(format!(
                "{route}: asks policy now ({answer:?}) — it is not public; delete its PUBLIC row"
            )),
            (true, None, Some(_)) => failures.push(format!(
                "{route}: asks policy now ({answer:?}) — the ratchet shrinks; delete its PENDING row"
            )),
            (false, None, None) => failures.push(format!(
                "{route}: answered a caller with no identity {} without asking policy — \
                 ask policy (CurrentUser + a Read check, or trust::can_read on an operator \
                 door), or name it PUBLIC with the reason it holds on every instance",
                answer.status
            )),
            (false, Some(_), Some(_)) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} jobs-API GET routes break the rule:\n  {}",
        failures.len(),
        gets.len(),
        failures.join("\n  ")
    );
}
