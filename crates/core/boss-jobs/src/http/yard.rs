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

/// How wide the read windows are. Trains: `yard::TRAIN_WINDOW`, one per
/// read. Gate-runs: `yard::GATE_RUN_WINDOW` for the recency read, and
/// the held read below. Cars: the dock's backing cars.
const CAR_WINDOW: i64 = 400;
use yard::{GATE_RUN_WINDOW, HELD_RUN_PAGE, HELD_RUN_PAGES};
/// Keep closed trains from the last two weeks in the "recent" window —
/// "recently arrived/cancelled", the tail the surface shows beside the
/// in-flight trains.
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

    // Trains: two reads, because one cannot hold both. The in-flight
    // trains are read whole — `status=open`, a handful at most — and the
    // recent tail through the retention window. They used to be ONE
    // "open OR closed since" read, cut at TRAIN_WINDOW and ordered by
    // `opened_on`, a DATE: on 2026-09-07 the yard opened 82 trains in a
    // day, the window sliced that same-day tie wherever Postgres chose,
    // the one open train fell out, and the board drew `trains: []` while
    // `/api/jobs?kind=pr-train&status=open` showed it. A day of arrivals
    // is the tail's problem now, never the open list's (pinned by
    // `a_day_of_arrivals_does_not_push_the_open_train_off_the_board`).
    let pr_train = |status, closed_since| JobFilter {
        kind: Some("pr-train".to_string()),
        status,
        closed_since,
        scope: scope.clone(),
        ..Default::default()
    };
    let in_flight = pr_train(Some(JobStatus::Open), None);
    let recent = pr_train(
        None,
        Some((now - chrono::Duration::days(RECENT_TRAIN_DAYS)).date_naive()),
    );
    let open_rows = match state
        .jobs
        .list_jobs(&in_flight, yard::TRAIN_WINDOW, 0)
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let recent_rows = match state.jobs.list_jobs(&recent, yard::TRAIN_WINDOW, 0).await {
        Ok((rows, _)) => rows,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Attach each train's steps — the phase and the block reason are facts
    // about the steps, so a status without them could only list. The
    // windowed read keeps live trains too (that is the window's contract);
    // the open read owns those, so only its terminal rows become the
    // recent list — `opened_on desc` from the adapter is already the
    // order that list wants.
    //
    // A failed steps read FAILS the request, like the two train reads
    // above it. `unwrap_or_default()` made it a train with no steps
    // instead: no phase, no block reason, and — because `holds_the_track`
    // is a fact about the steps — not holding the track, so a read
    // failure reported the track CLEAR (31783deb). The steps are not a
    // garnish on a train row; they are what the row means.
    let mut open_trains: Vec<(boss_core::job::Job, Vec<boss_core::job::Step>)> = Vec::new();
    for job in open_rows {
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(steps) => steps,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        open_trains.push((job, steps));
    }
    let mut closed_trains: Vec<(boss_core::job::Job, Vec<boss_core::job::Step>)> = Vec::new();
    for job in recent_rows
        .into_iter()
        .filter(|j| j.status != JobStatus::Open)
    {
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(steps) => steps,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        closed_trains.push((job, steps));
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

    // The dock, via the station registry — the one authoritative path,
    // the same one the departure board uses. `None` when the row cannot
    // be read: the dock is then UNREAD, which the payload says rather
    // than drawing an empty lane (see [`dock_cars`]).
    let dock_read = dock_cars(&state, scope.clone(), &user).await;
    // Derived ONCE, because two consumers answer with it: the boarding
    // block's depth, verdict and sentences, and `dock_source` beside the
    // dock lane. They were two reads of this same `Option` — honest, but
    // free to drift, and the two halves landed hours apart precisely
    // because each lived in the file the other change held (backlog
    // 2cb91534). One value cannot disagree with itself (CLAUDE.md §9a).
    let dock_reading = yard::Reading::of(&dock_read);

    // The cadence rows and the delivery policy — read straight from the
    // repositories this process holds. A read failure degrades to
    // "unknown cadence / no policy" rather than failing the whole yard:
    // the trains and the dock are the thing the operator came for, and a
    // cadence blip must not black out the board.
    //
    // `None` is "could not be read" — the read failed, or no cadence
    // repository is wired at all, exactly the two cases the dock's
    // `None` already covers. The rows then degrade to an empty list for
    // everything that walks them, and the READING rides beside them so
    // the payload says "unknown cadence" rather than the "No boarding
    // cadence is configured" it used to say here (31783deb). The
    // reading is derived from this same `Option`, never written by hand
    // beside it (CLAUDE.md §9a).
    let rules_read = match state.cadence.as_ref() {
        Some(repo) => repo.active_rules().await.ok(),
        None => None,
    };
    let cadence_reading = yard::Reading::of(&rules_read);
    let rules = rules_read.unwrap_or_default();
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
    let heartbeat_rule = rules.iter().find(|r| r.name == HEARTBEAT_RULE);
    let heartbeat_minutes = heartbeat_rule.and_then(|r| r.every_minutes).map(i64::from);
    // The board rules' last firings — the depth rule's, which the
    // cooldown hold is read from, and the clock rule's, so the last board
    // is the newest of ANY rule that departs a train. Read the way the
    // conductor reads them, one `last_firing(&rule.name)` per row; each
    // rule is found by SHAPE (`min_dock_depth`, `at_times` + a departing
    // verb), the same way the predicate finds its threshold and its
    // windows, so a renamed row moves both together. Until 43fb424f only
    // the depth rule was read, and a clock-window board never appeared
    // in `last_board_at` — two trains 16 min apart under a stated 45.
    //
    // Three outcomes, not two: a firing, no firing (the rule has never
    // boarded, or there is no such rule — an ANSWER), and a read that did
    // not answer. `.ok()` collapsed the third into the second, and the
    // three nulls that follow from it — `last_board_at`,
    // `cooldown_remaining_minutes`, `held_because` — read together as "no
    // cooldown in force, boards on the next tick", which is the
    // permissive answer (31783deb). One reading covers both rows: they
    // are one table on one connection, and a cooldown stated beside an
    // unknown last board would be half an answer wearing a whole one's
    // shape.
    let (depth_firing, clock_firing, last_board_reading) = match state.cadence.as_ref() {
        Some(repo) if cadence_reading == yard::Reading::Read => {
            let (depth, clock) = tokio::join!(
                firing_of(repo.as_ref(), yard::depth_rule(&rules)),
                firing_of(repo.as_ref(), yard::clock_rule(&rules)),
            );
            match (depth, clock) {
                (Ok(depth), Ok(clock)) => (depth, clock, yard::Reading::Read),
                _ => (None, None, yard::Reading::Unread),
            }
        }
        // With the rows unread there is nothing to read a firing under,
        // and the cadence reading is exactly the fact that says so.
        _ => (None, None, cadence_reading),
    };
    let policy = match state.delivery.as_ref() {
        Some(repo) => repo.active_policy("train-conductor").await.ok().flatten(),
        None => None,
    };

    // The stranded cross-ref: recent gate-runs (open and closed), each
    // with its steps so the green verdict can be read.
    //
    // TWO READS, because one cannot hold both. The recency read is the
    // newest GATE_RUN_WINDOW runs — the slots, the garage, limbo and the
    // stranded lane are about what gated lately. A HELD green is not: it
    // is the one state that exists to be SEEN until a person releases
    // it, and it stays exactly as long as the hold does. On 2026-09-15
    // the dev-pod car (gate-run 3164b0d5, green on `--hold` since 17:56Z
    // the day before) had 80 gate-runs open behind it, fell out of the
    // window, and the lane said none held (backlog 2fa96d34). A limit is
    // not a filter: the held read narrows on the hold itself
    // (`metadata_has = "hold"`, the `metadata ? $n` shape) and is merged
    // in by id, so a held green is named whatever gated after it. The
    // `total` of the recency read rides the payload as
    // `gate_runs_truncated`, so the page can say the recency lanes were
    // cut rather than let an empty stranded lane read as a fact.
    let gate_run = |metadata_has: Option<&str>| JobFilter {
        kind: Some("gate-run".to_string()),
        metadata_has: metadata_has.map(str::to_string),
        scope: scope.clone(),
        ..Default::default()
    };
    let (recent_runs, gate_runs_total) = state
        .jobs
        .list_jobs(&gate_run(None), GATE_RUN_WINDOW, 0)
        .await
        .unwrap_or_default();
    let gate_runs_truncated = gate_runs_total > GATE_RUN_WINDOW;
    let held_runs = {
        let filter = gate_run(Some("hold"));
        let mut out = Vec::new();
        for page in 0..HELD_RUN_PAGES {
            let Ok((rows, total)) = state
                .jobs
                .list_jobs(&filter, HELD_RUN_PAGE, page * HELD_RUN_PAGE)
                .await
            else {
                break;
            };
            let read = rows.len() as i64;
            out.extend(rows);
            if read < HELD_RUN_PAGE || (page + 1) * HELD_RUN_PAGE >= total {
                break;
            }
            if page + 1 == HELD_RUN_PAGES {
                tracing::warn!(
                    total,
                    read = out.len(),
                    "yard: held gate-run read hit its page cap — older holds unread"
                );
            }
        }
        out
    };
    let gate_runs = {
        let mut out: Vec<(boss_core::job::Job, Vec<boss_core::job::Step>)> =
            Vec::with_capacity(recent_runs.len() + held_runs.len());
        // The recency page first, so `held_greens`' first-seen-wins
        // de-dup keeps the order the page already had; a held run the
        // page holds already is not read twice. A TRAIN's own gate-run
        // carries a hold too (128b5496) and is excluded from every lane
        // this set feeds, so its steps are not fetched: measured
        // 2026-09-15, 43 of the 44 held runs on record were train gates.
        for job in recent_runs.into_iter().chain(
            held_runs
                .into_iter()
                .filter(|j| !crate::stranded::is_train_gate(&j.metadata)),
        ) {
            if out.iter().any(|(seen, _)| seen.id == job.id) {
                continue;
            }
            let steps = state.jobs.list_steps(&job.id).await.unwrap_or_default();
            out.push((job, steps));
        }
        out
    };

    // The arrived population the in-flight ETA is measured against — a
    // read of its OWN, narrowed in SQL on `metadata.outcome`.
    //
    // WHY IT CANNOT REUSE `closed_trains`. A board the consist check
    // refuses still opens a pr-train Job and cancels it, so the recent
    // tail is almost entirely zero-length cancellations: measured
    // 2026-09-10, 696 of 1,014 pr-trains are `cancelled`, and of the 40
    // most recent exactly ONE had arrived. Reaching 10 measurable
    // arrivals from the newest end takes a window 583 trains deep, so no
    // recency window a page can afford holds enough of them — the filter
    // has to be in the query, which is the same lesson `closed_since`
    // carries in its own doc comment on `JobFilter`. No steps are
    // fetched: every timing the estimate needs is in the Job's metadata,
    // so this costs one list read and no N+1.
    let arrived_trains = {
        let filter = JobFilter {
            kind: Some("pr-train".to_string()),
            metadata_contains: Some(serde_json::json!({ "outcome": "arrived" })),
            scope: scope.clone(),
            ..Default::default()
        };
        state
            .jobs
            .list_jobs(&filter, yard::TRAIN_WINDOW, 0)
            .await
            .map(|(rows, _)| rows)
            .unwrap_or_default()
    };

    // Every read that can fail QUIETLY states whether it answered: the
    // dock from the `Option` `dock_cars` already returns, the cadence
    // rows from theirs, the firing from its own `Result`. The trains and
    // the gate-runs are not here because a train read failing returns
    // 500 rather than an empty board (above) — the posture this seam
    // exists to extend to the rest.
    let status = yard::build_status_for(
        yard::YardInputs {
            open_trains: &open_trains,
            closed_trains: &closed_trains,
            dock_cars: dock_read.as_deref().unwrap_or(&[]),
            rules: &rules,
            board_firings: yard::BoardFirings {
                depth: depth_firing.as_ref(),
                clock: clock_firing.as_ref(),
            },
            policy: policy.as_ref(),
            gate_runs: &gate_runs,
            car_branches: &car_branches,
            settled_car_branches: &settled_car_branches,
            arrived_trains: &arrived_trains,
            now: Some(now),
        },
        dock_reading,
        yard::BoardingReadings {
            cadence: cadence_reading,
            last_board: last_board_reading,
        },
    );
    // The VERB the heartbeat rule runs (`reconcile`), read from its row.
    // This used to pass the rule's NAME, so `last_verb` said
    // `train-reconcile` — a label that was not the fact it named.
    let health = yard::conductor_health(
        last_firing.as_ref().map(|f| f.fired_at),
        heartbeat_rule.map(|r| r.verb.as_str()),
        last_firing.as_ref().and_then(|f| f.rc),
        heartbeat_minutes,
        Some(now),
    );
    Json(with_gate_run_window(
        with_dock_source(with_conductor(with_now(status, now), health), dock_reading),
        gate_runs_truncated,
    ))
    .into_response()
}

/// Whether the recency lanes were cut, stated on the payload beside the
/// window they read: `gate_runs_truncated` is "the record holds more
/// gate-runs than `gate_run_window`", so the page can say "the slots,
/// garage, limbo and stranded lanes read the newest N runs; held greens
/// are read from the record" instead of letting an empty lane read as a
/// fact about everything. Injected the way [`with_dock_source`] is, so
/// the marker composes without widening [`yard::build_status`].
fn with_gate_run_window(mut v: serde_json::Value, truncated: bool) -> serde_json::Value {
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "gate_runs_truncated".to_string(),
            serde_json::json!(truncated),
        );
        obj.insert(
            "gate_run_window".to_string(),
            serde_json::json!(GATE_RUN_WINDOW),
        );
    }
    v
}

