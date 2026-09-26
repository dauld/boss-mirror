//! Hook between Step state transitions and the global calendar.
//!
//! Per `docs/architecture-decisions.md` §Calendar (the
//! jobs↔calendar hook reserves before persistence). Every
//! step whose metadata carries the trio (`scheduled_at` as
//! RFC3339, `duration_minutes` as a positive number, `assignee_id`
//! as a string) gets a calendar reservation when it transitions
//! to `active`; the reservation is cancelled when the step
//! transitions to `skipped` (an abandoned branch).
//!
//! Behaviour intentionally narrow:
//!
//! - `pending/ready → active` with all three fields present →
//!   `calendar.reserve(...)`, BEFORE the step write
//!   ([`apply_step_transition`]). Conflict surfaces to the caller as
//!   409 in the HTTP layer; this module returns the error so the
//!   handler can decide.
//! - `active → skipped` → `calendar.cancel_by_reason(...)`, AFTER the
//!   step write landed ([`after_step_written`]). Errors logged but not
//!   surfaced — cancellation should never block a step transition (the
//!   cleanup is best-effort).
//!
//! Every undo here undoes only what its OWN attempt did (the review of
//! car 983696b5, backlog 558396ff, 2026-09-25). A refused write hands
//! back the one reservation it made, by that reservation's id — never
//! every hold keyed on the step, which includes the hold of a start
//! that raced it and landed. A release that runs before its write runs
//! for a write that may never land, so the skip's release runs after.
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
use boss_core::calendar::{
    Reservation, ReservationId, ReservationRequest, ReservationStrength, TimeWindow, reason,
};
use boss_core::job::{Step, StepStatus, Subject};

/// What the hook decided to do given (old, new) statuses + step
/// metadata. The HTTP handler maps this onto status codes; sim /
/// background paths can use it for tracing.
#[derive(Debug, PartialEq, Eq)]
pub enum HookOutcome {
    /// No reservation activity — either calendar isn't configured,
    /// the transition isn't one we hook on, or the step lacks the
    /// required scheduling fields.
    NoOp,
    /// A reservation was successfully created on the calendar — this
    /// one, which is the only hold a refused write may hand back.
    Reserved(ReservationId),
    /// The step's own hold on exactly this subject and window already
    /// stands: left by a start that landed, or by an attempt that died
    /// between its reservation and its write — or placed by a start
    /// racing this one, whose write may yet be refused and hand it back.
    /// Nothing was created, so nothing is owned and nothing is handed
    /// back (558396ff); a write that LANDS on it re-asserts it
    /// ([`hold_for_landed_start`]), because the hold it counted on is
    /// not its own to keep.
    AlreadyHeld,
    /// The hook attempted a reservation but the calendar said
    /// there's a conflict. The handler should translate this to
    /// 409 with the existing rows from the error payload.
    Conflict {
        existing_rows: Vec<boss_core::calendar::Reservation>,
    },
    /// One or more reservations were cancelled by reason.
    Cancelled { count: usize },
    /// A step that holds its time as stored — Active, or Completed over
    /// the time a refused racer reserved — asked for that time again and
    /// found another reservation holding it (backlog 4bdb8150). Unlike
    /// [`HookOutcome::Conflict`], nothing is refused: the step already
    /// moved. The hold is simply gone, and this is the record of it.
    HoldLost(LostHold),
}

/// A reservation a step owed and no longer has (backlog 4bdb8150, the
/// round-3 review of car 983696b5): the step, the time it holds as
/// stored, whose time it is, and the rows the calendar says hold that
/// time now.
///
/// The re-assertion that finds it runs after a step write that already
/// landed or was already refused, so there is no request left to refuse
/// and no caller waiting on it. It was a `tracing::warn` — a lost
/// reservation recorded only in a log line, which is a check nobody
/// reads. The handler records it as a `jobs.step.hold_lost` event
/// ([`crate::events::step_hold_lost_payload`]), and a dispatcher rule
/// (`infra/dispatcher/rules/`) opens the packet a person reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostHold {
    pub job_id: boss_core::job::JobId,
    pub step_id: boss_core::job::StepId,
    /// The assignee whose time the step holds.
    pub subject: Subject,
    pub window: TimeWindow,
    /// The reservations holding that time now, as the calendar
    /// answered the re-assertion.
    pub held_by: Vec<Reservation>,
}

