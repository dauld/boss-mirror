//! `GET /api/yard/regions?window=24h` — the IT system map's KPI read
//! (design 0524fc95, decided 2026-09-19; car 1).
//!
//! Nine regions — dock, gates, track, shed, arrivals, garage,
//! receiving, marshalling, shop-floor — each with a count, a
//! clear/busy/troubled
//! state and a trend over the window, computed ONCE here from the rows
//! this process already reads for the yard status, so the map (car 2),
//! the yard and `boss orient` answer with one voice. The aggregation is
//! [`crate::regions::regions`], pure and unit-tested; this handler is
//! the adapter that reads the rows.
//!
//! THE SAME PASS AS THE YARD STATUS. The status and the map used to be
//! two client-side derivations of six reads; here the map is a function
//! of the status's own pass ([`super::yard::read_yard`]) plus the
//! windowed tails the trends need — the closed trains, cars and
//! gate-runs of the last two windows, the receiving yard's inbound
//! packets, the marshalling stations' load and flow, and the shop
//! floor's runs, crews and declared capacity. Every read
//! that cannot answer states so (`None` → a troubled region with
//! `count: null`), never an empty region that reads as clear.

use super::*;

use crate::regions::{self, RegionInputs, RunnerHost, StationReading};
use crate::yard;

/// How many rows each windowed tail may hold. A limit is not a filter:
/// the reads narrow in the query (`closed_since`, `metadata_contains`)
/// and this only caps the page. Measured 2026-09-19 at ~17 arrivals a
/// day and ~112 gate-runs in two days, a week-wide window (the maximum,
/// two weeks of tail) fits under it.
const TAIL_WINDOW: i64 = 400;

#[derive(Debug, Deserialize, Default)]
pub(super) struct RegionsQuery {
    window: Option<String>,
}

pub(super) async fn yard_regions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(q): axum::extract::Query<RegionsQuery>,
) -> Response {
    // A window that cannot be read is refused, not defaulted: a map
    // asked for a week that answered a day would be a confident wrong
    // trend.
    let window_hours = match regions::parse_window(q.window.as_deref()) {
        Ok(h) => h,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let now = boss_clock_client::now_from(&state.clock).await;
    let rows = match read_map(&state, &user, now, window_hours).await {
        Ok(rows) => rows,
        Err(resp) => return resp,
    };
    let map = regions::regions(&rows.inputs(now, window_hours));
    Json(with_now(map, now)).into_response()
}

/// Everything ONE pass over the map's rows read, owned. Both
/// read-models over that pass — the regions (design 0524fc95) and the
/// borders (`super::borders`, design d2154293) — are functions of THESE
/// rows, never a second set of queries free to disagree with them
/// (which is the defect the regions read was built to end).
pub(super) struct MapRows {
    read: super::yard::YardRead,
    closed_trains: Vec<(Job, Vec<Step>)>,
    cars: Vec<(Job, Vec<Step>)>,
    gate_runs: Vec<Job>,
    inbound: Option<Vec<Job>>,
    stations: Option<Vec<StationReading>>,
    /// THE RUNNERS' EVIDENCE (design d2154293, car 5). A runner leaves
    /// no heartbeat this process can read — only the ops-requests it
    /// answered. `None` on any failed read, which the machine reports
    /// as unknown rather than as an idle runner.
    ops_requests: Option<Vec<(Job, Vec<Step>)>>,
    /// WHICH HOSTS SHOULD HAVE ONE (backlog 49ed87b4). The evidence
    /// above can only name the runners that HAPPENED to answer
    /// something, so a host that died is drawn nowhere — and an absent
    /// glyph is indistinguishable from a runner that does not exist.
    /// The estate registry is the independent statement of what should
    /// be there. `None` on a failed read: one unknown machine, never an
    /// estate with no runners in it.
    runner_hosts: Option<Vec<RunnerHost>>,
    /// THE SHOP FLOOR'S RUNS (backlog 94c6ffd0): the `agent-run`
    /// packets open or closed within reach. Steps ride the OPEN ones
    /// only — they are what says a run has finished without reporting —
    /// and there are at most a registry's worth of those.
    agent_runs: Option<Vec<(Job, Vec<Step>)>>,
    /// The crews standing on it: the open `work-session` packets.
    sessions: Option<Vec<Job>>,
    /// The bound those runs are read against, from the agents registry.
    run_capacity: Option<usize>,
}

impl MapRows {
    /// The rows as the pure aggregations take them.
    pub(super) fn inputs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        window_hours: i64,
    ) -> RegionInputs<'_> {
        RegionInputs {
            status: &self.read.status,
            dock_reading: self.read.dock_reading,
            open_trains: &self.read.open_trains,
            closed_trains: &self.closed_trains,
            cars: &self.cars,
            gate_runs: &self.gate_runs,
            inbound: self.inbound.as_deref(),
            stations: self.stations.as_deref(),
            conductor: Some(&self.read.health),
            ops_requests: self.ops_requests.as_deref(),
            runner_hosts: self.runner_hosts.as_deref(),
            agent_runs: self.agent_runs.as_deref(),
            sessions: self.sessions.as_deref(),
            run_capacity: self.run_capacity,
            now,
            window_hours,
        }
    }
}

