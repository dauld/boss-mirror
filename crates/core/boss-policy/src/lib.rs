//! Row-level authorization service.
//!
//! The contract types + the in-memory engine live in
//! `boss-policy-client`; this crate contains just the HTTP server,
//! the Postgres adapter, and the seeder binaries. Domain services
//! depend on `boss-policy-client` so they don't transitively pull
//! this crate's sqlx + axum service dep tree.
//!
//! The re-exports below keep `boss_policy::Foo` import paths working,
//! but new code should import from `boss_policy_client` directly.

#[cfg(feature = "postgres")]
pub mod postgres;

pub mod authority;
pub mod bootstrap;
pub mod http;

// Re-export everything from boss-policy-client so the historical
// `use boss_policy::{Action, Decision, ...}` paths keep compiling.
pub use boss_policy_client::defaults::{default_rules, shipped_resources};
pub use boss_policy_client::engine::PolicyEngine;
pub use boss_policy_client::in_memory::InMemoryPolicy;
pub use boss_policy_client::port::{PolicyError, PolicyRepository, ReconcileStats};
pub use boss_policy_client::predicates;
pub use boss_policy_client::seed_loader;
pub use boss_policy_client::types::*;

// Module-path re-exports so `boss_policy::port::PolicyRepository`
// import paths keep compiling.
pub use boss_policy_client::port;
pub use boss_policy_client::types;

#[cfg(feature = "postgres")]
pub use postgres::PgPolicy;
