//! The SHAPE of the dispatcher's cascade metadata — never its contents.
//!
//! The rule registry declares the reactive layer as `trigger event → rule →
//! handler(s)`. To render the full cascade the viz also needs what each
//! handler causes to be emitted downstream, and the jobs-api / external
//! consequences that re-enter the rule set. Both are facts about the
//! HANDLERS, and the handlers are not core's: whoever assembles the
//! [`HandlerRegistry`](crate::rules::handler::HandlerRegistry) declares
//! them, and hands them to [`HttpState`](crate::http::HttpState) to serve.
//!
//! WHY CORE HOLDS ONLY THE TYPE (backlog ec40e269, 2026-09-23). Until this
//! car the whole roster was a literal in this file: 51 handler names, 22
//! of which no product rule invokes — `people.hire`,
//! `commerce.invoice.issue` and the rest of the company-module vocabulary
//! only the example tenants' seeds name. A core crate spelling a tenant's
//! ledger postings is the assumption CLAUDE.md §10 refuses, and the
//! file's own comment already said where the fact belongs: "in the crate
//! whose code it describes".
//! That crate is `boss-dispatcher-handlers`, which carries the handler
//! implementations and the binary that registers them; the data now lives
//! there (`boss_dispatcher_handlers::cascade`), and core serves whatever
//! its assembler declared.

use serde::Serialize;
use std::collections::BTreeMap;

/// A cascade edge that closes a loop but is NOT a dispatcher rule — a
/// consequence of jobs-api's DAG mechanics or of an external
/// counterparty. The viz renders these with a distinct style so the
/// dispatcher's own rules stay legible.
#[derive(Debug, Clone, Serialize)]
pub struct SystemEdge {
    /// Emitted event the edge starts from.
    pub from: &'static str,
    /// Event/topic it leads to (often a `*` wildcard topic).
    pub to: &'static str,
    /// `"jobs-api"` (DAG mechanics) or `"external"` (a counterparty).
    pub kind: &'static str,
    /// Human-readable reason the edge exists.
    pub label: &'static str,
}

/// What the assembler of a dispatcher declares about the handlers it
/// registered: the event kind(s) each one causes to be emitted, keyed by
/// its registered name (an empty list is a pure sink), and the non-rule
/// edges that close the cascade's loops.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Cascade {
    pub handler_emits: BTreeMap<&'static str, Vec<&'static str>>,
    pub system_edges: Vec<SystemEdge>,
}
