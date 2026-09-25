//! The sensors port — the registry's one write and the poller's reads
//! and stamps. No outbox events, for the reason the module doc gives:
//! the reading is its own record, the packet it opens is the audit fact.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::types::{
    BatchOutcome, NewReading, PollStamp, RETENTION_DAYS, Reading, ReadingsWindow, SensorInput,
    SensorRow, Sweep,
};

#[derive(Debug, thiserror::Error)]
pub enum SensorsError {
    #[error("bad request: {0}")]
    BadRequest(String),
    /// A reading or a stamp named a sensor the registry does not hold.
    /// Loud and distinct rather than a silent no-op (the credentials
    /// port's posture): a reading against nothing is a finding.
    #[error("no sensor {0:?} in the registry")]
    UnknownSensor(String),
    #[error("storage: {0}")]
    Storage(String),
}

#[async_trait]
pub trait Sensors: Send + Sync {
    /// Every declared sensor, ordered by id.
    async fn list(&self) -> Result<Vec<SensorRow>, SensorsError>;

    /// Admit a tenant's declarations, insert-if-absent by id (the
    /// classes batch idiom): a second publish inserts nothing new and a
    /// row already there keeps its cursor. Validation is the caller's
    /// (`validate_sensor`), the same check at every door.
    async fn publish(
        &self,
        tenant_id: &str,
        sensors: &[SensorInput],
    ) -> Result<BatchOutcome, SensorsError>;

    /// Record observations, insert-if-absent by `(sensor, external_id)`.
    /// Returns how many were new.
    async fn record(
        &self,
        sensor_id: &str,
        readings: &[NewReading],
    ) -> Result<BatchOutcome, SensorsError>;

    /// The readings of one sensor that still owe a packet
    /// (`packet_id IS NULL`), oldest observation first.
    async fn unstamped(&self, sensor_id: &str) -> Result<Vec<Reading>, SensorsError>;

    /// One sensor's readings observed in `[since, until)`: how many,
    /// how many carry a packet, and which packets (backlog 35baed54).
    /// An undeclared sensor is `UnknownSensor`, never a count of zero.
    async fn window(
        &self,
        sensor_id: &str,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<ReadingsWindow, SensorsError>;

    /// Stamp the packet opened for one reading. Stamping twice with
    /// the same id is a no-op; the first stamp wins (a reading has one
    /// packet).
    async fn stamp(
        &self,
        sensor_id: &str,
        external_id: &str,
        packet_id: &str,
    ) -> Result<(), SensorsError>;

    /// Record that a poll ran: `last_polled_at` takes the stamp's
    /// instant, `cursor_at` advances to the stamp's cursor only if it
    /// is later than what is held (monotonic; `None` leaves it).
    async fn mark_polled(&self, sensor_id: &str, stamp: &PollStamp) -> Result<(), SensorsError>;

    /// Delete every reading observed before `before` that has its
    /// packet — a reading still owed one is evidence of work not yet
    /// opened and is kept whatever its age. Returns how many.
    async fn sweep(&self, before: DateTime<Utc>) -> Result<u64, SensorsError>;
}

/// The sweep as the door reports it: the cutoff is `now` less the
/// retention, and the adapter's count rides beside it.
pub async fn sweep_retention(
    repo: &dyn Sensors,
    now: DateTime<Utc>,
) -> Result<Sweep, SensorsError> {
    let before = now - chrono::Duration::days(RETENTION_DAYS);
    let deleted = repo.sweep(before).await?;
    Ok(Sweep {
        deleted,
        before,
        retention_days: RETENTION_DAYS,
    })
}
