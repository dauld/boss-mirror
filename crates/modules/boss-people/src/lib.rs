//! People domain — Boss employees, org chart, certifications.
//!
//! Every piece of work inside the company is ultimately done by someone,
//! so this crate is referenced by many others: refurb jobs cite the tech,
//! service tickets name the assignee, sales opportunities have an owner.

pub mod assets_client;
#[cfg(feature = "postgres")]
pub mod employee_changes;
mod grants;
pub mod http;
pub mod in_memory;
pub mod operator_baseline;
pub mod people_config;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod pto;
#[cfg(feature = "postgres")]
pub mod rebuild;
#[cfg(feature = "postgres")]
pub mod requisitions;
#[cfg(feature = "postgres")]
pub mod scope;
#[cfg(feature = "postgres")]
pub mod webauthn;
// Not gated: `port` and `in_memory` are ungated and both import
// `crate::types::Employee`. A `#[cfg]` here breaks the crate's default
// feature set — which is how it arrived, when #180 deleted the
// `pub mod search;` line below a `#[cfg(feature = "postgres")]` and
// left the attribute to bind to whatever came next.
pub mod types;
#[cfg(feature = "postgres")]
pub mod workflows;

pub use in_memory::InMemoryPeople;
pub use port::{PeopleError, PeopleRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgPeople;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_people};
pub use types::*;
pub mod events;
