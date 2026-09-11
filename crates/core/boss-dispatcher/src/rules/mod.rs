//! Rule registry + evaluator for the dispatcher's reactive layer.
//!
//! See docs/architecture-decisions.md §Dispatcher — the event
//! router for the architectural anchor. This module implements the data-driven
//! reactive layer: rules are TOML rows declaring
//! `(on_event, when?, do[], delay?)`; the matcher walks the registry
//! against incoming events and returns matched rules with their
//! handler args evaluated to concrete values.
//!
//! Pure-function-heavy; no I/O. Tests stay in-memory. The NATS
//! subscription layer + side-effect handler dispatch live in a
//! later pass and are unaware of this module's internals.

/// Re-export the shared DSL from `boss-expr` so existing imports
/// of `crate::rules::expr::Value` etc. keep working.
pub use boss_expr as expr;

pub mod authoring;
pub mod handler;
pub mod helpers_inventory;
pub mod jobs_spawn;
pub mod log_tail;
pub mod payload_contract;
pub mod registry;
pub mod runner;
pub mod schedule_runner;
pub mod seed;
