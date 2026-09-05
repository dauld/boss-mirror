//! `GET /api/yard/status` — the yard status read-model.
//!
//! "What is the yard doing, and why?" answered from the system of record
//! in one payload: in-flight trains and exactly where each sits (with the
//! block reason surfaced when one is stuck — the fact that used to live
//! buried in a step's metadata), the loading dock, the boarding predicate
//! rendered from the LIVE cadence rows, recent arrivals, and the cheap
//! stranded-green signal. The aggregation is [`crate::yard::build_status`],
//! pure and unit-tested; this handler is the adapter that reads the rows.
//!
//! WHY SERVER-SIDE. The `/api/cadence/*` and `/api/delivery/policy/*`
//! doors are operator-only (a browser session is neither guest nor
//! operator, so a client read gets 403). This process holds those
//! repositories already, so it reads them directly — no auth barrier, no
//! second round trip — and composes a browser-safe payload behind the
//! ordinary `job` read scope, exactly as `queue_age` and `stations_load`
//! do. The boarding predicate's numbers therefore come from the live
//! registry, never a constant baked into the page.

use super::*;

use crate::yard;

/// How wide the read windows are. Trains: enough to hold the open ones
/// plus the recent closed ones the status shows. Cars / gate-runs: the
/// stranded cross-ref wants the recent gating history and the dock's
/// backing cars.
const TRAIN_WINDOW: i64 = 60;
const CAR_WINDOW: i64 = 400;
const GATE_RUN_WINDOW: i64 = 60;
/// Keep closed trains from the last two weeks in the "recent" window —
/// combined with `status=open` as OR, this is "in flight OR recently
/// arrived/cancelled", the question the surface asks.
const RECENT_TRAIN_DAYS: i64 = 14;

