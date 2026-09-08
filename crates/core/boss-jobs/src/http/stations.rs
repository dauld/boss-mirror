//! Station read surfaces — the registry rows and the evaluated
//! queues (docs/design/stations.md).
//!
//! Reads pass through the SAME CurrentUser/policy path as the job
//! lists: the caller's read-scope Predicate on the `job` resource is
//! computed once and pushed into the packet query, so a station
//! queue can never show a caller a packet /api/jobs would hide. A
//! denied caller gets a clean empty collection, matching list_jobs.

use super::*;

use axum::extract::Path;

use crate::station_projection::derived_stations;
use crate::station_queue::evaluate_station;
use crate::stations::{StationError, StationRegistry, StationSpec};

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) fn stations_or_503<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<&Arc<dyn StationRegistry>, Response> {
    state.stations.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "station registry not configured",
        )
            .into_response()
    })
}

fn station_err_response(err: StationError) -> Response {
    match err {
        StationError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        StationError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        StationError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        // 422, not 400: the spec parsed and is well-formed JSON — it
        // just describes a queue that cannot behave as declared. Body
        // is the same `{ok, problems}` shape `_validate` returns, so
        // the editor renders a refused publish exactly like a failed
        // dry run.
        StationError::Unviable(problems) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(lint_result_json(&problems)),
        )
            .into_response(),
        StationError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

/// The lint result body — `{ok, problems}`. One definition shared by
/// the author-time dry run (200) and the publish refusal (422).
fn lint_result_json(problems: &[crate::station_lint::StationLintError]) -> serde_json::Value {
    serde_json::json!({
        "ok": problems.is_empty(),
        "problems": crate::station_lint::problems_json(problems),
    })
}

/// Station authoring is a network-configuration change, so it is
/// gated on the `workflow` resource — the same privilege that governs
/// the other registries a protocol is assembled from. A reader who
/// may see queues still cannot redraw them.
async fn station_policy_check<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    action: Action,
) -> Result<(), Response> {
    match state.policy.check(user, action, Resource::workflow()).await {
        Ok(Decision::Allow { .. }) => Ok(()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("policy check failed: {e}"),
        )
            .into_response()),
    }
}

/// Q's effective registry: the authored rows PLUS the stations the
/// active protocol set requires.
///
/// Two sources, one list, authored wins — the merge rule lives in
/// [`derived_stations`], which drops a projected row whose name is
/// already authored. Reading them together HERE rather than
/// materialising derived rows into the table is deliberate: a
/// projection persisted is a second copy that can go stale the moment
/// a protocol is published, and the whole reason the constraint
/// stations were never hand-authored is that keeping fifty-one rows in
/// step with the protocols is work nobody does.
///
/// A protocol-set read failure is an ERROR, not a fallback to the
/// authored rows. Serving four stations where there should be
/// fifty-five, with a 200 and no explanation, is the exact shape of
/// defect this whole change exists to remove.
async fn effective_stations<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    reg: &Arc<dyn StationRegistry>,
) -> Result<Vec<StationSpec>, Response> {
    let authored = reg.list_active().await.map_err(station_err_response)?;

    // No workflow registry wired: the same explicit seam every other
    // optional adapter keeps. There are no protocols to project from,
    // so the authored rows ARE the registry — not a degraded view of it.
    let Some(kinds) = state.kind_registry.as_ref() else {
        return Ok(authored);
    };
    let workflows = kinds.list_active(None).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot project stations — protocol set unreadable: {e}"),
        )
            .into_response()
    })?;

    let names: Vec<String> = authored.iter().map(|s| s.name.clone()).collect();
    let now = boss_clock_client::now_from(&state.clock).await;
    let mut all = authored;
    all.extend(derived_stations(&workflows, &names, now));
    all.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(all)
}

/// `GET /api/stations` — every active station row. The registry rows
/// themselves carry no packet data; the policy gate mirrors the job
/// list's posture (scope predicate on the `job` resource; a caller
/// who can see no packets sees no queues either).
pub(super) async fn list_stations<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let predicate = match state.policy.scope_predicate(&user, Resource::job()).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    if matches!(predicate, boss_policy_client::Predicate::None) {
        return Json(serde_json::json!({ "data": [], "total": 0 })).into_response();
    }
    match effective_stations(&state, reg).await {
        Ok(rows) => {
            let total = rows.len();
            Json(serde_json::json!({ "data": rows, "total": total })).into_response()
        }
        Err(r) => r,
    }
}

