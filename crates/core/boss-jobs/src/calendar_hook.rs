//! Hook between Step state transitions and the global calendar.
//!
//! Per `docs/architecture-decisions.md` §Calendar (the
//! jobs↔calendar hook reserves before persistence). Every
//! step whose metadata carries the trio (`scheduled_at` as
//! RFC3339, `duration_hours` as a positive number, `assignee_id`
//! as a string) gets a calendar reservation when it transitions
//! to `active`; the reservation is cancelled when the step
//! transitions to `skipped` (an abandoned branch).
//!
//! Behaviour intentionally narrow:
//!
//! - `pending/ready → active` with all three fields present →
//!   `calendar.reserve(...)`. Conflict surfaces to the caller as
//!   409 in the HTTP layer; this module returns the error so the
//!   handler can decide.
//! - `active → skipped` → `calendar.cancel_by_reason(...)`.
//!   Errors logged but not surfaced — cancellation should never
//!   block a step transition (the cleanup is best-effort).
//! - Calendar client `None` → no-op. Lets boss-jobs-api deploy
//!   independently of the calendar service rollout.
//!
//! Reservation re-scheduling (changing `scheduled_at` while the
//! step is already active) is explicitly out of scope for v1
//! — the caller would have to cancel + re-create the step, or use
//! a future `/api/jobs/.../reschedule` endpoint.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use boss_calendar_client::{CalendarClient, CalendarClientError};
use boss_core::calendar::{ReservationRequest, ReservationStrength, TimeWindow, reason};
use boss_core::job::{Step, StepStatus, Subject};

#[cfg(test)]
#[allow(unused_imports)]
use boss_core::primitives::Subject as _;

/// What the hook decided to do given (old, new) statuses + step
/// metadata. The HTTP handler maps this onto status codes; sim /
/// background paths can use it for tracing.
#[derive(Debug, PartialEq, Eq)]
pub enum HookOutcome {
    /// No reservation activity — either calendar isn't configured,
    /// the transition isn't one we hook on, or the step lacks the
    /// required scheduling fields.
    NoOp,
    /// A reservation was successfully created on the calendar.
    Reserved,
    /// The hook attempted a reservation but the calendar said
    /// there's a conflict. The handler should translate this to
    /// 409 with the existing rows from the error payload.
    Conflict {
        existing_rows: Vec<boss_core::calendar::Reservation>,
    },
    /// One or more reservations were cancelled by reason.
    Cancelled { count: usize },
}

/// Apply the calendar hook for a single step transition.
///
/// `actor` is recorded as `created_by` on a reservation creation
/// or surfaces as the cancellation actor.
pub async fn apply_step_transition(
    calendar: Option<&Arc<dyn CalendarClient>>,
    old: &Step,
    new: &Step,
    actor: &str,
) -> Result<HookOutcome, CalendarClientError> {
    let Some(calendar) = calendar else {
        return Ok(HookOutcome::NoOp);
    };

    let entering_progress = old.status != StepStatus::Active && new.status == StepStatus::Active;
    // A reservation only exists when the step has been Active (made
    // by the entering_progress branch above). Cancel it when an
    // Active step is Skipped (an abandoned branch). Completed
    // deliberately retains the reservation as a historical record of
    // past work (see done_does_not_cancel test below). v2 has no
    // Blocked state, so there's no pause/re-reserve arc — a step
    // waiting on a dependency is simply Pending and never held a
    // reservation in the first place.
    let leaving_active = old.status == StepStatus::Active && new.status == StepStatus::Skipped;

    if entering_progress {
        let Some((scheduled_at, duration_hours, assignee_id)) = scheduling_fields(new) else {
            return Ok(HookOutcome::NoOp);
        };
        let end =
            scheduled_at + chrono::Duration::milliseconds((duration_hours * 3_600_000.0) as i64);
        let window = match TimeWindow::new(scheduled_at, end) {
            Ok(w) => w,
            Err(msg) => {
                return Err(CalendarClientError::Invalid(msg.to_string()));
            }
        };
        let req = ReservationRequest {
            subject: Subject::new("employee", assignee_id),
            window,
            reason_kind: reason::JOB_STEP.to_string(),
            reason_ref_id: new.id.to_string(),
            strength: ReservationStrength::Hard,
            notes: None,
            created_by: actor.to_string(),
        };
        match calendar.reserve(req).await {
            Ok(_) => return Ok(HookOutcome::Reserved),
            Err(CalendarClientError::Conflict { existing }) => {
                return Ok(HookOutcome::Conflict {
                    existing_rows: existing,
                });
            }
            Err(other) => return Err(other),
        }
    }

    if leaving_active {
        // Best-effort cancellation. Never surface errors here —
        // the caller has already decided to waive/block; we don't
        // want a flaky calendar to wedge that.
        match calendar
            .cancel_by_reason(reason::JOB_STEP, &new.id.to_string(), actor)
            .await
        {
            Ok(count) => return Ok(HookOutcome::Cancelled { count }),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    step_id = %new.id,
                    "calendar cancel-by-reason failed; ignoring"
                );
                return Ok(HookOutcome::Cancelled { count: 0 });
            }
        }
    }

    Ok(HookOutcome::NoOp)
}

