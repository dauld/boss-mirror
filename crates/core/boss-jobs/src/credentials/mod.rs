//! Credentials registry — KNOWLEDGE about credentials as registry
//! data (packet 7ee101aa, second leg).
//!
//! Possession of a credential lives in Secrets and token files, where
//! it always has. What this registry holds is everything else: kind,
//! issuer, principal, scopes, where the value is stored, who consumes
//! it, and its rotation posture — so that "what can this token do?"
//! is a lookup, not an experiment. On 2026-09-02 an admin token went
//! half-used for days because its scope lived in nobody's head but
//! David's, and a 403 was how an agent learned /user was out of
//! scope. That question is now `GET /api/credentials/{id}`.
//!
//! THE ONE RULE: a row carries LOCATIONS, never contents. No secret
//! value ever enters this module, its table, or its HTTP surface.
//!
//! Readers: `boss credential list` (boss-cli), the weekly
//! forge-token-audit (compares live forge tokens against rows of kind
//! `forgejo-access-token`, both directions), and any agent asking a
//! scope question. Writes are migrations and the rotation path
//! (`rotated_at`/`notes`); the mutability decision is written down in
//! `infra/postgres/schema/202609031700-credentials-are-registry-rows.sql`.
//! The rotation path's write is the one HTTP write on this surface —
//! `POST /api/credentials/{id}/rotation/{phase}` — because the broker
//! is a dispatcher handler and handlers own no database: each phase
//! records a `credential.minted` / `.installed` / `.verified` /
//! `.revoked` event (declared in `event_kinds`, source `jobs`), and
//! the install phase stamps `rotated_at` in the same transaction.
//!
//! Hexagonal: port trait + Pg adapter + in-memory adapter + HTTP
//! door, the same shape as `delivery` and `cadence`.

pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod types;

pub use in_memory::InMemoryCredentials;
pub use port::{CredentialsError, CredentialsRegistry};
#[cfg(feature = "postgres")]
pub use postgres::PgCredentials;
pub use types::{CredentialRow, RotationPhase};