/// `GET /api/stations/load` — every station's depth and the age of
/// the oldest packet waiting in it, in one call.
///
/// WHY IT EXISTS. Reading congestion across the network meant calling
/// each station's queue in turn: on 2026-08-17 that was 55 round
/// trips, each re-fetching an overlapping packet set. The flow board
/// (Q1: "the station is the unit") and M's matchmaking both want the
/// same answer, so it is one surface rather than two.
///
/// AGE, NOT DEPTH, IS THE SIGNAL (flow-board Q2). Depth is close to
/// meaningless without a drain rate — ten role queues each read
/// exactly 48 the day this was written and none was a bottleneck, they
/// were a bug. Age needs no rate to interpret. So `oldest_age_days` is
/// what the board sorts by; depth and the advisory `over_limit` ride
/// along because a reader wants both.
///
/// WHAT `oldest_age_days` ACTUALLY MEASURES, stated rather than
/// implied: the age of the oldest MEMBER PACKET, from its `opened_on`.
/// That is packet age, not time-in-this-queue — a packet that spent
/// eight days in review before arriving here reads as eight days old
/// on arrival. Station membership is a packet-level predicate, so a
/// packet-level age is the honest per-station figure; it over-reports
/// rather than under-reports, which is the safer direction for a
/// congestion signal. The STEP-level answer — how long has this
/// obligation waited, from the `became_ready_at` stamp — is the
/// queue-age lens, `GET /api/jobs/queue-age` (2a0b034e).
///
/// OPEN PACKETS ONLY. Stations with a `terminal_window_days` also hold
/// recently-departed packets so a filer can see an outcome; those are
/// not congestion and are excluded here, so a load figure can be lower
/// than the same station's queue length.
///
/// Cost: one packet query plus one step query per packet, versus that
/// multiplied by the station count. Steps are fetched once and shared
/// across every predicate evaluation.
pub(super) async fn stations_load<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let predicate = match state.policy.scope_predicate(&user, Resource::job()).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    if matches!(predicate, boss_policy_client::Predicate::None) {
        return Json(serde_json::json!({ "data": [], "total": 0 })).into_response();
    }
    let stations = match effective_stations(&state, reg).await {
        Ok(s) => s,
        Err(r) => return r,
    };

    let scope = job_scope_from_predicate(&user, &predicate);
    let filter = JobFilter {
        status: Some(JobStatus::Open),
        scope,
        ..Default::default()
    };
    let (jobs, _total) = match state.jobs.list_jobs(&filter, MAX_LIMIT, 0).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Fetched ONCE and shared. Every constraint station matches on a
    // step, so per-station fetching would re-read the same rows 55
    // times.
    let mut packets = Vec::with_capacity(jobs.len());
    for job in jobs {
        let steps = state.jobs.list_steps(&job.id).await.unwrap_or_default();
        packets.push((job, steps));
    }

    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    let mut rows: Vec<serde_json::Value> = Vec::with_capacity(stations.len());
    for spec in &stations {
        // A per-actor station binds to the caller, exactly as its own
        // queue endpoint does; an unbindable one holds nothing rather
        // than everything.
        let Some(bound) = spec.bind_self(self_id(&user)) else {
            continue;
        };
        let members: Vec<&(boss_core::job::Job, Vec<boss_core::job::Step>)> = packets
            .iter()
            .filter(|(job, steps)| bound.predicate.matches(job, steps))
            .collect();
        let depth = members.len();
        let oldest = members.iter().map(|(j, _)| j.opened_on).min();
        rows.push(serde_json::json!({
            "station": bound.name,
            "kind": bound.kind,
            "depth": depth,
            "wip_limit": bound.wip_limit,
            "over_limit": bound.wip_limit.is_some_and(|l| depth as i64 > i64::from(l)),
            "oldest_opened_on": oldest,
            "oldest_age_days": oldest.map(|d| (today - d).num_days()),
            "capability_roles": bound.capability.as_ref().map(|c| c.roles.clone()),
        }));
    }
    let total = rows.len();
    Json(serde_json::json!({ "data": rows, "total": total })).into_response()
}

