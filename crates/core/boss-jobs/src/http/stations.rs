//! Station read surfaces — the registry rows and the evaluated
//! queues (docs/design/stations.md).
//!
//! Reads pass through the SAME CurrentUser/policy path as the job
//! lists: the caller's read-scope Predicate on the `job` resource is
//! computed once and pushed into the packet query, so a station
//! queue can never show a caller a packet /api/jobs would hide. A
//! denied caller is REFUSED (403), and a read that could not be made
//! fails the answer (500 naming it) — neither is ever an empty 200,
//! which is what an idle network says (backlogs 8dcd28ce, c11e9d3c).

use super::*;

use std::collections::BTreeMap;

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

/// The ACTIVE Workflow row per kind — what a step's agent block
/// resolves against ([`crate::agent_spec::resolved`]) and what
/// `station_reach` measures drift from.
///
/// Best-effort, and the degraded answer is the pre-51aef4dd one: with
/// no registry wired, or a read that fails, nothing resolves and each
/// station answers from the packets' own projections, as it did before
/// — a queue that still holds everything it held, rather than a
/// refusal that holds nothing.
pub(super) async fn active_rows<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> BTreeMap<String, crate::registry::WorkflowSpec> {
    match &state.kind_registry {
        Some(reg) => reg
            .list_active(None)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.kind.clone(), row))
            .collect(),
        None => BTreeMap::new(),
    }
}

/// The open packets `scope` reaches, each with its steps RESOLVED
/// against its kind's ACTIVE row — the one set every reader that
/// counts a station's members evaluates the predicate over. The load
/// and the yard's marshalling read (which the regions map and the
/// borders both take) each listed and resolved their own copy until
/// 2026-09-23, and the yard's copy never resolved: the agent station
/// read 213 in the load and 170 on the map (backlog 6c06ef65). One
/// definition, so the two cannot disagree again (CLAUDE.md §9a).
///
/// A step read that fails is an error, never an empty step list: a
/// packet with no steps matches no step clause, so it would drop out
/// of the count instead of failing the read.
pub(super) async fn resolved_open_packets<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
    active: &BTreeMap<String, crate::registry::WorkflowSpec>,
) -> Result<Vec<(boss_core::job::Job, Vec<boss_core::job::Step>)>, crate::port::JobsError> {
    let filter = JobFilter {
        status: Some(JobStatus::Open),
        scope,
        ..Default::default()
    };
    let (jobs, _total) = state.jobs.list_jobs(&filter, MAX_LIMIT, 0).await?;
    let mut packets = Vec::with_capacity(jobs.len());
    for job in jobs {
        let steps = state.jobs.list_steps(&job.id).await?;
        let steps = crate::agent_spec::resolved_steps(&steps, active.get(&job.kind));
        packets.push((job, steps));
    }
    Ok(packets)
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

/// The caller's packet-read predicate, or the refusal every station
/// read owes a caller who may read no packets.
///
/// One definition for the four read surfaces, because the posture is
/// one decision. Until 2026-09-23 each answered a denied scope with
/// `{"data": [], "total": 0}` and a 200 — the same bytes an idle
/// network sends — so a caller who could read nothing was told there
/// was nothing, and a board rendered it as calm (backlog 8dcd28ce).
/// A refused scope refuses: 403, naming the caller.
///
/// Refused on the TRANSLATED scope, not only `Predicate::None`: a
/// `DepartmentIs` grant for another department also translates to
/// [`JobScope::None`], and the queue it produced was the same empty
/// 200 for the same reason.
#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
async fn readable_predicate<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
) -> Result<boss_policy_client::Predicate, Response> {
    let predicate = state
        .policy
        .scope_predicate(user, Resource::job())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response()
        })?;
    match job_scope_from_predicate(user, &predicate) {
        JobScope::None => Err((
            StatusCode::FORBIDDEN,
            format!(
                "{} (role {}) may read no packets, so no station can be evaluated for them",
                user.id, user.role
            ),
        )
            .into_response()),
        _ => Ok(predicate),
    }
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
pub(super) async fn effective_stations<R: JobsRepository, B: EventBus>(
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

/// One station by name, resolved the way EVERY reader of a station
/// must resolve one: the authored row first, then the projection the
/// listing serves — the same two sources in the same order.
///
/// It is a function because it was two (923b6571, 2026-09-19). The
/// queue read fell back to the projection and the CLAIM did not, so a
/// claim naming a DERIVED station answered 404 — and since every
/// `(role, model)` agent inbox is derived (nothing authors
/// `a.platform-admin.opus-5-1m`; it is projected from the protocols'
/// agent blocks), no agent could claim from its own queue. A queue
/// that renders but cannot be claimed from is a display, not a
/// station. §9a: one definition, because two lookups of the same fact
/// drifted.
pub(super) async fn station_by_name<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    reg: &Arc<dyn StationRegistry>,
    name: &str,
) -> Result<StationSpec, Response> {
    match reg.get_active(name).await {
        Ok(s) => Ok(s),
        Err(StationError::NotFound(msg)) => effective_stations(state, reg)
            .await?
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| station_err_response(StationError::NotFound(msg))),
        Err(e) => Err(station_err_response(e)),
    }
}

