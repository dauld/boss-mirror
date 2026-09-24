//! BOSS assets domain — serial-numbered physical assets + their
//! append-only event logs.
//!
//! Every physical unit that enters Boss's custody is a `Asset` identified
//! by a `AssetId`. Its full life — custody (Registered → Received →
//! PutAway), commerce (Sold → Shipped → Installed → WarrantyStarted),
//! and service (TicketOpened/Closed, PartReplaced, Decommissioned) —
//! lives in one append-only `AssetEvent` log.
//!
//! `AssetCurrentState` is a projection from that log, rebuildable at any
//! time. The log is the source of truth; current state is a summary.
//!
//! Hexagonal: the domain defines `AssetsRepository` (a port). Postgres,
//! in-memory, NATS-backed adapters all implement the same trait.

pub mod asset_config;
pub mod asset_insights;
#[cfg(feature = "postgres")]
pub mod asset_parts;
pub mod bridge;
pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod project;
pub mod sse;
pub mod types;

pub use in_memory::InMemoryAssets;
pub use port::{AssetsError, AssetsRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgAssets;
pub use project::project;
pub use types::{
    AssetCurrentState, AssetEvent, AssetEventKind, AssetId, AssetLifecyclePhase, IntakeSource,
    WarrantyCoverage,
};