pub(super) async fn yard_status<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    // The same job read gate the other lenses use: an unreadable caller
    // gets an empty yard, not a 403; a scoped one sees exactly their
    // slice. The registry reads (cadence, policy) are describing the
    // pipeline's configuration, not packet content, so they ride the
    // same gate rather than a second one.
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
        return Json(empty_status()).into_response();
    }
    let scope = job_scope_from_predicate(&user, &predicate);
    let now = boss_clock_client::now_from(&state.clock).await;

    // Trains: open OR recently closed, in one query. `opened_on desc`
    // ordering from the adapter puts the newest first, which is the order
    // the recent list wants.
    let train_filter = JobFilter {
        kind: Some("pr-train".to_string()),
        status: Some(JobStatus::Open),
        closed_since: Some((now - chrono::Duration::days(RECENT_TRAIN_DAYS)).date_naive()),
        scope: scope.clone(),
        ..Default::default()
    };
    let trains = match state.jobs.list_jobs(&train_filter, TRAIN_WINDOW, 0).await {
        Ok((rows, _)) => rows,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Partition into open (in-flight) and closed (recent) and attach each
    // train's steps — the phase and the block reason are facts about the
    // steps, so a status without them could only list.
    let mut open_trains: Vec<(boss_core::job::Job, Vec<boss_core::job::Step>)> = Vec::new();
    let mut closed_trains: Vec<(boss_core::job::Job, Vec<boss_core::job::Step>)> = Vec::new();
    for job in trains {
        let steps = state.jobs.list_steps(&job.id).await.unwrap_or_default();
        if job.status == JobStatus::Open {
            open_trains.push((job, steps));
        } else {
            closed_trains.push((job, steps));
        }
    }

    // The cars: the dock's parked cars come from the station queue lens
    // (the registry predicate, not a hand-rolled filter); the car branch
    // set for the stranded cross-ref comes from the same ship-a-change
    // read the dock backs onto.
    let cars = {
        let filter = JobFilter {
            kind: Some("ship-a-change".to_string()),
            scope: scope.clone(),
            ..Default::default()
        };
        state
            .jobs
            .list_jobs(&filter, CAR_WINDOW, 0)
            .await
            .map(|(rows, _)| rows)
            .unwrap_or_default()
    };
    let branch_of = |c: &Job| {
        c.metadata
            .get("branch")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let car_branches: Vec<String> = cars.iter().filter_map(branch_of).collect();
    // Branches whose car reached a terminal. The garage drops these: work
    // that settled is not work awaiting rework, and its last gate-run
    // under that branch name stays red forever. Derived from the read we
    // already did rather than a second query.
    let settled_car_branches: Vec<String> = cars
        .iter()
        .filter(|c| c.status == JobStatus::Closed)
        .filter_map(branch_of)
        .collect();

    // The dock, via the station registry when it serves — the same
    // authoritative path the departure board uses. When no station
    // registry is wired (the in-memory spike), fall back to the parked
    // predicate over the cars we already fetched, so the dock is never
    // silently empty.
    let dock_cars = dock_cars(&state, scope.clone(), &user, &cars).await;

    // The cadence rows and the delivery policy — read straight from the
    // repositories this process holds. A read failure degrades to
    // "unknown cadence / no policy" rather than failing the whole yard:
    // the trains and the dock are the thing the operator came for, and a
    // cadence blip must not black out the board.
    let rules = match state.cadence.as_ref() {
        Some(repo) => repo.active_rules().await.unwrap_or_default(),
        None => Vec::new(),
    };
    // The conductor's liveness, from the record IT writes. The reconcile
    // rule is the heartbeat: it is the pass that keeps every train's
    // truth current, so its last firing is what "have we heard from the
    // conductor" means. Read straight from the repository this process
    // holds, like the rules and the policy above — and degraded to
    // "unknown" on any failure, never to "fine".
    const HEARTBEAT_RULE: &str = "train-reconcile";
    let last_firing = match state.cadence.as_ref() {
        Some(repo) => repo.last_firing(HEARTBEAT_RULE).await.ok().flatten(),
        None => None,
    };
    let heartbeat_minutes = rules
        .iter()
        .find(|r| r.name == HEARTBEAT_RULE)
        .and_then(|r| r.every_minutes)
        .map(i64::from);
    let policy = match state.delivery.as_ref() {
        Some(repo) => repo.active_policy("train-conductor").await.ok().flatten(),
        None => None,
    };

    // The stranded cross-ref: recent gate-runs (open and closed), each
    // with its steps so the green verdict can be read.
    let gate_runs = {
        let filter = JobFilter {
            kind: Some("gate-run".to_string()),
            scope,
            ..Default::default()
        };
        match state.jobs.list_jobs(&filter, GATE_RUN_WINDOW, 0).await {
            Ok((rows, _)) => {
                let mut out = Vec::with_capacity(rows.len());
                for job in rows {
                    let steps = state.jobs.list_steps(&job.id).await.unwrap_or_default();
                    out.push((job, steps));
                }
                out
            }
            Err(_) => Vec::new(),
        }
    };

    let status = yard::build_status(
        &open_trains,
        &closed_trains,
        &dock_cars,
        &rules,
        policy.as_ref(),
        &gate_runs,
        &car_branches,
        &settled_car_branches,
        Some(now),
    );
    let health = yard::conductor_health(
        last_firing.as_ref().map(|f| f.fired_at),
        Some(HEARTBEAT_RULE),
        last_firing.as_ref().and_then(|f| f.rc),
        heartbeat_minutes,
        Some(now),
    );
    Json(with_conductor(with_now(status, now), health)).into_response()
}

/// The dock's parked cars: the loading-dock station queue when a station
/// registry is wired, else the parked predicate over the fetched cars.
async fn dock_cars<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
    user: &boss_policy_client::User,
    fallback_cars: &[boss_core::job::Job],
) -> Vec<boss_core::job::Job> {
    if let Some(reg) = state.stations.as_ref()
        && let Ok(row) = reg.get_active("loading-dock").await
        && let Some(spec) = row.bind_self(self_id(user))
    {
        let filter = JobFilter {
            kind: spec.predicate.kind.clone(),
            status: Some(JobStatus::Open),
            scope,
            ..Default::default()
        };
        if let Ok((jobs, _)) = state.jobs.list_jobs(&filter, MAX_LIMIT, 0).await {
            let mut members = Vec::new();
            for job in jobs {
                let steps = state.jobs.list_steps(&job.id).await.unwrap_or_default();
                if spec.predicate.matches(&job, &steps) {
                    members.push(job);
                }
            }
            return members;
        }
    }
    // Fallback: the loading-dock predicate hand-rolled — an open
    // ship-a-change with a branch, not yet on a train, at review
    // ready/active. Kept only for the station-less spike path.
    fallback_cars
        .iter()
        .filter(|j| {
            j.status == JobStatus::Open
                && j.metadata.get("branch").is_some()
                && j.metadata.get("train").is_none()
        })
        .cloned()
        .collect()
}

/// The status with the clock instant attached — the same `now` field
/// `queue-age` returns, so a client renders elapsed times against the
/// server's clock rather than its own.
/// The conductor's liveness, attached to the payload. Injected here
/// rather than threaded through `build_status` so it composes with the
/// read-model instead of widening it — the same shape `with_now` uses.
fn with_conductor(mut v: serde_json::Value, health: yard::ConductorHealth) -> serde_json::Value {
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "conductor".to_string(),
            serde_json::to_value(health).unwrap_or_else(|_| serde_json::json!({})),
        );
    }
    v
}

fn with_now(status: yard::YardStatus, now: chrono::DateTime<chrono::Utc>) -> serde_json::Value {
    let mut v = serde_json::to_value(status).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("now".to_string(), serde_json::json!(now));
    }
    v
}

/// The empty yard a denied caller gets — well-formed, so the page renders
/// "nothing to show" rather than an error or a false-empty.
fn empty_status() -> serde_json::Value {
    let status = yard::build_status(&[], &[], &[], &[], None, &[], &[], &[], None);
    serde_json::to_value(status).unwrap_or_else(|_| serde_json::json!({}))
}
