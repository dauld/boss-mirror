//! boss-docs — design corpus index.
//!
//! Indexes markdown files under `docs/design/*.md` and exposes a READ
//! API over them. Git is the source of truth for doc prose; this crate
//! holds read-caches refreshed on demand plus a closed ledger of
//! answers recorded before the flush pipeline was deleted (2026-09-10,
//! backlog f5da586c — the decision is in
//! `docs/architecture-decisions.md`, "Design docs and the decision
//! record"). It writes nothing back.

pub mod config;
pub mod http;
pub mod in_memory;
pub mod parser;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod reindex;
pub mod types;

pub use in_memory::InMemoryDocsRepo;
pub use parser::{ParsedDoc, parse_doc};
pub use port::{DocsError, DocsRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgDocsRepo;
pub use types::*;
