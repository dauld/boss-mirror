//! The brewery's unified **prepare** phase — seed the entire model
//! through the public API, idempotently, clock-authoritatively.
//!
//! We're converging the brewery's four driver binaries
//! (`boss-brewery-bootstrap`, `boss-brewery-data-seed`,
//! `boss-brewery-engine`, `boss-brewery-sim`) into one tool. The
//! offline path and the live demo kept drifting because each owned
//! its own copy of "set up the model"; pulling that logic into one
//! library module is how the two paths stop diverging.
//!
//! [`prepare_model`] ([`model`]) is the single entry point — it
//! seeds the entire tenant (classes → Workflows → policy → data)
//! through one gateway URL. It composes the focused pieces, each a
//! pure public-API client (no DB credentials, no privileged internal
//! state — every write goes through a service behind its port):
//!
//! - [`publish_workflows`] (`boss_jobs::bootstrap`, shared with the
//!   used-device-shop tenant) — open one `workflow-design` Job per
//!   brewery Workflow and walk it to closure so the spec lands in
//!   the registry with full provenance.
//! - [`seed_tenant_data`] ([`tenant_data`]) — POST the brewery's
//!   operators, accounts, vendors, employees, catalog, assets, and
//!   opening balances, and prime the clock to the sim epoch first.
//! - policy grants come from [`boss_policy::bootstrap`], the same
//!   impl the `boss-policy-bootstrap` binary drives.
//!
//! All are idempotent: re-running after a partial failure resumes
//! cleanly (409-tolerant POSTs, deterministic opening-balance
//! `source_id`s). The thin `boss-brewery-bootstrap` /
//! `boss-brewery-data-seed` binaries are CLI shells over these fns.

pub mod model;
pub mod tenant_data;

pub use boss_jobs::bootstrap::publish_workflows;
pub use model::prepare_model;
pub use tenant_data::{SeedBases, seed_tenant_data};