/// `GET /api/stations` — every active station row. The registry rows
/// themselves carry no packet data; the policy gate is the one every
/// station read shares ([`readable_predicate`]: a caller who may read
/// no packets is refused, not shown an empty registry).
pub(super) async fn list_stations<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = readable_predicate(&state, &user).await {
        return r;
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
    // A caller who may read no packets is refused (403), not shown a
    // network with every depth at zero (backlog 8dcd28ce).
    let predicate = match readable_predicate(&state, &user).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let stations = match effective_stations(&state, reg).await {
        Ok(s) => s,
        Err(r) => return r,
    };

    let scope = job_scope_from_predicate(&user, &predicate);
    // The ACTIVE protocol per kind, read ONCE — the second half of the
    // omission question `station_reach` answers, and the row each
    // packet's agent block resolves against (backlog 51aef4dd), so the
    // depth here and the queue an agent reads count the same members.
    let active = active_rows(&state).await;

    // Fetched ONCE and shared. Every constraint station matches on a
    // step, so per-station fetching would re-read the same rows 55
    // times.
    let packets = match resolved_open_packets(&state, scope, &active).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    // Membership first, rows second: a row's `also_elsewhere` needs every
    // station's members before any row can be written.
    let memberships: Vec<(
        StationSpec,
        Vec<&(boss_core::job::Job, Vec<boss_core::job::Step>)>,
    )> = stations
        .iter()
        // A per-actor station binds to the caller, exactly as its own
        // queue endpoint does; an unbindable one holds nothing rather
        // than everything.
        .filter_map(|spec| spec.bind_self(self_id(&user)))
        .map(|bound| {
            let members = packets
                .iter()
                .filter(|(job, steps)| bound.predicate.matches(job, steps))
                .collect();
            (bound, members)
        })
        .collect();
    // STATIONS OVERLAP, so the depths do not add up to the work. A
    // packet stands at every station whose predicate it matches: on
    // 2026-09-23 the marshalling sidings summed to 517 over 303 distinct
    // packets — all 213 in the agent station also stood in
    // q.platform-admin.task — and nothing on the board said so (backlog
    // 140a2222). How many stations each packet stands at, counted over
    // the rows this read returns, so `distinct_packets` and each row's
    // `also_elsewhere` are figures the server counted, not ones a reader
    // has to derive by joining N queue reads by id.
    let standings: BTreeMap<String, usize> = memberships
        .iter()
        .flat_map(|(_, members)| members.iter().map(|(job, _)| job.id.to_string()))
        .fold(BTreeMap::new(), |mut acc, id| {
            *acc.entry(id).or_insert(0) += 1;
            acc
        });
    let mut rows: Vec<serde_json::Value> = Vec::with_capacity(memberships.len());
    for (bound, members) in &memberships {
        let depth = members.len();
        let also_elsewhere = members
            .iter()
            .filter(|(job, _)| standings.get(&job.id.to_string()).is_some_and(|n| *n > 1))
            .count();
        let oldest = members.iter().map(|(j, _)| j.opened_on).min();
        rows.push(serde_json::json!({
            "station": bound.name,
            "kind": bound.kind,
            "depth": depth,
            "wip_limit": bound.wip_limit,
            "over_limit": bound.wip_limit.is_some_and(|l| depth as i64 > i64::from(l)),
            "oldest_opened_on": oldest,
            "oldest_age_days": oldest.map(|d| (today - d).num_days()),
            // Packets this station's own predicate CANNOT see — absent,
            // not deprioritised, because a projected key never reached
            // their step (backlog abda9ab4). `depth + unreachable` is
            // what depth should have been; zero on a network with no
            // drift, and zero for the stations that read no projected
            // key at all.
            "unreachable": crate::station_reach::unreachable_packets(bound, &packets, &active),
            "capability_roles": bound.capability.as_ref().map(|c| c.roles.clone()),
            // Members that also stand at another station in `data`.
            "also_elsewhere": also_elsewhere,
        }));
    }
    let total = rows.len();
    Json(serde_json::json!({
        "data": rows,
        "total": total,
        // Distinct packets across every row's members — what the depth
        // column sums to once each packet is counted once.
        "distinct_packets": standings.len(),
    }))
    .into_response()
}

