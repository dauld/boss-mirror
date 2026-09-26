//! Field-service scheduling — operations-needs session 2 (F1-F3).
//!
//! Three entities:
//!   - `TechAvailability` — per-employee, time-bucketed slots
//!     (available, PTO, sick, holiday, training, blocked).
//!   - `ScheduledAssignment` — a reservation tying a tech + time to
//!     a specific Job. Scheduling is fundamentally cross-Job so it
//!     lives in its own tables rather than step metadata (D9).
//!   - `TechShiftPattern` — recurring weekly templates that
//!     materialize into TechAvailability rows.
//!
//! Hexagonal: port trait + pg adapter. The standalone week-grid
//! surface at `/service/schedule` and the tech-picker plugin both
//! consume the same query API.

pub mod events;
pub mod feed_token;
pub mod http;
pub mod ics;
pub mod materialize;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod types;

#[cfg(feature = "postgres")]
pub use rebuild::{RebuildReport as SchedulingRebuildReport, rebuild_scheduling};

pub use feed_token::{CalendarTokenSha256, mint_calendar_token};
pub use port::{SchedulingError, SchedulingRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgScheduling;
pub use types::{
    AssignmentKind, AssignmentStatus, AvailabilityKind, AvailabilitySource, NewScheduledAssignment,
    NewTechAvailability, ScheduledAssignment, TechAvailability, TechShiftPattern, WeekGridBlock,
    WeekGridRow,
};
