//! Convenience helper for the shift-pattern materializer binary —
//! the heavy lifting lives in `SchedulingRepository::materialize_shift_patterns`;
//! this wraps the usual window ("now → +8 weeks") for the cron job.

use chrono::{Duration, Utc};

use super::port::{SchedulingError, SchedulingRepository};

/// Default 8-week materialization window: far enough ahead to schedule
/// against, short enough that the cron job's writes stay small.
pub const DEFAULT_WEEKS_AHEAD: i64 = 8;

/// Materialize availability rows for the next `weeks_ahead` weeks
/// starting today. Idempotent via the port's existence check. Returns
/// rows inserted this call.
pub async fn materialize_next(
    repo: &dyn SchedulingRepository,
    weeks_ahead: i64,
) -> Result<i64, SchedulingError> {
    let from = Utc::now().date_naive();
    let to = from + Duration::weeks(weeks_ahead);
    repo.materialize_shift_patterns(from, to).await
}
