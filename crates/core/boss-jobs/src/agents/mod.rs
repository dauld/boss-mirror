//! The agents registry and the door that resolves an agent's login.
//!
//! WHY (backlog adf025df; design 6fda05ae, decided 2026-09-15). One
//! actor had two spellings: `claude@algedonic.dev` on 142 open-packet
//! step assignments and 12 completions, `claude:opus-5[1m]` on every
//! agent_runs row, and nothing relating them — so "what did this CPU
//! build, and what did it cost", the question agent_runs exists to
//! answer, could not be asked. The address is the ONE line in
//! `~/.config/boss/actor` on the dev pod, read by boss-cli's
//! identity.rs and by `infra/dev/boss-api` as the id every verb signs
//! with; it reached `assignee_id` through the claim path
//! (`assignee_id = user.id`) and `completed_by` through the ambient
//! actor, both generic doors.
//!
//! A human never reaches a step as an address: boss-gateway's oidc.rs
//! resolves the IdP's email to an `emp-*` id through the People domain
//! before any write is signed, failing closed when no employee matches.
//! An agent had no such resolution because there was no registry to
//! resolve against. This module is that registry and that resolution:
//!
//!   - [`port::AgentsRegistry`] — the one question the door asks:
//!     which registered actor does this login sign as?
//!   - [`door::LoginDoor`] — an axum layer, placed OUTSIDE the
//!     request-context middleware in `boss_jobs_api.rs`, that rewrites
//!     `X-Boss-User.id` from the alias to the agent id before the
//!     extractor, the ambient actor, the claim path or the completion
//!     stamp read it. One door, every handler.
//!
//! THE WINDOW IS OPEN. The decision refuses an unregistered agent login
//! the way oidc.rs refuses an unmatched human — AFTER a migration
//! window of one release. This car opens the window: an address no
//! alias maps is admitted exactly as before and COUNTED, one
//! [`door::UNRESOLVED_LOGIN`] event per admitted write naming the
//! login, method and path. The count since landing is what lets the
//! next car close the window knowing it refuses nothing live.
//!
//! Hexagonal, the same shape as `crate::credentials`: port + in-memory
//! double + Pg adapter behind the `postgres` feature, plus (since
//! backlog f56155f0, 2026-09-17) the HTTP door — `GET /api/agents` and
//! the tenant batch `POST /api/agents/batch` — and the one TOML loader
//! `boss tenant check` and `boss tenant publish` share
//! ([`seed::load_agents_toml`]). Until that car the registry's rows
//! arrived by migration only (the rate-card precedent: a wrong identity
//! is harder to notice than a missing one), which left the company's
//! own agent declared nowhere the product read; the batch is
//! insert-if-absent by id, so a migration-registered row is kept and
//! the publish names any field the tenant's declaration differs on.

pub mod door;
pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod seed;
pub mod types;

pub use door::{LoginDoor, Resolution, UNRESOLVED_LOGIN, decide, resolve_login};
pub use in_memory::InMemoryAgents;
pub use port::{AgentsError, AgentsRegistry};
#[cfg(feature = "postgres")]
pub use postgres::PgAgents;
pub use seed::{load_agents_toml, parse_agents_toml};
pub use types::{AgentInput, AgentRow, AgentsBatchOutcome, KeptAgent, validate_agent};
