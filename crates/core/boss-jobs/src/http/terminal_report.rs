//! `GET /api/workflows/{kind}/terminal-report` — Tier 1 of the
//! experiments program (docs/design/network-experiments.md): measure
//! what version pinning already records.
//!
//! Terminals are the measurement. Per workflow version of one kind —
//! the version each packet was PINNED to at admission — the report
//! states packet counts (total + by status), the outcome distribution
//! over closed packets, and open→close cycle-time stats. It reads the
//! jobs projection and nothing else: no admission changes, no new
//! state, one query. This is the surface that replaces the ad-hoc SQL
//! the week's brewery protocol iterations (tasting-panel v1→v2,
//! keg-return v1→v4, morning-brew v1→v2) were measured with.
//!
//! Tier 2 (packet 6ea5a12a) added the ARM dimension: cohorts group by
//! (version, `experiment_arm` stamp), so a split experiment's control
//! and candidate — and the unstamped bystanders on the same versions —
//! report as separate rows. See `crate::experiments`.

use super::*;

use axum::extract::{Path, Query};

#[derive(Deserialize)]
pub(super) struct TerminalReportQuery {
    /// Keep packets opened on/after this date.
    since: Option<chrono::NaiveDate>,
    /// `true` | `false` | `all` (default). The brewery experiments
    /// are simulated traffic and must be visible, which is why the
    /// default is `all` — and why the response labels which
    /// partition it reports rather than leaving the reader to guess.
    simulated: Option<String>,
}

/// Read-only. The registry is deliberately not consulted: a kind with
/// no packets (or no registry row at all) reports empty versions with
/// a 200 — absence of packets is a fact the report states, not an
/// error. Policy is the same read gate as every sibling GET under
/// /api/workflows (`Action::Read` on `Resource::workflow`), refused
/// with 403 — deliberately not the ungated shape of /api/jobs/summary.
pub(super) async fn workflow_terminal_report<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
    Query(q): Query<TerminalReportQuery>,
) -> Response {
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
        return r;
    }
    let simulated = match q.simulated.as_deref() {
        None | Some("all") => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("simulated must be true, false, or all — got {other:?}"),
            )
                .into_response();
        }
    };
    match state
        .jobs
        .workflow_terminal_report(&kind, q.since, simulated)
        .await
    {
        Ok(versions) => Json(serde_json::json!({
            "kind": kind,
            "since": q.since,
            "simulated": match simulated {
                None => "all",
                Some(true) => "true",
                Some(false) => "false",
            },
            "versions": versions,
        }))
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
