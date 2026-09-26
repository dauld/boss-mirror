//! Scheduling port — the trait the frontend's surfaces and the
//! materialize binary call into.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

use super::feed_token::CalendarTokenSha256;
use super::types::{
    NewScheduledAssignment, NewTechAvailability, ScheduledAssignment, TechAvailability,
    TechShiftPattern, WeekGridRow,
};

#[derive(Debug, thiserror::Error)]
pub enum SchedulingError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("storage: {0}")]
    Storage(String),
}

/// OUTBOX (phase 2): every mutation records its scheduling.* event
/// on the transactional outbox INSIDE the write transaction via the
/// caller's stamp (`boss_events::outbox::record_event_in_tx`);
/// boss-event-relay delivers to audit_log + NATS post-commit.
/// Deletes record only after the row actually deleted.
#[async_trait]
pub trait SchedulingRepository: Send + Sync {
    // ----- Availability -----
    async fn create_availability(
        &self,
        new: NewTechAvailability,
        stamp: &EventStamp,
    ) -> Result<TechAvailability, SchedulingError>;
    async fn list_availability(
        &self,
        employee_id: Option<&str>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<TechAvailability>, SchedulingError>;
    async fn delete_availability(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), SchedulingError>;

    // ----- Assignments -----
    async fn create_assignment(
        &self,
        new: NewScheduledAssignment,
        stamp: &EventStamp,
    ) -> Result<ScheduledAssignment, SchedulingError>;
    async fn get_assignment(
        &self,
        id: Uuid,
    ) -> Result<Option<ScheduledAssignment>, SchedulingError>;
    async fn list_assignments(
        &self,
        tech_id: Option<&str>,
        target_job_id: Option<Uuid>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ScheduledAssignment>, SchedulingError>;
    async fn update_assignment_status(
        &self,
        id: Uuid,
        status: super::types::AssignmentStatus,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), SchedulingError>;
    async fn delete_assignment(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), SchedulingError>;

    // ----- Shift patterns -----
    async fn upsert_shift_pattern(
        &self,
        employee_id: &str,
        day_of_week: i16,
        starts_at_time: chrono::NaiveTime,
        ends_at_time: chrono::NaiveTime,
        timezone: &str,
        effective_from: NaiveDate,
        stamp: &EventStamp,
    ) -> Result<TechShiftPattern, SchedulingError>;
    async fn list_shift_patterns(
        &self,
        employee_id: Option<&str>,
    ) -> Result<Vec<TechShiftPattern>, SchedulingError>;

    /// Materializes `tech_availability` rows from each active
    /// `tech_shift_pattern` for the date window `[from, to)`.
    /// Idempotent — skips rows that already exist from the same source
    /// (via source='shift-pattern' + overlapping time range).
    /// Returns the number of rows inserted.
    async fn materialize_shift_patterns(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<i64, SchedulingError>;

    // ----- Projections -----
    /// Returns per-employee blocks over [from, to). Employees with no
    /// availability AND no assignments are omitted.
    async fn week_grid(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        employee_ids: Option<&[String]>,
    ) -> Result<Vec<WeekGridRow>, SchedulingError>;

    // ----- ICS calendar feed -----
    //
    // Every method below takes or returns the token's DIGEST, never the
    // token (backlog 4aaff4dc, design 3101c506): the server keeps no
    // value that opens a feed, so no table, event, log line or backup
    // can leak one. See `super::feed_token`.

    /// When the tech's current feed was minted, or `None` if they have
    /// none. Not the URL: the server does not hold it.
    async fn calendar_feed_created_at(
        &self,
        employee_id: &str,
    ) -> Result<Option<DateTime<Utc>>, SchedulingError>;

    /// Writes `token_sha256` as the tech's single active feed, minting
    /// one or replacing whatever was there. Rotating invalidates the
    /// old URL.
    async fn rotate_calendar_token(
        &self,
        employee_id: &str,
        token_sha256: &CalendarTokenSha256,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), SchedulingError>;

    /// Replaces an EXISTING feed's digest with `token_sha256` — an
    /// operator's revocation. `NotFound`, with nothing written and no
    /// event, when the tech has no feed: there is nothing to revoke,
    /// and minting one for an id nobody checked is how an orphan row
    /// got made (round-2 review of car f46d9df2).
    async fn revoke_calendar_token(
        &self,
        employee_id: &str,
        token_sha256: &CalendarTokenSha256,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), SchedulingError>;

    /// How many feeds still open with a token the log holds in the
    /// clear — every row minted before digests, until the revoke below
    /// runs. Zero means no token ever written to the log opens a feed.
    async fn count_calendar_tokens_logged_raw(&self) -> Result<i64, SchedulingError>;

    /// Revokes every feed [`Self::count_calendar_tokens_logged_raw`]
    /// counts — one `scheduling.calendar-token.rotated` event each,
    /// carrying the digest of a discarded token — and returns how many.
    /// Events, not a migration: a migration's delete would be undone by
    /// the next rebuild, which replays the legacy events. Bounded to
    /// those rows, so a second run revokes nothing and never breaks a
    /// feed an employee has re-subscribed since.
    async fn revoke_calendar_tokens_logged_raw(
        &self,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<u64, SchedulingError>;

    /// Reverse lookup for the public `/ics/{token}/calendar.ics`
    /// endpoint, by the digest of the token presented. `None` means it
    /// matches nobody.
    async fn employee_by_calendar_token(
        &self,
        token_sha256: &CalendarTokenSha256,
    ) -> Result<Option<String>, SchedulingError>;
}
