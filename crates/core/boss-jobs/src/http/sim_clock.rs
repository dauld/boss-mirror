//! Sim-clock surface. The brewery-sim daemon advances a simulated
//! clock; these handlers expose its state and let an operator pause,
//! resume, restart, or subscribe to it.

use super::*;

/// Derive a SimClockState snapshot from clock-api's current state.
/// `None` in wall mode (no epoch_start — the badge collapses to
/// hidden because there's nothing meaningful to display).
pub(super) async fn sim_clock_state_from_clock(
    clock: &dyn boss_clock_client::ClockClient,
) -> Option<crate::port::SimClockState> {
    let now = clock.now().await;
    if !now.simulated {
        return None;
    }
    Some(crate::port::SimClockState {
        now: now.now,
        current_sim_date: now.now.date_naive(),
        epoch_start_date: now.epoch_start,
        epoch_end_date: now.epoch_end,
        paused: now.paused,
        restart_in_progress: now.restart_in_progress,
    })
}

/// Guard for the sim-control writes (pause / resume / restart-epoch).
/// They mutate the shared sim_clock and — for restart — trim audit_log,
/// so they're restricted to signed-in operators. The gateway mints a
/// read-only-floor session for an anonymous visitor (`visitor`, or
/// `audit-readonly` where the instance opts in — design 2830b6b7), and
/// `guest` is the no-`x-boss-user` default for a direct, ungatewayed
/// call. Neither is an operator, so both are refused with 403 — the same
/// read-only treatment policy gives every other write. The floor is
/// `boss_core::roles::is_read_only_floor`, not a role name: keyed on the
/// name `audit-readonly`, the new `visitor` passed. Denying the floor
/// covers "logged-in only" without an operator allowlist (roles are
/// tenant-extensible Classes). `Some(403)` short-circuits the handler;
/// `None` allows it.
fn operator_guard(user: &CurrentUser) -> Option<Response> {
    let role = user.0.role.as_str();
    if boss_core::roles::is_read_only_floor(role) || role == "guest" {
        return Some(
            (
                StatusCode::FORBIDDEN,
                "sim controls require a signed-in operator",
            )
                .into_response(),
        );
    }
    None
}

// Pause / resume the brewery-sim daemon's sim_clock. The daemon
// reads `sim_clock.paused` on every tick and stops advancing
// `current_sim_date` when true. Used by the operator Debug menu.
//
// Returns the post-update SimClockState so the caller can reflect
// the new state immediately without re-fetching /api/jobs/live.

pub(super) async fn sim_clock_pause<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    user: CurrentUser,
) -> Response {
    if let Some(resp) = operator_guard(&user) {
        return resp;
    }
    set_sim_clock_paused(&state, true).await
}

pub(super) async fn sim_clock_resume<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    user: CurrentUser,
) -> Response {
    if let Some(resp) = operator_guard(&user) {
        return resp;
    }
    set_sim_clock_paused(&state, false).await
}

/// SSE stream of sim_clock changes. The brewery-sim daemon writes
/// `current_sim_date` (and `paused` / `restart_in_progress`) on
/// every tick; this handler polls clock-api server-side every ~3s
/// and pushes a JSON frame when the observed state changes.
/// Clients (SimClockBadge) connect once and stay subscribed for
/// the session lifetime (see docs/design/sse-policy.md).
pub(super) async fn sim_clock_stream<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
) -> impl axum::response::IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use std::convert::Infallible;
    use std::time::Duration;

    let stream = async_stream::stream! {
        // Source of truth is clock-api, not the sim_clock Postgres
        // projection: reading the clock directly keeps the date the
        // SPA shows in lock-step with the timestamp events stamp,
        // with no within-tick lag.
        let initial = sim_clock_state_from_clock(state.clock.as_ref()).await;
        if let Some(snap) = initial.as_ref()
            && let Ok(json) = serde_json::to_string(snap)
        {
            yield Ok::<_, Infallible>(SseEvent::default().data(json));
        }
        let mut last: Option<crate::port::SimClockState> = initial;

        // Server-side poll loop. The dedupe filter only pushes
        // frames when something actually changed — most ticks
        // hold steady for one tick interval (~10s) so ~3s polls
        // mean at most 3-4 pushes per change. ClockClient has
        // an in-process 100ms cache so the 3s polls are cheap.
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(
            tokio::time::MissedTickBehavior::Delay,
        );
        loop {
            tick.tick().await;
            let next = sim_clock_state_from_clock(state.clock.as_ref()).await;
            let changed = match (&last, &next) {
                (None, None) => false,
                (None, Some(_)) | (Some(_), None) => true,
                (Some(a), Some(b)) => {
                    // Re-emit when sim-time changes by at least one
                    // minute. The formula clock advances continuously
                    // (warp=8640 = 144 sim-min per wall-sec), so this
                    // produces 3 frames per ~10-wall-second tick —
                    // enough for the badge's HH:MM to feel live without
                    // saturating the SSE stream.
                    let one_minute = chrono::Duration::minutes(1);
                    (b.now - a.now).abs() >= one_minute
                        || a.paused != b.paused
                        || a.restart_in_progress != b.restart_in_progress
                }
            };
            if changed
                && let Some(snap) = next.as_ref()
                && let Ok(json) = serde_json::to_string(snap)
            {
                yield Ok::<_, Infallible>(
                    SseEvent::default().data(json),
                );
            }
            last = next;
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(super) async fn set_sim_clock_paused<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    paused: bool,
) -> Response {
    if let Err(e) = state.jobs.set_sim_clock_paused(paused).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("set_paused: {e}"),
        )
            .into_response();
    }
    let after = sim_clock_state_from_clock(state.clock.as_ref()).await;
    Json(after).into_response()
}

/// Rewind the sim_clock to `epoch_start_date` and unpause so the
/// daemon loops the next 12 months. Doesn't drop projections —
/// for a clean baseline, run `reset-to-baseline.sh`. Used by the
/// SimClockBadge's "Restart epoch" button when the daemon hits
/// epoch_end and auto-pauses.
pub(super) async fn sim_clock_restart_epoch<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    user: CurrentUser,
) -> Response {
    if let Some(resp) = operator_guard(&user) {
        return resp;
    }
    if let Err(e) = state.jobs.restart_sim_clock_epoch().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("restart_epoch: {e}"),
        )
            .into_response();
    }
    let after = sim_clock_state_from_clock(state.clock.as_ref()).await;
    Json(after).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::{AccessTier, User};

    fn as_role(role: &str) -> CurrentUser {
        CurrentUser(User {
            id: "x".into(),
            role: role.into(),
            access_tier: AccessTier::User,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
    }

    /// Design 2830b6b7: the guard asks the read-only floor predicate,
    /// so the OSS guest's `visitor` is refused exactly as
    /// `audit-readonly` is. Keyed on the name alone it passed — a
    /// stranger could restart the epoch, which trims audit_log.
    #[test]
    fn every_read_only_floor_role_and_the_headerless_guest_are_refused() {
        for role in boss_core::roles::READ_ONLY_FLOOR_ROLES
            .into_iter()
            .chain(["guest"])
        {
            let refused = operator_guard(&as_role(role));
            assert_eq!(
                refused.map(|r| r.status()),
                Some(StatusCode::FORBIDDEN),
                "{role} must not drive the sim clock"
            );
        }
        assert!(operator_guard(&as_role(boss_core::roles::VISITOR_ROLE)).is_some());
    }

    #[test]
    fn a_signed_in_operator_passes() {
        for role in ["platform-admin", "service-tech"] {
            assert!(operator_guard(&as_role(role)).is_none(), "{role}");
        }
    }
}
