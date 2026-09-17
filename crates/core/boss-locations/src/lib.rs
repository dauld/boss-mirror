//! Location primitive — places (storefronts, HQ, warehouse zones,
//! field regions, …) with hierarchy + timezone + optional geo.
//!
//! See `docs/architecture-decisions.md` §Locations. The
//! `Location` type lives in `boss-core::primitives`; this crate
//! adds the persistence port, in-memory + Postgres adapters, and
//! an HTTP API: open reads plus ONE write, `POST /api/locations/batch`
//! (insert-if-absent by id), the door a tenant's `seeds/locations.toml`
//! goes through (backlog 1ec8312a, 2026-09-17). Authoring (edit /
//! retire) lands later when the admin UI does.
//!
//! Locations show up in two grammatical positions:
//! - **Subject** — the place itself, owned by this crate.
//! - **AttributePart** — `{ location_id, since_at }` hung on
//!   another Subject (e.g. `Employee.work_location`). Singleton
//!   per `(subject_id, attribute_name)` per the "no two places at
//!   once" invariant.

pub mod http;
pub mod in_memory;
pub mod locations_config;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;

pub use in_memory::InMemoryLocations;
pub use port::{LocationError, LocationRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgLocations;