/// One board rule's newest firing, by the read the conductor makes
/// (`last_firing(&rule.name)`). `Ok(None)` both for no such rule and for
/// a rule that has never fired — each an ANSWER; `Err` only when the read
/// did not answer, which the caller turns into [`yard::Reading::Unread`].
async fn firing_of(
    repo: &dyn crate::cadence::CadenceRepository,
    rule: Option<&crate::cadence::CadenceRuleRow>,
) -> Result<Option<crate::cadence::LastFiring>, crate::cadence::CadenceError> {
    match rule {
        Some(rule) => repo.last_firing(&rule.name).await,
        None => Ok(None),
    }
}

/// The cars standing ON the dock, with their steps, read from the
/// `loading-dock` station row — the ONE place the dock's membership rule
/// lives. The steps ride along because the HOLD — the fact that decides
/// whether a car can board — is written on the review step, so
/// [`yard::dock_lanes`] cannot split the dock without them; the station
/// read fetches them already, to evaluate the predicate.
///
/// Membership asks [`yard::on_the_dock`], which is the row's predicate
/// with the HOLD disregarded — a held car is on the dock and must be
/// listed there, even once the row stops admitting it for boarding.
///
/// `None` when the row cannot be read — no station registry wired (the
/// in-memory spike path), or the read failed. This used to hand-roll the
/// predicate instead and return the cars it matched, which made it the
/// THIRD copy of a rule the row defines (backlog 52fed017, after the
/// client's in c0708d66) and, worse, answered where it should have
/// erred: the hand-rolled rule carried no review-step term, so an
/// unreadable dock came back populated under a LAXER rule — or empty,
/// and "empty" is a claim an operator reads as a fact.
async fn dock_cars<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
    user: &boss_policy_client::User,
) -> Option<Vec<(boss_core::job::Job, Vec<boss_core::job::Step>)>> {
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
                // A failed steps read makes the dock UNREAD, which is
                // what this `Option` is for. Defaulted to no steps it
                // made a HELD car look unheld — boardable — which is the
                // permissive answer and the one lane this read exists to
                // get right.
                let steps = state.jobs.list_steps(&job.id).await.ok()?;
                if yard::on_the_dock(&spec.predicate, &job, &steps) {
                    members.push((job, steps));
                }
            }
            return Some(members);
        }
    }
    None
}

/// Where the dock's rows came from, stated on the payload: the station
/// row served (`station`), or it could not be read (`unavailable`).
/// Injected beside the read-model the way [`with_conductor`] is, so the
/// marker composes without widening [`yard::build_status`].
///
/// The same two cases the web lens settled on when its own copy of the
/// predicate was deleted (c0708d66). There is no third case: a dock read
/// from the row is a reading, and anything else is an admission — never
/// an empty lane that reads as a fact.
///
/// Takes the [`yard::Reading`] the boarding block is built from, not a
/// `bool` of its own: this marker and that block are two renderings of
/// one fact, and a `bool` here let the caller derive it twice (backlog
/// 2cb91534). Matching exhaustively also makes a third `Reading` variant
/// a compile error here rather than a silent `unavailable`.
fn with_dock_source(mut v: serde_json::Value, reading: yard::Reading) -> serde_json::Value {
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "dock_source".to_string(),
            serde_json::json!(match reading {
                yard::Reading::Read => "station",
                yard::Reading::Unread => "unavailable",
            }),
        );
    }
    v
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
    let status = yard::build_status(yard::YardInputs::default());
    serde_json::to_value(status).unwrap_or_else(|_| serde_json::json!({}))
}
