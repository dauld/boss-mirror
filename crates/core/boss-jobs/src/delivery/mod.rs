//! Delivery-policy registry — the rules the train conductor DECIDES by.
//!
//! `delivery_policy` holds the policy content of the delivery pipeline:
//! how many strikes hold a car back, how long a train may sit before its
//! consist is released, which lints the consist check runs over an
//! assembled tree, and the budgets that bound what a failure puts on the
//! record. `crates/orchestrators/boss-cli/src/train.rs` keeps the
//! mechanism — merge, push, open, watch, merge, report — and reads these
//! rows for every decision it used to make from a constant
//! (docs/design/delivery-as-protocol.md).
//!
//! WHY THIS IS NOT A `cadence_rules` ROW. `cadence_rules` answers WHEN a
//! `boss train` verb fires, and every one of its columns is a firing
//! parameter under a CHECK that admits exactly one basis group per row.
//! None of these values is a schedule; `stall_hours` would need a basis
//! that fires nothing, and its rule name would sit in `cadence_firings`
//! — the exactly-once claim log — never claiming a window. The readers
//! differ too: the cadence loop reads cadence rows to decide whether to
//! spawn a verb, and the conductor reads these inside the verb.
//!
//! Hexagonal: port trait + Pg adapter + in-memory adapter + HTTP door,
//! the same shape as `cadence` and `scheduling`. The row is DECLARED in
//! `infra/platform/delivery-policy/<name>.toml` and published by the
//! platform seed (`crate::delivery_policy_seed`, backlog 393d3234,
//! 2026-09-18); the two migrations that declared it before are history.

pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod types;

pub use in_memory::{InMemoryDeliveryPolicy, StoredPolicy};
pub use port::{DeliveryPolicyError, DeliveryPolicyRegistry, DeliveryPolicyRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgDeliveryPolicy;
pub use types::{DeliveryPolicyRow, DeliveryPolicySpec};