/// The map's read sequence. A caller whose scope is empty gets an
/// empty, well-formed map rather than a 403 — the same read gate every
/// queue surface keeps — so this answers empty rows, not an error.
pub(super) async fn read_map<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    now: chrono::DateTime<chrono::Utc>,
    window_hours: i64,
) -> Result<MapRows, Response> {
    let predicate = match state.policy.scope_predicate(user, Resource::job()).await {
        Ok(p) => p,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response());
        }
    };
    if matches!(predicate, boss_policy_client::Predicate::None) {
        return Ok(MapRows {
            read: empty_read(),
            closed_trains: Vec::new(),
            cars: Vec::new(),
            gate_runs: Vec::new(),
            inbound: Some(Vec::new()),
            stations: Some(Vec::new()),
            ops_requests: Some(Vec::new()),
            runner_hosts: Some(Vec::new()),
            agent_runs: Some(Vec::new()),
            sessions: Some(Vec::new()),
            run_capacity: None,
        });
    }
    let scope = job_scope_from_predicate(user, &predicate);

    let read = read_yard(state, user, scope.clone(), now).await?;

    // The tails reach back two windows: this one and the previous.
    let reach = (now - chrono::Duration::hours(2 * window_hours)).date_naive();
    let tail =
        |kind: &str, status: Option<JobStatus>, contains: Option<serde_json::Value>| JobFilter {
            kind: Some(kind.to_string()),
            status,
            closed_since: Some(reach),
            metadata_contains: contains,
            scope: scope.clone(),
            ..Default::default()
        };

    // Closed trains: the arrivals, narrowed on `outcome` in the query
    // (the recent tail is overwhelmingly refused boards — see
    // `read_yard`'s arrived read), WITH steps for the time at CI and
    // the boarding instant; and the cancellations, rows only — the
    // arrivals region reads `boarded_jobs` and `closed_at` off the
    // metadata to tell a red train from a refused board.
    let mut closed_trains: Vec<(Job, Vec<Step>)> = Vec::new();
    let arrived = match state
        .jobs
        .list_jobs(
            &tail(
                "pr-train",
                None,
                Some(serde_json::json!({ "outcome": "arrived" })),
            ),
            TAIL_WINDOW,
            0,
        )
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    };
    for job in arrived
        .into_iter()
        .filter(|j| j.status == JobStatus::Closed)
    {
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(steps) => steps,
            Err(e) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response());
            }
        };
        closed_trains.push((job, steps));
    }
    let cancelled = match state
        .jobs
        .list_jobs(
            &tail(
                "pr-train",
                None,
                Some(serde_json::json!({ "outcome": "cancelled" })),
            ),
            TAIL_WINDOW,
            0,
        )
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    };
    closed_trains.extend(
        cancelled
            .into_iter()
            .filter(|j| j.status == JobStatus::Closed)
            .map(|j| (j, Vec::new())),
    );

    // Cars: live OR closed within reach, in one read (`closed_since`
    // combines with `status` as OR), with steps — the shed is a step,
    // and so is the dock wait's start.
    let car_rows = match state
        .jobs
        .list_jobs(
            &tail("ship-a-change", Some(JobStatus::Open), None),
            MAX_LIMIT,
            0,
        )
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    };
    let mut cars: Vec<(Job, Vec<Step>)> = Vec::with_capacity(car_rows.len());
    for job in car_rows {
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(steps) => steps,
            Err(e) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response());
            }
        };
        cars.push((job, steps));
    }

    // Gate-runs: rows only. `opened_at`, `closed_at` and `outcome` are
    // on the metadata, which is all the duration and the reds need.
    let gate_runs = match state
        .jobs
        .list_jobs(
            &tail("gate-run", Some(JobStatus::Open), None),
            TAIL_WINDOW,
            0,
        )
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    };

    // The receiving yard's inbound packets: the kinds come from the
    // workflow registry (`regions::inbound_kinds`, the receiving page's
    // own rule), real partition only as that page reads them. No
    // registry, or a failed read → `None`: the region says it could not
    // be read rather than counting nothing.
    let inbound = match state.kind_registry.as_ref() {
        Some(reg) => match reg.list_active(None).await {
            Ok(specs) => {
                let filter = JobFilter {
                    kinds: Some(regions::inbound_kinds(&specs)),
                    status: Some(JobStatus::Open),
                    closed_since: Some(reach),
                    partition: Some(boss_core::partition::Partition::Real),
                    scope: scope.clone(),
                    ..Default::default()
                };
                state
                    .jobs
                    .list_jobs(&filter, MAX_LIMIT, 0)
                    .await
                    .ok()
                    .map(|(rows, _)| rows)
            }
            Err(_) => None,
        },
        None => None,
    };

    // The marshalling stations: every station but the dock (a region of
    // its own), bound to the caller as `/api/stations/load` binds them,
    // over the caller's open packets; and each station's counted flow in
    // this window and the previous, from two cube reads. Unread → `None`.
    let stations = marshalling_stations(state, user, scope.clone(), window_hours).await;

    // THE RUNNERS' EVIDENCE (design d2154293, car 5). A runner leaves
    // no heartbeat this process can read — only the ops-requests it
    // answered — so the newest request of each verb the map draws a
    // runner for is fetched here, with its steps (the `disposition`
    // rides the `execute` step). A failed read is `None`, which the
    // machine reports as unknown rather than as an idle runner.
    //
    // AND WHICH HOSTS SHOULD HAVE A RUNNER (backlog 49ed87b4): the
    // estate registry's own rows, so a host that has answered nothing
    // — including one that died — is still a machine on the map. The
    // evidence read above is widened to the newest request of each such
    // host, because ANY verb it answered proves the loop polled.
    let runner_hosts = match state.jobs.list_estate_nodes().await {
        Ok(nodes) => Some(regions::runner_hosts_of(&nodes)),
        Err(_) => None,
    };
    let host_ids: Vec<String> = runner_hosts
        .iter()
        .flatten()
        .map(|h| h.id.clone())
        .collect();
    let ops_requests = newest_ops_requests(state, scope.clone(), reach, &host_ids).await;

    // THE SHOP FLOOR (backlog 94c6ffd0): the runs in flight and the
    // ones that closed within reach, the sessions that dispatched them,
    // and the bound the agents registry declares. Each read answers
    // `None` on failure — the region then says it could not be read,
    // rather than drawing a shop with nobody in it.
    let agent_runs = read_agent_runs(state, scope.clone(), reach).await;
    let sessions = state
        .jobs
        .list_jobs(
            &JobFilter {
                kind: Some(regions::SESSION_KIND.to_string()),
                status: Some(JobStatus::Open),
                scope: scope.clone(),
                ..Default::default()
            },
            TAIL_WINDOW,
            0,
        )
        .await
        .ok()
        .map(|(rows, _)| rows);
    // A registry that could not be read and one that declares no cap
    // are the same drawing — no bound on the card — and neither is a
    // number the map would have to invent.
    let run_capacity = match state.agent_budget.as_ref() {
        Some(door) => door
            .agents
            .list()
            .await
            .ok()
            .and_then(|rows| regions::run_capacity(&rows)),
        None => None,
    };

    Ok(MapRows {
        read,
        closed_trains,
        cars,
        gate_runs,
        inbound,
        stations,
        ops_requests,
        runner_hosts,
        agent_runs,
        sessions,
        run_capacity,
    })
}