/// Apply the calendar hook for a single step transition, BEFORE the
/// step write: the reservation, so a hard conflict refuses the write
/// with nothing stored. The release on a skip is not here — it runs
/// once the write has landed ([`after_step_written`]).
///
/// `actor` is recorded as `created_by` on a reservation creation.
pub async fn apply_step_transition(
    calendar: Option<&Arc<dyn CalendarClient>>,
    old: &Step,
    new: &Step,
    actor: &str,
) -> Result<HookOutcome, CalendarClientError> {
    let Some(calendar) = calendar else {
        return Ok(HookOutcome::NoOp);
    };

    // From an OPEN status only. A terminal step never enters progress —
    // the handler refuses the write that would move it — and this hook
    // runs before that refusal, so reserving for it left a hold behind
    // a write that never happened (backlog 558396ff).
    let entering_progress = matches!(old.status, StepStatus::Pending | StepStatus::Ready)
        && new.status == StepStatus::Active;
    if !entering_progress {
        return Ok(HookOutcome::NoOp);
    }
    match start_request(new, actor)? {
        Some(req) => reserve_as_the_step(calendar, req).await,
        None => Ok(HookOutcome::NoOp),
    }
}

/// The hold a start of `step` asks for: its assignee, over its window,
/// keyed on the step. `None` when the step lacks a complete schedule.
fn start_request(
    step: &Step,
    actor: &str,
) -> Result<Option<ReservationRequest>, CalendarClientError> {
    let Some((scheduled_at, duration_minutes, assignee_id)) = scheduling_fields(step) else {
        return Ok(None);
    };
    let end = scheduled_at + chrono::Duration::milliseconds((duration_minutes * 60_000.0) as i64);
    let window = TimeWindow::new(scheduled_at, end)
        .map_err(|msg| CalendarClientError::Invalid(msg.to_string()))?;
    Ok(Some(ReservationRequest {
        subject: Subject::new("employee", assignee_id),
        window,
        reason_kind: reason::JOB_STEP.to_string(),
        reason_ref_id: step.id.to_string(),
        strength: ReservationStrength::Hard,
        notes: None,
        created_by: actor.to_string(),
    }))
}

/// Reserve `req` for its step, taking the step's own hold on exactly
/// that time as held rather than as a conflict.
async fn reserve_as_the_step(
    calendar: &Arc<dyn CalendarClient>,
    req: ReservationRequest,
) -> Result<HookOutcome, CalendarClientError> {
    match calendar.reserve(req.clone()).await {
        Ok(id) => Ok(HookOutcome::Reserved(id)),
        // THE STEP'S OWN HOLD ON THIS TIME IS NOT A CONFLICT (backlog
        // 558396ff). A start refused as stale (car 88123ae0 made that an
        // ordinary answer — "send it again") or cut off between its
        // reservation and its write can leave this exact hold behind,
        // and a re-send refused against it could never start the step.
        // It is taken as the step's hold, and NOTHING is released: a
        // start that raced this one and landed left the same hold, and
        // the review of car 983696b5 found the earlier fix — release
        // every hold keyed on the step, then reserve — erasing it, so a
        // refused racer left the landed start's Active step holding
        // nothing. A hold on other time, or anyone else's, still refuses.
        Err(CalendarClientError::Conflict { existing })
            if !existing.is_empty() && existing.iter().all(|r| is_this_hold(r, &req)) =>
        {
            Ok(HookOutcome::AlreadyHeld)
        }
        Err(CalendarClientError::Conflict { existing }) => Ok(HookOutcome::Conflict {
            existing_rows: existing,
        }),
        Err(other) => Err(other),
    }
}

/// Whether `held` is the very hold `req` asks for: the same step, the
/// same person, the same window.
fn is_this_hold(held: &Reservation, req: &ReservationRequest) -> bool {
    held.reason_kind == req.reason_kind
        && held.reason_ref_id == req.reason_ref_id
        && held.subject == req.subject
        && held.window == req.window
}