/// Pull (scheduled_at, duration_hours, assignee_id) out of step
/// metadata. Returns `None` if any are missing or malformed —
/// "incomplete schedule" means "no reservation".
fn scheduling_fields(step: &Step) -> Option<(DateTime<Utc>, f64, String)> {
    let assignee = step.assignee_id.as_ref()?.trim();
    if assignee.is_empty() {
        return None;
    }
    let scheduled = step
        .metadata
        .get("scheduled_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))?;
    let duration = step
        .metadata
        .get("duration_hours")
        .and_then(|v| v.as_f64())
        .filter(|n| *n > 0.0)?;
    Some((scheduled, duration, assignee.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_calendar_client::{FakeCalendarClient, FakeCall};
    use boss_core::job::StepId;
    use serde_json::json;

    fn step_with(
        status: StepStatus,
        assignee: Option<&str>,
        scheduled_at: Option<&str>,
        duration_hours: Option<f64>,
    ) -> Step {
        let mut metadata = serde_json::Map::new();
        if let Some(s) = scheduled_at {
            metadata.insert("scheduled_at".into(), json!(s));
        }
        if let Some(d) = duration_hours {
            metadata.insert("duration_hours".into(), json!(d));
        }
        Step {
            id: StepId::new(),
            job_id: boss_core::job::JobId::new(),
            kind: "service".into(),
            title: "test step".into(),
            spec_slug: None,
            status,
            assignee_id: assignee.map(String::from),
            blocked_by: vec![],
            sort_order: 0,
            sign_offs_required: Vec::new(),
            assurance_required: None,
            sign_offs: Vec::new(),
            fields: Vec::new(),
            metadata: serde_json::Value::Object(metadata),
            completed_on: None,
            completed_by: None,
            completed_at: None,
            notes: None,
            embedded_job: None,
            step_plugin_version: 0,
        }
    }

    #[tokio::test]
    async fn no_calendar_configured_is_a_no_op() {
        let old = step_with(
            StepStatus::Pending,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let new = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let out = apply_step_transition(None, &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
    }

    #[tokio::test]
    async fn pending_to_in_progress_with_full_metadata_reserves() {
        let cal: Arc<dyn CalendarClient> = Arc::new(FakeCalendarClient::new());
        let old = step_with(
            StepStatus::Pending,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let new = {
            let mut s = step_with(
                StepStatus::Active,
                Some("emp-1"),
                Some("2026-04-27T10:00:00Z"),
                Some(2.0),
            );
            // Re-use the SAME id as old for the reservation_ref_id.
            s.id = old.id;
            s
        };
        let out = apply_step_transition(Some(&cal), &old, &new, "svc-mgr")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::Reserved);
    }

    #[tokio::test]
    async fn missing_assignee_skips_reservation() {
        let cal: Arc<dyn CalendarClient> = Arc::new(FakeCalendarClient::new());
        let old = step_with(
            StepStatus::Pending,
            None,
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let new = step_with(
            StepStatus::Active,
            None,
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
    }

    #[tokio::test]
    async fn missing_scheduled_at_skips_reservation() {
        let cal: Arc<dyn CalendarClient> = Arc::new(FakeCalendarClient::new());
        let old = step_with(StepStatus::Pending, Some("emp-1"), None, Some(2.0));
        let new = step_with(StepStatus::Active, Some("emp-1"), None, Some(2.0));
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
    }

    #[tokio::test]
    async fn zero_or_negative_duration_skips_reservation() {
        let cal: Arc<dyn CalendarClient> = Arc::new(FakeCalendarClient::new());
        let old = step_with(
            StepStatus::Pending,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(0.0),
        );
        let new = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(0.0),
        );
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
    }

    #[tokio::test]
    async fn calendar_conflict_is_returned_not_thrown() {
        let fake = Arc::new(FakeCalendarClient::new());
        fake.stage_conflict(vec![]);
        let cal: Arc<dyn CalendarClient> = fake;
        let old = step_with(
            StepStatus::Pending,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let new = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert!(matches!(out, HookOutcome::Conflict { .. }));
    }

    #[tokio::test]
    async fn skipped_cancels_by_reason() {
        let fake = Arc::new(FakeCalendarClient::new());
        fake.set_cancel_by_reason_count(2);
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let old = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let mut new = old.clone();
        new.status = StepStatus::Skipped;
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::Cancelled { count: 2 });

        let calls = fake.calls();
        assert!(matches!(
            calls.last(),
            Some(FakeCall::CancelByReason(kind, _, _)) if kind.as_str() == reason::JOB_STEP
        ));
    }

    #[tokio::test]
    async fn done_does_not_cancel() {
        // A completed step's calendar entry stays as a record of
        // past work — we don't cancel on transition to Done.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let old = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let mut new = old.clone();
        new.status = StepStatus::Completed;
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn already_in_progress_update_is_no_op() {
        // Scheduling-field changes while in-progress aren't
        // re-reservations in v1 — the caller would have to cancel
        // + re-add the step.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let old = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(2.0),
        );
        let new = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T14:00:00Z"),
            Some(3.0),
        );
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
        assert!(fake.calls().is_empty());
    }
}