/// `GET /api/stations/{name}/queue` — the station's evaluated,
/// ordered queue: derived membership (the predicate, bound to the
/// caller, over their policy-scoped packets), data-declared
/// discipline, and the advisory `over_limit` verdict in the envelope.
pub(super) async fn station_queue<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    // Authored first, then the projection. A station that /api/stations
    // lists must have a queue that answers, or the registry advertises
    // doors that open onto nothing — so the lookup consults exactly the
    // same two sources the listing does, in the same order.
    let row = match reg.get_active(&name).await {
        Ok(s) => s,
        Err(StationError::NotFound(msg)) => {
            let found = match effective_stations(&state, reg).await {
                Ok(rows) => rows.into_iter().find(|s| s.name == name),
                Err(r) => return r,
            };
            match found {
                Some(s) => s,
                None => return station_err_response(StationError::NotFound(msg)),
            }
        }
        Err(e) => return station_err_response(e),
    };
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();

    // Bind the self placeholder ONCE, here, before any packet is
    // compared — a per-actor station is one registry row whose queue
    // depends on who is asking. A caller with no identity (guest) gets
    // the station's own empty queue: the envelope still describes the
    // station truthfully, it just holds nothing.
    let Some(spec) = row.bind_self(self_id(&user)) else {
        return Json(evaluate_station(&row, Vec::new(), today)).into_response();
    };

    // One policy path with /api/jobs: scope predicate → JobScope,
    // pushed into the adapter query.
    let predicate = match state.policy.scope_predicate(&user, Resource::job()).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    let scope = job_scope_from_predicate(&user, &predicate);

    // The evaluation universe: in-flight packets, because stations
    // hold in-flight traffic. A station declaring a terminal window
    // also wants recently-departed packets, so its status filter opens
    // up and the window narrows it back down in `evaluate_station` —
    // the pure half, where the rule is testable without a database.
    //
    // Kind and the bound `metadata_equals` push down into SQL so the
    // MAX_LIMIT page is drawn from the packets that can actually be
    // members. Without the metadata push-down, a per-actor station on
    // a busy install would page through the newest 1000 packets of the
    // whole company and find few of the caller's own.
    let filter = JobFilter {
        kind: spec.predicate.kind.clone(),
        status: spec
            .terminal_window_days
            .is_none()
            .then_some(JobStatus::Open),
        metadata_contains: metadata_containment(&spec.predicate),
        scope,
        ..Default::default()
    };
    let (jobs, _total) = match state.jobs.list_jobs(&filter, MAX_LIMIT, 0).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Steps are fetched when the predicate reads step state, or when
    // the station's lens declares it needs them to draw the queue —
    // "where has this packet got to" is a fact about its steps, and a
    // surface without them can only render a list.
    let needs_steps =
        spec.predicate.needs_steps() || spec.lens.as_ref().is_some_and(|l| l.with_steps);
    let mut packets = Vec::with_capacity(jobs.len());
    for job in jobs {
        let steps = if needs_steps {
            state.jobs.list_steps(&job.id).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        packets.push((job, steps));
    }

    Json(evaluate_station(&spec, packets, today)).into_response()
}

// ---------------------------------------------------------------------------
// Authoring — the runtime write path
// ---------------------------------------------------------------------------
//
// Stations are the substrate's routing table, and David's ratified
// answer (2026-08-13) was that they must be editable at run time:
// "stations need to be editable at run time. They should be data in a
// registry." The registry and the port already existed; without these
// routes the only way to redraw a queue was a SQL seed and a deploy,
// which is precisely the leak the three-layer reading calls out — a
// protocol that cannot be replaced without a deploy has leaked into
// the substrate.

/// `POST /api/stations` — append a draft version. Version numbering
/// is the registry's business (max+1); a draft is work in progress and
/// is deliberately NOT linted, matching the Workflow registry.
pub(super) async fn create_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<crate::stations::StationSpec>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Create).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/_validate` — author-time dry run. Lints a spec
/// WITHOUT persisting, calling the same `station_lint::gate_active`
/// the publish path enforces, so an editor showing "no problems"
/// publishes cleanly and a refused publish shows the same list.
///
/// Always 200: lint failures are data, not an HTTP error. The 422 on
/// publish and this 200 carry the same body.
pub(super) async fn validate_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<crate::stations::StationSpec>,
) -> Response {
    // Gated like create — the dry run is an authoring affordance.
    if let Err(r) = station_policy_check(&state, &user, Action::Create).await {
        return r;
    }
    let problems = match crate::station_lint::gate_active(&spec) {
        Ok(()) => Vec::new(),
        Err(p) => p,
    };
    (StatusCode::OK, Json(lint_result_json(&problems))).into_response()
}

/// `GET /api/stations/{name}/versions` — every version of one name,
/// oldest first, drafts and retired included. The audit view: what
/// this queue used to be, and what is staged to replace it.
pub(super) async fn list_station_versions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.list_versions(&name).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `GET /api/stations/{name}/versions/{version}` — one historical row.
pub(super) async fn get_station_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((name, version)): Path<(String, i32)>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.get_version(&name, version).await {
        Ok(row) => Json(row).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/{name}/publish` — promote the latest draft to
/// ACTIVE, retiring the incumbent.
///
/// The viability gate runs inside `StationRegistry::publish`, against
/// the draft row the transaction actually promotes — not against a
/// copy re-read here, which could race a concurrent author. An
/// unviable draft comes back as `StationError::Unviable` and leaves as
/// 422 + the problem list.
pub(super) async fn publish_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Update).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.publish(&name, &actor, now).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/{name}/retire` — close the station. Idempotent:
/// retiring an already-retired name is a 204 that records nothing.
pub(super) async fn retire_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, Action::Update).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.retire(&name, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => station_err_response(e),
    }
}

/// The `metadata_equals` clause of an already-BOUND predicate as a
/// containment document the adapter can push into SQL. `None` when the
/// predicate declares none.
///
/// Only ever built from a bound predicate: pushing an unbound `"@me"`
/// down would ask the database for packets that literally wrote the
/// placeholder.
fn metadata_containment(
    predicate: &crate::station_queue::StationPredicate,
) -> Option<serde_json::Value> {
    if predicate.metadata_equals.is_empty() {
        return None;
    }
    Some(serde_json::Value::Object(
        predicate
            .metadata_equals
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    ))
}