/// The shop floor's runs: open ones WITH their steps (a run that
/// finished without reporting is a `building` done over a `reported`
/// still open), plus the ones that closed within reach as rows — the
/// trend reads its two instants off the metadata. `None` on any failed
/// read: an unread floor is troubled, never empty.
async fn read_agent_runs<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    scope: crate::port::JobScope,
    reach: chrono::NaiveDate,
) -> Option<Vec<(Job, Vec<Step>)>> {
    let filter = JobFilter {
        kind: Some(crate::agent_budget::RUN_KIND.to_string()),
        status: Some(JobStatus::Open),
        closed_since: Some(reach),
        scope,
        ..Default::default()
    };
    let (rows, _) = state.jobs.list_jobs(&filter, TAIL_WINDOW, 0).await.ok()?;
    let mut out = Vec::with_capacity(rows.len());
    for job in rows {
        let steps = if job.status == JobStatus::Open {
            state.jobs.list_steps(&job.id).await.ok()?
        } else {
            Vec::new()
        };
        out.push((job, steps));
    }
    Some(out)
}

/// The empty pass: every read answered, nothing in it.
fn empty_read() -> super::yard::YardRead {
    super::yard::YardRead {
        status: yard::build_status(yard::YardInputs::default()),
        dock_reading: yard::Reading::Read,
        health: yard::conductor_health(None, None, None, None, None),
        gate_runs_truncated: false,
        open_trains: Vec::new(),
    }
}