/// The calendar half of a step write that LANDED: `stored` is the row
/// as that write stored it. The handler's write is judged on the row
/// version its read saw (backlog 6ec22d71), so a write that answered Ok
/// stored exactly the row it computed.
///
/// A reservation only exists when the step has been Active. Cancel it
/// when an Active step is Skipped (an abandoned branch). Completed
/// deliberately retains the reservation as a historical record of past
/// work (see `done_does_not_cancel` below). v2 has no Blocked state, so
/// there's no pause/re-reserve arc — a step waiting on a dependency is
/// simply Pending and never held a reservation in the first place.
///
/// AFTER THE WRITE (backlog 558396ff). This ran before the write, so a
/// skip refused as stale released the hold of a step that stayed
/// Active, and a skip that a completion overtook released the hold that
/// is the completed step's record.
///
/// Best-effort: never surfaces an error — the step has already moved,
/// and a flaky calendar must not wedge that.
pub async fn after_step_written(
    calendar: Option<&Arc<dyn CalendarClient>>,
    old: &Step,
    stored: &Step,
    actor: &str,
) -> HookOutcome {
    let Some(calendar) = calendar else {
        return HookOutcome::NoOp;
    };
    if !(old.status == StepStatus::Active && stored.status == StepStatus::Skipped) {
        return HookOutcome::NoOp;
    }
    match calendar
        .cancel_by_reason(reason::JOB_STEP, &stored.id.to_string(), actor)
        .await
    {
        Ok(count) => HookOutcome::Cancelled { count },
        Err(e) => {
            tracing::warn!(
                error = %e,
                step_id = %stored.id,
                "calendar cancel-by-reason failed; ignoring"
            );
            HookOutcome::Cancelled { count: 0 }
        }
    }
}

/// Undo a [`HookOutcome::Reserved`] whose step write was then refused
/// (backlog 558396ff). The hook reserves BEFORE the write so a conflict
/// refuses with nothing stored; the price is that a write refused after
/// it — the stale-read 409, a storage error — must hand the hold back,
/// or the refusal's "nothing was written" is untrue of the calendar.
///
/// BY ITS ID, NEVER BY THE STEP. Every hold on this step carries the
/// same reason key, including one a racing start placed and whose write
/// landed; a release by reason erased that one too (the review of car
/// 983696b5). The id is the hold this attempt made and nothing else.
///
/// THE LANDED ROW DECIDES (the round-2 review of car 983696b5). The
/// write that refused this one may be the start this attempt's own
/// reservation let through: a racing start found that reservation on
/// the same step, person and window, took it as the step's hold
/// ([`HookOutcome::AlreadyHeld`]), placed nothing, and landed. Handing
/// it back then left an Active step holding no time. So `read_stored`
/// — the step as the store holds it now — is consulted twice:
///
/// - BEFORE the release: a step that is Active over exactly the time
///   this attempt reserved keeps the reservation; it is that step's
///   hold now, and nothing is cancelled.
/// - AFTER it: a start that landed between that read and the cancel is
///   re-held ([`hold_for_landed_start`]). With the landed start's own
///   re-assertion that closes the race in either order — whichever of
///   the two runs last finds the step Active and no hold, and places one.
///
/// ACTIVE OR COMPLETED (the round-3 review of car 983696b5, backlog
/// dc7c91cc). The start this attempt let through may also have completed
/// before this write was judged, and a completed step keeps its hold as
/// the record of the work (`done_does_not_cancel`). Reading only Active,
/// the refused racer found the step Completed, handed back the one hold
/// it had, and the second read re-held nothing: the reviewer's probe
/// left 0 live holds where 1 was owed. So both reads take a step that is
/// Completed over exactly the time this attempt reserved as holding it.
/// A step completed straight from Ready, never Active, is not told apart
/// by the row; it keeps the hold over its own scheduled time too.
///
/// Best-effort like the skip's release: the refusal is already the
/// answer, and a hold this misses is the step's own — its re-send takes
/// it as [`HookOutcome::AlreadyHeld`] rather than refusing against it.
/// What the re-hold found is returned, so a hold another reservation
/// took in the gap reaches the caller as [`HookOutcome::HoldLost`] to
/// record (backlog 4bdb8150).
pub async fn release_after_refused_write<F, Fut>(
    calendar: Option<&Arc<dyn CalendarClient>>,
    reservation: ReservationId,
    attempted: &Step,
    actor: &str,
    read_stored: F,
) -> HookOutcome
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<Step>>,
{
    let Some(calendar) = calendar else {
        return HookOutcome::NoOp;
    };
    let holds_this_time = |stored: &Step| {
        matches!(stored.status, StepStatus::Active | StepStatus::Completed)
            && scheduling_fields(stored).is_some()
            && scheduling_fields(stored) == scheduling_fields(attempted)
    };
    if read_stored().await.is_some_and(|s| holds_this_time(&s)) {
        return HookOutcome::NoOp;
    }
    if let Err(e) = calendar.cancel(reservation, actor).await {
        tracing::warn!(
            error = %e,
            step_id = %attempted.id,
            "calendar: could not release the reservation of a refused step write"
        );
    }
    match read_stored().await {
        Some(stored) if holds_this_time(&stored) => reassert_hold(calendar, &stored, actor).await,
        Some(stored) => hold_for_landed_start(Some(calendar), &stored, actor).await,
        None => HookOutcome::NoOp,
    }
}