/// The window `GET /api/stations/flow` counts over when the caller
/// names none. A day is the shortest window in which every queue on
/// this network has had a chance to be worked at least once, so a
/// shorter default would report "not draining" about queues that were
/// merely quiet overnight.
const DEFAULT_FLOW_WINDOW_HOURS: i64 = 24;
/// Thirty days. The window is a filter over the log, not a page, so
/// the cost of a wide one is a wider scan; this caps it.
const MAX_FLOW_WINDOW_HOURS: i64 = 720;

#[derive(Debug, Deserialize, Default)]
pub(super) struct FlowQuery {
    window_hours: Option<i64>,
}

/// `GET /api/stations/flow` — the drain rate, per station.
///
/// WHY IT EXISTS. [`stations_load`] answers depth and the age of the
/// oldest packet, and says in its own header that depth is close to
/// meaningless without a drain rate. Depth tells an operator what is
/// waiting; it does not tell them whether a queue is FORMING, which is
/// a rate question — is it growing faster than it drains. That is the
/// question this answers, and it needs no new stamp anywhere: the two
/// transitions a step-waiting station's membership turns on are
/// already in the log as `step.ready.<kind>` and `step.done.<kind>`.
///
/// THE WINDOW IS WALL CLOCK. Not the sim clock, and not event time:
/// `boss-views/src/flow.rs` states the doctrine and the incident, and
/// the port method says the same. A rate on sim time would be a rate
/// about a brewery, not about a queue.
///
/// A STATION WHOSE FLOW CANNOT BE COUNTED SAYS SO. `arrived`, `served`
/// and `net` come back `null` with a `basis` of `"unavailable"` and a
/// sentence naming the clause that blinded it — never a zero, which
/// on this surface reads as "nothing is waiting". See
/// [`crate::station_flow`] for which predicates are countable and why.
///
/// The predicate is evaluated UNBOUND, so a per-actor station reports
/// blind rather than one person's flow dressed as the network's.
pub(super) async fn stations_flow<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(q): axum::extract::Query<FlowQuery>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    // Same read gate as every other station surface: a caller who may
    // read no packets is refused (403), never shown a calm network.
    if let Err(r) = readable_predicate(&state, &user).await {
        return r;
    }
    let stations = match effective_stations(&state, reg).await {
        Ok(s) => s,
        Err(r) => return r,
    };

    let window_hours = q
        .window_hours
        .unwrap_or(DEFAULT_FLOW_WINDOW_HOURS)
        .clamp(1, MAX_FLOW_WINDOW_HOURS);
    // `wall_now()`, not `state.clock` — the sanctioned wall reading,
    // and the only correct one here. The clock port answers the
    // BUSINESS question ("what day is it in this company's timeline"),
    // which on a demo deployment is the simulator's; a rate measured on
    // it would be a rate about a brewery, not about a queue. The window
    // has to be the same instant the log's `created_at` is written on.
    let as_of = boss_clock_client::wall_now();
    let since = as_of - chrono::Duration::hours(window_hours);
    let cells = match state.jobs.step_flow_cube(since).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let rows: Vec<serde_json::Value> = stations
        .iter()
        .map(
            |spec| match crate::station_flow::station_flow(&spec.predicate, &cells) {
                Ok(flow) => serde_json::json!({
                    "station": spec.name,
                    "kind": spec.kind,
                    "basis": "step-events",
                    "arrived": flow.arrived,
                    "served": flow.served,
                    "net": flow.net(),
                    "unavailable_reason": serde_json::Value::Null,
                }),
                Err(blind) => serde_json::json!({
                    "station": spec.name,
                    "kind": spec.kind,
                    "basis": "unavailable",
                    "arrived": serde_json::Value::Null,
                    "served": serde_json::Value::Null,
                    "net": serde_json::Value::Null,
                    "unavailable_reason": blind.reason(),
                }),
            },
        )
        .collect();
    let total = rows.len();
    Json(serde_json::json!({
        "data": rows,
        "total": total,
        "window_hours": window_hours,
        "since": since,
        "as_of": as_of,
    }))
    .into_response()
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
    // same two sources the listing does, in the same order, through the
    // one resolver the claim door also uses.
    let row = match station_by_name(&state, reg, &name).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();

    // Bind the self placeholder ONCE, here, before any packet is
    // compared — a per-actor station is one registry row whose queue
    // depends on who is asking. A caller with no identity is REFUSED
    // (401): there is nobody to bind `@me` to, so there is no queue to
    // answer with. It used to get the station's envelope with an empty
    // `data` and a 200 — the same bytes as "you have filed nothing",
    // a claim about the caller the server cannot make (backlog
    // c11e9d3c). It still never falls back to the unbound row.
    let Some(spec) = row.bind_self(self_id(&user)) else {
        return (
            StatusCode::UNAUTHORIZED,
            format!(
                "station {name} is per-actor (@me) and this request names no actor to bind it to"
            ),
        )
            .into_response();
    };

    // One policy path with /api/jobs: scope predicate → JobScope,
    // pushed into the adapter query — and a scope that admits nothing
    // is refused, not evaluated into an empty queue.
    let predicate = match readable_predicate(&state, &user).await {
        Ok(p) => p,
        Err(r) => return r,
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
    // A step clause may read the agent block, which a packet admitted
    // before its kind declared one does not carry: resolve it against
    // the ACTIVE row before the predicate reads it (backlog 51aef4dd —
    // 47 page-audit packets sat three days absent from the agent
    // station). One registry read per queue, only when steps are read.
    let active = if needs_steps {
        active_rows(&state).await
    } else {
        BTreeMap::new()
    };
    let mut packets = Vec::with_capacity(jobs.len());
    // The steps AS RECORDED, for a lens that draws them: the resolution
    // decides membership, it is never shown as what the packet holds.
    let mut recorded: BTreeMap<String, Vec<boss_core::job::Step>> = BTreeMap::new();
    for job in jobs {
        let steps = if needs_steps {
            // A failed read is NOT an empty step list. Taken as one
            // (`unwrap_or_default()`, until 2026-09-23), the step clause
            // could not match, so the packet silently left the queue and
            // the 200 reported one member fewer (backlog c11e9d3c). The
            // whole answer fails instead, naming the packet.
            let steps = match state.jobs.list_steps(&job.id).await {
                Ok(s) => s,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "cannot evaluate station {name} — steps of packet {} unreadable: {e}",
                            job.id
                        ),
                    )
                        .into_response();
                }
            };
            let resolved = crate::agent_spec::resolved_steps(&steps, active.get(&job.kind));
            recorded.insert(job.id.to_string(), steps);
            resolved
        } else {
            Vec::new()
        };
        packets.push((job, steps));
    }

    let mut queue = evaluate_station(&spec, packets, today);
    for (id, steps) in queue.steps.iter_mut() {
        if let Some(as_recorded) = recorded.remove(id) {
            *steps = as_recorded;
        }
    }
    Json(queue).into_response()
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
