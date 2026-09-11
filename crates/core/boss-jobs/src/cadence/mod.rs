//! Cadence registry — the schedule the train conductor runs on.
//!
//! `cadence_rules` says WHEN each `boss train` verb should fire (every
//! N minutes, at fixed times of day, or when the loading dock reaches
//! a depth); `cadence_firings` is the exactly-once claim log and the
//! measurement record of what each firing cost.
//!
//! Both tables predate this module — what is new is the door. The
//! conductor used to open its own `PgPool`, which meant it could only
//! run somewhere with database credentials, and (2026-08-13) could
//! silently read a different database than the one an operator was
//! inspecting. Serving the registry over the jobs API makes the
//! conductor a normal HTTP client of the system of record.
//!
//! Hexagonal: port trait + Pg adapter + in-memory adapter, same shape
//! as `scheduling`.

pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod types;

pub use in_memory::InMemoryCadence;
pub use port::{CadenceError, CadenceRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgCadence;
pub use types::{
    CadenceRuleRow, ClaimResult, FiringOutcome, LastFiring, NewFiring, departs_a_train,
};