/// The calendar half of a START that LANDED: `attempted` is the row the
/// start computed its hold from, `stored` the row as the write left it,
/// and `reserved` / `took_held` what [`apply_step_transition`] did.
///
/// THE STORED ROW DECIDES WHAT IS HELD (the adversarial review of car
/// 611fbffd, 2026-09-26). The step PUT writes over the very row it read,
/// so for it the two are one. The claim CAS judges only status and
/// holder, so a `scheduled_at` or `duration_minutes` merged between the
/// claim's read and its CAS lands under it, and the claim's hold stood
/// on the OLD window while the Active step said another. So when a
/// start landed on a schedule other than the one it reserved for, the
/// reservation it made is handed back BY ITS ID (never by the step —
/// 983696b5) and the time is held as stored.
///
/// Otherwise: a start that took a hold it did not place re-asserts it,
/// since the racer that placed it may be refused and hand it back
/// (983696b5). A lost hold either way comes back as
/// [`HookOutcome::HoldLost`] for the caller to record (4bdb8150).
pub async fn settle_landed_start(
    calendar: Option<&Arc<dyn CalendarClient>>,
    old: &Step,
    reserved: Option<ReservationId>,
    took_held: bool,
    attempted: &Step,
    stored: &Step,
    actor: &str,
) -> HookOutcome {
    let Some(calendar) = calendar else {
        return HookOutcome::NoOp;
    };
    let entered = matches!(old.status, StepStatus::Pending | StepStatus::Ready)
        && stored.status == StepStatus::Active;
    if entered && scheduling_fields(attempted) != scheduling_fields(stored) {
        if let Some(id) = reserved
            && let Err(e) = calendar.cancel(id, actor).await
        {
            tracing::warn!(
                error = %e,
                step_id = %stored.id,
                "calendar: could not release a start's hold on the time it no longer holds"
            );
        }
        return hold_for_landed_start(Some(calendar), stored, actor).await;
    }
    if took_held {
        return hold_for_landed_start(Some(calendar), stored, actor).await;
    }
    HookOutcome::NoOp
}

/// Re-assert the hold of a step that is Active as stored: reserve its
/// time, taking its own hold on exactly that time as already held.
///
/// For a start that LANDED without a reservation of its own — its hook
/// answered [`HookOutcome::AlreadyHeld`], on a hold a racing start had
/// placed — and for a refused racer that finds such a start landed
/// after it handed its reservation back (backlog 558396ff, the round-2
/// review of car 983696b5). Either way the step is Active and the hold
/// it counted on belonged to an attempt that may have released it.
///
/// Best-effort: the step has already moved; a failure is logged, and a
/// time another reservation holds is returned as
/// [`HookOutcome::HoldLost`] for the caller to record (backlog
/// 4bdb8150). A step not Active, or without a complete schedule, holds
/// nothing.
pub async fn hold_for_landed_start(
    calendar: Option<&Arc<dyn CalendarClient>>,
    stored: &Step,
    actor: &str,
) -> HookOutcome {
    let Some(calendar) = calendar else {
        return HookOutcome::NoOp;
    };
    if stored.status != StepStatus::Active {
        return HookOutcome::NoOp;
    }
    reassert_hold(calendar, stored, actor).await
}

/// Reserve `stored`'s scheduled time as its hold, whatever its status —
/// the caller has decided the step holds that time. Best-effort, logged;
/// a time another reservation holds is [`HookOutcome::HoldLost`], which
/// the caller records (backlog 4bdb8150).
async fn reassert_hold(
    calendar: &Arc<dyn CalendarClient>,
    stored: &Step,
    actor: &str,
) -> HookOutcome {
    let req = match start_request(stored, actor) {
        Ok(Some(req)) => req,
        Ok(None) => return HookOutcome::NoOp,
        Err(e) => {
            tracing::warn!(
                error = %e,
                step_id = %stored.id,
                "calendar: could not re-assert the hold of a step"
            );
            return HookOutcome::NoOp;
        }
    };
    let (subject, window) = (req.subject.clone(), req.window);
    match reserve_as_the_step(calendar, req).await {
        Ok(HookOutcome::Conflict { existing_rows }) => {
            tracing::warn!(
                step_id = %stored.id,
                existing = existing_rows.len(),
                "calendar: a held step's time is held by another reservation; recording the lost hold"
            );
            HookOutcome::HoldLost(LostHold {
                job_id: stored.job_id,
                step_id: stored.id,
                subject,
                window,
                held_by: existing_rows,
            })
        }
        Ok(held) => held,
        Err(e) => {
            tracing::warn!(
                error = %e,
                step_id = %stored.id,
                "calendar: could not re-assert the hold of a step"
            );
            HookOutcome::NoOp
        }
    }
}

