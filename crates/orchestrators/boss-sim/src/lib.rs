//! `boss-sim` — shape-driven simulation primitives.
//!
//! The crate ships the alphabet that every per-tenant engine
//! (`boss-brewery-engine`, `boss-used-device-shop-engine`)
//! composes: the day-loop runner + Periodic / Counterparty engines
//! (`engines`), the Workflow/Step/Subject day-loop body
//! (`shape_driven`), the SimOutput trait and its in-memory + live
//! HTTP adapters (`output`), the seeded PRNG (`rng`), and the calendar
//! registry (`calendar`). Sales tax is not the sim's: the ledger
//! computes it per invoice from the rates the tenant published from
//! its `seeds/tax.toml` (the bundled `tax_rates` copy had no caller
//! and was deleted, backlog fc27a0ce, 2026-09-19).
//!
//! A tenant is described as data: a `tenant.toml`, a
//! `workflows.toml`, and a per-tenant engine crate.

/// Actor-coverage telemetry — is the sim driving the WHOLE roster?
/// Pure computation of per-role roster vs acting vs completions,
/// rendered by the simulator SPA so an under-simulated tenant is
/// visible on its face.
pub mod actor_coverage;
/// Per-actor API-call tallies — the cockpit's "how the sim engages the
/// API, by who's acting" telemetry. Shared between the workforce + the
/// live output adapter.
pub mod api_activity;
pub mod calendar;
pub mod engines;
pub mod event_routes;
pub mod output;
pub mod rng;
pub mod scheduler;
pub mod shape_driven;
/// The sim-as-workforce executor: reads the live system (clock, open
/// assignments, real inventory) and works steps through the public API.
/// HTTP-only — it drives a live API stack.
#[cfg(feature = "http")]
pub mod workforce;
