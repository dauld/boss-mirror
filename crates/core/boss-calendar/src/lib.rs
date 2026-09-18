//! `boss-calendar` — implementation of the global-calendar
//! primitive (decision record: `docs/architecture-decisions.md`
//! §Calendar).
//!
//! The crate ships:
//! - `port::CalendarClient` — the trait every caller (boss-jobs,
//!   boss-people PTO, future Meeting modules) talks to.
//! - `in_memory::InMemoryCalendar` — for tests + sim runs.
//! - `postgres::PgCalendar` — the production adapter; relies on
//!   the `calendar_no_hard_overlap` GIST exclusion constraint to
//!   enforce the no-double-book invariant.
//! - `http::router(...)` — axum HTTP surface mounted by the
//!   `boss-calendar-api` binary.
//!
//! Wire types live in `boss_core::calendar` so domain crates can
//! build a `ReservationRequest` without depending on this crate.

pub mod calendar_config;
pub mod events;
pub mod http;
pub mod in_memory;
pub mod port;

#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;

pub use calendar_config::{CalendarApiConfig, CalendarConfigError};
pub use http::{CalendarApiState, router};
pub use in_memory::InMemoryCalendar;
pub use port::{BusinessCalendarsOutcome, CalendarClient, CalendarError};

#[cfg(feature = "postgres")]
pub use postgres::PgCalendar;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_calendar};
