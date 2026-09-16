//! Surface opens — which surfaces an operator opens, held by the system.
//!
//! David, 2026-09-16, running the company as one human plus agents: "I
//! am mostly concerned about my personal HCI … the UI and good
//! transparency into underlying state has been really important for
//! driving reliability and trust." Then: "let's measure which surfaces I
//! open for a week." Measured that day: nothing recorded a route open
//! per actor. The SPA routes client-side, the audit log carries no
//! per-request events BY DECISION (docs/architecture-decisions.md
//! §Policy & auth), and boss-observability aggregates service health,
//! not page opens (backlog 628f182b).
//!
//! **Not an audit event, and not a projection of one.** A page open is
//! a measurement of the operator's attention, not a state change of the
//! company, and the decision above keeps that class out of the log. So
//! this is the `cadence` shape — the row IS the record, no outbox, no
//! rebuilder — and not the `agent_runs` one. Hexagonal like both:
//! domain types + port + in-memory adapter + Pg adapter + HTTP door.
//!
//! **The actor is the session's, never the body's.** The door reads it
//! off the `x-boss-user` header the gateway signs from the cookie, the
//! way every write on this service is signed, and refuses a
//! machine-shaped actor: an automation or an agent login has no
//! surface to open. Agents' automated reads never arrive here by
//! construction — they do not run the SPA — so what the table holds is
//! an operator's own attention, which is the consent David gave.
//!
//! **The route is a pattern.** `/ux/jobs/:jobId`, minted client-side
//! off the parsed route (apps/web/src/shell/surface-opens.ts), never
//! the uuid: a busy day on one packet is one surface, not a hundred.
//!
//! **Retention is thirty days**, swept by the chore that reads the
//! table (`infra/surface-usage.sh`, kind `maintenance-surface-usage`).
//! [`RETENTION_DAYS`] is a constant until the retention registry lands
//! (backlog 16115a17); then it is a retention row and the constant
//! goes. Counts per route per actor per day are the whole product.

pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod types;

pub use in_memory::InMemorySurfaceOpens;
pub use port::{SurfaceOpens, SurfaceOpensError, sweep_retention};
#[cfg(feature = "postgres")]
pub use postgres::PgSurfaceOpens;
pub use types::{NewSurfaceOpen, RETENTION_DAYS, Rollup, RouteCount, Sweep, validate_route};