/// The newest ops-request of each verb [`regions::runner_verbs`] names
/// and of each host the estate registry declares a runner on, with its
/// steps. One windowed read, then one `list_steps` per verb and per
/// host — bounded by the runner table and the estate, not by how busy
/// either has been. `None` on ANY failure: an unread runner is not an
/// idle one.
async fn newest_ops_requests<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
    reach: chrono::NaiveDate,
    hosts: &[String],
) -> Option<Vec<(Job, Vec<Step>)>> {
    let filter = JobFilter {
        kind: Some("ops-request".to_string()),
        status: Some(JobStatus::Open),
        closed_since: Some(reach),
        scope,
        ..Default::default()
    };
    let (rows, _) = state.jobs.list_jobs(&filter, TAIL_WINDOW, 0).await.ok()?;
    let opened = |j: &Job| {
        j.metadata
            .get("opened_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let mut out = Vec::new();
    for verb in regions::runner_verbs() {
        let newest = rows
            .iter()
            .filter(|j| j.metadata.get("verb").and_then(serde_json::Value::as_str) == Some(verb))
            .max_by_key(|j| opened(j));
        if let Some(job) = newest {
            let steps = state.jobs.list_steps(&job.id).await.ok()?;
            out.push((job.clone(), steps));
        }
    }
    // The same rows again, newest per HOST — whatever verb it was. A
    // request already collected for its verb is not fetched twice.
    for host in hosts {
        let newest = rows
            .iter()
            .filter(|j| {
                j.metadata.get("host").and_then(serde_json::Value::as_str) == Some(host.as_str())
            })
            .max_by_key(|j| opened(j));
        if let Some(job) = newest {
            if out.iter().any(|(j, _): &(Job, Vec<Step>)| j.id == job.id) {
                continue;
            }
            let steps = state.jobs.list_steps(&job.id).await.ok()?;
            out.push((job.clone(), steps));
        }
    }
    Some(out)
}

/// The station readings the marshalling region is judged on, or `None`
/// when the registry (or the packets under it) could not be read.
async fn marshalling_stations<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    scope: crate::port::JobScope,
    window_hours: i64,
) -> Option<Vec<StationReading>> {
    let reg = state.stations.as_ref()?;
    let specs = effective_stations(state.as_ref(), reg).await.ok()?;
    let filter = JobFilter {
        status: Some(JobStatus::Open),
        scope,
        ..Default::default()
    };
    let (jobs, _) = state.jobs.list_jobs(&filter, MAX_LIMIT, 0).await.ok()?;
    let mut packets = Vec::with_capacity(jobs.len());
    for job in jobs {
        let steps = state.jobs.list_steps(&job.id).await.ok()?;
        packets.push((job, steps));
    }
    // The flow cube over two windows: the previous window's counts are
    // the wider read minus the narrower. Wall clock, as the flow
    // surface reads it — the window has to be the instant the log's
    // `created_at` is written on, not the sim's day.
    let wall = boss_clock_client::wall_now();
    let window = chrono::Duration::hours(window_hours);
    let cells_now = state.jobs.step_flow_cube(wall - window).await.ok()?;
    let cells_both = state
        .jobs
        .step_flow_cube(wall - window - window)
        .await
        .ok()?;
    Some(
        specs
            .iter()
            .filter(|spec| spec.name != "loading-dock")
            .filter_map(|spec| {
                let bound = spec.bind_self(self_id(user))?;
                let members: Vec<String> = packets
                    .iter()
                    .filter(|(job, steps)| bound.predicate.matches(job, steps))
                    .map(|(job, _)| job.id.to_string())
                    .collect();
                let over_limit = bound
                    .wip_limit
                    .is_some_and(|l| members.len() as i64 > i64::from(l));
                let served = crate::station_flow::station_flow(&spec.predicate, &cells_now)
                    .ok()
                    .map(|f| f.served);
                let served_both = crate::station_flow::station_flow(&spec.predicate, &cells_both)
                    .ok()
                    .map(|f| f.served);
                Some(StationReading {
                    name: bound.name.clone(),
                    over_limit,
                    members,
                    served,
                    previous_served: served.zip(served_both).map(|(w, b)| b - w),
                })
            })
            .collect(),
    )
}

/// The map with the clock instant attached — the same `now` field the
/// yard status returns, so a client renders elapsed times against the
/// server's clock rather than its own.
fn with_now(map: regions::Regions, now: chrono::DateTime<chrono::Utc>) -> serde_json::Value {
    let mut v = serde_json::to_value(map).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("now".to_string(), serde_json::json!(now));
    }
    v
}