/// Pull (scheduled_at, duration_minutes, assignee_id) out of step
/// metadata. Returns `None` if any are missing or malformed —
/// "incomplete schedule" means "no reservation".
///
/// `duration_minutes` is the one name: the `scheduling` StepType
/// declares it and the Schedule button writes it. This read
/// `duration_hours` until 2026-09-24 — a name no StepType declares and
/// nothing writes into step metadata — so no scheduled step ever
/// reserved (backlog a0b8e5bd).
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
        .get("duration_minutes")
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
        duration_minutes: Option<f64>,
    ) -> Step {
        let mut metadata = serde_json::Map::new();
        if let Some(s) = scheduled_at {
            metadata.insert("scheduled_at".into(), json!(s));
        }
        if let Some(d) = duration_minutes {
            metadata.insert("duration_minutes".into(), json!(d));
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
            Some(120.0),
        );
        let new = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(120.0),
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
            Some(120.0),
        );
        let new = {
            let mut s = step_with(
                StepStatus::Active,
                Some("emp-1"),
                Some("2026-04-27T10:00:00Z"),
                Some(120.0),
            );
            // Re-use the SAME id as old for the reservation_ref_id.
            s.id = old.id;
            s
        };
        let out = apply_step_transition(Some(&cal), &old, &new, "svc-mgr")
            .await
            .unwrap();
        assert!(matches!(out, HookOutcome::Reserved(_)), "{out:?}");
    }

    #[tokio::test]
    async fn the_schedule_buttons_write_makes_a_reservation() {
        // The step shaped EXACTLY as SchedulingSurface.svelte's
        // `persist('active')` PUTs it: status active, the assignee on
        // the step, and metadata carrying location / scheduled_at /
        // `duration_minutes` as an integer — the name the live
        // `scheduling` StepType declares. Until 2026-09-24 the hook
        // read `duration_hours`, which nothing writes, so this exact
        // write reserved nothing (backlog a0b8e5bd).
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let metadata = json!({
            "location": "bay-3",
            "scheduled_at": "2026-04-27T10:00:00Z",
            "duration_minutes": 90,
        });
        let old = step_with(StepStatus::Ready, None, None, None);
        let mut new = old.clone();
        new.status = StepStatus::Active;
        new.assignee_id = Some("emp-1".into());
        new.metadata = metadata;

        let out = apply_step_transition(Some(&cal), &old, &new, "emp-1")
            .await
            .unwrap();
        assert!(matches!(out, HookOutcome::Reserved(_)), "{out:?}");

        let calls = fake.calls();
        let Some(FakeCall::Reserve(req)) = calls.last() else {
            panic!("expected a Reserve call, got {calls:?}");
        };
        assert_eq!(req.subject, Subject::new("employee", "emp-1"));
        assert_eq!(req.reason_ref_id, new.id.to_string());
        assert_eq!(
            req.window.end - req.window.start,
            chrono::Duration::minutes(90),
            "the window is the step's duration_minutes, not hours"
        );
    }

    #[tokio::test]
    async fn missing_assignee_skips_reservation() {
        let cal: Arc<dyn CalendarClient> = Arc::new(FakeCalendarClient::new());
        let old = step_with(
            StepStatus::Pending,
            None,
            Some("2026-04-27T10:00:00Z"),
            Some(120.0),
        );
        let new = step_with(
            StepStatus::Active,
            None,
            Some("2026-04-27T10:00:00Z"),
            Some(120.0),
        );
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
    }

    #[tokio::test]
    async fn missing_scheduled_at_skips_reservation() {
        let cal: Arc<dyn CalendarClient> = Arc::new(FakeCalendarClient::new());
        let old = step_with(StepStatus::Pending, Some("emp-1"), None, Some(120.0));
        let new = step_with(StepStatus::Active, Some("emp-1"), None, Some(120.0));
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
            Some(120.0),
        );
        let new = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(120.0),
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
            Some(120.0),
        );
        let mut new = old.clone();
        new.status = StepStatus::Skipped;
        // Nothing before the write: a skip that is then refused must
        // not have released the hold of a step that stays Active.
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());

        let out = after_step_written(Some(&cal), &old, &new, "test").await;
        assert_eq!(out, HookOutcome::Cancelled { count: 2 });

        let calls = fake.calls();
        assert!(matches!(
            calls.last(),
            Some(FakeCall::CancelByReason(kind, _, _)) if kind.as_str() == reason::JOB_STEP
        ));
    }

    #[tokio::test]
    async fn a_skip_whose_row_stayed_completed_releases_nothing() {
        // The skip read the step Active; a completion landed first, and
        // a terminal row does not move. The stored row decides.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let old = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(120.0),
        );
        let mut stored = old.clone();
        stored.status = StepStatus::Completed;
        let out = after_step_written(Some(&cal), &old, &stored, "test").await;
        assert_eq!(out, HookOutcome::NoOp);
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
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
            Some(120.0),
        );
        let mut new = old.clone();
        new.status = StepStatus::Completed;
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
        assert_eq!(
            after_step_written(Some(&cal), &old, &new, "test").await,
            HookOutcome::NoOp
        );
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn a_terminal_step_asked_to_start_reserves_nothing() {
        // The handler refuses to move a terminal step, but only AFTER
        // this hook runs — so a reservation made here would outlive the
        // refused write (backlog 558396ff). And a Completed step's own
        // reservation is its record of past work: the residue release
        // must never reach it.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        for terminal in [StepStatus::Completed, StepStatus::Skipped] {
            let old = step_with(
                terminal,
                Some("emp-1"),
                Some("2026-04-27T10:00:00Z"),
                Some(120.0),
            );
            let mut new = old.clone();
            new.status = StepStatus::Active;
            let out = apply_step_transition(Some(&cal), &old, &new, "test")
                .await
                .unwrap();
            assert_eq!(out, HookOutcome::NoOp);
        }
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    /// A start of `step` from Ready, and the hold such a start places —
    /// as a row the calendar would hand back in a conflict.
    fn a_start_and_its_hold(step_id_owner: Option<&str>) -> (Step, Step, Reservation) {
        let old = step_with(
            StepStatus::Ready,
            Some("emp-1"),
            Some("2026-04-27T10:00:00Z"),
            Some(120.0),
        );
        let mut new = old.clone();
        new.status = StepStatus::Active;
        let start = DateTime::parse_from_rfc3339("2026-04-27T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let held = Reservation {
            id: ReservationId::new(),
            subject: Subject::new("employee", "emp-1"),
            window: TimeWindow::new(start, start + chrono::Duration::minutes(120)).unwrap(),
            reason_kind: reason::JOB_STEP.to_string(),
            reason_ref_id: step_id_owner
                .map(String::from)
                .unwrap_or_else(|| new.id.to_string()),
            strength: ReservationStrength::Hard,
            notes: None,
            created_by: "emp-1".into(),
            created_at: start,
            cancelled_at: None,
        };
        (old, new, held)
    }

    #[tokio::test]
    async fn a_start_releases_nothing_before_it_reserves() {
        // The review of car 983696b5 (backlog 558396ff): a start that
        // released every hold keyed on its step before reserving erased
        // the hold of a racing start that had LANDED. It reserves, only.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (old, new, _) = a_start_and_its_hold(None);
        let out = apply_step_transition(Some(&cal), &old, &new, "emp-1")
            .await
            .unwrap();
        assert!(matches!(out, HookOutcome::Reserved(_)), "{out:?}");
        let calls = fake.calls();
        assert!(
            matches!(
                calls.as_slice(),
                [FakeCall::Reserve(req)] if req.reason_ref_id == new.id.to_string()
            ),
            "one reserve and no cancel: {calls:?}"
        );
    }

    #[tokio::test]
    async fn the_steps_own_hold_on_the_same_time_is_already_held() {
        let fake = Arc::new(FakeCalendarClient::new());
        let (old, new, held) = a_start_and_its_hold(None);
        fake.stage_conflict(vec![held]);
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let out = apply_step_transition(Some(&cal), &old, &new, "emp-1")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::AlreadyHeld);
        assert!(
            fake.calls()
                .iter()
                .all(|c| matches!(c, FakeCall::Reserve(_))),
            "taken as held, nothing released: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn another_steps_hold_or_other_time_still_conflicts() {
        let (old, new, anothers) = a_start_and_its_hold(Some("another-step"));
        let (_, _, mut own_elsewhere) = a_start_and_its_hold(None);
        own_elsewhere.reason_ref_id = new.id.to_string();
        own_elsewhere.window = TimeWindow::new(
            own_elsewhere.window.start + chrono::Duration::minutes(30),
            own_elsewhere.window.end + chrono::Duration::minutes(30),
        )
        .unwrap();
        for staged in [vec![anothers], vec![own_elsewhere], vec![]] {
            let fake = Arc::new(FakeCalendarClient::new());
            fake.stage_conflict(staged.clone());
            let cal: Arc<dyn CalendarClient> = fake;
            let out = apply_step_transition(Some(&cal), &old, &new, "emp-1")
                .await
                .unwrap();
            assert!(
                matches!(out, HookOutcome::Conflict { .. }),
                "{staged:?} -> {out:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_write_hands_back_its_own_reservation_by_id() {
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (old, new, _) = a_start_and_its_hold(None);
        let mine = ReservationId::new();
        // The row stayed Ready: whatever refused the write, no start landed.
        let reads = stored_reads([Some(old.clone()), Some(old)]);
        release_after_refused_write(Some(&cal), mine, &new, "emp-1", reads).await;
        let calls = fake.calls();
        assert!(
            matches!(calls.as_slice(), [FakeCall::Cancel(id, _)] if *id == mine),
            "by id, never by the step's reason key: {calls:?}"
        );
    }

    /// A `read_stored` that answers `rows` in order, then nothing.
    fn stored_reads<const N: usize>(
        rows: [Option<Step>; N],
    ) -> impl Fn() -> std::future::Ready<Option<Step>> {
        let rows = std::sync::Mutex::new(std::collections::VecDeque::from(rows));
        move || std::future::ready(rows.lock().unwrap().pop_front().flatten())
    }

    #[tokio::test]
    async fn a_refused_start_keeps_its_reservation_for_the_start_it_let_through() {
        // The round-2 review of car 983696b5 (backlog 558396ff): the
        // write that refused this start was a racing start that took this
        // one's reservation as the step's hold and landed. The step is
        // Active over exactly that time: the reservation is its hold now.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (_, new, _) = a_start_and_its_hold(None);
        let reads = stored_reads([Some(new.clone())]);
        release_after_refused_write(Some(&cal), ReservationId::new(), &new, "emp-1", reads).await;
        assert!(fake.calls().is_empty(), "kept: {:?}", fake.calls());
    }

    #[tokio::test]
    async fn a_start_landing_after_the_refused_one_read_the_row_is_held_again() {
        // The racing start landed between the refused write's read and
        // its cancel: the cancel removed the only hold, so the second
        // read finds the step Active and holds its time again.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (old, new, _) = a_start_and_its_hold(None);
        let mine = ReservationId::new();
        let reads = stored_reads([Some(old), Some(new.clone())]);
        release_after_refused_write(Some(&cal), mine, &new, "emp-1", reads).await;
        let calls = fake.calls();
        assert!(
            matches!(
                calls.as_slice(),
                [FakeCall::Cancel(id, _), FakeCall::Reserve(req)]
                    if *id == mine && req.reason_ref_id == new.id.to_string()
            ),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_start_keeps_its_reservation_for_a_step_that_completed_over_it() {
        // The round-3 review of car 983696b5 (backlog dc7c91cc, SF-A):
        // the start this attempt let through landed on its reservation and
        // then COMPLETED. A completed step keeps its hold
        // (`done_does_not_cancel`), so nothing is cancelled.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (_, new, _) = a_start_and_its_hold(None);
        let mut completed = new.clone();
        completed.status = StepStatus::Completed;
        let reads = stored_reads([Some(completed)]);
        release_after_refused_write(Some(&cal), ReservationId::new(), &new, "emp-1", reads).await;
        assert!(fake.calls().is_empty(), "kept: {:?}", fake.calls());
    }

    #[tokio::test]
    async fn a_start_that_completed_after_the_refused_one_read_the_row_is_held_again() {
        // The same race, the other order: the refused write read the row
        // Ready and cancelled, and the start it let through had landed and
        // completed by the second read. The completed step's time is held
        // again, as an Active step's is.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (old, new, _) = a_start_and_its_hold(None);
        let mut completed = new.clone();
        completed.status = StepStatus::Completed;
        let mine = ReservationId::new();
        let reads = stored_reads([Some(old), Some(completed)]);
        release_after_refused_write(Some(&cal), mine, &new, "emp-1", reads).await;
        let calls = fake.calls();
        assert!(
            matches!(
                calls.as_slice(),
                [FakeCall::Cancel(id, _), FakeCall::Reserve(req)]
                    if *id == mine && req.reason_ref_id == new.id.to_string()
            ),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_start_hands_back_its_reservation_when_the_step_completed_on_other_time() {
        // Completed over a DIFFERENT schedule: the reservation is not that
        // step's hold, and it goes back.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (_, new, _) = a_start_and_its_hold(None);
        let mut elsewhere = new.clone();
        elsewhere.status = StepStatus::Completed;
        elsewhere.metadata["scheduled_at"] = json!("2026-04-27T14:00:00Z");
        let mine = ReservationId::new();
        let reads = stored_reads([Some(elsewhere.clone()), Some(elsewhere)]);
        release_after_refused_write(Some(&cal), mine, &new, "emp-1", reads).await;
        let calls = fake.calls();
        assert!(
            matches!(calls.as_slice(), [FakeCall::Cancel(id, _)] if *id == mine),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn a_landed_start_re_asserts_only_an_active_steps_hold() {
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (old, new, held) = a_start_and_its_hold(None);
        // Ready: nothing to hold.
        assert_eq!(
            hold_for_landed_start(Some(&cal), &old, "emp-1").await,
            HookOutcome::NoOp
        );
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        // Active, its hold gone: placed again.
        assert!(matches!(
            hold_for_landed_start(Some(&cal), &new, "emp-1").await,
            HookOutcome::Reserved(_)
        ));
        // Active, its hold standing: taken as held, nothing doubled.
        fake.stage_conflict(vec![held]);
        assert_eq!(
            hold_for_landed_start(Some(&cal), &new, "emp-1").await,
            HookOutcome::AlreadyHeld
        );
    }

    /// The hold a step whose time another reservation took has lost —
    /// what the re-assertion must report, naming the step, its window
    /// and the row now holding that time.
    fn lost_to(step: &Step, holder: &Reservation) -> HookOutcome {
        HookOutcome::HoldLost(LostHold {
            job_id: step.job_id,
            step_id: step.id,
            subject: Subject::new("employee", "emp-1"),
            window: holder.window,
            held_by: vec![holder.clone()],
        })
    }

    #[tokio::test]
    async fn a_landed_start_whose_time_another_took_reports_the_hold_lost() {
        // Backlog 4bdb8150 (the round-3 review of car 983696b5). The
        // start landed on a hold it did not place, the racer that placed
        // it handed it back, and before the re-assertion a third party
        // reserved the assignee's time. The step is Active and holds
        // nothing. That was a `tracing::warn` nobody reads; it is now an
        // outcome the handler records.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (_, new, anothers) = a_start_and_its_hold(Some("another-step"));
        fake.stage_conflict(vec![anothers.clone()]);
        assert_eq!(
            hold_for_landed_start(Some(&cal), &new, "emp-1").await,
            lost_to(&new, &anothers)
        );
    }

    #[tokio::test]
    async fn a_refused_start_whose_re_hold_finds_the_time_taken_reports_the_hold_lost() {
        // The refused racer handed its reservation back, found the start
        // it let through landed, and re-held — but the time was taken in
        // between. Reported, never only logged.
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (old, new, anothers) = a_start_and_its_hold(Some("another-step"));
        fake.stage_conflict(vec![anothers.clone()]);
        let reads = stored_reads([Some(old), Some(new.clone())]);
        let out =
            release_after_refused_write(Some(&cal), ReservationId::new(), &new, "emp-1", reads)
                .await;
        assert_eq!(out, lost_to(&new, &anothers));
    }

    #[tokio::test]
    async fn a_refused_write_that_hands_back_its_hold_reports_nothing_lost() {
        let fake = Arc::new(FakeCalendarClient::new());
        let cal: Arc<dyn CalendarClient> = fake.clone();
        let (old, new, _) = a_start_and_its_hold(None);
        let reads = stored_reads([Some(old.clone()), Some(old)]);
        let out =
            release_after_refused_write(Some(&cal), ReservationId::new(), &new, "emp-1", reads)
                .await;
        assert_eq!(
            out,
            HookOutcome::NoOp,
            "the step never started: no hold owed"
        );
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
            Some(120.0),
        );
        let new = step_with(
            StepStatus::Active,
            Some("emp-1"),
            Some("2026-04-27T14:00:00Z"),
            Some(180.0),
        );
        let out = apply_step_transition(Some(&cal), &old, &new, "test")
            .await
            .unwrap();
        assert_eq!(out, HookOutcome::NoOp);
        assert!(fake.calls().is_empty());
    }
}
