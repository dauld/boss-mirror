//! `GET /api/yard/regions?window=24h` — the IT system map's KPI read
//! (design 0524fc95, decided 2026-09-19; car 1).
//!
//! Eight regions — dock, gates, track, shed, arrivals, garage,
//! receiving, marshalling — each with a count, a clear/busy/troubled
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
//! packets, and the marshalling stations' load and flow. Every read
//! that cannot answer states so (`None` → a troubled region with
//! `count: null`), never an empty region that reads as clear.

use super::*;

use crate::regions::{self, RegionInputs, StationReading};
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
    // The same job read gate the yard status keeps: an unreadable caller
    // gets an empty map, not a 403.
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
    let now = boss_clock_client::now_from(&state.clock).await;
    if matches!(predicate, boss_policy_client::Predicate::None) {
        let status = yard::build_status(yard::YardInputs::default());
        let empty = regions::regions(&RegionInputs {
            status: &status,
            dock_reading: yard::Reading::Read,
            open_trains: &[],
            closed_trains: &[],
            cars: &[],
            gate_runs: &[],
            inbound: Some(&[]),
            stations: Some(&[]),
            now,
            window_hours,
        });
        return Json(with_now(empty, now)).into_response();
    }
    let scope = job_scope_from_predicate(&user, &predicate);

    let read = match read_yard(&state, &user, scope.clone(), now).await {
        Ok(read) => read,
        Err(resp) => return resp,
    };

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
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    for job in arrived
        .into_iter()
        .filter(|j| j.status == JobStatus::Closed)
    {
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(steps) => steps,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut cars: Vec<(Job, Vec<Step>)> = Vec::with_capacity(car_rows.len());
    for job in car_rows {
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(steps) => steps,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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
    let stations = marshalling_stations(&state, &user, scope, window_hours).await;

    let map = regions::regions(&RegionInputs {
        status: &read.status,
        dock_reading: read.dock_reading,
        open_trains: &read.open_trains,
        closed_trains: &closed_trains,
        cars: &cars,
        gate_runs: &gate_runs,
        inbound: inbound.as_deref(),
        stations: stations.as_deref(),
        now,
        window_hours,
    });
    Json(with_now(map, now)).into_response()
}

/// The station readings the marshalling region is judged on, or `None`
/// when the registry (or the packets under it) could not be read.
async fn marshalling_stations<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    scope: crate::port::JobScope,
    window_hours: i64,
) -> Option<Vec<StationReading>> {
    let reg = state.stations.as_ref()?;
    let specs = effective_stations(state, reg).await.ok()?;
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
